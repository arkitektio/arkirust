//! The mesh on a board: an app's TCP routed between the deployment's mesh
//! and the network, and the hand-off of the login's mesh key to the app's
//! mesh node.
//!
//! Without std there is no local proxy into the mesh: the platform reaches
//! mesh addresses itself ([`fakts::MeshRoute::Direct`]). [`Routed`] is that
//! platform TCP: tailnet addresses (and the names the mesh resolves to
//! them) over the mesh node's TCP, everything else over embassy-net's. Give
//! it to `serve_http` and `Board`, so logins, mesh aliases and the agent
//! all go through it. [`MeshHandoff`] is the `MeshJoiner` fakts gets: it
//! passes the login's [`MeshJoin`] to the task that runs the mesh node
//! (which is not `Send`) and answers once that node joined.
//!
//! This crate does not depend on a mesh: anything with `TcpConnect` and
//! `Dns` over the tailnet (arkitekt-mesh-embassy's `Mesh`) will do.

use alloc::boxed::Box;
use alloc::string::String;
use core::fmt;
use core::net::{IpAddr, SocketAddr};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use fakts::{MeshJoin, MeshJoiner, MeshRoute};

/// Whether `ip` is a tailnet address: 100.64.0.0/10 (IPv4) or
/// fd7a:115c:a1e0::/48 (IPv6), as Tailscale and ionskale assign them.
pub fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// TCP and names over the mesh for the tailnet, over the network for the
/// rest.
pub struct Routed<'a, M, T, D> {
    pub mesh: &'a M,
    pub tcp: &'a T,
    pub dns: &'a D,
}

/// A connection or error of either side.
#[derive(Debug)]
pub enum Either<A, B> {
    Mesh(A),
    Net(B),
}

impl<A: fmt::Debug, B: fmt::Debug> fmt::Display for Either<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Either::Mesh(e) => write!(f, "mesh: {e:?}"),
            Either::Net(e) => write!(f, "{e:?}"),
        }
    }
}

impl<A: fmt::Debug, B: fmt::Debug> core::error::Error for Either<A, B> {}

impl<A: embedded_io_async::Error, B: embedded_io_async::Error> embedded_io_async::Error
    for Either<A, B>
{
    fn kind(&self) -> ErrorKind {
        match self {
            Either::Mesh(e) => e.kind(),
            Either::Net(e) => e.kind(),
        }
    }
}

impl<A: ErrorType, B: ErrorType> ErrorType for Either<A, B> {
    type Error = Either<A::Error, B::Error>;
}

impl<A: Read, B: Read> Read for Either<A, B> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            Either::Mesh(c) => c.read(buf).await.map_err(Either::Mesh),
            Either::Net(c) => c.read(buf).await.map_err(Either::Net),
        }
    }
}

impl<A: Write, B: Write> Write for Either<A, B> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        match self {
            Either::Mesh(c) => c.write(buf).await.map_err(Either::Mesh),
            Either::Net(c) => c.write(buf).await.map_err(Either::Net),
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        match self {
            Either::Mesh(c) => c.flush().await.map_err(Either::Mesh),
            Either::Net(c) => c.flush().await.map_err(Either::Net),
        }
    }
}

impl<M: TcpConnect, T: TcpConnect, D> TcpConnect for Routed<'_, M, T, D> {
    type Error = Either<M::Error, T::Error>;
    type Connection<'c>
        = Either<M::Connection<'c>, T::Connection<'c>>
    where
        Self: 'c;

    async fn connect<'c>(
        &'c self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'c>, Self::Error> {
        if is_tailnet(remote.ip()) {
            self.mesh
                .connect(remote)
                .await
                .map(Either::Mesh)
                .map_err(Either::Mesh)
        } else {
            self.tcp
                .connect(remote)
                .await
                .map(Either::Net)
                .map_err(Either::Net)
        }
    }
}

impl<M: Dns, T, D: Dns> Dns for Routed<'_, M, T, D> {
    type Error = Either<M::Error, D::Error>;

    /// The mesh's names first (a peer's hostname, or one in the tailnet's
    /// domain), kept only when they are tailnet addresses; then the network's.
    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(ip);
        }
        if let Ok(ip) = self.mesh.get_host_by_name(host, same(&addr_type)).await {
            if is_tailnet(ip) {
                return Ok(ip);
            }
        }
        self.dns
            .get_host_by_name(host, addr_type)
            .await
            .map_err(Either::Net)
    }

    async fn get_host_by_address(
        &self,
        addr: IpAddr,
        result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        self.dns
            .get_host_by_address(addr, result)
            .await
            .map_err(Either::Net)
    }
}

/// `AddrType` again (it is not `Copy`).
fn same(addr_type: &AddrType) -> AddrType {
    match addr_type {
        AddrType::IPv4 => AddrType::IPv4,
        AddrType::IPv6 => AddrType::IPv6,
        AddrType::Either => AddrType::Either,
    }
}

/// The `MeshJoiner` fakts gets on a board: the login's [`MeshJoin`] goes to
/// the task that runs the mesh node ([`next_join`](Self::next_join)), and
/// fakts waits until that task says the node joined
/// ([`joined`](Self::joined)) or will not ([`stay_off`](Self::stay_off)).
pub struct MeshHandoff {
    joins: Signal<CriticalSectionRawMutex, MeshJoin>,
    answers: Signal<CriticalSectionRawMutex, Result<bool, String>>,
}

impl Default for MeshHandoff {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshHandoff {
    pub const fn new() -> Self {
        Self {
            joins: Signal::new(),
            answers: Signal::new(),
        }
    }

    /// For the mesh task: the next join fakts asks for (its key, its
    /// coordination server, the node's hostname).
    pub async fn next_join(&self) -> MeshJoin {
        self.joins.wait().await
    }

    /// For the mesh task: the node is on the mesh; mesh aliases go through
    /// [`Routed`].
    pub fn joined(&self) {
        self.answers.signal(Ok(true));
    }

    /// For the mesh task: no mesh (no key, no node that joined before).
    pub fn stay_off(&self) {
        self.answers.signal(Ok(false));
    }

    /// For the mesh task: joining failed; the login fails with it.
    pub fn failed(&self, reason: String) {
        self.answers.signal(Err(reason));
    }
}

#[async_trait::async_trait]
impl MeshJoiner for MeshHandoff {
    async fn join(&self, join: MeshJoin) -> fakts::Result<Option<MeshRoute>> {
        self.answers.reset();
        self.joins.signal(join);
        match self.answers.wait().await {
            Ok(true) => Ok(Some(MeshRoute::Direct)),
            Ok(false) => Ok(None),
            Err(reason) => Err(fakts::FaktsError::Mesh(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailnet_ranges() {
        for ip in ["100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0:ab12::1"] {
            assert!(is_tailnet(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "100.63.255.255",
            "100.128.0.1",
            "10.0.0.1",
            "fd7a:115c:a1e1::1",
            "2001:db8::1",
        ] {
            assert!(!is_tailnet(ip.parse().unwrap()), "{ip}");
        }
    }
}
