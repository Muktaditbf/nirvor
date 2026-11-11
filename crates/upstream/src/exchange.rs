//! One query/answer exchange with a server: UDP, with TCP fallback on truncation.

use hickory_proto::op::{Message, MessageType};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::{Instant, timeout, timeout_at};
use tracing::debug;

/// Why forwarding failed.
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    /// Network error talking to the upstream.
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// No matching answer within the timeout.
    #[error("timed out")]
    Timeout,
    /// The upstream sent something we could not use.
    #[error("bad response: {0}")]
    Bad(String),
    /// Every usable upstream failed; holds the last error.
    #[error("all upstreams failed, last: {0}")]
    AllFailed(Box<UpstreamError>),
    /// No upstream is configured or allowed right now.
    #[error("no usable upstream")]
    NoneUsable,
}

/// Asks one server: UDP first, TCP if the UDP answer was truncated. Gives each step `wait`.
pub async fn exchange(
    server: SocketAddr,
    query: &Message,
    wait: Duration,
) -> Result<Message, UpstreamError> {
    let wire = query
        .to_vec()
        .map_err(|e| UpstreamError::Bad(e.to_string()))?;
    let msg = udp_exchange(server, query, &wire, wait).await?;
    if !msg.metadata.truncation {
        return Ok(msg);
    }
    debug!(%server, "truncated over UDP, retrying over TCP");
    timeout(wait, tcp_exchange(server, query, &wire))
        .await
        .map_err(|_| UpstreamError::Timeout)?
}

/// One UDP round trip. Ignores packets that are not the answer to `query`.
async fn udp_exchange(
    server: SocketAddr,
    query: &Message,
    wire: &[u8],
    wait: Duration,
) -> Result<Message, UpstreamError> {
    let local: SocketAddr = match server {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let sock = UdpSocket::bind(local).await?;
    sock.connect(server).await?;
    sock.send(wire).await?;
    let deadline = Instant::now() + wait;
    let mut buf = vec![0u8; 65_535];
    loop {
        let n = timeout_at(deadline, sock.recv(&mut buf))
            .await
            .map_err(|_| UpstreamError::Timeout)??;
        if let Ok(msg) = Message::from_vec(&buf[..n])
            && matches(query, &msg)
        {
            return Ok(msg);
        }
    }
}

/// One TCP round trip with the RFC 1035 §4.2.2 length prefix.
async fn tcp_exchange(
    server: SocketAddr,
    query: &Message,
    wire: &[u8],
) -> Result<Message, UpstreamError> {
    let len = u16::try_from(wire.len()).map_err(|_| UpstreamError::Bad("query too long".into()))?;
    let mut stream = TcpStream::connect(server).await?;
    let mut out = Vec::with_capacity(wire.len() + 2);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(wire);
    stream.write_all(&out).await?;
    let n = usize::from(stream.read_u16().await?);
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf).await?;
    let msg = Message::from_vec(&buf).map_err(|e| UpstreamError::Bad(e.to_string()))?;
    if !matches(query, &msg) {
        return Err(UpstreamError::Bad(
            "TCP answer does not match the query".into(),
        ));
    }
    Ok(msg)
}

/// True if `msg` is a response to `query`: same ID and same question. Guards against spoofed or stray packets.
pub fn matches(query: &Message, msg: &Message) -> bool {
    msg.metadata.message_type == MessageType::Response
        && msg.metadata.id == query.metadata.id
        && msg.queries == query.queries
}
