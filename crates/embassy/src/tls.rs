//! TLS over any `embedded_io_async` stream: rustls's unbuffered client
//! (sans-IO) with the RustCrypto provider, without std.
//!
//! Every buffer lives in the stream (the ciphertext received, the plaintext
//! not yet read, the records not yet written), and bytes count as consumed
//! only once the await that moved them returned. So a `read` dropped by a
//! `select` that took another branch loses nothing: it is cancel-safe as
//! long as the transport's own `read` is, as embassy-net's TCP is.
//! (embedded-tls, before, read each record header into its stack and lost
//! it when cancelled.)

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;

use embedded_io_async::{ErrorKind, Read, Write};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::UnbufferedClientConnection;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::conn::{ConnError, Verify};

/// On a board, getrandom (which the RustCrypto provider's key exchange
/// draws from) is the runtime's randomness: the board's RNG.
#[cfg(target_os = "none")]
fn runtime_random(dest: &mut [u8]) -> Result<(), getrandom::Error> {
    arkitekt_spec::rt::fill_random(dest);
    Ok(())
}

#[cfg(target_os = "none")]
getrandom::register_custom_getrandom!(runtime_random);

/// The step buffers grow by: no doubling (a 16 KiB record would make a
/// 32 KiB buffer), no more than needed rounded up to it.
const STEP: usize = 4096;

/// What a buffer of `len` bytes needing `more` grows to.
fn grown(len: usize, more: usize) -> usize {
    (len + more).div_ceil(STEP) * STEP
}

/// Room for `more` in `plain`.
fn grow(plain: &mut VecDeque<u8>, more: usize) {
    let (len, cap) = (plain.len(), plain.capacity());
    if len + more > cap {
        plain.reserve_exact(grown(len, more) - len);
    }
}

/// How much ciphertext is read from the transport at a time.
const READ_CHUNK: usize = 4096;

/// The wall clock, for certificate validity: the runtime's.
#[derive(Debug)]
struct Clock;

impl TimeProvider for Clock {
    fn current_time(&self) -> Option<UnixTime> {
        let secs = u64::try_from(arkitekt_spec::rt::now().timestamp()).ok()?;
        Some(UnixTime::since_unix_epoch(core::time::Duration::from_secs(
            secs,
        )))
    }
}

/// Accepts any certificate (signatures over the handshake still checked).
#[derive(Debug)]
struct Unverified(Arc<CryptoProvider>);

impl ServerCertVerifier for Unverified {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// A client config: TLS 1.3 and 1.2, checked against `verify`.
pub fn config(verify: &Verify) -> Result<Arc<ClientConfig>, rustls::Error> {
    let provider = Arc::new(rustls_rustcrypto::provider());
    let builder = ClientConfig::builder_with_details(provider.clone(), Arc::new(Clock))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?;
    let config = match verify {
        Verify::Ca(der) => {
            let mut roots = RootCertStore::empty();
            roots.add(CertificateDer::from(der.clone()))?;
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        Verify::Unverified => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Unverified(provider)))
            .with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// What the state machine waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// More ciphertext.
    Read,
    /// Nothing: the handshake is done, application data may go.
    Ready,
    Closed,
}

/// A record buffer: the caller's ([`conn::open`](crate::conn::open)) or the
/// stream's own ([`conn::open_owned`](crate::conn::open_owned)).
pub enum Buf<'b> {
    Borrowed(&'b mut Vec<u8>),
    Owned(Vec<u8>),
}

impl core::ops::Deref for Buf<'_> {
    type Target = Vec<u8>;

    fn deref(&self) -> &Vec<u8> {
        match self {
            Buf::Borrowed(b) => b,
            Buf::Owned(b) => b,
        }
    }
}

impl core::ops::DerefMut for Buf<'_> {
    fn deref_mut(&mut self) -> &mut Vec<u8> {
        match self {
            Buf::Borrowed(b) => b,
            Buf::Owned(b) => b,
        }
    }
}

/// A TLS client stream; see the module docs.
pub struct TlsStream<'b, C> {
    io: C,
    conn: UnbufferedClientConnection,
    /// Ciphertext received, not yet processed (from the front).
    incoming: Buf<'b>,
    /// Records encoded, not yet written (from the front).
    outgoing: Buf<'b>,
    plain: VecDeque<u8>,
    /// The server closed (close_notify, or the transport's end).
    eof: bool,
}

fn tls_error(e: rustls::Error) -> ConnError {
    tracing::debug!("TLS: {e}");
    ConnError(ErrorKind::InvalidData)
}

impl<'b, C: Read + Write> TlsStream<'b, C> {
    /// Shake hands with `host` over `io`.
    pub async fn connect(
        io: C,
        host: &str,
        config: Arc<ClientConfig>,
        buffers: (Buf<'b>, Buf<'b>),
    ) -> Result<Self, ConnError> {
        let name = ServerName::try_from(host.to_string())
            .map_err(|_| ConnError(ErrorKind::InvalidInput))?;
        let conn = UnbufferedClientConnection::new(config, name).map_err(tls_error)?;
        let (mut incoming, mut outgoing) = buffers;
        incoming.clear();
        outgoing.clear();
        let mut stream = Self {
            io,
            conn,
            incoming,
            outgoing,
            plain: VecDeque::new(),
            eof: false,
        };
        loop {
            let (wait, _) = stream.process(None)?;
            stream.send_pending().await?;
            match wait {
                Wait::Ready => return Ok(stream),
                Wait::Read => stream.receive().await?,
                Wait::Closed => return Err(ConnError(ErrorKind::ConnectionAborted)),
            }
        }
    }

    /// Run rustls as far as it goes without I/O: records decrypted into
    /// `plain`, records to send into `outgoing`, and (if `data` is given and
    /// the connection is ready) `data` encrypted. Returns what it waits for
    /// and whether `data` went.
    fn process(&mut self, data: Option<&[u8]>) -> Result<(Wait, bool), ConnError> {
        let mut sent = false;
        loop {
            let UnbufferedStatus { mut discard, state } =
                self.conn.process_tls_records(&mut self.incoming[..]);
            let state = state.map_err(tls_error)?;
            let wait = match state {
                ConnectionState::ReadTraffic(mut traffic) => {
                    while let Some(record) = traffic.next_record() {
                        let record = record.map_err(tls_error)?;
                        discard += record.discard;
                        grow(&mut self.plain, record.payload.len());
                        self.plain.extend(record.payload);
                    }
                    None
                }
                ConnectionState::EncodeTlsData(mut encode) => {
                    // A record fits 4 KiB mostly; else as large as it asks.
                    let start = self.outgoing.len();
                    self.outgoing.resize(start + 4096, 0);
                    let encoded = match encode.encode(&mut self.outgoing[start..]) {
                        Err(EncodeError::InsufficientSize(e)) => {
                            self.outgoing.resize(start + e.required_size, 0);
                            encode.encode(&mut self.outgoing[start..])
                        }
                        other => other,
                    };
                    match encoded {
                        Ok(n) => self.outgoing.truncate(start + n),
                        Err(_) => {
                            self.outgoing.truncate(start);
                            return Err(ConnError(ErrorKind::Other));
                        }
                    }
                    None
                }
                // What was encoded is in `outgoing`, sent before anything
                // after it: rustls may go on.
                ConnectionState::TransmitTlsData(transmit) => {
                    transmit.done();
                    None
                }
                ConnectionState::BlockedHandshake => Some(Wait::Read),
                ConnectionState::WriteTraffic(mut write) => {
                    if let Some(data) = data.filter(|_| !sent) {
                        let start = self.outgoing.len();
                        let mut room = data.len() + 1024;
                        loop {
                            self.outgoing.resize(start + room, 0);
                            match write.encrypt(data, &mut self.outgoing[start..]) {
                                Ok(n) => {
                                    self.outgoing.truncate(start + n);
                                    break;
                                }
                                Err(EncryptError::InsufficientSize(e)) => room = e.required_size,
                                Err(_) => {
                                    self.outgoing.truncate(start);
                                    return Err(ConnError(ErrorKind::Other));
                                }
                            }
                        }
                        sent = true;
                    }
                    Some(Wait::Ready)
                }
                ConnectionState::PeerClosed => {
                    self.eof = true;
                    Some(Wait::Ready)
                }
                ConnectionState::Closed => Some(Wait::Closed),
                // Servers only.
                _ => Some(Wait::Closed),
            };
            self.incoming.drain(..discard);
            if let Some(wait) = wait {
                return Ok((wait, sent));
            }
        }
    }

    /// Write `outgoing`; what was written leaves it at once (cancel-safe).
    async fn send_pending(&mut self) -> Result<(), ConnError> {
        while !self.outgoing.is_empty() {
            let n = self
                .io
                .write(&self.outgoing[..])
                .await
                .map_err(|e| ConnError(embedded_io_async::Error::kind(&e)))?;
            if n == 0 {
                return Err(ConnError(ErrorKind::WriteZero));
            }
            self.outgoing.drain(..n);
        }
        self.io
            .flush()
            .await
            .map_err(|e| ConnError(embedded_io_async::Error::kind(&e)))
    }

    /// Read ciphertext; it is kept only once the read returned.
    async fn receive(&mut self) -> Result<(), ConnError> {
        let mut chunk = [0u8; READ_CHUNK];
        let n = self
            .io
            .read(&mut chunk)
            .await
            .map_err(|e| ConnError(embedded_io_async::Error::kind(&e)))?;
        if n == 0 {
            self.eof = true;
        }
        let (len, cap) = (self.incoming.len(), self.incoming.capacity());
        if len + n > cap {
            self.incoming.reserve_exact(grown(len, n) - len);
        }
        self.incoming.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ConnError> {
        loop {
            if !self.plain.is_empty() {
                let n = buf.len().min(self.plain.len());
                for (slot, byte) in buf.iter_mut().zip(self.plain.drain(..n)) {
                    *slot = byte;
                }
                return Ok(n);
            }
            if self.eof {
                return Ok(0);
            }
            let (wait, _) = self.process(None)?;
            // Alerts and the like rustls wants sent.
            self.send_pending().await?;
            if !self.plain.is_empty() {
                continue;
            }
            match wait {
                Wait::Closed => return Ok(0),
                _ if self.eof => return Ok(0),
                _ => self.receive().await?,
            }
        }
    }

    /// Encrypt and send `data` whole. Not cancel-safe (as `write_all`): once
    /// `data` is encrypted it goes, so it must not be given again.
    pub async fn write(&mut self, data: &[u8]) -> Result<usize, ConnError> {
        if data.is_empty() {
            return Ok(0);
        }
        loop {
            let (wait, sent) = self.process(Some(data))?;
            if sent {
                self.send_pending().await?;
                return Ok(data.len());
            }
            self.send_pending().await?;
            match wait {
                Wait::Closed => return Err(ConnError(ErrorKind::BrokenPipe)),
                // Ready but not sent: only after the peer closed.
                Wait::Ready => return Err(ConnError(ErrorKind::BrokenPipe)),
                Wait::Read => self.receive().await?,
            }
        }
    }

    pub async fn flush(&mut self) -> Result<(), ConnError> {
        self.send_pending().await
    }
}

/// Boxed: the connection state is large.
pub type BoxedTls<'b, C> = Box<TlsStream<'b, C>>;
