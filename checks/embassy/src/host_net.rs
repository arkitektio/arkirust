//! The host's network as embedded-nal-async, for the host binaries: TCP
//! through async-io (its own reactor thread), names through the host's
//! resolver, randomness from the OS.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use arkitekt_embassy::Entropy;
use embedded_io_adapters::futures_03::FromFutures;
use embedded_nal_async::{AddrType, Dns, TcpConnect};

/// The host's TCP as embedded-nal-async.
pub struct HostTcp;

impl TcpConnect for HostTcp {
    type Error = std::io::Error;
    type Connection<'a> = FromFutures<async_io::Async<std::net::TcpStream>>;

    async fn connect<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        Ok(FromFutures::new(
            async_io::Async::<std::net::TcpStream>::connect(remote).await?,
        ))
    }
}

/// The host's resolver as embedded-nal-async.
pub struct HostDns;

impl Dns for HostDns {
    type Error = std::io::Error;

    async fn get_host_by_name(
        &self,
        host: &str,
        addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        (host, 0)
            .to_socket_addrs()?
            .map(|a| a.ip())
            .find(|ip| match addr_type {
                AddrType::IPv4 => ip.is_ipv4(),
                AddrType::IPv6 => ip.is_ipv6(),
                AddrType::Either => true,
            })
            .ok_or_else(|| std::io::Error::other(format!("{host} does not resolve")))
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(std::io::Error::other("no reverse lookups"))
    }
}

pub struct HostEntropy;

impl Entropy for HostEntropy {
    fn fill(&self, bytes: &mut [u8]) {
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), bytes)
    }
}
