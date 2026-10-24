//! The test stack (`testing/stack`: lok, rekuest, takt) for the crates'
//! end-to-end tests, which include it with `#[path]`.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// The key lok's config seeds (`mandate_keys`), for the user `demo` in `demo`.
pub const MANDATE_KEY: &str = "Y22joLbkjm4vtXMj_T4FD3U99Mb71pTFnUe-8KToAQI";

pub fn stack_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testing/stack")
}

/// `n` distinct free ports: every listener is held until all are picked.
fn free_ports(n: usize) -> Vec<u16> {
    let listeners: Vec<TcpListener> = (0..n)
        .map(|_| TcpListener::bind("127.0.0.1:0").expect("a free port"))
        .collect();
    listeners
        .iter()
        .map(|l| l.local_addr().expect("bound").port())
        .collect()
}

/// The running stack, taken down (volumes included) however the test ends.
pub struct Stack {
    project: String,
    env: Vec<(&'static str, String)>,
    pub lok: u16,
    pub rekuest: u16,
    _config: tempfile::TempDir,
}

impl Stack {
    pub fn up() -> Self {
        let ports = free_ports(3);
        let (lok, rekuest, rustfs) = (ports[0], ports[1], ports[2]);

        // lok advertises rekuest's address and anchors its issuer to its own
        // port, which rekuest then trusts: the ports go into both configs
        // before anything starts.
        let config = tempfile::Builder::new()
            .prefix("arkirust-stack-")
            .tempdir()
            .expect("a temp dir");
        let render = |name: &str| {
            let template = std::fs::read_to_string(stack_dir().join("configs").join(name))
                .expect("the stack's config");
            let file = config.path().join(name);
            std::fs::write(
                &file,
                template
                    .replace("__LOK_HOST_PORT__", &lok.to_string())
                    .replace("__REKUEST_HOST_PORT__", &rekuest.to_string()),
            )
            .expect("the rendered config");
            // Readable by the docker daemon, which mounts it.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644));
            }
            file.to_string_lossy().into_owned()
        };
        let (lok_config, rekuest_config) = (render("lok.yaml"), render("rekuest.yaml"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(config.path(), std::fs::Permissions::from_mode(0o755));
        }

        let stack = Self {
            // One project per stack: tests in one binary run side by side.
            project: {
                static STACKS: std::sync::atomic::AtomicUsize =
                    std::sync::atomic::AtomicUsize::new(0);
                let n = STACKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("arkirust-e2e-{}-{n}", std::process::id())
            },
            env: vec![
                ("LOK_HOST_PORT", lok.to_string()),
                ("REKUEST_HOST_PORT", rekuest.to_string()),
                ("RUSTFS_HOST_PORT", rustfs.to_string()),
                ("LOK_CONFIG_FILE", lok_config),
                ("REKUEST_CONFIG_FILE", rekuest_config),
            ],
            lok,
            rekuest,
            _config: config,
        };
        // A locally built tag has nothing to pull: keep going with the local image.
        stack.compose(&["pull", "--ignore-pull-failures", "--quiet"]);
        let up = stack.compose(&["up", "--detach"]);
        assert!(
            up.status.success(),
            "the stack did not start: {}",
            String::from_utf8_lossy(&up.stderr)
        );
        stack
    }

    pub fn compose(&self, args: &[&str]) -> Output {
        Command::new("docker")
            .args(["compose", "--project-name", &self.project])
            .args(args)
            .envs(self.env.iter().map(|(k, v)| (*k, v.as_str())))
            .current_dir(stack_dir())
            .output()
            .expect("docker runs")
    }

    /// Wait until `service` answers `GET /ht` on its host port.
    pub async fn wait_healthy(&self, service: &str, port: u16) {
        let url = format!("http://localhost:{port}/ht");
        let deadline = Instant::now() + Duration::from_secs(300);
        let http = reqwest::Client::new();
        loop {
            if let Ok(resp) = http.get(&url).timeout(Duration::from_secs(5)).send().await {
                if resp.status().is_success() {
                    return;
                }
            }
            if Instant::now() > deadline {
                let logs = self.compose(&["logs", "--no-log-prefix", "--tail", "50", service]);
                panic!(
                    "{service} never became healthy at {url}:\n{}",
                    String::from_utf8_lossy(&logs.stdout)
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // A failed test shows what the servers said.
        if std::thread::panicking() {
            let logs = self.compose(&["logs", "--tail", "80"]);
            eprintln!("{}", String::from_utf8_lossy(&logs.stdout));
        }
        self.compose(&["down", "--volumes", "--remove-orphans", "--timeout", "1"]);
    }
}
