//! One connection to a host: plain TCP, or TLS 1.3 over it (embedded-tls).
//!
//! The server's certificate is checked against the CA given in
//! [`Verify::Ca`] (the root of its chain, DER), and its name against the
//! host's; [`Verify::Unverified`] skips both and says so. The TLS record
//! buffers are the caller's, so a connection borrows them for its life.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::net::SocketAddr;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, TlsClock, TlsConfig, TlsConnection, TlsContext,
    TlsError, TlsVerifier,
};

/// The largest TLS record a server may send.
pub const TLS_RECORD: usize = 16_640;
/// The server's certificate, as the verifier keeps it (DER).
const CERT_SIZE: usize = 8 * 1024;

/// How to check a TLS server.
#[derive(Debug, Clone)]
pub enum Verify {
    /// Against this CA certificate (DER): the root of the server's chain.
    Ca(Vec<u8>),
    /// Not at all: anyone in the path can pose as the server.
    Unverified,
}

/// The two TLS record buffers a connection borrows.
pub struct TlsBuffers {
    read: Vec<u8>,
    write: Vec<u8>,
}

impl Default for TlsBuffers {
    fn default() -> Self {
        Self {
            read: vec![0; TLS_RECORD],
            write: vec![0; TLS_RECORD],
        }
    }
}

/// See the module docs.
pub enum Transport<'b, C: Read + Write + 'b> {
    Plain(C),
    /// Boxed: the TLS state is large.
    Tls(Box<TlsConnection<'b, C, Aes128GcmSha256>>),
}

/// An I/O error of either.
#[derive(Debug)]
pub struct ConnError(pub ErrorKind);

impl core::fmt::Display for ConnError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

impl core::error::Error for ConnError {}

impl embedded_io_async::Error for ConnError {
    fn kind(&self) -> ErrorKind {
        self.0
    }
}

impl<C: Read + Write> ErrorType for Transport<'_, C> {
    type Error = ConnError;
}

fn tls_kind(e: TlsError) -> ConnError {
    match e {
        TlsError::Io(kind) => ConnError(kind),
        _ => ConnError(ErrorKind::Other),
    }
}

impl<C: Read + Write> Read for Transport<'_, C> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ConnError> {
        match self {
            Self::Plain(c) => c
                .read(buf)
                .await
                .map_err(|e| ConnError(embedded_io_async::Error::kind(&e))),
            Self::Tls(c) => c.read(buf).await.map_err(tls_kind),
        }
    }
}

impl<C: Read + Write> Write for Transport<'_, C> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, ConnError> {
        match self {
            Self::Plain(c) => c
                .write(buf)
                .await
                .map_err(|e| ConnError(embedded_io_async::Error::kind(&e))),
            Self::Tls(c) => c.write(buf).await.map_err(tls_kind),
        }
    }

    async fn flush(&mut self) -> Result<(), ConnError> {
        match self {
            Self::Plain(c) => c
                .flush()
                .await
                .map_err(|e| ConnError(embedded_io_async::Error::kind(&e))),
            Self::Tls(c) => c.flush().await.map_err(tls_kind),
        }
    }
}

/// Resolve `host`, connect to `port`, and (if `secure`) shake hands.
pub async fn open<'a, 'b, T: TcpConnect, D: Dns>(
    tcp: &'a T,
    dns: &D,
    secure: bool,
    host: &str,
    port: u16,
    verify: &Verify,
    buffers: &'b mut TlsBuffers,
) -> Result<Transport<'b, T::Connection<'a>>, String>
where
    'a: 'b,
{
    let ip = match host.parse() {
        Ok(ip) => ip,
        Err(_) => dns
            .get_host_by_name(host, AddrType::Either)
            .await
            .map_err(|e| format!("resolve {host}: {e:?}"))?,
    };
    let socket = tcp
        .connect(SocketAddr::new(ip, port))
        .await
        .map_err(|e| format!("connect to {host}:{port}: {e:?}"))?;
    if !secure {
        return Ok(Transport::Plain(socket));
    }
    let mut tls = TlsConnection::new(socket, &mut buffers.read, &mut buffers.write);
    let config = TlsConfig::new().with_server_name(host);
    let mut provider = Provider {
        verifier: match verify {
            Verify::Ca(der) => Some(CertVerifier::new(Certificate::X509(der.as_slice()))),
            Verify::Unverified => {
                tracing::warn!("TLS to {} without verifying the server", host);
                None
            }
        },
    };
    tls.open(TlsContext::new(&config, &mut provider))
        .await
        .map_err(|e| format!("TLS with {host}:{port}: {e:?}"))?;
    Ok(Transport::Tls(Box::new(tls)))
}

/// The wall clock, for certificate validity.
struct Clock;

impl TlsClock for Clock {
    fn now() -> Option<u64> {
        u64::try_from(rekuest::rt::now().timestamp()).ok()
    }
}

/// Randomness for the handshake: the runtime's (the board's RNG).
struct Rng;

impl rand_core::RngCore for Rng {
    fn next_u32(&mut self) -> u32 {
        let mut bytes = [0u8; 4];
        rekuest::rt::fill_random(&mut bytes);
        u32::from_le_bytes(bytes)
    }

    fn next_u64(&mut self) -> u64 {
        let mut bytes = [0u8; 8];
        rekuest::rt::fill_random(&mut bytes);
        u64::from_le_bytes(bytes)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        rekuest::rt::fill_random(dest)
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        rekuest::rt::fill_random(dest);
        Ok(())
    }
}

impl rand_core::CryptoRng for Rng {}

struct Provider<'c> {
    verifier: Option<CertVerifier<'c, Aes128GcmSha256, Clock, CERT_SIZE>>,
}

impl CryptoProvider for Provider<'_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = Vec<u8>;

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        Rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        self.verifier.as_mut().ok_or(TlsError::Unimplemented)
    }
}
