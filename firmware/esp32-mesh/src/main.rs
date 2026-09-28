//! An ESP32 on the mesh: bring a link up (Wi-Fi, or PPP over the USB-serial
//! cable), join with `arkitekt-mesh`, then fetch a page from a peer every
//! 10 s, logging the path taken and heap figures along the way.
//!
//! Settings come from `mesh.env` at build time (see `build.rs`). Each round
//! also logs one `MESH-TEST ok|fail …` line, for scripts to wait on (the
//! mesh lab's `lab.sh esp32-watch`).

mod logger;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::delay::{TickType, NON_BLOCK};
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::uart::{config::Config as UartConfig, UartDriver};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::netif::{EspNetif, EspNetifDriver, NetifStack, PppConfiguration};
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use esp_idf_svc::sys;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};
use log::{error, info, warn};
use mesh::driver::{Config, Limits, Node};
use mesh::keys::NodeIdentity;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

macro_rules! setting {
    ($name:literal, $default:literal) => {
        match option_env!($name) {
            Some(v) => v,
            None => $default,
        }
    };
}

/// `wifi` or `ppp` (over UART0, the USB-serial port).
const LINK: &str = setting!("MESH_LINK", "wifi");
const WIFI_SSID: &str = setting!("WIFI_SSID", "");
const WIFI_PASS: &str = setting!("WIFI_PASS", "");
const PPP_BAUD: &str = setting!("MESH_PPP_BAUD", "115200");
/// Where logs go once PPP is up: the host's end of the link.
const PPP_LOG_TO: &str = setting!("MESH_PPP_LOG_TO", "10.77.0.1:5514");
const CONTROL_URL: &str = setting!("MESH_CONTROL_URL", "");
const AUTH_KEY: &str = setting!("MESH_AUTH_KEY", "");
const PEER: &str = setting!("MESH_PEER", "peer");
/// `false`: DERP relays only, no direct UDP paths.
const DIRECT: &str = setting!("MESH_DIRECT", "true");
/// The mesh thread's stack (KiB). `MESH-TEST` reports how much of it was
/// ever used, to size this.
const MESH_STACK_KIB: &str = setting!("MESH_STACK_KIB", "48");
/// Where to set the clock from before joining (DERP's certificate check
/// needs the date). Empty: `pool.ntp.org` on Wi-Fi, no sync over PPP (which
/// has no internet; point it at the host, e.g. the mesh lab's NTP).
const SNTP_SERVER: &str = setting!("MESH_SNTP_SERVER", "");
/// With a CA embedded (`MESH_CA_PEM_FILE`): trust only it, not the public
/// roots, which would cost every verified connection ~15 KiB of heap.
const CA_ONLY: &str = setting!("MESH_CA_ONLY", "true");
#[cfg(mesh_ca)]
const CA_PEM: &[u8] = include_bytes!(env!("MESH_CA_PEM_PATH"));

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Set when the link came back after a loss: the node should rebind.
static LINK_CHANGED: AtomicBool = AtomicBool::new(false);

/// Free heap, the lowest it has been, and the largest allocatable block.
fn heap(label: &str) {
    // SAFETY: plain reads of the allocator's statistics.
    let (free, min, largest) = unsafe {
        (
            sys::esp_get_free_heap_size(),
            sys::esp_get_minimum_free_heap_size(),
            sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_8BIT),
        )
    };
    info!(
        "heap [{label}]: {} KiB free, {} KiB lowest, {} KiB largest block",
        free / 1024,
        min / 1024,
        largest / 1024
    );
}

fn main() {
    sys::link_patches();
    logger::init();
    if let Err(e) = run() {
        error!("{e}");
    }
    loop {
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn run() -> Result<()> {
    heap("boot");
    // Why the last run ended (a crash shows up here on the next boot; over
    // PPP the panic message itself went to the UART, which pppd reads).
    // SAFETY: a plain read of the reset cause.
    let reason = unsafe { sys::esp_reset_reason() };
    #[allow(non_upper_case_globals)]
    let reason_name = match reason {
        sys::esp_reset_reason_t_ESP_RST_POWERON => "power-on",
        sys::esp_reset_reason_t_ESP_RST_EXT => "external pin",
        sys::esp_reset_reason_t_ESP_RST_SW => "software restart",
        sys::esp_reset_reason_t_ESP_RST_PANIC => "PANIC (crash or out of memory)",
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => "interrupt watchdog",
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "task watchdog",
        sys::esp_reset_reason_t_ESP_RST_WDT => "other watchdog",
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "brownout",
        _ => "other",
    };
    info!("reset reason: {reason_name} ({reason})");
    // tokio's reactor wakes itself through eventfd; kept mounted for good.
    let _eventfd = esp_idf_svc::io::vfs::MountedEventfs::mount(5)?;

    let peripherals = Peripherals::take()?;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let identity = load_identity(EspNvs::new(nvs.clone(), "mesh", true)?)?;

    match LINK {
        "ppp" => ppp(
            peripherals.uart0,
            peripherals.pins.gpio1,
            peripherals.pins.gpio3,
            identity,
        ),
        _ => {
            let Some(wifi) = wifi(peripherals.modem, sysloop.clone(), nvs)? else {
                return Ok(()); // scanned only
            };
            heap("link up");
            let _ = spawn_mesh(identity)?.join();
            drop(wifi);
            Ok(())
        }
    }
}

/// The node runs on tokio in a thread with room for Rust's stack use.
fn spawn_mesh(identity: NodeIdentity) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("mesh".into())
        .stack_size(MESH_STACK_KIB.parse::<usize>().unwrap_or(48) * 1024)
        .spawn(move || {
            // Kept for as long as the node runs: it keeps the clock in step.
            let _sntp = sync_clock();
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a tokio runtime");
            rt.block_on(mesh_main(identity));
        })
}

/// Set the clock over SNTP, waiting up to 30 s; a node started without it
/// fails DERP's certificate check (the date is 1970), but still tries.
fn sync_clock() -> Option<esp_idf_svc::sntp::EspSntp<'static>> {
    use esp_idf_svc::sntp::{EspSntp, SntpConf, SyncStatus};

    let server = match (SNTP_SERVER, LINK) {
        ("", "ppp") => {
            warn!("no MESH_SNTP_SERVER over PPP: the clock is not set");
            return None;
        }
        ("", _) => "pool.ntp.org",
        (server, _) => server,
    };
    let mut conf = SntpConf::default();
    for slot in conf.servers.iter_mut() {
        *slot = server;
    }
    let sntp = match EspSntp::new(&conf) {
        Ok(sntp) => sntp,
        Err(e) => {
            warn!("SNTP did not start: {e}");
            return None;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while sntp.get_sync_status() != SyncStatus::Completed {
        if Instant::now() > deadline {
            warn!("no time from {server} within 30 s; continuing with the clock unset");
            return Some(sntp);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    info!("clock set from {server}: {now} s since the epoch");
    Some(sntp)
}

/// Wi-Fi as a station. Without an SSID: list the networks in range instead.
fn wifi(
    modem: esp_idf_svc::hal::modem::Modem<'static>,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
) -> Result<Option<BlockingWifi<EspWifi<'static>>>> {
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sysloop.clone(), Some(nvs))?, sysloop)?;
    if WIFI_SSID.is_empty() {
        wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
        wifi.start()?;
        let mut aps = wifi.scan()?;
        aps.sort_by_key(|ap| std::cmp::Reverse(ap.signal_strength));
        info!("no WIFI_SSID set; {} networks in range:", aps.len());
        for ap in aps {
            info!(
                "  {:>4} dBm  ch {:>2}  {:<22} {:?}",
                ap.signal_strength,
                ap.channel,
                format!("{:?}", ap.auth_method),
                ap.ssid.as_str()
            );
        }
        return Ok(None);
    }
    wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid: WIFI_SSID.try_into().map_err(|_| "WIFI_SSID too long")?,
        password: WIFI_PASS.try_into().map_err(|_| "WIFI_PASS too long")?,
        auth_method: if WIFI_PASS.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        ..Default::default()
    }))?;
    wifi.start()?;
    info!("connecting to Wi-Fi {WIFI_SSID:?}");
    wifi.connect()?;
    wifi.wait_netif_up()?;
    info!("Wi-Fi up: {}", wifi.wifi().sta_netif().get_ip_info()?.ip);
    Ok(Some(wifi))
}

/// PPP over UART0 (the USB-serial port) to `pppd` on the host. The console
/// shares that UART, so logging moves to UDP over the link once it is up.
/// Pumps received bytes into PPP for good; the node starts once the link has
/// an address.
fn ppp(
    uart: esp_idf_svc::hal::uart::UART0<'static>,
    tx: esp_idf_svc::hal::gpio::Gpio1<'static>,
    rx: esp_idf_svc::hal::gpio::Gpio3<'static>,
    identity: NodeIdentity,
) -> Result<()> {
    let baud: u32 = PPP_BAUD.parse().map_err(|_| "MESH_PPP_BAUD is not a number")?;
    let log_to: SocketAddr = PPP_LOG_TO.parse().map_err(|_| "MESH_PPP_LOG_TO is not ip:port")?;
    info!("starting PPP on UART0 at {baud} baud; logs continue over UDP to {log_to}");
    logger::hold();
    // Let the console drain before the UART changes hands.
    std::thread::sleep(Duration::from_millis(100));

    let uart = UartDriver::new(
        uart,
        tx,
        rx,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &UartConfig::new().baudrate(Hertz(baud)),
    )?;
    let (mut uart_tx, uart_rx) = uart.into_split();

    let mut driver = EspNetifDriver::new(
        EspNetif::new(NetifStack::Ppp)?,
        |netif| {
            netif.set_ppp_conf(&PppConfiguration {
                phase_events_enabled: false,
                ..Default::default()
            })
        },
        move |data| {
            uart_tx.write(data)?;
            Ok(())
        },
    )?;
    driver.start()?;

    let mut identity = Some(identity);
    let mut _mesh = None;
    let mut buf = [0u8; 512];
    let wait = TickType::from(Duration::from_millis(500)).0;
    let mut last_try = Instant::now();
    let mut last_check = Instant::now();
    let mut link_up = false;
    loop {
        // Wait for a first byte (briefly), then take whatever else is there.
        let n = uart_rx.read(&mut buf[..1], wait).unwrap_or(0);
        if n > 0 {
            let m = uart_rx.read(&mut buf[1..], NON_BLOCK).unwrap_or(0);
            let _ = driver.rx(&buf[..n + m]);
        }

        // Watch the link: (re)start negotiation while it is down, start the
        // node the first time it is up, and flag a rebind when it comes back.
        if last_check.elapsed() < Duration::from_secs(2) {
            continue;
        }
        last_check = Instant::now();
        let ip = driver
            .netif()
            .get_ip_info()
            .map(|i| i.ip)
            .unwrap_or(Ipv4Addr::UNSPECIFIED);
        if ip.is_unspecified() {
            if link_up {
                link_up = false;
                warn!("PPP down");
            }
            // lwIP gives up on an unanswered link: ask again until pppd is there.
            if last_try.elapsed() > Duration::from_secs(10) {
                last_try = Instant::now();
                // SAFETY: the netif handle is live for as long as `driver`.
                unsafe {
                    sys::esp_netif_action_start(
                        driver.netif().handle() as *mut core::ffi::c_void,
                        core::ptr::null_mut(),
                        0,
                        core::ptr::null_mut(),
                    );
                }
            }
            continue;
        }
        if link_up {
            continue;
        }
        link_up = true;
        match identity.take() {
            Some(id) => {
                logger::to_udp(log_to)?;
                info!("PPP up: {ip}");
                heap("link up");
                _mesh = Some(spawn_mesh(id)?);
            }
            None => {
                info!("PPP back: {ip}; rebinding the node");
                LINK_CHANGED.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// The node's keys live in NVS, made on first boot.
fn load_identity(mut nvs: EspNvs<NvsDefault>) -> Result<NodeIdentity> {
    let mut buf = [0u8; 512];
    if let Some(json) = nvs.get_str("identity", &mut buf)? {
        if let Ok(identity) = serde_json::from_str(json) {
            info!("identity from NVS");
            return Ok(identity);
        }
        warn!("unreadable identity in NVS; making a new one");
    }
    let identity = NodeIdentity::generate();
    nvs.set_str("identity", &serde_json::to_string(&identity)?)?;
    info!("new identity stored in NVS");
    Ok(identity)
}

async fn mesh_main(identity: NodeIdentity) {
    // No ring on ESP-IDF: rustls with a pure-Rust provider.
    let _ = rustls_rustcrypto::provider().install_default();
    #[cfg(mesh_ca)]
    {
        match mesh::driver::net::add_trust_roots_pem(CA_PEM) {
            Ok(n) => info!("trusting {n} embedded CA certificate(s)"),
            Err(e) => error!("the embedded CA is unusable: {e}"),
        }
        mesh::driver::net::trust_public_roots(CA_ONLY == "false");
    }
    heap("before join");

    let config = Config {
        control_url: CONTROL_URL.into(),
        identity,
        auth_key: (!AUTH_KEY.is_empty()).then(|| AUTH_KEY.into()),
        hostname: "esp32-mesh".into(),
        ephemeral: false,
        tags: vec![],
        direct: DIRECT != "false",
        limits: Limits::small(),
    };
    // Control may not be up yet (or the link may still be settling): retry.
    let node = loop {
        match Node::start(config.clone()).await {
            Ok(node) => break node,
            Err(e) => {
                warn!("could not join the mesh at {CONTROL_URL}: {e}; retrying in 5 s");
                heap("join failed");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };
    info!(
        "on the mesh as {:?} with {} peers",
        node.addresses(),
        node.netmap().peers().len()
    );
    heap("joined");
    let node = Arc::new(node);
    // The peer we report to keeps its tunnel whatever the peer cap says.
    if let Err(e) = node.set_priority_peer(PEER) {
        warn!("no priority peer {PEER}: {e}");
    }
    tokio::spawn(serve_status(node.clone()));

    loop {
        if LINK_CHANGED.swap(false, Ordering::SeqCst) {
            node.rebind();
        }
        let fetched = get(&node, PEER).await;
        match &fetched {
            Ok(body) => info!("GET http://{PEER}/ -> {body}"),
            Err(e) => warn!("GET http://{PEER}/ failed: {e}"),
        }
        let direct = node.resolve(PEER).ok().and_then(|ip| node.direct_path(ip));
        info!(
            "path to {PEER}: {}",
            direct.map_or("DERP".into(), |a| format!("direct via {a}"))
        );
        heap("after request");
        // SAFETY: a plain read of the allocator's statistics.
        let lowest = unsafe { sys::esp_get_minimum_free_heap_size() } / 1024;
        // SAFETY: the calling task's own stack; ESP-IDF counts it in bytes.
        let stack_free = unsafe { sys::uxTaskGetStackHighWaterMark(core::ptr::null_mut()) } as usize;
        let stack_size = MESH_STACK_KIB.parse::<usize>().unwrap_or(48) * 1024;
        let stack_used = stack_size.saturating_sub(stack_free) / 1024;
        let ok = fetched.as_ref().is_ok_and(|body| body.contains("200"));
        info!(
            "MESH-TEST {} peers={} path={} heap_low={lowest}KiB stack_used={stack_used}KiB/{}KiB",
            if ok { "ok" } else { "fail" },
            node.netmap().peers().len(),
            if direct.is_some() { "direct" } else { "derp" },
            stack_size / 1024,
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

/// `GET /` on port 80 over the tailnet: the device's state as JSON.
async fn serve_status(node: Arc<Node>) {
    let mut listener = match node.listen(80) {
        Ok(listener) => listener,
        Err(e) => {
            warn!("no status endpoint: {e}");
            return;
        }
    };
    info!("status on http://{}:80/", node.addresses()[0]);
    loop {
        let (mut stream, from) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                warn!("status: {e}");
                continue;
            }
        };
        let mut request = [0u8; 256];
        let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut request)).await;
        let body = status_json(&node);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
        info!("status served to {from}");
    }
}

fn status_json(node: &Node) -> String {
    // SAFETY: plain reads of allocator, scheduler and log-clock statistics.
    let (free, low, largest, stack_free, uptime_ms) = unsafe {
        (
            sys::esp_get_free_heap_size(),
            sys::esp_get_minimum_free_heap_size(),
            sys::heap_caps_get_largest_free_block(sys::MALLOC_CAP_8BIT),
            sys::uxTaskGetStackHighWaterMark(core::ptr::null_mut()),
            sys::esp_log_timestamp(),
        )
    };
    let path = node
        .resolve(PEER)
        .ok()
        .and_then(|ip| node.direct_path(ip))
        .map_or("derp".to_string(), |a| format!("direct {a}"));
    serde_json::json!({
        "addresses": node.addresses(),
        "uptime_ms": uptime_ms,
        "heap_free": free,
        "heap_low": low,
        "heap_largest_block": largest,
        "mesh_stack_free": stack_free,
        "peers": node.netmap().peers().len(),
        "active_peers": node.active_peers(),
        "udp_port": node.udp_port(),
        "filtered_packets": node.filtered_packets(),
        "path_to_peer": path,
    })
    .to_string()
}

async fn get(node: &Node, host: &str) -> std::io::Result<String> {
    let mut stream = node.dial(host, 80).await?;
    let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let text = String::from_utf8_lossy(&response);
    let status = text.lines().next().unwrap_or_default().to_owned();
    let body = text.split("\r\n\r\n").nth(1).unwrap_or_default();
    Ok(format!("{status} | {body}"))
}
