//! Local port-forward listeners. The net thread hands us the server's desired rule set
//! (port-1 tag 5) via [`ForwardManager::reconcile`]; we keep one `127.0.0.1` listener
//! thread per rule, and per accepted socket open a data connection to the control-server
//! (`forward_addr`, its `:forward` port), send a [`ForwardHeader`], and splice. Status
//! (listening / bind error) is reported back through the `report` closure, which the net
//! thread turns into a port-1 tag-2 frame. Plain std threads/sockets — no async, no GTK.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wire::forward::{ForwardHeader, ForwardRule, ForwardState, ForwardStatusMsg};

/// Reports a rule's status back toward the server (the net thread frames it as tag 2).
pub type StatusReport = Arc<dyn Fn(ForwardStatusMsg) + Send + Sync>;

struct ListenerHandle {
    rule: ForwardRule,
    stop: Arc<AtomicBool>,
}

pub struct ForwardManager {
    report: StatusReport,
    active: Mutex<HashMap<String, ListenerHandle>>, // rule id → its listener
}

impl ForwardManager {
    pub fn new(report: StatusReport) -> Self {
        Self { report, active: Mutex::new(HashMap::new()) }
    }

    /// Make the running listeners match `rules`: stop any that were removed or whose
    /// `(local_port, remote_port, host_id)` changed; start listeners for new rules;
    /// leave unchanged rules running. `forward_addr` is `host:port` of the server's data
    /// port. Idempotent — a reconnect that pushes the same set is a no-op.
    pub fn reconcile(&self, rules: Vec<ForwardRule>, forward_addr: String) {
        let wanted: HashMap<String, ForwardRule> =
            rules.into_iter().map(|r| (r.id.clone(), r)).collect();
        let mut active = self.active.lock().unwrap();
        // Stop removed/changed listeners.
        active.retain(|id, h| {
            let keep = wanted.get(id).is_some_and(|w| *w == h.rule);
            if !keep {
                h.stop.store(true, Ordering::SeqCst);
            }
            keep
        });
        // Start new listeners.
        for (id, rule) in wanted {
            if active.contains_key(&id) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            let handle = ListenerHandle { rule: rule.clone(), stop: stop.clone() };
            let report = self.report.clone();
            let fa = forward_addr.clone();
            std::thread::spawn(move || run_listener(rule, fa, report, stop));
            active.insert(id, handle);
        }
    }
}

fn run_listener(rule: ForwardRule, forward_addr: String, report: StatusReport, stop: Arc<AtomicBool>) {
    let bind = format!("127.0.0.1:{}", rule.local_port);
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            report(ForwardStatusMsg {
                host_id: rule.host_id.clone(),
                id: rule.id.clone(),
                state: ForwardState::Error,
                error: Some(format!("{bind}: {e}")),
            });
            return;
        }
    };
    listener.set_nonblocking(true).ok();
    report(ForwardStatusMsg {
        host_id: rule.host_id.clone(),
        id: rule.id.clone(),
        state: ForwardState::Listening,
        error: None,
    });
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        match listener.accept() {
            Ok((sock, _)) => {
                let (fa, rule) = (forward_addr.clone(), rule.clone());
                std::thread::spawn(move || tunnel(sock, fa, rule));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return,
        }
    }
}

fn tunnel(local: TcpStream, forward_addr: String, rule: ForwardRule) {
    let mut up = match TcpStream::connect(&forward_addr) {
        Ok(s) => s,
        Err(_) => return,
    };
    up.set_nodelay(true).ok();
    let hdr = ForwardHeader {
        token: None,
        host_id: rule.host_id,
        id: rule.id,
        remote_port: rule.remote_port,
    };
    let Ok(body) = serde_json::to_vec(&hdr) else { return };
    if up.write_all(&(body.len() as u32).to_be_bytes()).is_err() || up.write_all(&body).is_err() {
        return;
    }
    // Bound the status handshake read so a stalled/hostile server can't park this thread
    // forever; cleared before the splice so an established idle tunnel isn't subject to it.
    let _ = up.set_read_timeout(Some(std::time::Duration::from_secs(10)));
    let mut status = [0u8; 1];
    if up.read_exact(&mut status).is_err() || status[0] != 0 {
        return; // dial failed server-side
    }
    let _ = up.set_read_timeout(None);
    splice(local, up);
}

fn splice(a: TcpStream, b: TcpStream) {
    let (mut a_rd, mut b_wr) = match (a.try_clone(), b.try_clone()) {
        (Ok(x), Ok(y)) => (x, y),
        _ => return,
    };
    let (mut b_rd, mut a_wr) = (b, a);
    let t = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_rd, &mut b_wr);
        let _ = b_wr.shutdown(std::net::Shutdown::Write);
    });
    let _ = std::io::copy(&mut b_rd, &mut a_wr);
    let _ = a_wr.shutdown(std::net::Shutdown::Write);
    let _ = t.join();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// A stub control-server data port: accept one conn, read the framed header, reply
    /// 0x00, then echo everything.
    fn spawn_stub() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut lb = [0u8; 4];
            s.read_exact(&mut lb).unwrap();
            let mut body = vec![0u8; u32::from_be_bytes(lb) as usize];
            s.read_exact(&mut body).unwrap();
            let _hdr: wire::forward::ForwardHeader = serde_json::from_slice(&body).unwrap();
            s.write_all(&[0u8]).unwrap();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).unwrap();
            s.write_all(&buf[..n]).unwrap();
        });
        addr
    }

    #[test]
    fn reconcile_binds_and_tunnels() {
        let stub = spawn_stub();
        let local = free_port();
        let reports: Arc<Mutex<Vec<ForwardStatusMsg>>> = Arc::new(Mutex::new(Vec::new()));
        let r2 = reports.clone();
        let mgr = ForwardManager::new(Arc::new(move |m| r2.lock().unwrap().push(m)));
        mgr.reconcile(
            vec![ForwardRule { host_id: "h".into(), id: "f".into(), remote_port: 3000, local_port: local }],
            stub,
        );
        // Give the listener a moment to bind.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let mut c = TcpStream::connect(("127.0.0.1", local)).unwrap();
        c.write_all(b"hey").unwrap();
        let mut got = [0u8; 3];
        c.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hey");
        assert!(reports.lock().unwrap().iter().any(|m| matches!(m.state, ForwardState::Listening)));
    }

    #[test]
    fn reconcile_reports_bind_conflict() {
        let local = free_port();
        let _hog = TcpListener::bind(("127.0.0.1", local)).unwrap(); // hold the port
        let reports: Arc<Mutex<Vec<ForwardStatusMsg>>> = Arc::new(Mutex::new(Vec::new()));
        let r2 = reports.clone();
        let mgr = ForwardManager::new(Arc::new(move |m| r2.lock().unwrap().push(m)));
        mgr.reconcile(
            vec![ForwardRule { host_id: "h".into(), id: "f".into(), remote_port: 1, local_port: local }],
            "127.0.0.1:1".into(),
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(reports.lock().unwrap().iter().any(|m| matches!(m.state, ForwardState::Error)));
    }
}
