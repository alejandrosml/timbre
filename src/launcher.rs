//! The launcher. Claude starts `timbre serve`; on macOS this first process
//! runs the real server as a child and relays its standard input and output:
//!
//! - the child is started responsible for itself, so macOS checks Timbre's
//!   own permissions rather than Claude's (see responsibility.rs);
//! - when the child exits with [`RESTART`] (it was waiting for Full Disk
//!   Access and has just seen it granted), the launcher starts a new one and
//!   replays the MCP handshake: Claude keeps its session and its tools, and
//!   requests still unanswered are sent again.
//!
//! Elsewhere the server runs in this process, unless `TIMBRE_LAUNCHER=1`
//! asks for the launcher (the end-to-end test uses it on Linux).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Exit code by which the server asks to be started again.
pub const RESTART: i32 = 75;
/// Set in the child: it runs under the launcher.
const LAUNCHED: &str = "TIMBRE_LAUNCHED";
/// Set by hand (0.4.2), it still keeps everything in one process.
const LEGACY: &str = "TIMBRE_DISCLAIMED";

/// Whether this process is a server started by the launcher.
pub fn launched() -> bool {
    std::env::var_os(LAUNCHED).is_some()
}

/// A started child.
pub struct Spawned {
    pub stdin: Option<Box<dyn Write + Send>>,
    pub stdout: Option<Box<dyn Read + Send>>,
    pub wait: Box<dyn FnOnce() -> i32 + Send>,
}

/// How children are started: responsible for themselves on macOS when the
/// system allows it, as plain children otherwise.
#[derive(Clone, Copy)]
enum Spawner {
    #[cfg(target_os = "macos")]
    Disclaimed,
    Plain,
}

impl Spawner {
    fn pick() -> Spawner {
        #[cfg(target_os = "macos")]
        if crate::responsibility::can_disclaim() {
            return Spawner::Disclaimed;
        }
        Spawner::Plain
    }

    /// Starts `timbre <args>` under the launcher, with piped standard input
    /// and output (`pipes`) or with all three on /dev/null.
    fn spawn(self, args: &[OsString], pipes: bool) -> std::io::Result<Spawned> {
        match self {
            #[cfg(target_os = "macos")]
            Spawner::Disclaimed => {
                let c = crate::responsibility::spawn_disclaimed(args, &[(LAUNCHED, "1")], pipes)
                    .map_err(std::io::Error::other)?;
                let pid = c.pid;
                Ok(Spawned {
                    stdin: c.stdin.map(|f| Box::new(f) as Box<dyn Write + Send>),
                    stdout: c.stdout.map(|f| Box::new(f) as Box<dyn Read + Send>),
                    wait: Box::new(move || crate::responsibility::wait(pid)),
                })
            }
            Spawner::Plain => {
                use std::process::{Command, Stdio};
                let mut cmd = Command::new(std::env::current_exe()?);
                cmd.args(args).env(LAUNCHED, "1");
                if pipes {
                    cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
                } else {
                    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                }
                let mut child = cmd.spawn()?;
                Ok(Spawned {
                    stdin: child.stdin.take().map(|s| Box::new(s) as Box<dyn Write + Send>),
                    stdout: child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
                    wait: Box::new(move || match child.wait() {
                        Ok(status) => {
                            use std::os::unix::process::ExitStatusExt;
                            status.code().or_else(|| status.signal().map(|s| 128 + s)).unwrap_or(1)
                        }
                        Err(_) => 1,
                    }),
                })
            }
        }
    }
}

/// In the process Claude started: run the server under the launcher when
/// it is wanted, and return the code to exit with. `None`: serve here.
pub fn run_if_needed() -> Option<i32> {
    if launched() || std::env::var_os(LEGACY).is_some() {
        return None;
    }
    // Always on macOS: even when this process already answers for its own
    // permissions (Claude starting the executable of Timbre.app), the
    // launcher is what lets the server restart once access is granted.
    let forced = std::env::var("TIMBRE_LAUNCHER").is_ok_and(|v| v == "1");
    if !(cfg!(target_os = "macos") || forced) {
        return None;
    }
    let spawner = Spawner::pick();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // The first start must work, or the server runs here instead.
    let first = match spawner.spawn(&args, true) {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("could not start the server with its own permissions, Claude's apply: {e}");
            return None;
        }
    };
    Some(relay(spawner, &args, first))
}

/// In a launched server: exit when the launcher is gone, in case Claude
/// stops it without closing standard input.
pub fn watch_parent() {
    if !launched() {
        return;
    }
    let launcher = std::os::unix::process::parent_id();
    let _ = std::thread::Builder::new().name("timbre-launcher".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if std::os::unix::process::parent_id() != launcher {
                tracing::info!("launcher gone, exiting");
                std::process::exit(0);
            }
        }
    });
}

/// Exit codes of `timbre probe`.
pub const PROBE_OK: i32 = 0;
/// Mail's data is there but macOS does not let Timbre read it.
pub const PROBE_NO_ACCESS: i32 = 1;
/// Something else: Mail never set up, a damaged or unknown database.
pub const PROBE_OTHER: i32 = 2;

/// In a launched server that cannot read Mail's data yet: check every few
/// seconds, from a fresh process (macOS settles a process's access when it
/// starts), keep `why` up to date, and ask the launcher for a restart once
/// Mail's data is readable.
pub fn restart_when_readable(why: Arc<Mutex<String>>) {
    if !launched() {
        return;
    }
    let _ = std::thread::Builder::new().name("timbre-access".into()).spawn(move || {
        let started = Instant::now();
        let spawner = Spawner::pick();
        let mut code = PROBE_NO_ACCESS;
        loop {
            // Access is usually granted within minutes; other causes (Mail to
            // set up) take longer to change.
            let early = started.elapsed() < Duration::from_secs(600);
            let pause = match code {
                PROBE_NO_ACCESS if early => 2,
                PROBE_NO_ACCESS => 10,
                _ if early => 5,
                _ => 30,
            };
            std::thread::sleep(Duration::from_secs(pause));
            let Ok(probe) = spawner.spawn(&["probe".into()], true) else { continue };
            drop(probe.stdin);
            let mut said = String::new();
            if let Some(mut out) = probe.stdout {
                let _ = out.read_to_string(&mut said);
            }
            code = (probe.wait)();
            if code == PROBE_OK {
                tracing::info!("Mail's data is readable now: restarting");
                std::process::exit(RESTART);
            }
            let said = said.trim();
            if !said.is_empty() {
                *why.lock().unwrap_or_else(|e| e.into_inner()) = said.to_string();
            }
        }
    });
}

/// What the launcher keeps between the client (Claude) and the server.
#[derive(Default)]
struct Relay {
    /// Lines for the current server, written by its own thread.
    to_child: Option<mpsc::Sender<String>>,
    /// Lines may go straight to the server (it has the handshake).
    ready: bool,
    /// Notifications and replies from Claude while the server restarts.
    queue: Vec<String>,
    /// Claude's `initialize` request (id, line), and whether it was answered.
    initialize: Option<(Value, String)>,
    init_answered: bool,
    /// Claude's `notifications/initialized`.
    initialized: Option<String>,
    /// Requests not answered yet, in arrival order: sent again on restart.
    pending: BTreeMap<u64, (Value, String)>,
    seq: u64,
    /// Id of the replayed `initialize`, whose answer Claude already has.
    replaying: Option<Value>,
    client_closed: bool,
}

impl Relay {
    fn write_child(&mut self, line: &str) {
        if let Some(tx) = &self.to_child
            && tx.send(line.to_string()).is_err()
        {
            self.to_child = None;
        }
    }

    /// A new server: replay the handshake if Claude completed it, else send
    /// what waits.
    fn start_child(&mut self, tx: mpsc::Sender<String>) {
        if self.client_closed {
            return; // Claude is gone: `tx` dropped, the server reads end of input and stops
        }
        self.to_child = Some(tx);
        match self.initialize.clone() {
            Some((id, line)) if self.init_answered => {
                self.ready = false;
                self.replaying = Some(id);
                self.write_child(&line);
            }
            _ => self.flush(),
        }
    }

    /// Send unanswered requests in order, then the other waiting messages.
    fn flush(&mut self) {
        if self.initialize.is_none() && self.pending.is_empty() {
            // A client without a handshake: notifications and replies meant
            // for the previous server must not be the new one's first message.
            self.queue.clear();
        }
        let lines: Vec<String> =
            self.pending.values().map(|(_, l)| l.clone()).chain(std::mem::take(&mut self.queue)).collect();
        for l in &lines {
            self.write_child(l);
        }
        self.ready = true;
    }

    fn on_client(&mut self, line: String, msg: Option<&Value>) {
        let method = msg.and_then(|m| m.get("method")).and_then(Value::as_str);
        let id = msg.and_then(|m| m.get("id")).filter(|v| !v.is_null()).cloned();
        if method == Some("notifications/cancelled")
            && let Some(cancelled) = msg.and_then(|m| m.pointer("/params/requestId"))
        {
            // Not to be run again after a restart.
            self.pending.retain(|_, (p, _)| !same_id(p, cancelled));
        }
        match (method, id) {
            (Some(method), Some(id)) => {
                if method == "initialize" {
                    self.initialize = Some((id.clone(), line.clone()));
                    self.init_answered = false;
                }
                self.seq += 1;
                self.pending.insert(self.seq, (id, line.clone()));
                if self.ready {
                    self.write_child(&line);
                }
            }
            _ => {
                if method == Some("notifications/initialized") {
                    self.initialized = Some(line.clone());
                    if !self.ready {
                        return; // sent after the replayed handshake
                    }
                }
                if self.ready {
                    self.write_child(&line);
                } else {
                    self.queue.push(line);
                }
            }
        }
    }

    /// Bookkeeping for a message from the server; false when Claude must not
    /// see it (the answer to a replayed handshake).
    fn on_child(&mut self, msg: Option<&Value>) -> bool {
        let Some(m) = msg else { return true };
        if m.get("method").is_some() {
            return true;
        }
        let Some(id) = m.get("id").filter(|v| !v.is_null()) else { return true };
        if self.replaying.as_ref().is_some_and(|r| same_id(r, id)) {
            self.replaying = None;
            if let Some(l) = self.initialized.clone() {
                self.write_child(&l);
            }
            self.flush();
            return false;
        }
        if let Some(seq) = self.pending.iter().find(|(_, (p, _))| same_id(p, id)).map(|(s, _)| *s) {
            self.pending.remove(&seq);
        }
        if self.initialize.as_ref().is_some_and(|(i, _)| same_id(i, id)) {
            self.init_answered = true;
        }
        true
    }
}

/// JSON-RPC ids compare by value: 7 and 7.0 are the same request.
fn same_id(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Lines of a stream, as text even when a byte is not UTF-8 (rmcp ignores
/// a line it cannot read; it must not end the session).
fn lines_lossy(mut input: impl BufRead) -> impl Iterator<Item = String> {
    let mut buf = Vec::new();
    std::iter::from_fn(move || {
        buf.clear();
        match input.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(String::from_utf8_lossy(&buf).trim_end_matches(['\n', '\r']).to_string()),
        }
    })
}

fn lock(state: &Mutex<Relay>) -> MutexGuard<'_, Relay> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Write one line per message to a server's standard input.
fn writer(mut stdin: Box<dyn Write + Send>, rx: mpsc::Receiver<String>) {
    for line in rx {
        if stdin.write_all(line.as_bytes()).and_then(|_| stdin.write_all(b"\n")).and_then(|_| stdin.flush()).is_err() {
            break;
        }
    }
}

/// Forward a server's messages to Claude.
fn forward_child(stdout: Box<dyn Read + Send>, state: &Mutex<Relay>) {
    let out = std::io::stdout();
    for line in lines_lossy(BufReader::new(stdout)) {
        let msg = serde_json::from_str::<Value>(&line).ok();
        if !lock(state).on_child(msg.as_ref()) {
            continue;
        }
        let mut o = out.lock();
        if o.write_all(line.as_bytes()).and_then(|_| o.write_all(b"\n")).and_then(|_| o.flush()).is_err() {
            break;
        }
    }
}

/// Forward Claude's messages to the server, keeping what a restart needs.
fn forward_client(input: impl BufRead, state: &Mutex<Relay>) {
    for line in lines_lossy(input) {
        if line.trim().is_empty() {
            continue;
        }
        let msg = serde_json::from_str::<Value>(&line).ok();
        lock(state).on_client(line, msg.as_ref());
    }
    let mut st = lock(state);
    st.client_closed = true;
    st.to_child = None; // the server reads end of input and stops
}

fn relay(spawner: Spawner, args: &[OsString], first: Spawned) -> i32 {
    let state = Arc::new(Mutex::new(Relay::default()));
    let mut next = Some(first);
    let mut client = None;
    loop {
        let child = match next.take() {
            Some(c) => c,
            None => {
                // A busy system may refuse a new process for a moment.
                let mut tries = 0;
                loop {
                    match spawner.spawn(args, true) {
                        Ok(c) => break c,
                        Err(e) if tries < 5 => {
                            tracing::warn!("could not restart the server ({e}), trying again");
                            tries += 1;
                            std::thread::sleep(Duration::from_secs(1));
                        }
                        Err(e) => {
                            tracing::error!("could not restart the server: {e}");
                            return 1;
                        }
                    }
                }
            }
        };
        let (Some(stdin), Some(stdout)) = (child.stdin, child.stdout) else { return 1 };
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("timbre-to-server".into()).spawn(move || writer(stdin, rx));
        lock(&state).start_child(tx);
        let reader = {
            let state = state.clone();
            std::thread::Builder::new().name("timbre-from-server".into()).spawn(move || forward_child(stdout, &state))
        };
        if client.is_none() {
            let state = state.clone();
            client = std::thread::Builder::new()
                .name("timbre-from-claude".into())
                .spawn(move || forward_client(std::io::stdin().lock(), &state))
                .ok();
        }
        let code = (child.wait)();
        if let Ok(reader) = reader {
            let _ = reader.join();
        }
        let mut st = lock(&state);
        st.to_child = None;
        st.ready = false;
        if code != RESTART {
            return code;
        }
        if st.client_closed {
            return 0;
        }
        tracing::info!("starting the server again");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: Value) -> (String, Value) {
        (v.to_string(), v)
    }

    fn sent(rx: &mpsc::Receiver<String>) -> Vec<Value> {
        rx.try_iter().map(|l| serde_json::from_str(&l).unwrap()).collect()
    }

    #[test]
    fn a_restart_replays_the_handshake_and_unanswered_requests() {
        let mut r = Relay::default();
        let (tx, rx) = mpsc::channel();
        r.start_child(tx);
        let (l, v) = line(serde_json::json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}));
        r.on_client(l, Some(&v));
        assert!(r.on_child(Some(&serde_json::json!({"jsonrpc":"2.0","id":0,"result":{}}))));
        let (l, v) = line(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        r.on_client(l, Some(&v));
        let (l, v) = line(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"a"}}));
        r.on_client(l, Some(&v));
        let (l, v) = line(serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"b"}}));
        r.on_client(l, Some(&v));
        assert!(r.on_child(Some(&serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}))));
        assert_eq!(sent(&rx).len(), 4, "everything went straight to the first server");

        // The server exits for a restart; Claude asks something meanwhile.
        r.to_child = None;
        r.ready = false;
        let (l, v) = line(serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}));
        r.on_client(l, Some(&v));
        let (l, v) = line(serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}));
        r.on_client(l, Some(&v));

        let (tx, rx) = mpsc::channel();
        r.start_child(tx);
        let first = sent(&rx);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["method"], "initialize");
        // Its answer stays with the launcher; then the rest follows in order.
        assert!(!r.on_child(Some(&serde_json::json!({"jsonrpc":"2.0","id":0,"result":{}}))));
        let rest: Vec<Value> = sent(&rx);
        let methods: Vec<&str> = rest.iter().map(|m| m["method"].as_str().unwrap()).collect();
        assert_eq!(methods, ["notifications/initialized", "tools/call", "tools/list", "notifications/cancelled"]);
        assert_eq!(rest[1]["id"], 2, "the unanswered call is sent again, the answered one is not");
        assert!(r.ready);
    }

    #[test]
    fn a_restart_before_the_handshake_ends_sends_it_normally() {
        let mut r = Relay::default();
        let (tx, _rx) = mpsc::channel();
        r.start_child(tx);
        let (l, v) = line(serde_json::json!({"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}));
        r.on_client(l, Some(&v));
        r.to_child = None;
        r.ready = false;
        let (tx, rx) = mpsc::channel();
        r.start_child(tx);
        let again = sent(&rx);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0]["method"], "initialize");
        // Claude still waits for this answer: it goes through.
        assert!(r.on_child(Some(&serde_json::json!({"jsonrpc":"2.0","id":7,"result":{}}))));
        assert!(r.init_answered);
    }
}
