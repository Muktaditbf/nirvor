//! UDP and TCP listeners.

use crate::config::Config;
use crate::handler::{Resolver, Transport};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tracing::{debug, info, warn};

/// A bound but not yet running server. Binding first lets tests use port 0 and read the real ports.
pub struct Server {
    udp: Vec<Arc<UdpSocket>>,
    tcp: Vec<TcpListener>,
    resolver: Arc<Resolver>,
    udp_slots: Arc<Semaphore>,
    tcp_slots: Arc<Semaphore>,
    tcp_idle: Duration,
    udp_per_addr: usize,
}

impl Server {
    /// Binds UDP and TCP on every `server.listen` address.
    pub async fn bind(config: &Config) -> io::Result<Self> {
        let storage = nirvor_storage::Storage::open(&config.resilience.data_dir.join("nirvor.db"))
            .await
            .map_err(io::Error::other)?;
        let mut udp = Vec::new();
        let mut tcp = Vec::new();
        let per_addr = config
            .server
            .udp_sockets
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        for &addr in &config.server.listen {
            let first = udp_socket(addr)?;
            // Same port for the extra sockets and TCP, even when the config said port 0.
            let bound = first.local_addr()?;
            udp.push(Arc::new(first));
            for _ in 1..per_addr.max(1) {
                udp.push(Arc::new(udp_socket(bound)?));
            }
            tcp.push(TcpListener::bind(bound).await?);
        }
        let server = Self {
            udp,
            tcp,
            resolver: Resolver::new(config, storage).map_err(io::Error::other)?,
            udp_slots: Arc::new(Semaphore::new(config.limits.max_inflight_udp)),
            tcp_slots: Arc::new(Semaphore::new(config.limits.max_tcp_connections)),
            tcp_idle: Duration::from_millis(config.limits.tcp_idle_timeout_ms),
            udp_per_addr: per_addr.max(1),
        };
        server
            .resolver
            .blocklists
            .set_sources(config.policy.blocklists.clone())
            .await;
        Ok(server)
    }

    /// The addresses actually bound, one per listen entry.
    pub fn local_addrs(&self) -> io::Result<Vec<SocketAddr>> {
        let mut out: Vec<SocketAddr> = Vec::new();
        for s in &self.udp {
            let a = s.local_addr()?;
            if !out.contains(&a) {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// The shared resolver (cache, metrics, upstreams).
    pub fn resolver(&self) -> Arc<Resolver> {
        self.resolver.clone()
    }

    /// Serves until a listener fails.
    pub async fn run(self) -> io::Result<()> {
        let mut tasks = JoinSet::new();
        tokio::spawn(self.resolver.upstreams.clone().run_health_checks());
        tokio::spawn(self.resolver.root.clone().run());
        tokio::spawn(self.resolver.clone().run_snapshots());
        tokio::spawn(self.resolver.clone().run_pinned());
        tokio::spawn(self.resolver.clone().run_mode_monitor());
        tokio::spawn(self.resolver.clone().run_history());
        let every = Duration::from_secs(self.resolver.settings().policy.reload_check_secs.max(5));
        tokio::spawn(self.resolver.blocklists.clone().run(every));
        let metrics = self.resolver.metrics.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(1));
            loop {
                every.tick().await;
                metrics.tick();
            }
        });
        let mut announced = Vec::new();
        for sock in self.udp {
            let addr = sock.local_addr()?;
            if !announced.contains(&addr) {
                info!(%addr, sockets = self.udp_per_addr, "listening on UDP");
                announced.push(addr);
            }
            tasks.spawn(udp_loop(
                sock,
                self.resolver.clone(),
                self.udp_slots.clone(),
            ));
        }
        for listener in self.tcp {
            info!(addr = %listener.local_addr()?, "listening on TCP");
            tasks.spawn(tcp_loop(
                listener,
                self.resolver.clone(),
                self.tcp_slots.clone(),
                self.tcp_idle,
            ));
        }
        match tasks.join_next().await {
            Some(Ok(result)) => result,
            Some(Err(e)) => Err(io::Error::other(e)),
            None => Ok(()),
        }
    }
}

/// A non-blocking UDP socket with SO_REUSEPORT, so several sockets can share one port and the
/// kernel spreads queries across them.
fn udp_socket(addr: SocketAddr) -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        s.set_only_v6(true)?;
    }
    s.set_reuse_port(true)?;
    s.set_nonblocking(true)?;
    s.bind(&addr.into())?;
    UdpSocket::from_std(s.into())
}

async fn udp_loop(
    sock: Arc<UdpSocket>,
    resolver: Arc<Resolver>,
    slots: Arc<Semaphore>,
) -> io::Result<()> {
    let mut buf = vec![0u8; 65_535];
    loop {
        let (n, peer) = match sock.recv_from(&mut buf).await {
            Ok(r) => r,
            // ICMP errors from earlier replies surface here on some systems; they are not fatal.
            Err(e) => {
                debug!(error = %e, "UDP receive error");
                continue;
            }
        };
        let packet = buf[..n].to_vec();
        let r = resolver.clone();
        let mut reply = Box::pin(async move { r.handle(&packet, Transport::Udp, peer.ip()).await });
        // Cache hits finish on the first poll: answer inline, no task spawn. Spawning re-polls
        // the future with a real waker, so the no-op waker used here is never relied on.
        let polled = reply.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        if let Poll::Ready(out) = polled {
            if let Some(out) = out
                && let Err(e) = sock.send_to(&out, peer).await
            {
                debug!(%peer, error = %e, "UDP send failed");
            }
            continue;
        }
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            warn!(%peer, "too many UDP queries in flight, dropping");
            continue;
        };
        let sock = sock.clone();
        tokio::spawn(async move {
            if let Some(out) = reply.await
                && let Err(e) = sock.send_to(&out, peer).await
            {
                debug!(%peer, error = %e, "UDP send failed");
            }
            drop(permit);
        });
    }
}

async fn tcp_loop(
    listener: TcpListener,
    resolver: Arc<Resolver>,
    slots: Arc<Semaphore>,
    idle: Duration,
) -> io::Result<()> {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "TCP accept failed");
                continue;
            }
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            warn!(%peer, "too many TCP connections, closing");
            continue;
        };
        let resolver = resolver.clone();
        tokio::spawn(async move {
            if let Err(e) = tcp_conn(stream, &resolver, idle, peer.ip()).await {
                debug!(%peer, error = %e, "TCP connection ended");
            }
            drop(permit);
        });
    }
}

/// Answers queries on one connection in order until the client closes or goes idle.
async fn tcp_conn(
    mut stream: TcpStream,
    resolver: &Arc<Resolver>,
    idle: Duration,
    peer: std::net::IpAddr,
) -> io::Result<()> {
    loop {
        let len = match timeout(idle, stream.read_u16()).await {
            Err(_) => return Ok(()), // idle timeout
            Ok(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Ok(r) => usize::from(r?),
        };
        let mut packet = vec![0u8; len];
        timeout(idle, stream.read_exact(&mut packet))
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        let Some(reply) = resolver.handle(&packet, Transport::Tcp, peer).await else {
            return Ok(()); // unanswerable: close, as there is no ID to reply to
        };
        let Ok(reply_len) = u16::try_from(reply.len()) else {
            return Err(io::Error::other("reply longer than 65535 bytes"));
        };
        let mut out = Vec::with_capacity(reply.len() + 2);
        out.extend_from_slice(&reply_len.to_be_bytes());
        out.extend_from_slice(&reply);
        stream.write_all(&out).await?;
    }
}
