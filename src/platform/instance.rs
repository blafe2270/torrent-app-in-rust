//! Single instance: a second launch (e.g. clicking a magnet link while the app
//! is open) forwards its arguments to the running window and exits.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

const MAGIC: &str = "RUSTORRENT/1";

/// Per-user loopback port, so different users don't talk to each other.
fn addr() -> SocketAddr {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let hash = user
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b));
    SocketAddr::from(([127, 0, 0, 1], 47_000 + (hash % 900) as u16))
}

/// Returns `true` if another instance took the arguments (so this one should
/// exit). Otherwise starts listening for future launches.
pub fn forward_or_listen(args: &[String]) -> bool {
    let addr = addr();
    if let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let mut msg = format!("{MAGIC}\n");
        if args.is_empty() {
            msg.push('\n'); // just "show the window"
        }
        for a in args {
            msg.push_str(a);
            msg.push('\n');
        }
        msg.push_str("END\n");
        if s.write_all(msg.as_bytes()).is_ok() {
            let mut reply = String::new();
            if BufReader::new(&s).read_line(&mut reply).is_ok() && reply.trim() == "OK" {
                return true;
            }
        }
    }
    match TcpListener::bind(addr) {
        Ok(listener) => {
            std::thread::Builder::new()
                .name("single-instance".into())
                .spawn(move || serve(listener))
                .ok();
        }
        Err(e) => tracing::debug!("single-instance listener unavailable: {e}"),
    }
    false
}

fn serve(listener: TcpListener) {
    for conn in listener.incoming().flatten() {
        let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reader = BufReader::new(&conn);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.trim_end() != MAGIC {
            continue;
        }
        let mut args = Vec::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if line.trim_end() == "END" => break,
                Ok(_) => args.push(line.trim_end_matches(['\r', '\n']).to_string()),
            }
        }
        let _ = (&conn).write_all(b"OK\n");
        for a in args {
            super::deliver(a);
        }
    }
}
