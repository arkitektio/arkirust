//! State history in SQLite (port of `rekuest.contrib.sql_lite`).
//!
//! The same store is the agent's [`Sink`] (it records sessions, snapshots
//! and patches as they are published) and the routes' retriever (it answers
//! "what did the state look like at revision N").
//!
//! The schema, the SQL and the time format are those of the Python sink, so
//! a database written by one can be read by the other. An in-memory store
//! (`HistoryStore::memory()`) uses the same code on `:memory:`.
//!
//! The store also keeps the agent's [journal](crate::journal): every task
//! event, lock change and state message in one order (`journal` table). It
//! is written by the [`Journal`](crate::journal::Journal) and read by the
//! journal routes.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Map, Value};

pub use crate::journal::iso_from_ms;
use crate::journal::now_ms;
use crate::journal::{Fold, JournalEntry, JournalSink};
use crate::state::{apply_op, PublishedPatch, Sink};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS state_snapshots (
    state_id TEXT NOT NULL,
    global_revision INTEGER NOT NULL,
    event_time INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    state_data TEXT NOT NULL,
    PRIMARY KEY (state_id, global_revision, session_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
CREATE TABLE IF NOT EXISTS state_patches (
    state_id TEXT NOT NULL,
    global_current_rev INTEGER NOT NULL,
    global_future_rev INTEGER NOT NULL,
    event_time INTEGER NOT NULL,
    correlation_id TEXT,
    session_id TEXT NOT NULL,
    op TEXT NOT NULL,
    path TEXT NOT NULL,
    value TEXT,
    PRIMARY KEY (state_id, global_current_rev, session_id),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id),
    CHECK (global_future_rev = global_current_rev + 1)
);
CREATE INDEX IF NOT EXISTS idx_patches_state_time ON state_patches(state_id, event_time);
CREATE INDEX IF NOT EXISTS idx_snapshots_state_time ON state_snapshots(state_id, event_time);
CREATE INDEX IF NOT EXISTS idx_patches_correlation ON state_patches(state_id, correlation_id);
CREATE INDEX IF NOT EXISTS idx_patches_session ON state_patches(state_id, session_id);
CREATE TABLE IF NOT EXISTS journal (
    session_id TEXT NOT NULL,
    pos INTEGER NOT NULL,
    global_rev INTEGER NOT NULL,
    event_time INTEGER NOT NULL,
    kind TEXT NOT NULL,
    task_id TEXT,
    action_key TEXT,
    subject TEXT,
    message_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (session_id, pos),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
CREATE INDEX IF NOT EXISTS idx_journal_task ON journal(task_id, session_id, pos);
CREATE INDEX IF NOT EXISTS idx_journal_time ON journal(session_id, event_time);
CREATE INDEX IF NOT EXISTS idx_journal_kind ON journal(session_id, kind, pos);
CREATE TABLE IF NOT EXISTS journal_sync (
    session_id TEXT PRIMARY KEY,
    acked_pos INTEGER NOT NULL DEFAULT 0
);
";

/// Columns added after the first release; older databases are migrated.
const MIGRATIONS: &[(&str, &str, &str)] = &[
    (
        "state_snapshots",
        "global_revision",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "state_patches",
        "global_current_rev",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "state_patches",
        "global_future_rev",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("journal", "step", "INTEGER"),
];

/// A state's value at a revision.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    pub timepoint: String,
    pub data: Value,
    pub global_revision: i64,
    pub session_id: String,
}

/// One recorded patch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PatchEvent {
    pub timepoint: String,
    pub state_id: String,
    pub global_current_rev: i64,
    pub global_future_rev: i64,
    pub correlation_id: String,
    pub session_id: String,
    pub patch: Value,
}

/// The revisions a task's patches span.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskBoundary {
    pub correlation_id: String,
    pub start_global_revision: i64,
    pub end_global_revision: i64,
    pub start_time: String,
    pub end_time: String,
}

/// The revisions a session's patches span.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionBoundary {
    pub session_id: String,
    pub start_global_revision: i64,
    pub end_global_revision: i64,
    pub start_time: String,
    pub end_time: String,
}

/// One state (`Single`) or every state (`Many`) at a revision.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum StateAt {
    Single(Snapshot),
    Many(Vec<Snapshot>),
}

fn patch_document(op: &str, path: &str, value: Option<Value>) -> Value {
    let mut doc = Map::new();
    doc.insert("op".into(), Value::String(op.into()));
    doc.insert("path".into(), Value::String(path.into()));
    if op != "remove" {
        doc.insert("value".into(), value.unwrap_or(Value::Null));
    }
    Value::Object(doc)
}

/// Replay `patches` onto `anchor`; the result is stamped with the last patch.
fn replay(anchor: Snapshot, patches: &[PatchEvent]) -> Snapshot {
    let mut data = anchor.data;
    for event in patches {
        let op = event.patch["op"].as_str().unwrap_or_default();
        let path = event.patch["path"].as_str().unwrap_or_default();
        apply_op(
            &mut data,
            op,
            path,
            event.patch.get("value").unwrap_or(&Value::Null),
        );
    }
    match patches.last() {
        Some(last) => Snapshot {
            timepoint: last.timepoint.clone(),
            data,
            global_revision: last.global_future_rev,
            session_id: last.session_id.clone(),
        },
        None => Snapshot {
            timepoint: anchor.timepoint,
            data,
            global_revision: anchor.global_revision,
            session_id: anchor.session_id,
        },
    }
}

const PATCH_COLUMNS: &str =
    "state_id, global_current_rev, global_future_rev, event_time, correlation_id, session_id, op, path, value";

fn patch_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PatchEvent> {
    let value: Option<String> = row.get(8)?;
    let op: String = row.get(6)?;
    let path: String = row.get(7)?;
    Ok(PatchEvent {
        state_id: row.get(0)?,
        global_current_rev: row.get(1)?,
        global_future_rev: row.get(2)?,
        timepoint: iso_from_ms(row.get(3)?),
        correlation_id: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        session_id: row.get(5)?,
        patch: patch_document(
            &op,
            &path,
            value.and_then(|v| serde_json::from_str(&v).ok()),
        ),
    })
}

/// SQLite-backed state history. Cheap to clone.
#[derive(Clone)]
pub struct HistoryStore {
    conn: Arc<Mutex<Connection>>,
    current_session: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for HistoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStore").finish_non_exhaustive()
    }
}

impl HistoryStore {
    /// Open (or create) a database file.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::from_connection(Connection::open(path)?)
    }

    /// A store that lives only as long as the process.
    pub fn memory() -> anyhow::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> anyhow::Result<Self> {
        // Several processes may share a file (an app and its restarted self).
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        for (table, column, definition) in MIGRATIONS {
            let exists = {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
                let names: Vec<String> = stmt
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(Result::ok)
                    .collect();
                names.iter().any(|name| name == column)
            };
            if !exists {
                conn.execute_batch(&format!(
                    "ALTER TABLE {table} ADD COLUMN {column} {definition}"
                ))?;
            }
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            current_session: Arc::default(),
        })
    }

    /// Run `f` on the connection off the async runtime.
    async fn with<R, F>(&self, f: F) -> anyhow::Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<R> + Send + 'static,
    {
        let conn = self.conn.clone();
        Ok(tokio::task::spawn_blocking(move || f(&conn.lock().expect("sqlite lock"))).await??)
    }

    fn session_or_current(&self, session_id: Option<&str>) -> Option<String> {
        session_id
            .map(str::to_owned)
            .or_else(|| self.current_session.lock().expect("session lock").clone())
    }

    async fn boundaries(
        &self,
        column: &'static str,
        id: String,
        state_id: Option<String>,
    ) -> anyhow::Result<Option<(i64, i64, i64, i64)>> {
        self.with(move |conn| {
            let state_filter = if state_id.is_some() { " AND state_id = ?2" } else { "" };
            let sql = format!(
                "SELECT MIN(global_current_rev), MAX(global_future_rev), MIN(event_time), MAX(event_time) \
                 FROM state_patches WHERE {column} = ?1{state_filter}"
            );
            let mut stmt = conn.prepare(&sql)?;
            let map = |row: &rusqlite::Row<'_>| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            };
            let row = match &state_id {
                Some(state) => stmt.query_row(params![id, state], map)?,
                None => stmt.query_row(params![id], map)?,
            };
            Ok(match row {
                (Some(a), Some(b), Some(c), Some(d)) => Some((a, b, c, d)),
                _ => None,
            })
        })
        .await
    }

    pub async fn task_boundaries(
        &self,
        correlation_id: &str,
        state_id: Option<&str>,
    ) -> anyhow::Result<Option<TaskBoundary>> {
        let id = correlation_id.to_owned();
        Ok(self
            .boundaries("correlation_id", id.clone(), state_id.map(str::to_owned))
            .await?
            .map(|(start, end, t0, t1)| TaskBoundary {
                correlation_id: id,
                start_global_revision: start,
                end_global_revision: end,
                start_time: iso_from_ms(t0),
                end_time: iso_from_ms(t1),
            }))
    }

    pub async fn session_boundaries(
        &self,
        session_id: &str,
        state_id: Option<&str>,
    ) -> anyhow::Result<Option<SessionBoundary>> {
        let id = session_id.to_owned();
        Ok(self
            .boundaries("session_id", id.clone(), state_id.map(str::to_owned))
            .await?
            .map(|(start, end, t0, t1)| SessionBoundary {
                session_id: id,
                start_global_revision: start,
                end_global_revision: end,
                start_time: iso_from_ms(t0),
                end_time: iso_from_ms(t1),
            }))
    }

    /// Patches from `after` on (the patch `after -> after+1` included).
    pub async fn forward_events(
        &self,
        after: i64,
        state_id: Option<&str>,
        session_id: Option<&str>,
        count: i64,
    ) -> anyhow::Result<Vec<PatchEvent>> {
        let (state_id, session_id) = (state_id.map(str::to_owned), session_id.map(str::to_owned));
        self.with(move |conn| {
            let mut sql =
                format!("SELECT {PATCH_COLUMNS} FROM state_patches WHERE global_current_rev >= ?");
            let mut args: Vec<rusqlite::types::Value> = vec![after.into()];
            if let Some(state) = state_id {
                sql.push_str(" AND state_id = ?");
                args.push(state.into());
            }
            if let Some(session) = session_id {
                sql.push_str(" AND session_id = ?");
                args.push(session.into());
            }
            sql.push_str(" ORDER BY global_current_rev ASC, state_id ASC LIMIT ?");
            args.push(count.into());
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args), patch_from_row)?;
            rows.collect()
        })
        .await
    }

    /// Patches within `[from, to]`. `state_ids: None` (or empty) means every state.
    pub async fn between(
        &self,
        from: i64,
        to: i64,
        state_ids: Option<Vec<String>>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Vec<PatchEvent>> {
        if to < from {
            return Ok(vec![]);
        }
        let session_id = session_id.map(str::to_owned);
        self.with(move |conn| {
            let mut sql = format!(
                "SELECT {PATCH_COLUMNS} FROM state_patches WHERE global_current_rev >= ? AND global_future_rev <= ?"
            );
            let mut args: Vec<rusqlite::types::Value> = vec![from.into(), to.into()];
            if let Some(states) = state_ids.filter(|s| !s.is_empty()) {
                sql.push_str(&format!(" AND state_id IN ({})", vec!["?"; states.len()].join(", ")));
                args.extend(states.into_iter().map(Into::into));
            }
            if let Some(session) = session_id {
                sql.push_str(" AND session_id = ?");
                args.push(session.into());
            }
            sql.push_str(" ORDER BY global_current_rev ASC, state_id ASC");
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args), patch_from_row)?;
            rows.collect()
        })
        .await
    }

    fn state_ids(conn: &Connection, session_id: Option<&str>) -> rusqlite::Result<Vec<String>> {
        let mut ids = std::collections::BTreeSet::new();
        for table in ["state_snapshots", "state_patches"] {
            let (sql, args): (String, Vec<rusqlite::types::Value>) = match session_id {
                Some(session) => (
                    format!("SELECT DISTINCT state_id FROM {table} WHERE session_id = ?"),
                    vec![session.to_owned().into()],
                ),
                None => (format!("SELECT DISTINCT state_id FROM {table}"), vec![]),
            };
            let mut stmt = conn.prepare(&sql)?;
            for id in stmt.query_map(rusqlite::params_from_iter(args), |row| {
                row.get::<_, String>(0)
            })? {
                ids.insert(id?);
            }
        }
        Ok(ids.into_iter().collect())
    }

    fn state_at_revision(
        conn: &Connection,
        revision: i64,
        state_id: &str,
        session_id: Option<&str>,
    ) -> rusqlite::Result<Option<Snapshot>> {
        let session_filter = if session_id.is_some() {
            " AND session_id = ?3"
        } else {
            ""
        };
        let sql = format!(
            "SELECT global_revision, event_time, session_id, state_data FROM state_snapshots \
             WHERE state_id = ?1 AND global_revision <= ?2{session_filter} ORDER BY global_revision DESC LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let map = |row: &rusqlite::Row<'_>| {
            let data: String = row.get(3)?;
            Ok(Snapshot {
                global_revision: row.get(0)?,
                timepoint: iso_from_ms(row.get(1)?),
                session_id: row.get(2)?,
                data: serde_json::from_str(&data).unwrap_or(Value::Null),
            })
        };
        let anchor = match session_id {
            Some(session) => stmt
                .query_row(params![state_id, revision, session], map)
                .optional()?,
            None => stmt
                .query_row(params![state_id, revision], map)
                .optional()?,
        };
        let Some(anchor) = anchor else {
            return Ok(None);
        };

        let sql = format!(
            "SELECT {PATCH_COLUMNS} FROM state_patches WHERE state_id = ?1 AND global_current_rev >= ?2 \
             AND global_future_rev <= ?3{} ORDER BY global_current_rev ASC",
            if session_id.is_some() { " AND session_id = ?4" } else { "" }
        );
        let mut stmt = conn.prepare(&sql)?;
        let patches: Vec<PatchEvent> = match session_id {
            Some(session) => stmt
                .query_map(
                    params![state_id, anchor.global_revision, revision, session],
                    patch_from_row,
                )?
                .collect::<rusqlite::Result<_>>()?,
            None => stmt
                .query_map(
                    params![state_id, anchor.global_revision, revision],
                    patch_from_row,
                )?
                .collect::<rusqlite::Result<_>>()?,
        };
        Ok(Some(replay(anchor, &patches)))
    }

    /// One state (with `state_id`) or every state at `revision`.
    pub async fn state_at(
        &self,
        revision: i64,
        state_id: Option<&str>,
        session_id: Option<&str>,
    ) -> anyhow::Result<Option<StateAt>> {
        let (state_id, session_id) = (state_id.map(str::to_owned), session_id.map(str::to_owned));
        self.with(move |conn| {
            Ok(match state_id {
                Some(state) => {
                    Self::state_at_revision(conn, revision, &state, session_id.as_deref())?
                        .map(StateAt::Single)
                }
                None => {
                    let mut all = vec![];
                    for state in Self::state_ids(conn, session_id.as_deref())? {
                        if let Some(snapshot) =
                            Self::state_at_revision(conn, revision, &state, session_id.as_deref())?
                        {
                            all.push(snapshot);
                        }
                    }
                    Some(StateAt::Many(all))
                }
            })
        })
        .await
    }

    /// Up to `before` snapshots at or before `revision` and `after` after it, per state.
    pub async fn snapshots_around(
        &self,
        revision: i64,
        state_id: Option<&str>,
        session_id: Option<&str>,
        before: i64,
        after: i64,
    ) -> anyhow::Result<Vec<Snapshot>> {
        let (state_id, session_id) = (state_id.map(str::to_owned), session_id.map(str::to_owned));
        self.with(move |conn| {
            let states = match state_id {
                Some(state) => vec![state],
                None => Self::state_ids(conn, session_id.as_deref())?,
            };
            let session_filter = if session_id.is_some() { " AND session_id = ?4" } else { "" };
            let map = |row: &rusqlite::Row<'_>| {
                let data: String = row.get(3)?;
                Ok(Snapshot {
                    global_revision: row.get(0)?,
                    timepoint: iso_from_ms(row.get(1)?),
                    session_id: row.get(2)?,
                    data: serde_json::from_str(&data).unwrap_or(Value::Null),
                })
            };
            let mut out = vec![];
            for state in states {
                for (cmp, order, limit, reverse) in [("<=", "DESC", before, true), (">", "ASC", after, false)] {
                    let sql = format!(
                        "SELECT global_revision, event_time, session_id, state_data FROM state_snapshots \
                         WHERE state_id = ?1 AND global_revision {cmp} ?2{session_filter} \
                         ORDER BY global_revision {order} LIMIT ?3"
                    );
                    let mut stmt = conn.prepare(&sql)?;
                    let mut rows: Vec<Snapshot> = match &session_id {
                        Some(session) => stmt
                            .query_map(params![state, revision, limit, session], map)?
                            .collect::<rusqlite::Result<_>>()?,
                        None => stmt
                            .query_map(params![state, revision, limit], map)?
                            .collect::<rusqlite::Result<_>>()?,
                    };
                    if reverse {
                        rows.reverse();
                    }
                    out.extend(rows);
                }
            }
            Ok(out)
        })
        .await
    }
}

#[async_trait]
impl Sink for HistoryStore {
    async fn create_session(&self) -> anyhow::Result<String> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let id = session_id.clone();
        self.with(move |conn| {
            conn.execute(
                "INSERT INTO sessions (session_id, created_at) VALUES (?, ?)",
                params![id, now_ms()],
            )
        })
        .await?;
        *self.current_session.lock().expect("session lock") = Some(session_id.clone());
        Ok(session_id)
    }

    async fn dump_snapshot(
        &self,
        session_id: &str,
        global_rev: u64,
        snapshots: &Map<String, Value>,
    ) -> anyhow::Result<()> {
        let session_id = self
            .session_or_current(Some(session_id))
            .unwrap_or_default();
        let rows: Vec<(String, String)> = snapshots
            .iter()
            .map(|(state, data)| (state.clone(), data.to_string()))
            .collect();
        self.with(move |conn| {
            let now = now_ms();
            for (state, data) in rows {
                conn.execute(
                    "INSERT OR IGNORE INTO state_snapshots (state_id, global_revision, event_time, session_id, state_data) \
                     VALUES (?, ?, ?, ?, ?)",
                    params![state, global_rev as i64, now, session_id, data],
                )?;
            }
            Ok(())
        })
        .await
    }

    async fn write_patch(&self, patch: &PublishedPatch) -> anyhow::Result<()> {
        let patch = patch.clone();
        let session_id = self
            .session_or_current(Some(&patch.session_id))
            .unwrap_or_default();
        self.with(move |conn| {
            conn.execute(
                "INSERT INTO state_patches (state_id, global_current_rev, global_future_rev, event_time, \
                 correlation_id, session_id, op, path, value) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    patch.state_name,
                    patch.global_rev as i64 - 1,
                    patch.global_rev as i64,
                    (patch.ts * 1000.0) as i64,
                    patch.task_id,
                    session_id,
                    patch.op,
                    patch.path,
                    (!patch.value.is_null()).then(|| patch.value.to_string()),
                ],
            )
        })
        .await
        .map(|_| ())
    }

    async fn is_caught_up_to(&self, global_rev: u64) -> anyhow::Result<bool> {
        let session = self.session_or_current(None);
        let max: Option<i64> = self
            .with(move |conn| match session {
                Some(session) => conn.query_row(
                    "SELECT MAX(global_future_rev) FROM state_patches WHERE session_id = ?",
                    params![session],
                    |row| row.get(0),
                ),
                None => conn.query_row(
                    "SELECT MAX(global_future_rev) FROM state_patches",
                    [],
                    |row| row.get(0),
                ),
            })
            .await?;
        Ok(max.unwrap_or(0) >= global_rev as i64)
    }
}

#[async_trait]
impl JournalSink for HistoryStore {
    async fn write_entries(&self, entries: &[Arc<JournalEntry>]) -> anyhow::Result<()> {
        let entries = entries.to_vec();
        self.with(move |conn| {
            let tx = conn.unchecked_transaction()?;
            if let Some(first) = entries.first() {
                // A remote agent's sessions are not created by the store; order them by first entry.
                tx.execute(
                    "INSERT OR IGNORE INTO sessions (session_id, created_at) VALUES (?, ?)",
                    params![first.session_id, first.event_time],
                )?;
            }
            {
                let mut stmt = tx.prepare_cached(&format!(
                    "INSERT OR IGNORE INTO journal ({JOURNAL_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
                ))?;
                for e in &entries {
                    stmt.execute(params![
                        e.session_id,
                        e.pos as i64,
                        e.global_rev as i64,
                        e.event_time,
                        e.kind,
                        e.task_id,
                        e.action_key,
                        e.subject,
                        e.message_id,
                        e.payload.to_string(),
                        e.step.map(|s| s as i64),
                    ])?;
                }
            }
            tx.commit()
        })
        .await
    }
}

const JOURNAL_COLUMNS: &str =
    "session_id, pos, global_rev, event_time, kind, task_id, action_key, subject, message_id, payload, step";

/// Kinds that belong to the whole session rather than to a task, state or lock.
const SESSION_KINDS: &str = "'SESSION_INIT', 'STATE_SNAPSHOT'";
/// Kinds that carry state values.
const STATE_KINDS: &str = "'SESSION_INIT', 'STATE_SNAPSHOT', 'STATE_PATCH'";

fn entry_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JournalEntry> {
    let event_time: i64 = row.get(3)?;
    let payload: String = row.get(9)?;
    Ok(JournalEntry {
        session_id: row.get(0)?,
        pos: row.get::<_, i64>(1)? as u64,
        global_rev: row.get::<_, i64>(2)? as u64,
        timepoint: iso_from_ms(event_time),
        event_time,
        kind: row.get(4)?,
        task_id: row.get(5)?,
        step: row.get::<_, Option<i64>>(10)?.map(|s| s as u64),
        action_key: row.get(6)?,
        subject: row.get(7)?,
        message_id: row.get(8)?,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
    })
}

/// Which journal entries to read. Key filters route as the websocket does.
#[derive(Debug, Clone, Default)]
pub struct EntryQuery {
    /// Entries after this position.
    pub after: u64,
    /// Up to and including this position.
    pub until: Option<u64>,
    pub limit: Option<u64>,
    pub kinds: Option<Vec<String>>,
    pub task_id: Option<String>,
    pub action_keys: Option<Vec<String>>,
    pub state_keys: Option<Vec<String>>,
    pub lock_keys: Option<Vec<String>>,
}

fn in_list(column: &str, values: &[String], args: &mut Vec<rusqlite::types::Value>) -> String {
    args.extend(values.iter().cloned().map(Into::into));
    format!("{column} IN ({})", vec!["?"; values.len()].join(", "))
}

impl HistoryStore {
    /// The server has persisted everything of `session_id` up to `pos`. Never lowers it.
    pub async fn set_acked(&self, session_id: &str, pos: u64) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        self.with(move |conn| {
            conn.execute(
                "INSERT INTO journal_sync (session_id, acked_pos) VALUES (?1, ?2) \
                 ON CONFLICT(session_id) DO UPDATE SET acked_pos = MAX(acked_pos, excluded.acked_pos)",
                params![session_id, pos as i64],
            )
        })
        .await
        .map(|_| ())
    }

    /// Entries the server has not acknowledged, of every session but `except`,
    /// oldest session first, each in order.
    pub async fn unacked_entries(&self, except: Option<&str>) -> anyhow::Result<Vec<JournalEntry>> {
        let except = except.map(str::to_owned).unwrap_or_default();
        self.with(move |conn| {
            let columns = JOURNAL_COLUMNS
                .split(", ")
                .map(|c| format!("j.{c}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = conn.prepare(&format!(
                "SELECT {columns} FROM journal j \
                 LEFT JOIN journal_sync y ON y.session_id = j.session_id \
                 LEFT JOIN sessions s ON s.session_id = j.session_id \
                 WHERE j.session_id != ?1 AND j.pos > COALESCE(y.acked_pos, 0) \
                 ORDER BY COALESCE(s.created_at, 0), j.session_id, j.pos"
            ))?;
            let rows = stmt.query_map(params![except], entry_from_row)?;
            rows.collect()
        })
        .await
    }

    /// Delete acknowledged entries recorded before `before_ms`. Returns how many.
    pub async fn prune_acked(&self, before_ms: i64) -> anyhow::Result<usize> {
        self.with(move |conn| {
            conn.execute(
                "DELETE FROM journal WHERE event_time < ?1 AND pos <= \
                 COALESCE((SELECT acked_pos FROM journal_sync y WHERE y.session_id = journal.session_id), 0)",
                params![before_ms],
            )
        })
        .await
    }

    /// Journal entries of a session, in order.
    pub async fn journal_entries(
        &self,
        session_id: &str,
        query: EntryQuery,
    ) -> anyhow::Result<Vec<JournalEntry>> {
        let session_id = session_id.to_owned();
        self.with(move |conn| {
            let mut args: Vec<rusqlite::types::Value> = vec![session_id.into(), (query.after as i64).into()];
            let mut sql = format!("SELECT {JOURNAL_COLUMNS} FROM journal WHERE session_id = ? AND pos > ?");
            if let Some(until) = query.until {
                sql.push_str(" AND pos <= ?");
                args.push((until as i64).into());
            }
            if let Some(kinds) = query.kinds.as_ref().filter(|k| !k.is_empty()) {
                sql.push_str(&format!(" AND {}", in_list("kind", kinds, &mut args)));
            }
            if let Some(task) = query.task_id {
                sql.push_str(" AND task_id = ?");
                args.push(task.into());
            }
            if query.action_keys.is_some() || query.state_keys.is_some() || query.lock_keys.is_some() {
                let keyed = |column: &str, keys: &Option<Vec<String>>, args: &mut Vec<rusqlite::types::Value>| match keys {
                    Some(keys) if !keys.is_empty() => in_list(column, keys, args),
                    _ => "1".to_owned(),
                };
                let state = keyed("subject", &query.state_keys, &mut args);
                let lock = keyed("subject", &query.lock_keys, &mut args);
                let action = keyed("action_key", &query.action_keys, &mut args);
                sql.push_str(&format!(
                    " AND ((kind = 'STATE_PATCH' AND {state}) OR (kind IN ('LOCK', 'UNLOCK') AND {lock}) \
                     OR kind IN ({SESSION_KINDS}) OR (kind NOT IN ('STATE_PATCH', 'LOCK', 'UNLOCK', {SESSION_KINDS}) \
                     AND (action_key IS NULL OR {action})))"
                ));
            }
            sql.push_str(" ORDER BY pos ASC");
            if let Some(limit) = query.limit {
                sql.push_str(" LIMIT ?");
                args.push((limit as i64).into());
            }
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args), entry_from_row)?;
            rows.collect()
        })
        .await
    }

    /// Every entry of one task, in order.
    pub async fn journal_task_entries(&self, task_id: &str) -> anyhow::Result<Vec<JournalEntry>> {
        let task_id = task_id.to_owned();
        self.with(move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOURNAL_COLUMNS} FROM journal WHERE task_id = ? ORDER BY session_id, pos"
            ))?;
            let rows = stmt.query_map(params![task_id], entry_from_row)?;
            rows.collect()
        })
        .await
    }

    /// The last position of a session at or before `ms` (epoch milliseconds).
    pub async fn journal_pos_at_time(
        &self,
        session_id: &str,
        ms: i64,
    ) -> anyhow::Result<Option<u64>> {
        let session_id = session_id.to_owned();
        self.with(move |conn| {
            conn.query_row(
                "SELECT MAX(pos) FROM journal WHERE session_id = ? AND event_time <= ?",
                params![session_id, ms],
                |row| row.get::<_, Option<i64>>(0),
            )
        })
        .await
        .map(|pos| pos.map(|p| p as u64))
    }

    /// The last position of a session.
    pub async fn journal_last_pos(&self, session_id: &str) -> anyhow::Result<Option<u64>> {
        let session_id = session_id.to_owned();
        self.with(move |conn| {
            conn.query_row(
                "SELECT MAX(pos) FROM journal WHERE session_id = ?",
                params![session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
        })
        .await
        .map(|pos| pos.map(|p| p as u64))
    }

    /// The entry at `pos` and the world as of it: states replayed from the
    /// last snapshot entry, tasks and locks folded from every entry before.
    pub async fn journal_world(
        &self,
        session_id: &str,
        pos: u64,
    ) -> anyhow::Result<Option<(JournalEntry, Fold)>> {
        let session_id = session_id.to_owned();
        self.with(move |conn| {
            let at = conn
                .query_row(
                    &format!("SELECT {JOURNAL_COLUMNS} FROM journal WHERE session_id = ? AND pos = ?"),
                    params![session_id, pos as i64],
                    entry_from_row,
                )
                .optional()?;
            let Some(at) = at else { return Ok(None) };
            let anchor: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COALESCE(MAX(pos), 0) FROM journal WHERE session_id = ? AND pos <= ? AND kind IN ({SESSION_KINDS})"
                    ),
                    params![session_id, pos as i64],
                    |row| row.get(0),
                )?;
            let mut fold = Fold::default();
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOURNAL_COLUMNS} FROM journal WHERE session_id = ?1 AND pos <= ?2 \
                 AND ((kind IN ({STATE_KINDS}) AND pos >= ?3) OR kind NOT IN ({STATE_KINDS})) ORDER BY pos ASC"
            ))?;
            for entry in stmt.query_map(params![session_id, pos as i64, anchor], entry_from_row)? {
                fold.apply(&entry?);
            }
            Ok(Some((at, fold)))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch(
        session: &str,
        rev: u64,
        op: &str,
        path: &str,
        value: Value,
        task: &str,
    ) -> PublishedPatch {
        PublishedPatch {
            session_id: session.into(),
            global_rev: rev,
            state_name: "Camera".into(),
            ts: 1_700_000_000.0 + rev as f64,
            op: op.into(),
            path: path.into(),
            value,
            task_id: Some(task.into()),
        }
    }

    #[tokio::test]
    async fn records_and_replays() {
        let store = HistoryStore::memory().unwrap();
        let session = store.create_session().await.unwrap();
        let mut baseline = Map::new();
        baseline.insert("Camera".into(), json!({"exposure": 1, "tags": []}));
        store.dump_snapshot(&session, 0, &baseline).await.unwrap();
        store
            .write_patch(&patch(&session, 1, "replace", "/exposure", json!(2), "t1"))
            .await
            .unwrap();
        store
            .write_patch(&patch(&session, 2, "add", "/tags/0", json!("a"), "t2"))
            .await
            .unwrap();
        store
            .write_patch(&patch(&session, 3, "remove", "/tags/0", Value::Null, "t2"))
            .await
            .unwrap();

        let Some(StateAt::Single(at2)) = store
            .state_at(2, Some("Camera"), Some(&session))
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(at2.data, json!({"exposure": 2, "tags": ["a"]}));
        assert_eq!(at2.global_revision, 2);
        assert_eq!(at2.timepoint, "2023-11-14T22:13:22Z");

        let Some(StateAt::Many(all)) = store.state_at(3, None, Some(&session)).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(all[0].data, json!({"exposure": 2, "tags": []}));

        let between = store.between(0, 2, None, Some(&session)).await.unwrap();
        assert_eq!(between.len(), 2);
        assert_eq!(
            between[1].patch,
            json!({"op": "add", "path": "/tags/0", "value": "a"})
        );
        let forward = store
            .forward_events(2, None, Some(&session), 100)
            .await
            .unwrap();
        assert_eq!(forward.len(), 1, "after 2 is exactly the patch 2 -> 3");
        assert_eq!(forward[0].patch, json!({"op": "remove", "path": "/tags/0"}));

        let task = store.task_boundaries("t2", None).await.unwrap().unwrap();
        assert_eq!(
            (task.start_global_revision, task.end_global_revision),
            (1, 3)
        );
        assert!(store.task_boundaries("nope", None).await.unwrap().is_none());
        let bounds = store
            .session_boundaries(&session, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (bounds.start_global_revision, bounds.end_global_revision),
            (0, 3)
        );

        assert!(store.is_caught_up_to(3).await.unwrap());
        assert!(!store.is_caught_up_to(4).await.unwrap());
        let around = store
            .snapshots_around(1, None, Some(&session), 1, 1)
            .await
            .unwrap();
        assert_eq!(around.len(), 1);
    }

    #[test]
    fn iso_format() {
        assert_eq!(
            iso_from_ms(1_700_000_000_123),
            "2023-11-14T22:13:20.123000Z"
        );
        assert_eq!(iso_from_ms(1_700_000_000_000), "2023-11-14T22:13:20Z");
    }
}
