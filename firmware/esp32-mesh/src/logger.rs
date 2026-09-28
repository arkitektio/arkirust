//! The firmware's log: to the serial console until PPP takes the UART over,
//! held in memory while the link comes up, then sent as UDP datagrams to
//! the host (`socat -u UDP-RECV:5514 STDOUT` there shows them).

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;

use log::{LevelFilter, Log, Metadata, Record};

/// Lines kept while there is nowhere to send them.
const HELD_LINES: usize = 200;

enum Sink {
    Console,
    Held(VecDeque<String>),
    Udp(UdpSocket, SocketAddr),
}

pub struct Logger {
    sink: Mutex<Sink>,
}

static LOGGER: Logger = Logger {
    sink: Mutex::new(Sink::Console),
};

pub fn init() {
    log::set_logger(&LOGGER).expect("the only logger");
    log::set_max_level(LevelFilter::Info);
}

/// Stop writing to the console (the UART is about to carry PPP): ESP-IDF's
/// own logs are silenced, ours are held.
pub fn hold() {
    // SAFETY: a C string literal; sets the level of every ESP-IDF log tag.
    unsafe { esp_idf_svc::sys::esp_log_level_set(c"*".as_ptr(), esp_idf_svc::sys::esp_log_level_t_ESP_LOG_NONE) };
    *LOGGER.sink.lock().unwrap() = Sink::Held(VecDeque::new());
}

/// Send the log to `to` over UDP from now on, starting with what was held.
pub fn to_udp(to: SocketAddr) -> std::io::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let mut sink = LOGGER.sink.lock().unwrap();
    if let Sink::Held(lines) = &mut *sink {
        for line in lines.drain(..) {
            let _ = socket.send_to(line.as_bytes(), to);
        }
    }
    *sink = Sink::Udp(socket, to);
    Ok(())
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // SAFETY: reads a tick counter.
        let ms = unsafe { esp_idf_svc::sys::esp_log_timestamp() };
        let line = format!("{} ({ms}) {}: {}\n", record.level(), record.target(), record.args());
        let Ok(mut sink) = self.sink.lock() else { return };
        match &mut *sink {
            Sink::Console => print!("{line}"),
            Sink::Held(lines) => {
                if lines.len() >= HELD_LINES {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
            Sink::Udp(socket, to) => {
                let _ = socket.send_to(line.as_bytes(), *to);
            }
        }
    }

    fn flush(&self) {}
}
