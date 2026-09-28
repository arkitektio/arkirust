//! The `arkitekt-meshd` sidecar: our mesh node (`crates/meshd`) in a child
//! process.
//!
//! The protocol: the auth key goes in `$ARKITEKT_MESH_AUTHKEY`, and meshd
//! prints one JSON line on stdout, `{"event":"ready","proxy":…}` or
//! `{"event":"error","code":…,"message":…}`. It exits when its stdin closes,
//! so it never outlives the app.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::{needs_login, prepare_statedir, Join, MeshOptions};
use crate::error::{FaktsError, Result};

const MESHD: &str = if cfg!(windows) {
    "arkitekt-meshd.exe"
} else {
    "arkitekt-meshd"
};

fn find_meshd(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = configured {
        return Ok(path.to_owned());
    }
    if let Some(path) = std::env::var_os("ARKITEKT_MESHD").filter(|p| !p.is_empty()) {
        return Ok(path.into());
    }
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_owned));
    let data_dir = dirs::data_local_dir().map(|dir| dir.join("arkitekt").join("bin"));
    let on_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    beside_exe
        .into_iter()
        .chain(data_dir)
        .chain(on_path)
        .map(|dir| dir.join(MESHD))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            FaktsError::Mesh(
                "`arkitekt-meshd` was not found; download it from \
                 https://github.com/arkitektio/arkirust/releases (or `pip install arkitekt-meshd`) \
                 and put it on PATH, or set ARKITEKT_MESHD / MeshOptions::meshd"
                    .into(),
            )
        })
}

/// One line of meshd's stdout.
#[derive(Debug, Deserialize)]
struct Event {
    event: String,
    #[serde(default)]
    proxy: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// A running sidecar; stopped when dropped.
#[derive(Debug)]
pub struct Sidecar {
    _child: Child,
    // meshd exits when its stdin closes; its stdout stays open so a late
    // write does not kill it with SIGPIPE.
    _stdin: ChildStdin,
    _stdout: BufReader<ChildStdout>,
    proxy_url: String,
    statedir: PathBuf,
}

impl Sidecar {
    /// The local HTTP proxy into the mesh, e.g. `http://127.0.0.1:41234`.
    pub fn proxy_url(&self) -> &str {
        &self.proxy_url
    }

    pub fn statedir(&self) -> &Path {
        &self.statedir
    }

    /// Whether a node was already joined in `statedir` (`identity.json`
    /// and `control-url`, as `mesh::driver::Session` keeps them).
    pub(crate) fn has_state(statedir: &Path) -> bool {
        statedir.join("identity.json").is_file() && statedir.join("control-url").is_file()
    }

    /// Start meshd in `statedir` and wait until it is connected.
    pub(crate) async fn start(
        options: &MeshOptions,
        statedir: PathBuf,
        hostname: &str,
        join: Join,
    ) -> Result<Self> {
        let meshd = find_meshd(options.meshd.as_deref())?;

        prepare_statedir(&statedir).await?;
        let log_path = statedir.join("meshd.log");
        let failed = |what: String| FaktsError::Mesh(format!("{what}; see {}", log_path.display()));

        let mut command = Command::new(&meshd);
        command
            .arg(format!("--statedir={}", statedir.display()))
            .arg(format!("--hostname={hostname}"))
            .arg("--listen=127.0.0.1:0")
            .arg(format!("--timeout={}s", options.timeout.as_secs().max(1)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(&log_path)?)
            .kill_on_drop(true);
        if let Some(coord_url) = &join.coord_url {
            command.arg(format!("--control-url={coord_url}"));
        }
        // The key never goes on the command line, where `ps` would show it.
        match &join.auth_key {
            Some(key) => command.env("ARKITEKT_MESH_AUTHKEY", key),
            None => command.env_remove("ARKITEKT_MESH_AUTHKEY"),
        };
        let mut child = command
            .spawn()
            .map_err(|e| FaktsError::Mesh(format!("could not start {}: {e}", meshd.display())))?;
        tracing::debug!(
            "started {} for the mesh in {}",
            meshd.display(),
            statedir.display()
        );
        if join.auth_key.is_some() {
            tracing::info!("joining the mesh as {hostname}");
        }

        let stdin = child.stdin.take().expect("stdin is piped");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut line = String::new();
        // meshd enforces the timeout itself; this only guards against a hang.
        let read = tokio::time::timeout(
            options.timeout + Duration::from_secs(10),
            stdout.read_line(&mut line),
        )
        .await
        .map_err(|_| failed("the mesh did not connect in time".into()))??;
        if read == 0 {
            let status = child.wait().await?;
            return Err(failed(format!("arkitekt-meshd exited ({status})")));
        }
        let event: Event = serde_json::from_str(line.trim()).map_err(|e| {
            failed(format!(
                "arkitekt-meshd sent an unexpected line ({e}): {}",
                line.trim()
            ))
        })?;

        match (event.event.as_str(), event.proxy) {
            ("ready", Some(proxy_url)) => {
                tracing::info!("connected to the mesh as {hostname}; proxy at {proxy_url}");
                Ok(Self {
                    _child: child,
                    _stdin: stdin,
                    _stdout: stdout,
                    proxy_url,
                    statedir,
                })
            }
            _ if event.code.as_deref() == Some("needs_login") => Err(needs_login()),
            _ if event.code.as_deref() == Some("locked") => Err(failed(format!(
                "another sidecar is already running in {}",
                statedir.display()
            ))),
            _ => Err(failed(format!(
                "arkitekt-meshd failed ({}): {}",
                event.code.as_deref().unwrap_or(&event.event),
                event.message.as_deref().unwrap_or_default()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in meshd: records its arguments and key, then plays `reply`.
    #[cfg(unix)]
    fn fake_meshd(dir: &Path, reply: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-meshd");
        let script = format!(
            "#!/bin/sh\n\
             echo \"$@\" > '{dir}/args'\n\
             echo \"$ARKITEKT_MESH_AUTHKEY\" > '{dir}/key'\n\
             {reply}\n",
            dir = dir.display()
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn options(meshd: PathBuf) -> MeshOptions {
        MeshOptions {
            meshd: Some(meshd),
            timeout: Duration::from_secs(10),
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ready_line_gives_the_proxy_and_the_key_stays_off_argv() {
        let dir = tempfile::tempdir().unwrap();
        // Waits on stdin like the real one.
        let meshd = fake_meshd(
            dir.path(),
            "echo '{\"event\":\"ready\",\"proxy\":\"http://127.0.0.1:4321\",\"ips\":[\"100.64.0.7\"]}'\ncat >/dev/null",
        );
        let join = Join {
            coord_url: Some("https://mesh.example".into()),
            auth_key: Some("tskey-secret".into()),
        };
        let sidecar = Sidecar::start(&options(meshd), dir.path().join("node"), "my-app", join)
            .await
            .unwrap();
        assert_eq!(sidecar.proxy_url(), "http://127.0.0.1:4321");

        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(args.contains("--hostname=my-app"), "{args}");
        assert!(
            args.contains("--control-url=https://mesh.example"),
            "{args}"
        );
        assert!(
            !args.contains("tskey-secret"),
            "the key leaked into argv: {args}"
        );
        let key = std::fs::read_to_string(dir.path().join("key")).unwrap();
        assert_eq!(key.trim(), "tskey-secret");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn needs_login_asks_to_authorize_again() {
        let dir = tempfile::tempdir().unwrap();
        let meshd = fake_meshd(
            dir.path(),
            "echo '{\"event\":\"error\",\"code\":\"needs_login\",\"message\":\"no key\"}'\nexit 1",
        );
        let join = Join {
            coord_url: None,
            auth_key: None,
        };
        let err = Sidecar::start(&options(meshd), dir.path().join("node"), "my-app", join)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no mesh key was granted"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_silent_exit_points_at_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let meshd = fake_meshd(dir.path(), "exit 3");
        let join = Join {
            coord_url: None,
            auth_key: None,
        };
        let err = Sidecar::start(&options(meshd), dir.path().join("node"), "my-app", join)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("meshd.log"), "{err}");
    }

    /// Needs `arkitekt-meshd` (on PATH or in `$ARKITEKT_MESHD`): `cargo test
    /// -p fakts --features mesh -- --ignored sidecar`.
    #[tokio::test]
    #[ignore]
    async fn sidecar_without_a_key_refuses_to_join() {
        let dir = tempfile::tempdir().unwrap();
        let options = MeshOptions {
            timeout: Duration::from_secs(30),
            ..Default::default()
        };
        let join = Join {
            coord_url: Some("http://127.0.0.1:1".into()),
            auth_key: None,
        };
        let err = Sidecar::start(&options, dir.path().join("node"), "arkitekt-test", join)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no mesh key was granted"), "{err}");
    }
}
