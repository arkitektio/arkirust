//! One connection to a host: plain TCP, or TLS over it (rustls's
//! unbuffered client with the RustCrypto provider: [`crate::tls`]).
//!
//! The server's certificate is checked against the CA given in
//! [`Verify::Ca`] (the root of its chain, DER), and its name against the
//! host's; [`Verify::Unverified`] skips both and says so. The TLS record
//! buffers are the caller's, so a connection borrows them for its life.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::net::SocketAddr;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};

use crate::tls::{Buf, TlsStream};

/// The largest TLS record a server may send.
pub const TLS_RECORD: usize = 16_640;

/// How to check a TLS server.
#[derive(Debug, Clone)]
pub enum Verify {
    /// Against this CA certificate (DER): the one that signed the last
    /// certificate the server sends (ISRG Root X1 for Let's Encrypt).
    Ca(Vec<u8>),
    /// Not at all: anyone in the path can pose as the server.
    Unverified,
}

/// The two TLS record buffers a connection borrows: ciphertext received
/// and records to send (each grows to a record as it needs).
pub struct TlsBuffers {
    read: Vec<u8>,
    write: Vec<u8>,
}

impl Default for TlsBuffers {
    fn default() -> Self {
        Self {
            read: Vec::with_capacity(TLS_RECORD),
            write: Vec::with_capacity(TLS_RECORD),
        }
    }
}

/// See the module docs.
pub enum Transport<'b, C: Read + Write + 'b> {
    Plain(C),
    /// Boxed: the TLS state is large.
    Tls(Box<TlsStream<'b, C>>),
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

impl<C: Read + Write> Read for Transport<'_, C> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ConnError> {
        match self {
            Self::Plain(c) => c
                .read(buf)
                .await
                .map_err(|e| ConnError(embedded_io_async::Error::kind(&e))),
            Self::Tls(c) => c.read(buf).await,
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
            Self::Tls(c) => c.write(buf).await,
        }
    }

    async fn flush(&mut self) -> Result<(), ConnError> {
        match self {
            Self::Plain(c) => c
                .flush()
                .await
                .map_err(|e| ConnError(embedded_io_async::Error::kind(&e))),
            Self::Tls(c) => c.flush().await,
        }
    }
}

/// Resolve `host`, connect to `port`, and (if `secure`) shake hands, into
/// the caller's record buffers.
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
    let buffers = (
        Buf::Borrowed(&mut buffers.read),
        Buf::Borrowed(&mut buffers.write),
    );
    connect(tcp, dns, secure, host, port, verify, buffers).await
}

/// As [`open`], the record buffers the connection's own: for connections
/// made again and again (a mesh node's control and DERP), whose buffers
/// go with them.
pub async fn open_owned<'a, T: TcpConnect, D: Dns>(
    tcp: &'a T,
    dns: &D,
    secure: bool,
    host: &str,
    port: u16,
    verify: &Verify,
) -> Result<Transport<'a, T::Connection<'a>>, String> {
    let buffers = (Buf::Owned(Vec::new()), Buf::Owned(Vec::new()));
    connect(tcp, dns, secure, host, port, verify, buffers).await
}

async fn connect<'a, 'b, T: TcpConnect, D: Dns>(
    tcp: &'a T,
    dns: &D,
    secure: bool,
    host: &str,
    port: u16,
    verify: &Verify,
    buffers: (Buf<'b>, Buf<'b>),
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
    if matches!(verify, Verify::Unverified) {
        tracing::warn!("TLS to {} without verifying the server", host);
    }
    let config = crate::tls::config(verify).map_err(|e| format!("TLS with {host}:{port}: {e}"))?;
    let tls = TlsStream::connect(socket, host, config, buffers)
        .await
        .map_err(|e| format!("TLS with {host}:{port}: {e:?}"))?;
    Ok(Transport::Tls(Box::new(tls)))
}
