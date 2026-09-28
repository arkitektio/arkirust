//! The mesh sidecar: a userspace `tailscaled` joined to the deployment's
//! tailnet (ionscale), exposing a local HTTP proxy that mesh aliases are
//! reached through.
//!
//! No root and no TUN device: `tailscaled --tun=userspace-networking` with
//! its own state directory and socket, so it runs next to (and independent
//! of) a system tailscale. The node is keyed by the app's identity
//! (`sub`/`organization`/`hub`), so it is joined once with the key from the
//! first token and re-used on every later start.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

use crate::error::{FaktsError, Result};

/// Where to find tailscale and keep the sidecar's state.
#[derive(Debug, Clone)]
pub struct MeshOptions {
    /// The `tailscaled` binary (default: `PATH`, then the usual install locations).
    pub tailscaled: Option<PathBuf>,
    /// The `tailscale` CLI (default: `PATH`, then the usual install locations).
    pub tailscale: Option<PathBuf>,
    /// Where node state lives (default: `<state dir>/arkitekt/mesh`).
    pub state_root: Option<PathBuf>,
    /// The node's hostname (default: `<app identifier>-<device id>`).
    pub hostname: Option<String>,
    /// How long joining and connecting may take.
    pub timeout: Duration,
}

impl Default for MeshOptions {
    fn default() -> Self {
        Self {
            tailscaled: None,
            tailscale: None,
            state_root: None,
            hostname: None,
            timeout: Duration::from_secs(90),
        }
    }
}

/// Where tailscale is often installed, off a non-root `PATH`.
const FALLBACK_DIRS: &[&str] = &["/usr/sbin", "/usr/local/bin", "/usr/local/sbin", "/usr/bin"];

fn find_binary(configured: Option<&Path>, name: &str) -> Result<PathBuf> {
    if let Some(path) = configured {
        return Ok(path.to_owned());
    }
    let on_path = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default();
    on_path
        .into_iter()
        .chain(FALLBACK_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            FaktsError::Mesh(format!(
                "`{name}` was not found; install tailscale (https://tailscale.com/download) \
                 or set its path in MeshOptions"
            ))
        })
}

impl MeshOptions {
    pub(crate) fn state_root(&self) -> PathBuf {
        self.state_root.clone().unwrap_or_else(|| {
            dirs::state_dir()
                .or_else(dirs::data_local_dir)
                .unwrap_or_else(|| PathBuf::from(".arkitekt"))
                .join("arkitekt")
                .join("mesh")
        })
    }
}

/// A DNS label: lowercase alphanumerics and dashes, at most 63 characters.
pub(crate) fn hostname_label(raw: &str) -> String {
    let mut label: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    label.truncate(63);
    let label = label.trim_matches('-').to_owned();
    if label.is_empty() {
        "arkitekt-app".into()
    } else {
        label
    }
}

/// What joining a fresh node needs.
pub(crate) struct Login {
    pub coord_url: String,
    pub auth_key: String,
}

/// A running sidecar; stopped when dropped.
#[derive(Debug)]
pub struct Sidecar {
    _child: Child,
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

    /// Whether a node was already joined in `statedir`.
    pub(crate) fn has_state(statedir: &Path) -> bool {
        statedir.join("tailscaled.state").is_file()
    }

    /// Start `tailscaled` in `statedir`, join with `login` if the node is not
    /// logged in yet, and wait until it is connected.
    pub(crate) async fn start(
        options: &MeshOptions,
        statedir: PathBuf,
        hostname: &str,
        login: Option<Login>,
    ) -> Result<Self> {
        let tailscaled = find_binary(options.tailscaled.as_deref(), "tailscaled")?;
        let tailscale = find_binary(options.tailscale.as_deref(), "tailscale")?;

        tokio::fs::create_dir_all(&statedir).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&statedir, std::fs::Permissions::from_mode(0o700)).await;
        }
        let socket = statedir.join("tailscaled.sock");
        let log_path = statedir.join("tailscaled.log");
        let log = std::fs::File::create(&log_path)?;

        // A free port for the proxy (released just before tailscaled binds it).
        let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
        let proxy_url = format!("http://127.0.0.1:{port}");

        let mut child = Command::new(&tailscaled)
            .arg("--tun=userspace-networking")
            .arg(format!("--statedir={}", statedir.display()))
            .arg(format!("--socket={}", socket.display()))
            .arg(format!("--outbound-http-proxy-listen=127.0.0.1:{port}"))
            // Pick a free WireGuard port: a system tailscaled holds the default.
            .arg("--port=0")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| FaktsError::Mesh(format!("could not start {}: {e}", tailscaled.display())))?;
        tracing::debug!("started tailscaled for the mesh in {}", statedir.display());

        let cli = Cli {
            tailscale,
            socket,
            log_path,
        };
        let deadline = Instant::now() + options.timeout;
        let mut joined = false;
        loop {
            if let Some(status) = child.try_wait()? {
                return Err(cli.failed(format!("tailscaled exited ({status})")));
            }
            match cli.backend_state().await.as_deref() {
                Some("Running") => break,
                Some("NeedsLogin") if !joined => {
                    let Some(login) = &login else {
                        return Err(FaktsError::Mesh(
                            "this node is not on the mesh and no mesh key was granted; \
                             authorize the app again (no_cache) and allow mesh access"
                                .into(),
                        ));
                    };
                    cli.up(login, hostname, deadline.saturating_duration_since(Instant::now()))
                        .await?;
                    joined = true;
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(cli.failed("the mesh did not connect in time".into()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        // A daemon orphaned by a killed run can answer on the same socket;
        // make sure it is ours that is running, with its proxy listening.
        if let Some(status) = child.try_wait()? {
            return Err(cli.failed(format!(
                "tailscaled exited ({status}); is another sidecar still running in {}?",
                statedir.display()
            )));
        }
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            return Err(cli.failed(format!("the mesh proxy is not listening on {proxy_url}")));
        }
        tracing::info!("connected to the mesh as {hostname}; proxy at {proxy_url}");

        Ok(Self {
            _child: child,
            proxy_url,
            statedir,
        })
    }
}

/// The `tailscale` CLI, talking to our own daemon.
struct Cli {
    tailscale: PathBuf,
    socket: PathBuf,
    log_path: PathBuf,
}

impl Cli {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.tailscale);
        command
            .arg(format!("--socket={}", self.socket.display()))
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    fn failed(&self, what: String) -> FaktsError {
        FaktsError::Mesh(format!("{what}; see {}", self.log_path.display()))
    }

    /// The daemon's `BackendState`, or `None` while it is not answering yet.
    async fn backend_state(&self) -> Option<String> {
        let output = self
            .command()
            .args(["status", "--json"])
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        // `status` exits non-zero while logged out, but still prints the JSON.
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        status["BackendState"].as_str().map(str::to_owned)
    }

    async fn up(&self, login: &Login, hostname: &str, timeout: Duration) -> Result<()> {
        tracing::info!(
            "joining the mesh at {} as {hostname} (tailscale up --authkey=***)",
            login.coord_url
        );
        let run = self
            .command()
            .arg("up")
            .arg("--reset")
            .arg(format!("--login-server={}", login.coord_url))
            .arg(format!("--authkey={}", login.auth_key))
            .arg(format!("--hostname={hostname}"))
            .output();
        let output = tokio::time::timeout(timeout, run)
            .await
            .map_err(|_| self.failed("`tailscale up` did not finish in time".into()))??;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(self.failed(format!("`tailscale up` failed: {}", stderr.trim())));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_are_dns_labels() {
        assert_eq!(hostname_label("My App_v2"), "my-app-v2");
        assert_eq!(hostname_label("--x--"), "x");
        assert_eq!(hostname_label("!!!"), "arkitekt-app");
        assert_eq!(hostname_label(&"a".repeat(80)).len(), 63);
    }

    /// Needs `tailscaled` and `tailscale` installed: `cargo test -p fakts
    /// --features mesh -- --ignored sidecar`.
    #[tokio::test]
    #[ignore]
    async fn sidecar_without_a_key_refuses_to_join() {
        let dir = tempfile::tempdir().unwrap();
        let options = MeshOptions {
            timeout: Duration::from_secs(30),
            ..Default::default()
        };
        let err = Sidecar::start(&options, dir.path().join("node"), "arkitekt-test", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no mesh key was granted"), "{err}");
    }
}
