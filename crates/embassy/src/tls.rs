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
//!
//! Sessions are resumed: [`config`] makes one config per [`Verify`] and
//! hands it out again (rustls resumes a session only under the verifier it
//! was made with), so a server a board shook hands with before takes a key
//! exchange only (TLS 1.3 tickets), not its certificate chain again, which
//! costs seconds there.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;

use embedded_io_async::{ErrorKind, Read, Write};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::{
    ClientSessionStore, Resumption, Tls12ClientSessionValue, Tls13ClientSessionValue,
    UnbufferedClientConnection,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::time_provider::TimeProvider;
use rustls::unbuffered::{ConnectionState, EncodeError, EncryptError, UnbufferedStatus};
use rustls::{ClientConfig, DigitallySignedStruct, NamedGroup, RootCertStore, SignatureScheme};

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

/// Servers whose sessions a config keeps.
const SESSION_SERVERS: usize = 8;
/// TLS 1.3 tickets kept per server (rustls's own cache keeps as many).
const TICKETS_PER_SERVER: usize = 8;

/// What a config remembers of one server.
struct Server {
    name: ServerName<'static>,
    kx_hint: Option<NamedGroup>,
    tls12: Option<Tls12ClientSessionValue>,
    /// Oldest first.
    tls13: VecDeque<Tls13ClientSessionValue>,
}

/// The sessions to resume, on a spin lock: rustls's `ClientSessionMemoryCache`
/// is made differently with and without its `std` feature, which another
/// crate in the build may turn on. Least recently stored server goes first.
#[derive(Default)]
struct Sessions(spin::Mutex<VecDeque<Server>>);

impl core::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Sessions")
    }
}

impl Sessions {
    /// Changes the server's entry, made if new (the oldest dropped for it),
    /// moved to the back.
    fn store(&self, name: ServerName<'static>, change: impl FnOnce(&mut Server)) {
        let mut servers = self.0.lock();
        let mut server = match servers.iter().position(|s| s.name == name) {
            Some(i) => servers.remove(i).expect("a found index"),
            None => {
                if servers.len() == SESSION_SERVERS {
                    servers.pop_front();
                }
                Server {
                    name,
                    kx_hint: None,
                    tls12: None,
                    tls13: VecDeque::new(),
                }
            }
        };
        change(&mut server);
        servers.push_back(server);
    }

    fn read<R>(&self, name: &ServerName<'_>, read: impl FnOnce(&mut Server) -> R) -> Option<R> {
        self.0.lock().iter_mut().find(|s| s.name == *name).map(read)
    }
}

impl ClientSessionStore for Sessions {
    fn set_kx_hint(&self, name: ServerName<'static>, group: NamedGroup) {
        self.store(name, |s| s.kx_hint = Some(group));
    }

    fn kx_hint(&self, name: &ServerName<'_>) -> Option<NamedGroup> {
        self.read(name, |s| s.kx_hint).flatten()
    }

    fn set_tls12_session(&self, name: ServerName<'static>, value: Tls12ClientSessionValue) {
        self.store(name, |s| s.tls12 = Some(value));
    }

    fn tls12_session(&self, name: &ServerName<'_>) -> Option<Tls12ClientSessionValue> {
        self.read(name, |s| s.tls12.clone()).flatten()
    }

    fn remove_tls12_session(&self, name: &ServerName<'static>) {
        self.read(name, |s| s.tls12 = None);
    }

    fn insert_tls13_ticket(&self, name: ServerName<'static>, value: Tls13ClientSessionValue) {
        self.store(name, |s| {
            if s.tls13.len() == TICKETS_PER_SERVER {
                s.tls13.pop_front();
            }
            s.tls13.push_back(value);
        });
    }

    fn take_tls13_ticket(&self, name: &ServerName<'static>) -> Option<Tls13ClientSessionValue> {
        self.read(name, |s| s.tls13.pop_back()).flatten()
    }
}

/// The configs made, one per [`Verify`] (a firmware has one or two).
static CONFIGS: spin::Mutex<Vec<(Verify, Arc<ClientConfig>)>> = spin::Mutex::new(Vec::new());

/// A client config: TLS 1.3 and 1.2, checked against `verify`, its sessions
/// resumed. The same one each time for the same `verify`.
pub fn config(verify: &Verify) -> Result<Arc<ClientConfig>, rustls::Error> {
    if let Some((_, config)) = CONFIGS.lock().iter().find(|(v, _)| v == verify) {
        return Ok(config.clone());
    }
    let config = make_config(verify)?;
    CONFIGS.lock().push((verify.clone(), config.clone()));
    Ok(config)
}

fn make_config(verify: &Verify) -> Result<Arc<ClientConfig>, rustls::Error> {
    let provider = Arc::new(rustls_rustcrypto::provider());
    let builder = ClientConfig::builder_with_details(provider.clone(), Arc::new(Clock))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?;
    let roots = |ders: &[&Vec<u8>]| -> Result<RootCertStore, rustls::Error> {
        let mut roots = RootCertStore::empty();
        for der in ders {
            roots.add(CertificateDer::from((*der).clone()))?;
        }
        Ok(roots)
    };
    let mut config = match verify {
        Verify::Ca(der) => builder
            .with_root_certificates(roots(&[der])?)
            .with_no_client_auth(),
        Verify::Roots(ders) => builder
            .with_root_certificates(roots(&ders.iter().collect::<Vec<_>>())?)
            .with_no_client_auth(),
        Verify::Unverified => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Unverified(provider)))
            .with_no_client_auth(),
    };
    config.resumption = Resumption::store(Arc::new(Sessions::default()));
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

    /// Whether the handshake was full or resumed a session.
    pub fn handshake_kind(&self) -> Option<rustls::HandshakeKind> {
        self.conn.handshake_kind()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_keep_the_servers_last_stored() {
        let sessions = Sessions::default();
        let name = |i: usize| ServerName::try_from(alloc::format!("s{i}.example")).unwrap();
        for i in 0..=SESSION_SERVERS {
            sessions.set_kx_hint(name(i), NamedGroup::X25519);
        }
        // The first went for the one over.
        assert_eq!(sessions.kx_hint(&name(0)), None);
        assert_eq!(sessions.kx_hint(&name(1)), Some(NamedGroup::X25519));
        // Storing again moves a server to the back: s2 goes next, not s1.
        sessions.set_kx_hint(name(1), NamedGroup::secp256r1);
        sessions.set_kx_hint(name(SESSION_SERVERS + 1), NamedGroup::X25519);
        assert_eq!(sessions.kx_hint(&name(1)), Some(NamedGroup::secp256r1));
        assert_eq!(sessions.kx_hint(&name(2)), None);
        assert_eq!(sessions.0.lock().len(), SESSION_SERVERS);
    }

    #[test]
    fn one_config_per_verify_so_sessions_resume() {
        let a = Verify::Roots(Vec::new());
        let first = config(&a).unwrap();
        assert!(Arc::ptr_eq(&first, &config(&a).unwrap()));
        assert!(!Arc::ptr_eq(&first, &config(&Verify::Unverified).unwrap()));
    }
}
