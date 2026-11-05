//! Just enough IMAP to do what Mail lets no script do: make and delete a
//! mailbox on the mail server.
//!
//! Always encrypted: TLS from the first byte, or STARTTLS before anything
//! else is said, the certificate checked against the system's own trust
//! settings, as Mail checks it. The password is sent only once the
//! connection is encrypted, and is never logged.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use unicode_normalization::UnicodeNormalization;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The same, for each of several addresses of a server.
const CONNECT_EACH: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest answer accepted from a server: a list of mailboxes is a few
/// kilobytes, so anything near this is not a mail server answering.
const MAX_ANSWER: usize = 4 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte (port 993).
    Tls,
    /// Plain at first, then STARTTLS before anything else (port 143).
    StartTls,
}

/// A mail server, as the account in Mail is set up to reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub user: String,
}

impl Server {
    /// Port 993 (or none given: Mail chooses) is TLS, 143 is STARTTLS, any
    /// other port TLS unless Mail says it does not use SSL there. Never
    /// unencrypted.
    pub fn new(host: &str, port: Option<i64>, ssl: Option<bool>, user: &str) -> Server {
        let port = port.and_then(|p| u16::try_from(p).ok()).filter(|p| *p != 0);
        let (port, security) = match (port, ssl) {
            (None | Some(993), _) => (993, Security::Tls),
            (Some(143), _) => (143, Security::StartTls),
            (Some(p), Some(false)) => (p, Security::StartTls),
            (Some(p), _) => (p, Security::Tls),
        };
        Server { host: host.trim().to_string(), port, security, user: user.to_string() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server turned the user name or the password down.
    Auth(String),
    /// The server would not sign in, with a message for the user
    /// (`[ALERT]`): Gmail asking for an app password, an account to unlock
    /// on the provider's website. The password may be right.
    Alert(String),
    /// The server could not be reached, or the connection failed: name not
    /// found, timeout, certificate not trusted.
    Network(String),
    /// The server answered NO or BAD to a command.
    Refused(String),
    /// The server's answer made no sense as IMAP.
    Protocol(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Auth(t) => write!(f, "the server did not accept the password ({t})"),
            Error::Alert(t) => write!(f, "the server says: {}", alert_text(t)),
            Error::Network(t) => write!(f, "{t}"),
            Error::Refused(t) => write!(f, "the server refused: {t}"),
            Error::Protocol(t) => write!(f, "the server's answer was not understood ({t})"),
        }
    }
}

/// The server's message without its response code.
pub(crate) fn alert_text(t: &str) -> &str {
    let t = t.trim();
    match t.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((_, rest)) if !rest.trim().is_empty() => rest.trim(),
        _ => t,
    }
}

/// What a refused sign-in means, from the server's answer: the password
/// turned down (`[AUTHENTICATIONFAILED]`, or a NO that says no more), a
/// message for the user (`[ALERT]`), or the server unable or unwilling to
/// sign anyone in now (`[UNAVAILABLE]`, `[EXPIRED]`, `[CONTACTADMIN]`, a
/// BAD…), when the password may well be right.
fn sign_in_refused(status: Status, text: String) -> Error {
    let t = text.trim_start();
    let code = t
        .strip_prefix('[')
        .and_then(|r| r.split(|c: char| c == ']' || c.is_whitespace()).next())
        .map(str::to_ascii_uppercase);
    match (status, code.as_deref()) {
        (Status::No, None | Some("AUTHENTICATIONFAILED" | "AUTHORIZATIONFAILED")) => Error::Auth(text),
        (Status::No, Some("ALERT")) => Error::Alert(text),
        _ => Error::Refused(text),
    }
}

fn net(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Network("the server stopped answering".into()),
        io::ErrorKind::InvalidData => Error::Network(format!("the secure connection failed: {e}")),
        _ => Error::Network(format!("the connection to the server failed: {e}")),
    }
}

// ---------------------------------------------------------------- the wire

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// What a server sends, line by line: its text, with each literal (a
/// string sent as `{n}` and n raw bytes) kept apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(Vec<u8>),
    Literal(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Atom(Vec<u8>),
    /// A quoted string or a literal, unescaped.
    Str(Vec<u8>),
    Open,
    Close,
}

impl Tok {
    fn is(&self, word: &str) -> bool {
        matches!(self, Tok::Atom(a) if a.eq_ignore_ascii_case(word.as_bytes()))
    }
    /// The bytes of an atom or a string; `NIL` is none.
    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Tok::Atom(a) if a.eq_ignore_ascii_case(b"NIL") => None,
            Tok::Atom(a) | Tok::Str(a) => Some(a),
            _ => None,
        }
    }
}

fn tokenize(parts: &[Part]) -> Vec<Tok> {
    let mut out = vec![];
    for p in parts {
        let t = match p {
            Part::Literal(b) => {
                out.push(Tok::Str(b.clone()));
                continue;
            }
            Part::Text(t) => t,
        };
        let mut i = 0;
        while i < t.len() {
            match t[i] {
                b' ' => i += 1,
                b'(' => {
                    out.push(Tok::Open);
                    i += 1;
                }
                b')' => {
                    out.push(Tok::Close);
                    i += 1;
                }
                b'"' => {
                    i += 1;
                    let mut s = vec![];
                    while i < t.len() && t[i] != b'"' {
                        if t[i] == b'\\' && i + 1 < t.len() {
                            i += 1;
                        }
                        s.push(t[i]);
                        i += 1;
                    }
                    i += 1;
                    out.push(Tok::Str(s));
                }
                _ => {
                    let start = i;
                    while i < t.len() && !matches!(t[i], b' ' | b'(' | b')' | b'"') {
                        i += 1;
                    }
                    out.push(Tok::Atom(t[start..i].to_vec()));
                }
            }
        }
    }
    out
}

/// The text of an answer, literals left out, for messages.
fn text_of(parts: &[Part]) -> String {
    let mut s = String::new();
    for p in parts {
        match p {
            Part::Text(t) => s.push_str(&String::from_utf8_lossy(t)),
            Part::Literal(_) => s.push_str("{…}"),
        }
    }
    s
}

/// `{12}` at the end of a line: a literal of 12 bytes follows.
fn literal_at_end(line: &[u8]) -> Option<(usize, usize)> {
    let inner = line.strip_suffix(b"}")?;
    let open = inner.iter().rposition(|&b| b == b'{')?;
    let digits = &inner[open + 1..];
    let digits = digits.strip_suffix(b"+").unwrap_or(digits);
    if digits.is_empty() || digits.len() > 9 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((open, n))
}

/// A string that can travel quoted; anything else goes as a literal.
fn quotable(b: &[u8]) -> bool {
    b.len() <= 1000 && b.iter().all(|&c| (1..=0x7f).contains(&c) && c != b'\r' && c != b'\n')
}

enum Arg<'a> {
    Atom(&'a str),
    Str(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    No,
    Bad,
}

/// How a command ended: its status and what the server said with it.
#[derive(Debug, Clone)]
struct Done {
    status: Status,
    text: String,
}

impl Done {
    fn ok(self) -> Result<(), Error> {
        match self.status {
            Status::Ok => Ok(()),
            Status::No | Status::Bad => Err(Error::Refused(self.text)),
        }
    }
}

// ---------------------------------------------------------------- session

/// A mailbox the server lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name as the server spells it (modified UTF-7), sent back as is.
    pub raw: Vec<u8>,
    /// The name decoded, accents composed.
    pub name: String,
    pub delimiter: Option<char>,
    /// Its attributes, lowercase: `\noselect`, `\sent`…
    pub attrs: Vec<String>,
}

impl Entry {
    /// The path, one part per level of the hierarchy.
    pub fn segments(&self) -> Vec<String> {
        match self.delimiter {
            Some(d) => self.name.split(d).map(str::to_string).collect(),
            None => vec![self.name.clone()],
        }
    }

    pub fn has(&self, attr: &str) -> bool {
        self.attrs.iter().any(|a| a.eq_ignore_ascii_case(attr))
    }

    /// What the server keeps the mailbox for (RFC 6154 special use), if
    /// anything: sent mail, drafts, trash, junk, archive, all mail…
    pub fn special_use(&self) -> Option<&str> {
        const SPECIAL: &[&str] =
            &["\\all", "\\archive", "\\drafts", "\\flagged", "\\junk", "\\sent", "\\trash", "\\important"];
        self.attrs.iter().find(|a| SPECIAL.contains(&a.as_str())).map(|a| &a[1..])
    }
}

pub struct Session {
    io: BufReader<Stream>,
    tag: u32,
    caps: Vec<String>,
}

/// Connect, encrypt, and sign in.
pub fn open(server: &Server, password: &str) -> Result<Session, Error> {
    let mut s = connect(server)?;
    s.login(&server.user, password)?;
    Ok(s)
}

pub fn connect(server: &Server) -> Result<Session, Error> {
    let tcp = tcp(&server.host, server.port)?;
    match server.security {
        Security::Tls => Session::greet(Stream::Tls(Box::new(tls(&server.host, tcp)?))),
        Security::StartTls => Session::greet(Stream::Plain(tcp))?.starttls(&server.host),
    }
}

fn tcp(host: &str, port: u16) -> Result<TcpStream, Error> {
    let addrs: Vec<std::net::SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| Error::Network(format!("{host} could not be found ({e})")))?
        .take(4)
        .collect();
    // Several addresses (IPv6 and IPv4): less time for each, so a network
    // where one kind does not go through costs seconds, not minutes.
    let each = if addrs.len() > 1 { CONNECT_EACH } else { CONNECT_TIMEOUT };
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, each) {
            Ok(s) => {
                s.set_read_timeout(Some(IO_TIMEOUT)).map_err(net)?;
                s.set_write_timeout(Some(IO_TIMEOUT)).map_err(net)?;
                let _ = s.set_nodelay(true);
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    let why = last.map_or_else(|| "no address".to_string(), |e| e.to_string());
    Err(Error::Network(format!("{host}:{port} could not be reached ({why})")))
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, Error> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            use rustls_platform_verifier::BuilderVerifierExt;
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let builder = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?;
            let config = builder.with_platform_verifier().map_err(|e| e.to_string())?.with_no_client_auth();
            Ok(Arc::new(config))
        })
        .clone()
        .map_err(|e| Error::Network(format!("encryption could not be set up: {e}")))
}

fn tls(host: &str, tcp: TcpStream) -> Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>, Error> {
    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| Error::Network(format!("'{host}' is not a server name")))?;
    let conn = rustls::ClientConnection::new(tls_config()?, name).map_err(|e| Error::Network(e.to_string()))?;
    let mut s = rustls::StreamOwned::new(conn, tcp);
    // The handshake now, so that a certificate the system does not trust is
    // said as such rather than as a failed read.
    while s.conn.is_handshaking() {
        s.conn.complete_io(&mut s.sock).map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                Error::Network(format!("the server's certificate or encryption was not accepted ({e})"))
            } else {
                net(e)
            }
        })?;
    }
    Ok(s)
}

impl Session {
    fn greet(stream: Stream) -> Result<Session, Error> {
        let mut s = Session { io: BufReader::new(stream), tag: 0, caps: vec![] };
        let r = s.read_answer()?;
        let toks = tokenize(&r);
        if toks.first().is_some_and(|t| t.is("*")) && toks.get(1).is_some_and(|t| t.is("OK")) {
            s.learn_caps(&r);
            return Ok(s);
        }
        if toks.get(1).is_some_and(|t| t.is("BYE")) {
            return Err(Error::Network(format!("the server turned the connection away: {}", text_of(&r))));
        }
        Err(Error::Protocol("this is not an IMAP mail server greeting".into()))
    }

    #[cfg(test)]
    fn over_plain(tcp: TcpStream) -> Result<Session, Error> {
        Session::greet(Stream::Plain(tcp))
    }

    fn starttls(mut self, host: &str) -> Result<Session, Error> {
        if self.caps.is_empty() {
            self.capability()?;
        }
        if !self.has("STARTTLS") {
            return Err(Error::Network(
                "the server offers no encryption on this port, and Timbre never sends a password unencrypted".into(),
            ));
        }
        self.command(&[Arg::Atom("STARTTLS")])?.1.ok()?;
        // Nothing may come between the server's OK and the encryption: bytes
        // already there were not sent by the server encrypted (an attack).
        if !self.io.buffer().is_empty() {
            return Err(Error::Protocol("data arrived before encryption started".into()));
        }
        let Stream::Plain(tcp) = self.io.into_inner() else {
            return Err(Error::Protocol("already encrypted".into()));
        };
        let mut s = Session { io: BufReader::new(Stream::Tls(Box::new(tls(host, tcp)?))), tag: self.tag, caps: vec![] };
        // What the server said it can do before encryption is not trusted.
        s.capability()?;
        Ok(s)
    }

    fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c.eq_ignore_ascii_case(cap))
    }

    /// Capabilities announced in an answer: `* CAPABILITY …` or a
    /// `[CAPABILITY …]` code.
    fn learn_caps(&mut self, parts: &[Part]) {
        let Some(Part::Text(first)) = parts.first() else { return };
        let text = String::from_utf8_lossy(first);
        let upper = text.to_ascii_uppercase();
        let list = if upper.starts_with("* CAPABILITY ") {
            Some(&text["* CAPABILITY ".len()..])
        } else if let Some(i) = upper.find("[CAPABILITY ") {
            let rest = &text[i + "[CAPABILITY ".len()..];
            Some(rest.split(']').next().unwrap_or(rest))
        } else {
            None
        };
        if let Some(list) = list {
            self.caps = list.split_whitespace().map(str::to_ascii_uppercase).collect();
        }
    }

    fn capability(&mut self) -> Result<(), Error> {
        self.command(&[Arg::Atom("CAPABILITY")])?.1.ok()
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let s = self.io.get_mut();
        s.write_all(bytes).map_err(net)?;
        s.flush().map_err(net)
    }

    /// One answer from the server: a line, with the literals it announces.
    fn read_answer(&mut self) -> Result<Vec<Part>, Error> {
        let mut parts = vec![];
        let mut total = 0usize;
        loop {
            let mut line = vec![];
            let room = MAX_ANSWER.checked_sub(total).ok_or_else(|| Error::Protocol("answer too long".into()))?;
            let n = (&mut self.io).take(room as u64 + 2).read_until(b'\n', &mut line).map_err(net)?;
            if n == 0 {
                return Err(Error::Network("the server closed the connection".into()));
            }
            if line.pop() != Some(b'\n') {
                return Err(Error::Protocol("answer too long or cut off".into()));
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            total += line.len();
            if total > MAX_ANSWER {
                return Err(Error::Protocol("answer too long".into()));
            }
            if let Some((at, len)) = literal_at_end(&line) {
                if len > MAX_ANSWER - total {
                    return Err(Error::Protocol("answer too long".into()));
                }
                let mut buf = vec![0u8; len];
                self.io.read_exact(&mut buf).map_err(net)?;
                total += len;
                line.truncate(at);
                parts.push(Part::Text(line));
                parts.push(Part::Literal(buf));
                continue;
            }
            parts.push(Part::Text(line));
            return Ok(parts);
        }
    }

    /// Send a command; strings go quoted, or as literals once the server
    /// says to go on. Returns its tag.
    fn send(&mut self, args: &[Arg]) -> Result<String, Error> {
        self.tag += 1;
        let tag = format!("T{}", self.tag);
        let mut line = tag.clone().into_bytes();
        for a in args {
            line.push(b' ');
            match a {
                Arg::Atom(s) => line.extend_from_slice(s.as_bytes()),
                Arg::Str(b) if quotable(b) => {
                    line.push(b'"');
                    for &c in *b {
                        if c == b'"' || c == b'\\' {
                            line.push(b'\\');
                        }
                        line.push(c);
                    }
                    line.push(b'"');
                }
                Arg::Str(b) => {
                    line.extend_from_slice(format!("{{{}}}\r\n", b.len()).as_bytes());
                    self.write(&line)?;
                    line.clear();
                    self.go_on(&tag)?;
                    line.extend_from_slice(b);
                }
            }
        }
        line.extend_from_slice(b"\r\n");
        self.write(&line)?;
        Ok(tag)
    }

    /// Wait for the server's "+" (go on); a NO or BAD ends the command.
    fn go_on(&mut self, tag: &str) -> Result<(), Error> {
        loop {
            let r = self.read_answer()?;
            let toks = tokenize(&r);
            match toks.first() {
                Some(t) if t.is("+") => return Ok(()),
                Some(t) if t.is("*") => {
                    self.untagged(&r, &toks)?;
                }
                Some(t) if t.is(tag) => return Err(Error::Refused(rest_after(&r, 2))),
                _ => return Err(Error::Protocol(format!("unexpected answer: {}", text_of(&r)))),
            }
        }
    }

    fn untagged(&mut self, r: &[Part], toks: &[Tok]) -> Result<(), Error> {
        if toks.get(1).is_some_and(|t| t.is("BYE")) {
            return Err(Error::Network(format!("the server closed the connection: {}", rest_after(r, 2))));
        }
        self.learn_caps(r);
        Ok(())
    }

    /// The answers up to the command's own; untagged ones are returned.
    fn finish(&mut self, tag: &str) -> Result<(Vec<Vec<Part>>, Done), Error> {
        let mut untagged = vec![];
        loop {
            let r = self.read_answer()?;
            let toks = tokenize(&r);
            match toks.first() {
                Some(t) if t.is("*") => {
                    self.untagged(&r, &toks)?;
                    untagged.push(r);
                }
                Some(t) if t.is(tag) => {
                    let status = match toks.get(1) {
                        Some(s) if s.is("OK") => Status::Ok,
                        Some(s) if s.is("NO") => Status::No,
                        _ => Status::Bad,
                    };
                    self.learn_caps(&r);
                    return Ok((untagged, Done { status, text: rest_after(&r, 2) }));
                }
                // A challenge nobody asked for: cancel it.
                Some(t) if t.is("+") => self.write(b"*\r\n")?,
                _ => return Err(Error::Protocol(format!("unexpected answer: {}", text_of(&r)))),
            }
        }
    }

    fn command(&mut self, args: &[Arg]) -> Result<(Vec<Vec<Part>>, Done), Error> {
        let tag = self.send(args)?;
        self.finish(&tag)
    }

    /// Sign in: SASL PLAIN when offered, LOGIN otherwise.
    pub fn login(&mut self, user: &str, password: &str) -> Result<(), Error> {
        if self.caps.is_empty() {
            self.capability()?;
        }
        let done = if self.has("AUTH=PLAIN") {
            let mut raw = Vec::with_capacity(user.len() + password.len() + 2);
            raw.push(0);
            raw.extend_from_slice(user.as_bytes());
            raw.push(0);
            raw.extend_from_slice(password.as_bytes());
            let token = base64(&raw, BASE64, true);
            if self.has("SASL-IR") {
                self.command(&[Arg::Atom("AUTHENTICATE"), Arg::Atom("PLAIN"), Arg::Atom(&token)])?.1
            } else {
                let tag = self.send(&[Arg::Atom("AUTHENTICATE"), Arg::Atom("PLAIN")])?;
                match self.go_on(&tag) {
                    Ok(()) => {
                        self.write(format!("{token}\r\n").as_bytes())?;
                        self.finish(&tag)?.1
                    }
                    Err(Error::Refused(t)) => Done { status: Status::No, text: t },
                    Err(e) => return Err(e),
                }
            }
        } else if self.has("LOGINDISABLED") {
            return Err(Error::Refused("the server takes no password over this connection".into()));
        } else {
            self.command(&[Arg::Atom("LOGIN"), Arg::Str(user.as_bytes()), Arg::Str(password.as_bytes())])?.1
        };
        match done.status {
            Status::Ok => {
                // What it can do once signed in (its OK may have said so).
                self.capability()?;
                Ok(())
            }
            status => Err(sign_in_refused(status, done.text)),
        }
    }

    /// Every mailbox of the account.
    pub fn list(&mut self) -> Result<Vec<Entry>, Error> {
        let (answers, done) = self.command(&[Arg::Atom("LIST"), Arg::Str(b""), Arg::Str(b"*")])?;
        done.ok()?;
        Ok(answers.iter().filter_map(|r| parse_list(&tokenize(r))).collect())
    }

    /// The server's personal namespace: where its mailboxes go ("INBOX." at
    /// some providers), and its hierarchy delimiter.
    pub fn namespace(&mut self) -> Result<Option<(String, Option<char>)>, Error> {
        if !self.has("NAMESPACE") {
            return Ok(None);
        }
        let (answers, done) = self.command(&[Arg::Atom("NAMESPACE")])?;
        done.ok()?;
        Ok(answers.iter().find_map(|r| parse_namespace(&tokenize(r))))
    }

    /// How many messages a mailbox holds, by the server's count: from the
    /// answer about that very mailbox (a server may tell of others too).
    pub fn messages(&mut self, raw: &[u8]) -> Result<u64, Error> {
        let (answers, done) = self.command(&[Arg::Atom("STATUS"), Arg::Str(raw), Arg::Atom("(MESSAGES)")])?;
        done.ok()?;
        answers
            .iter()
            .find_map(|r| parse_status(&tokenize(r), raw))
            .ok_or_else(|| Error::Protocol("no message count in the server's answer".into()))
    }

    pub fn delete(&mut self, raw: &[u8]) -> Result<(), Error> {
        self.command(&[Arg::Atom("DELETE"), Arg::Str(raw)])?.1.ok()
    }

    pub fn create(&mut self, raw: &[u8]) -> Result<(), Error> {
        self.command(&[Arg::Atom("CREATE"), Arg::Str(raw)])?.1.ok()
    }

    /// Subscribe (or not): some mail apps show only the mailboxes one is
    /// subscribed to.
    pub fn subscribe(&mut self, raw: &[u8], on: bool) -> Result<(), Error> {
        self.command(&[Arg::Atom(if on { "SUBSCRIBE" } else { "UNSUBSCRIBE" }), Arg::Str(raw)])?.1.ok()
    }

    pub fn logout(mut self) {
        let _ = self.command(&[Arg::Atom("LOGOUT")]);
    }
}

/// The text of an answer after its first `n` words.
fn rest_after(parts: &[Part], n: usize) -> String {
    let t = text_of(parts);
    let mut rest = t.as_str();
    for _ in 0..n {
        rest = rest.trim_start().split_once(' ').map_or("", |(_, r)| r);
    }
    rest.trim().to_string()
}

/// `* LIST (\HasNoChildren \Sent) "/" "Sent"`
fn parse_list(toks: &[Tok]) -> Option<Entry> {
    if !(toks.first()?.is("*") && toks.get(1)?.is("LIST") && *toks.get(2)? == Tok::Open) {
        return None;
    }
    let close = 3 + toks[3..].iter().position(|t| *t == Tok::Close)?;
    let attrs = toks[3..close]
        .iter()
        .filter_map(|t| match t {
            Tok::Atom(a) => Some(String::from_utf8_lossy(a).to_lowercase()),
            _ => None,
        })
        .collect();
    let delimiter = match toks.get(close + 1)?.bytes() {
        Some([d]) if d.is_ascii() => Some(*d as char),
        _ => None,
    };
    let raw = toks.get(close + 2)?.bytes()?.to_vec();
    Some(Entry { name: decode_name(&raw), raw, delimiter, attrs })
}

/// `* STATUS "Archives" (MESSAGES 0)`, about the mailbox `raw` (INBOX in
/// any case).
fn parse_status(toks: &[Tok], raw: &[u8]) -> Option<u64> {
    if !(toks.first()?.is("*") && toks.get(1)?.is("STATUS")) {
        return None;
    }
    let name = toks.get(2)?.bytes()?;
    if !(name == raw || (name.eq_ignore_ascii_case(b"INBOX") && raw.eq_ignore_ascii_case(b"INBOX"))) {
        return None;
    }
    let pos = toks.iter().position(|t| t.is("MESSAGES"))?;
    match toks.get(pos + 1)? {
        Tok::Atom(n) => std::str::from_utf8(n).ok()?.parse().ok(),
        _ => None,
    }
}

/// `* NAMESPACE (("INBOX." ".")) NIL NIL`: the first personal namespace.
fn parse_namespace(toks: &[Tok]) -> Option<(String, Option<char>)> {
    if !(toks.first()?.is("*") && toks.get(1)?.is("NAMESPACE")) {
        return None;
    }
    if *toks.get(2)? != Tok::Open || *toks.get(3)? != Tok::Open {
        return None;
    }
    let prefix = decode_name(toks.get(4)?.bytes()?);
    let delimiter = match toks.get(5)?.bytes() {
        Some([d]) if d.is_ascii() => Some(*d as char),
        _ => None,
    };
    Some((prefix, delimiter))
}

/// A mailbox name from the server: modified UTF-7, or UTF-8 from servers
/// that send it so; accents composed.
pub fn decode_name(raw: &[u8]) -> String {
    let s = mutf7_decode(raw).unwrap_or_else(|| String::from_utf8_lossy(raw).into_owned());
    s.nfc().collect()
}

// ---------------------------------------------------------------- encodings

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// IMAP's variant for mailbox names: ',' for '/'.
const BASE64_IMAP: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+,";

fn base64(data: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for k in 0..4 {
            if k < chars {
                out.push(alphabet[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

fn unbase64(text: &[u8], alphabet: &[u8; 64]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        let v = alphabet.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // Leftover bits must be zero padding (RFC 3501's modified UTF-7).
    (acc == 0).then_some(out)
}

/// A mailbox name in IMAP's modified UTF-7 (RFC 3501 §5.1.3).
pub fn mutf7_encode(s: &str) -> Vec<u8> {
    fn flush(out: &mut Vec<u8>, pending: &mut Vec<u16>) {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        out.push(b'&');
        out.extend_from_slice(base64(&bytes, BASE64_IMAP, false).as_bytes());
        out.push(b'-');
        pending.clear();
    }
    let mut out = vec![];
    let mut pending: Vec<u16> = vec![];
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut out, &mut pending);
            if c == '&' {
                out.extend_from_slice(b"&-");
            } else {
                out.push(c as u8);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut out, &mut pending);
    out
}

/// A name in modified UTF-7, or none if it is not valid as such.
pub fn mutf7_decode(raw: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        if c == b'&' {
            let end = i + 1 + raw[i + 1..].iter().position(|&b| b == b'-')?;
            if end == i + 1 {
                out.push('&');
            } else {
                let bytes = unbase64(&raw[i + 1..end], BASE64_IMAP)?;
                if bytes.len() % 2 != 0 {
                    return None;
                }
                let units: Vec<u16> = bytes.chunks(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
                for ch in char::decode_utf16(units) {
                    out.push(ch.ok()?);
                }
            }
            i = end + 1;
        } else if (0x20..=0x7e).contains(&c) {
            out.push(c as char);
            i += 1;
        } else {
            return None;
        }
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod testing {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    use super::Session;

    /// A pretend IMAP server: answers each command line from `script`, in
    /// order, and records what it was sent.
    pub fn fake(script: Vec<(&'static str, &'static str)>) -> (Session, std::thread::JoinHandle<Vec<String>>) {
        fake_with("IMAP4rev1 AUTH=PLAIN NAMESPACE", script)
    }

    /// The same, the server announcing `caps` in its greeting.
    pub fn fake_with(
        caps: &'static str,
        script: Vec<(&'static str, &'static str)>,
    ) -> (Session, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let mut w = sock.try_clone().unwrap();
            let mut r = BufReader::new(sock);
            w.write_all(format!("* OK [CAPABILITY {caps}] ready\r\n").as_bytes()).unwrap();
            let mut seen = vec![];
            let mut tag = String::new();
            for (expect, reply) in script {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let line = line.trim_end().to_string();
                // A line without a tag answers the server's "+" (the
                // command's tag stays the one before).
                let cmd = match line.split_once(' ') {
                    Some((t, c)) => {
                        tag = t.to_string();
                        c
                    }
                    None => line.as_str(),
                };
                assert!(cmd.starts_with(expect), "expected {expect}, got {cmd}");
                let mut cmd = cmd.to_string();
                if cmd.ends_with('}') {
                    // A literal: say go on, then read it.
                    let n: usize = cmd.rsplit_once('{').unwrap().1.trim_end_matches('}').parse().unwrap();
                    w.write_all(b"+ go on\r\n").unwrap();
                    let mut buf = vec![0u8; n + 2];
                    std::io::Read::read_exact(&mut r, &mut buf).unwrap();
                    cmd.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                seen.push(cmd);
                w.write_all(reply.replace("$T", &tag).as_bytes()).unwrap();
            }
            seen
        });
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        (Session::over_plain(tcp).unwrap(), handle)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::fake;
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn modified_utf7_both_ways() {
        let cases = [
            ("INBOX", "INBOX"),
            ("Messages envoyés", "Messages envoy&AOk-s"),
            ("Entwürfe", "Entw&APw-rfe"),
            ("Pots & Pans", "Pots &- Pans"),
            ("日本語", "&ZeVnLIqe-"),
            ("[Gmail]/Corbeille", "[Gmail]/Corbeille"),
            ("~peter/mail/台北/日本語", "~peter/mail/&U,BTFw-/&ZeVnLIqe-"),
            ("😀 fun", "&2D3eAA- fun"),
        ];
        for (plain, wire) in cases {
            assert_eq!(String::from_utf8(mutf7_encode(plain)).unwrap(), wire, "{plain}");
            assert_eq!(mutf7_decode(wire.as_bytes()).as_deref(), Some(plain), "{wire}");
        }
        // Not modified UTF-7: raw UTF-8 (sent by some servers), an open
        // shift, odd bytes.
        assert_eq!(mutf7_decode("envoyés".as_bytes()), None);
        assert_eq!(decode_name("envoyés".as_bytes()), "envoyés");
        assert_eq!(mutf7_decode(b"a&AOk"), None);
        assert_eq!(mutf7_decode(b"&AO-"), None);
        // Accents composed, however the server wrote them.
        assert_eq!(decode_name(&mutf7_encode("envoye\u{301}s")), "envoyés");
    }

    #[test]
    fn sasl_base64() {
        assert_eq!(base64(b"\0user\0pass", BASE64, true), "AHVzZXIAcGFzcw==");
        assert_eq!(base64(b"ab", BASE64, true), "YWI=");
        assert_eq!(base64(b"abc", BASE64, true), "YWJj");
    }

    #[test]
    fn server_answers_are_read() {
        let list = |s: &str| parse_list(&tokenize(&[Part::Text(s.as_bytes().to_vec())]));
        let e = list(r#"* LIST (\HasNoChildren \Sent) "/" "[Gmail]/Messages envoy&AOk-s""#).unwrap();
        assert_eq!(e.name, "[Gmail]/Messages envoyés");
        assert_eq!(e.segments(), ["[Gmail]", "Messages envoyés"]);
        assert_eq!(e.special_use(), Some("sent"));
        let e = list(r#"* LIST (\HasNoChildren) "." INBOX.Archives"#).unwrap();
        assert_eq!(e.segments(), ["INBOX", "Archives"]);
        assert_eq!(e.special_use(), None);
        let e = list(r#"* LIST () NIL "Flat""#).unwrap();
        assert_eq!((e.delimiter, e.segments()), (None, vec!["Flat".to_string()]));
        let e = list(r#"* LIST (\Noselect) "\\" "A\\B""#).unwrap();
        assert_eq!((e.delimiter, e.name.as_str(), e.has("\\NoSelect")), (Some('\\'), "A\\B", true));
        // A name sent as a literal.
        let parts = vec![
            Part::Text(br#"* LIST (\HasNoChildren) "/" "#.to_vec()),
            Part::Literal(b"Odd \"name\"".to_vec()),
            Part::Text(vec![]),
        ];
        assert_eq!(parse_list(&tokenize(&parts)).unwrap().name, "Odd \"name\"");
        assert_eq!(parse_list(&tokenize(&[Part::Text(b"* LSUB () \"/\" x".to_vec())])), None);

        let status = |s: &str, of: &str| parse_status(&tokenize(&[Part::Text(s.as_bytes().to_vec())]), of.as_bytes());
        assert_eq!(status(r#"* STATUS "INBOX/Archives" (MESSAGES 0)"#, "INBOX/Archives"), Some(0));
        assert_eq!(status(r#"* STATUS blurdybloop (MESSAGES 231 UIDNEXT 44292)"#, "blurdybloop"), Some(231));
        assert_eq!(status(r#"* STATUS inbox (MESSAGES 3)"#, "INBOX"), Some(3));
        // About another mailbox: not this one's count.
        assert_eq!(status(r#"* STATUS "Other" (MESSAGES 0)"#, "Target"), None);
        assert_eq!(status(r#"* STATUS "target" (MESSAGES 0)"#, "Target"), None);
        let ns = |s: &str| parse_namespace(&tokenize(&[Part::Text(s.as_bytes().to_vec())]));
        assert_eq!(ns(r#"* NAMESPACE (("INBOX." ".")) NIL NIL"#), Some(("INBOX.".into(), Some('.'))));
        assert_eq!(ns(r##"* NAMESPACE (("" "/")) NIL (("#shared/" "/"))"##), Some((String::new(), Some('/'))));
        assert_eq!(literal_at_end(b"* LIST () \"/\" {12}"), Some((14, 12)));
        assert_eq!(literal_at_end(b"* OK fine"), None);
        assert_eq!(
            rest_after(&[Part::Text(b"T3 NO [AUTHENTICATIONFAILED] Invalid".to_vec())], 2),
            "[AUTHENTICATIONFAILED] Invalid"
        );
    }

    #[test]
    fn server_settings_from_mail() {
        let s = Server::new("imap.laposte.net ", Some(993), Some(true), "a@laposte.net");
        assert_eq!((s.host.as_str(), s.port, s.security), ("imap.laposte.net", 993, Security::Tls));
        assert_eq!(Server::new("h", Some(0), None, "u").port, 993);
        assert_eq!(Server::new("h", None, Some(false), "u").security, Security::Tls);
        assert_eq!(Server::new("h", Some(143), Some(true), "u").security, Security::StartTls);
        assert_eq!(Server::new("h", Some(1143), Some(false), "u").security, Security::StartTls);
        assert_eq!(Server::new("h", Some(70000), None, "u").port, 993);
    }

    #[test]
    fn a_session_signs_in_lists_and_deletes() {
        let (mut s, server) = fake(vec![
            ("AUTHENTICATE PLAIN", "+ \r\n"),
            ("AHVzZXIAcMOkc3M=", "$T OK [CAPABILITY IMAP4rev1 NAMESPACE] signed in\r\n"),
            ("CAPABILITY", "* CAPABILITY IMAP4rev1 NAMESPACE\r\n$T OK done\r\n"),
            ("NAMESPACE", "* NAMESPACE ((\"INBOX/\" \"/\")) NIL NIL\r\n$T OK\r\n"),
            (
                "LIST \"\" \"*\"",
                "* LIST (\\HasChildren) \"/\" INBOX\r\n* LIST (\\HasNoChildren) \"/\" \"INBOX/Archives\"\r\n\
                 * LIST (\\HasNoChildren) \"/\" {19}\r\nINBOX/Ann&AOk-e \"1\"\r\n$T OK listed\r\n",
            ),
            ("STATUS \"INBOX/Archives\" (MESSAGES)", "* STATUS \"INBOX/Archives\" (MESSAGES 0)\r\n$T OK\r\n"),
            ("DELETE \"INBOX/Archives\"", "$T OK deleted\r\n"),
            ("CREATE {", "$T NO [ALREADYEXISTS] it exists\r\n"),
            ("LOGOUT", "* BYE see you\r\n$T OK bye\r\n"),
        ]);
        s.login("user", "päss").unwrap();
        assert_eq!(s.namespace().unwrap(), Some(("INBOX/".into(), Some('/'))));
        let all = s.list().unwrap();
        assert_eq!(
            all.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["INBOX", "INBOX/Archives", "INBOX/Année \"1\""]
        );
        assert_eq!(s.messages(&all[1].raw).unwrap(), 0);
        s.delete(&all[1].raw).unwrap();
        // 8-bit bytes travel as a literal; a refusal is said.
        let e = s.create("Été".as_bytes()).unwrap_err();
        assert!(matches!(e, Error::Refused(ref t) if t.contains("ALREADYEXISTS")), "{e:?}");
        s.logout();
        let seen = server.join().unwrap();
        assert_eq!(seen[7], "CREATE {5}Été");
    }

    #[test]
    fn a_wrong_password_is_told_apart_from_a_server_in_trouble() {
        let login = |answer: &'static str| {
            let (mut s, _server) = fake(vec![("AUTHENTICATE PLAIN", "+\r\n"), ("", answer)]);
            s.login("u", "p").unwrap_err()
        };
        assert!(
            matches!(login("$T NO [AUTHENTICATIONFAILED] Invalid credentials\r\n"), Error::Auth(t) if t.contains("Invalid"))
        );
        assert!(matches!(login("$T NO Login failed.\r\n"), Error::Auth(_)));
        // A message for the user: the password may be right; it is told.
        let alert =
            login("$T NO [ALERT] Application-specific password required: https://support.google.com/x (Failure)\r\n");
        assert!(matches!(alert, Error::Alert(_)), "{alert:?}");
        assert!(alert.to_string().starts_with("the server says: Application-specific password required"), "{alert}");
        // The server unable or unwilling now: never taken for a wrong password.
        for answer in [
            "$T NO [UNAVAILABLE] try later\r\n",
            "$T NO [CONTACTADMIN] account locked\r\n",
            "$T NO [EXPIRED] password expired\r\n",
            "$T NO [PRIVACYREQUIRED] use TLS\r\n",
            "$T BAD invalid SASL response\r\n",
        ] {
            assert!(matches!(login(answer), Error::Refused(_)), "{answer}");
        }
    }

    /// An answer past the size limit ending with a bare line feed and a
    /// literal: refused, never a wrapped-around limit.
    #[test]
    fn an_endless_answer_is_refused() {
        let mut huge = vec![b'x'; MAX_ANSWER + 1];
        huge.extend_from_slice(b"{5}\n");
        let answer: &'static str = Box::leak(String::from_utf8(huge).unwrap().into_boxed_str());
        let (mut s, _server) = fake(vec![("NOOP", answer)]);
        let e = s.command(&[Arg::Atom("NOOP")]).unwrap_err();
        assert!(matches!(e, Error::Protocol(_)), "{e:?}");
    }

    #[test]
    fn login_is_used_when_the_server_offers_no_sasl() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let mut w = sock.try_clone().unwrap();
            let mut r = BufReader::new(sock);
            w.write_all(b"* OK [CAPABILITY IMAP4rev1] hi\r\n").unwrap();
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(b"T1 OK in\r\n").unwrap();
            let mut caps = String::new();
            r.read_line(&mut caps).unwrap();
            w.write_all(b"* CAPABILITY IMAP4rev1\r\nT2 OK\r\n").unwrap();
            line
        });
        let tcp = TcpStream::connect(addr).unwrap();
        let mut s = Session::over_plain(tcp).unwrap();
        s.login("me@x.fr", r#"a"b\c"#).unwrap();
        assert_eq!(server.join().unwrap(), "T1 LOGIN \"me@x.fr\" \"a\\\"b\\\\c\"\r\n");
    }
}
