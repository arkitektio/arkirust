# The agent journal

One ordered record of everything an agent reports: task events, lock changes, state patches, snapshots, the session baseline, and (when served) the `ASSIGN` that started each task. The record is persisted, and any position in it can be replayed.

This document is the contract shared by the Rust agent (`rekuest` crate), the Python agent (`rekuest` package, `contrib.fastapi` and `contrib.sql_lite`) and the rekuest server.

## Why

Before the journal, there were two unrelated counters:

- **`global_rev`** numbered state patches and was persisted.
- **`seq`** numbered task events. It was per connection, restarted on reconnect, and was not persisted.

No key related a `YIELD` to the patches around it. On top of that, several races meant the wire order was not the causal order:

- `seq` was taken before the broadcast lock.
- Python sent task events directly but queued patches, so a `COMPLETED` could overtake its patches.
- A cancelled task could still publish a patch after its `CANCELLED`.

## Entries

| field | |
|---|---|
| `session_id` | the agent session (a new one per process start) |
| `pos` | 1, 2, 3, … per session, no gaps; `(session_id, pos)` is the durable key |
| `global_rev` | the state revision **after** this entry (patches bump it; everything else carries the current value) |
| `timepoint` / `event_time` | when the agent recorded it (ISO 8601 / epoch ms) |
| `kind` | the wire `type`: `ASSIGN`, `PROGRESS`, `LOG`, `YIELD`, `STARTED`, `PAUSED`, `RESUMED`, `COMPLETED`, `FAILED`, `CRITICAL`, `CANCELLED`, `INTERRUPTED`, `LOCK`, `UNLOCK`, `STATE_PATCH`, `STATE_SNAPSHOT`, `SESSION_INIT` |
| `task_id` | the task the entry belongs to (`STATE_PATCH`: the changing task; `UNLOCK`: the task that held the lock) |
| `action_key` | the task's action key (`interface`, else `action`, else `task`) |
| `subject` | the state (`STATE_PATCH`) or lock key (`LOCK`/`UNLOCK`) |
| `message_id` | the frame's `id`, the same one that went on the wire |
| `payload` | the frame, without the stream-level `seq` (and without `token` for `ASSIGN`) |

`REGISTER` and `HEARTBEAT_ANSWER` are not journaled. Messages before the first session are passed on unrecorded.

**The world at `pos` P:**

- **States:** the last `SESSION_INIT`/`STATE_SNAPSHOT` entry at or before P, plus the `STATE_PATCH` entries after it, up to P.
- **Tasks and locks:** every entry up to P, folded.

## Ordering rules

1. `pos` assignment, the persistence enqueue and the hand-off to the transport happen under **one lock**. Delivery order is therefore `pos` order.
2. Within a task, program order holds: `ASSIGN` < `PROGRESS`/`LOCK` < patches, `YIELD`s, … < the terminal event < `UNLOCK`.
3. **Nothing is recorded for a task after its terminal entry.** Each task has a *gate*:
   - Reports and state changes enter the gate for their synchronous duration.
   - The check happens before anything is changed, so a multi-op update is all or nothing.
   - The terminal report (at the end of a run, or on cancel/interrupt) closes the gate first. Closing refuses new entries and waits for those in flight.
   - Entering twice on one thread is allowed; for example, a log inside an update closure.
   - `LOCK`/`UNLOCK` are not gated: a lock is always released, after the end.
4. A `STATE_SNAPSHOT` at revision N comes before the patch that reaches N+1 and already contains it (as before).

## Storage (SQLite, same file as the state history)

```sql
CREATE TABLE IF NOT EXISTS journal (
    session_id TEXT NOT NULL,
    pos INTEGER NOT NULL,
    global_rev INTEGER NOT NULL,
    event_time INTEGER NOT NULL,          -- epoch milliseconds
    kind TEXT NOT NULL,
    task_id TEXT,
    action_key TEXT,
    subject TEXT,
    message_id TEXT NOT NULL,
    payload TEXT NOT NULL,                -- JSON
    PRIMARY KEY (session_id, pos),
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
CREATE INDEX IF NOT EXISTS idx_journal_task ON journal(task_id, session_id, pos);
CREATE INDEX IF NOT EXISTS idx_journal_time ON journal(session_id, event_time);
CREATE INDEX IF NOT EXISTS idx_journal_kind ON journal(session_id, kind, pos);
```

- Inserts are `INSERT OR IGNORE`, so a re-sent entry is a no-op.
- The `sessions`, `state_snapshots` and `state_patches` tables are still written as before, so the existing history routes are unchanged.

## HTTP routes (served agent)

`{session_id}` may be `current`.

| route | |
|---|---|
| `GET /journal` | the watermark `{session_id, pos, global_rev}` |
| `GET /journal/{session_id}?after=&until=&limit=&kinds=&task_id=&action_keys=&state_keys=&lock_keys=` | `{session_id, after, last_pos, entries}` in `pos` order. Key filters route like the websocket: patches by state, locks by key, session-wide entries always, the rest by action key. |
| `GET /journal/{session_id}/at/{pos}` | `{session_id, pos, global_rev, timepoint, entry, states, tasks, locks}` as of `pos` |
| `GET /journal/{session_id}/at?timestamp=` | the same at the last entry at or before a time (epoch ms or RFC 3339) |
| `GET /tasks/{task_id}/events` | `{task_id, task, entries}`. Works after the task has ended, unlike `/tasks/{task_id}`. |
| `GET /session_info` | also has `current_pos` |

**A task, folded:**
- Fields: `task`, `action_key`, `interface`, `reference`, `status`, `done`, `progress`, `message`, `error`, `yields`, `last_returns`, `first_pos`, `last_pos`.
- `status` is one of `ASSIGNED`, `RUNNING`, `PAUSED`, or the terminal kind.

**Locks** map key → holding task.

## Websocket opt-in (served agent)

A client that sends `"journal": true` in its first frame opts in:

```json
{"type": "INIT", "journal": true, "resume_after": 41, "session_id": "…", "action_keys": ["…"]}
```

- **The reply INIT** keeps Python's fields and adds a `journal` object. It is taken **under the journal lock**, so it is consistent with the watermark:
  ```json
  "journal": {"session_id": "…", "pos": 57, "global_rev": 12, "resync": false,
              "states": {…}, "tasks": {…}, "locks": {…}}
  ```
- **Every following frame** is the Python frame plus `pos` and `journal_session`.
  - The names are chosen not to clash: state frames already have `session_id`, and `STATE_PATCH` has `ts`.
  - Journal subscribers also get the session-wide frames (`SESSION_INIT`, `STATE_SNAPSHOT`) and `ASSIGN`.
- **With `resume_after: N`:** the server first sends the entries in `(N, pos]` that match the filters (from memory, else from storage). Then it sends live frames after `pos`. Nothing is missed or repeated.
  - Replayed frames have no `seq`, which is stream-level.
- **`resync: true`** is sent, with no backlog, when `N` is ahead of the watermark or `session_id` is not the current session. `session_id` is required with `resume_after` unless `N` is 0 (replay from the start): a position alone may be from before a restart. Drop local state and use the INIT.
- **A client that does not opt in** gets exactly Python's frames, as before.

## Agent ↔ server (remote agents)

- **Every journaled frame** carries `pos`, `journal_session` and `agent_ts` (seconds), in the envelope next to `id` and `seq`.
  - `REGISTER` never gets new fields, because the server forbids extras there.
  - Other frames tolerate extras, so old servers ignore the new fields.
- **`INIT`** from a journal-capable server has `"journal": true`.
- **Once the server has received a `pos`-carrying frame** on a connection, it acknowledges cumulatively with `{"type": "JOURNAL_ACK", "journal_session": "…", "pos": N}`, meaning "persisted up to N".
  - It sends no `JOURNAL_ACK` before that, because an old agent would not parse an unknown message.
- **With `journal: true` the agent retains every journaled frame** until a `JOURNAL_ACK` covers it. It re-sends them in `pos` order after reconnecting.
  - The server makes them idempotent on `(agent, journal_session, pos)`.
- **Without it**, today's behaviour applies: only terminal reports are retained, until `EVENT_ACK`.

---

# Journal v2

Version 2 adds five things: instant recording, a write-ahead journal in every mode, locally minted shelve ids, a per-task step counter, and effect entries for durable actions. Every v1 rule above still holds.

## Recording never waits

- Recording an entry is synchronous and never waits on I/O. This covers a state change, a report, a lock, an assignment, a shelve or an effect.
- Each entry is numbered under the journal lock, applied to the in-memory fold and appended to an in-memory outbox. Then the call returns.
- Delivery happens in background drains, one per sink, each in `pos` order and at its own pace:
  1. the local write-ahead journal (SQLite);
  2. the server transport (remote mode);
  3. websocket subscribers (served mode), each with its own queue.
- **Values that need I/O to serialize**, such as a structure whose shrink uploads:
  - The entry is reserved with its `pos` and `global_rev` fixed and a *pending* value.
  - A resolver fills the value in later.
  - Every drain stops at the first unresolved entry, so order still holds.

## New entry fields

| field | |
|---|---|
| `step` | For entries with a `task_id`: 1, 2, 3, … per task, over every entry of that task (ASSIGN, reports, patches, locks, shelves, effects), with no gaps. `(task_id, step)` addresses "the k-th thing task T did". |

- **On the wire:** journaled frames carry it as `task_step`, next to `pos`, `journal_session` and `agent_ts`. It is not called `step`, because `ASSIGN` already has a `step` flag.
- **Storage:** column `step INTEGER`, added to the `journal` table. Add it with `ALTER TABLE journal ADD COLUMN step INTEGER` when it is missing.

## New entry kinds

These kinds are **never** sent to legacy websocket subscribers and **never** carry `seq`.

| kind | payload (besides `type`, `id`, `task`) | meaning |
|---|---|---|
| `ASSIGN` | the assignment, without `token` | Now recorded in remote mode too, so a task's history starts with its inputs. |
| `CALL` | `effect_id`, `reference`, `action`/`interface`, `args` | The task asked for a child task. `reference` = `effect_id`. |
| `CALL_RESULT` | `effect_id`, `status` (terminal kind), `returns?`, `error?` | The child's outcome, as the task saw it. |
| `NOW` | `effect_id`, `value` (epoch seconds, float) | The task read the clock. |
| `RANDOM` | `effect_id`, `value` (hex string) | The task drew random bytes. |
| `SLEEP` | `effect_id`, `until` (epoch seconds, float) | The task slept until a deadline. |

- **`effect_id`** is `"{task}:{step}"`, where `step` is that entry's step. The journal fills it in, so it is deterministic.
- **Replay (future work):** an effect entry lets replay return the recorded result instead of running the effect again.
- **Idempotency:** `CALL`'s `reference` makes a re-issued child call after a restart idempotent on the server, because `AssignRequest` is idempotent on `(caller, reference)`.
- **Task helpers:**
  - `task.now()`, `task.random(n)`, `task.sleep(d)`
  - `task.call(...)` (where the language has a caller)
  - Each helper records its effect. Each would consult a recorded history first, which is empty until a replay engine exists.

## Shelving: the agent mints the id

- **`shelve(value)` is synchronous.**
  - It mints `resource_id = uuid4 hex`, stores the value in the agent's local shelf, and records a `SHELVE` entry. It returns `resource_id` at once.
  - The frame is the existing `SHELVE`: `{ref, identifier, resource_id, label?, description?}`, with `ref` = `resource_id`.
  - The value's reference is `{"__identifier": …, "object": resource_id}`. There is no round trip, and no `SHELVED` reply is awaited.
- **`label` / `description`** may be resolved later, as pending fields of the entry. Recording never waits on them.
- **Dropping a value** (answering `COLLECT`, or on its own): remove it from the shelf and record `UNSHELVE {ref, drawer: resource_id}`. Don't wait.
- **Served mode:** shelving works the same, with no server.
- **Server:**
  - A journaled `SHELVE` upserts the drawer on `(agent's shelve, resource_id)`, marks it agent-minted, and replies nothing.
  - For agent-minted drawers, `UNSHELVE`, `COLLECT` and the GraphQL drawer operations address the drawer by `resource_id`. Old agents keep pk references and `SHELVED` replies.
  - Drawers are cleared on `REGISTER` only when the agent's `session_id` changes. A reconnect of the same process keeps them.
- **Old servers** answer `SHELVED`, which the agent ignores, and address `COLLECT` by pk, which the agent can't resolve. This degrades gracefully.

## Write-ahead journal everywhere

- **Every agent keeps the journal in a local SQLite file**, in remote mode too. The file holds the `journal` table plus:
  ```sql
  CREATE TABLE IF NOT EXISTS journal_sync (
      session_id TEXT PRIMARY KEY,
      acked_pos INTEGER NOT NULL DEFAULT 0
  );
  ```
- **`JOURNAL_ACK {journal_session, pos}`** raises `acked_pos`, and never lowers it.
- **Restart:** on start, entries of earlier sessions with `pos > acked_pos` are queued ahead of the new session's. After the first `INIT` with `journal: true`, they are sent in `(session created_at, pos)` order, then the current session's.
  - With an old server (no `journal`), nothing of an earlier session is sent.
- **Reconnect:** nothing is sent between `REGISTER` and `INIT`. After `INIT`, the agent resends from the lowest unacked position, in order.
- **Kinds that are only sent to a journal-capable server:**
  - `ASSIGN` and the effect kinds. An old server would refuse them as unknown.
  - An old server doesn't ack by position, so the gaps this leaves don't matter.
  - `SHELVE`/`UNSHELVE` go to every server: an old one still records the drawer, and its `SHELVED` reply is ignored.
- **Pruning:** entries with `pos <= acked_pos` older than a retention window (default 7 days) may be deleted. Served agents keep everything by default, because they serve history. The server applies the same retention setting to its journal.
