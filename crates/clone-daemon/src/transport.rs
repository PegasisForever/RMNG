//! SOCK_SEQPACKET transport to control-server's media ingest. Each datagram is a
//! JSON `DaemonMsg`/`ServerMsg`; dmabuf fds ride along via `SCM_RIGHTS`. SEQPACKET
//! gives one-message-per-datagram framing, so a frame and its fds stay together.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use anyhow::{Context, Result, anyhow};
use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType, UnixAddr,
    connect, recvmsg, sendmsg, socket,
};
use wire::socket::{DaemonMsg, ServerMsg};

const MAX_PACKET_BYTES: usize = 32 * 1024 * 1024;

pub struct Transport {
    fd: OwnedFd,
    /// Puts a chunked message back together. One thread reads this connection, so the state
    /// sits behind a mutex rather than in `recv`'s signature.
    joiner: std::sync::Mutex<wire::socket::chunk::Reassembler>,
    /// Message ids for [`wire::socket::chunk::split`], unique within this connection.
    next_id: std::sync::atomic::AtomicU64,
}

impl Transport {
    /// Connect to control-server's bind-mounted media socket.
    pub fn connect(path: &str) -> Result<Self> {
        let fd = socket(AddressFamily::Unix, SockType::SeqPacket, SockFlag::empty(), None)
            .context("socket(AF_UNIX, SEQPACKET)")?;
        let addr = UnixAddr::new(path).context("UnixAddr")?;
        connect(fd.as_raw_fd(), &addr).with_context(|| format!("connect {path}"))?;
        Ok(Self {
            fd,
            joiner: std::sync::Mutex::new(Default::default()),
            next_id: std::sync::atomic::AtomicU64::new(1),
        })
    }

    /// Send a framed message + optional dmabuf fds (SCM_RIGHTS).
    ///
    /// A message past the datagram ceiling goes as several chunks (see
    /// [`wire::socket::chunk`]). Only a copied-out clipboard payload ever reaches that size;
    /// the messages that carry fds are small, so the fds ride the first datagram either way.
    pub fn send(&self, msg: &DaemonMsg, fds: &[RawFd]) -> Result<()> {
        use std::sync::atomic::Ordering;
        let json = serde_json::to_vec(msg)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        for (i, datagram) in wire::socket::chunk::split(&json, id).into_iter().enumerate() {
            let iov = [IoSlice::new(&datagram)];
            let scm = [ControlMessage::ScmRights(fds)];
            let cmsgs: &[ControlMessage] = if fds.is_empty() || i > 0 { &[] } else { &scm };
            sendmsg::<()>(self.fd.as_raw_fd(), &iov, cmsgs, MsgFlags::empty(), None)
                .context("sendmsg")?;
        }
        Ok(())
    }

    /// Receive one `ServerMsg` (server → daemon carries no fds). Blocking until a whole
    /// message has arrived, which for a chunked one means every chunk of it.
    pub fn recv(&self) -> Result<ServerMsg> {
        loop {
            let packet_len = recv_packet_len(self.fd.as_raw_fd())?;
            let mut buf = vec![0u8; packet_len];
            let mut iov = [IoSliceMut::new(&mut buf)];
            let mut cmsg = nix::cmsg_space!([RawFd; 8]);
            let msg: nix::sys::socket::RecvMsg<()> =
                recvmsg(self.fd.as_raw_fd(), &mut iov, Some(&mut cmsg), MsgFlags::empty())
                    .context("recvmsg")?;
            // Drain any (unexpected) fds so they don't leak.
            if let Ok(cmsgs) = msg.cmsgs() {
                for c in cmsgs {
                    if let ControlMessageOwned::ScmRights(fds) = c {
                        for fd in fds {
                            let _ = nix::unistd::close(fd);
                        }
                    }
                }
            }
            let n = msg.bytes;
            if n == 0 {
                return Err(anyhow!("peer closed the media socket"));
            }
            let Some(whole) = self.joiner.lock().unwrap().push(&buf[..n]) else {
                continue; // A chunk, and not the last one.
            };
            return serde_json::from_slice(&whole).context("decode ServerMsg");
        }
    }
}

fn recv_packet_len(fd: RawFd) -> Result<usize> {
    let mut one = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut one)];
    let msg: nix::sys::socket::RecvMsg<()> =
        recvmsg(fd, &mut iov, None, MsgFlags::MSG_PEEK | MsgFlags::MSG_TRUNC)
            .context("recvmsg peek")?;
    let n = msg.bytes;
    if n == 0 {
        return Err(anyhow!("peer closed the media socket"));
    }
    if n > MAX_PACKET_BYTES {
        return Err(anyhow!("packet too large: {n} bytes > {MAX_PACKET_BYTES}"));
    }
    Ok(n)
}
