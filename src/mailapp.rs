//! Acting in Mail.app through JavaScript for Automation (JXA).
//!
//! Scripts are fixed strings compiled into Timbre. Data never goes into the
//! script text: it is passed as JSON on standard input, so no subject, body
//! or address can be interpreted as code. Messages are addressed directly by
//! account, mailbox and id (`messages.byId`), which takes milliseconds
//! instead of the seconds a `whose` scan costs.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::Mailbox;

/// Where a mailbox lives, as Mail's scripting interface needs it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Loc {
    pub account_id: String,
    pub account: String,
    pub path: Vec<String>,
    pub local: bool,
}

impl From<&Mailbox> for Loc {
    fn from(m: &Mailbox) -> Self {
        Loc { account_id: m.account_id.clone(), account: m.account.clone(), path: m.path.clone(), local: m.local }
    }
}

const PRELUDE: &str = r#"
ObjC.import('Foundation');
function readInput() {
  const data = $.NSFileHandle.fileHandleWithStandardInput.readDataToEndOfFile;
  const str = $.NSString.alloc.initWithDataEncoding(data, $.NSUTF8StringEncoding);
  return JSON.parse(ObjC.unwrap(str));
}
const Mail = Application('Mail');
const _mb = {};
function account(loc) {
  if (loc.local) return null;
  if (loc.account_id) { try { const a = Mail.accounts.byId(loc.account_id); a.name(); return a; } catch (e) {} }
  if (loc.account) { try { const a = Mail.accounts.byName(loc.account); a.name(); return a; } catch (e) {} }
  const all = Mail.accounts();
  for (let k = 0; k < all.length; k++) {
    try { if (loc.account_id && String(all[k].id()).toUpperCase() === loc.account_id) return all[k]; } catch (e) {}
  }
  throw new Error('Mail account not found: ' + (loc.account || loc.account_id));
}
function mailbox(loc) {
  const key = JSON.stringify(loc);
  if (_mb[key]) return _mb[key];
  const container = account(loc) || Mail;
  const at = (path) => {
    try { const mb = container.mailboxes.byName(path.join('/')); mb.name(); return mb; } catch (e) { if (fatal(e)) throw e; }
    try { let cur = container; for (const p of path) { cur = cur.mailboxes.byName(p); } cur.name(); return cur; } catch (e) { if (fatal(e)) throw e; }
    return null;
  };
  // The path as Mail's database writes it, then with accents composed and
  // decomposed (Mail's scripting may write them the other way), each also
  // from Mail's path prefix on where an account keeps every mailbox inside
  // INBOX (La Poste, Orange): the server's "INBOX/Archives/2009" is Mail's
  // "Archives/2009".
  const paths = [];
  const seen = {};
  for (const form of ['', 'NFC', 'NFD']) {
    const p = form ? loc.path.map((s) => String(s).normalize(form)) : loc.path;
    const ways = p.length > 1 && String(p[0]).toUpperCase() === 'INBOX' ? [p, p.slice(1)] : [p];
    for (const w of ways) { const k = w.join('/'); if (w.length && !seen[k]) { seen[k] = true; paths.push(w); } }
  }
  let found = null;
  for (const p of paths) { found = at(p); if (found) break; }
  if (!found) {
    // Only when the name leaves no doubt: never another mailbox that
    // happens to share the last part of the path.
    const leaves = [];
    for (const p of paths) { const leaf = p[p.length - 1]; if (leaves.indexOf(leaf) < 0) leaves.push(leaf); }
    for (const leaf of leaves) {
      const hits = container.mailboxes.whose({name: leaf})();
      if (hits.length === 1) { found = hits[0]; break; }
    }
  }
  if (!found) throw new Error('Mailbox not found in Mail: ' + (loc.account ? loc.account + ' / ' : '') + loc.path.join('/'));
  _mb[key] = found;
  return found;
}
// A message by id in the mailbox the user sees it in; for Gmail, where
// Mail stores it (All Mail) if not found there. Returns [message, alt used].
function find(loc, id, alt) {
  try { const m = mailbox(loc).messages.byId(id); m.id(); return [m, false]; }
  catch (e) { if (fatal(e) || !alt) throw e; }
  const m = mailbox(alt).messages.byId(id);
  m.id();
  return [m, true];
}
function message(loc, id, alt) { return find(loc, id, alt)[0]; }
function addRecipients(msg, i) {
  for (const a of (i.to || [])) msg.toRecipients.push(Mail.ToRecipient({address: a}));
  for (const a of (i.cc || [])) msg.ccRecipients.push(Mail.CcRecipient({address: a}));
  for (const a of (i.bcc || [])) msg.bccRecipients.push(Mail.BccRecipient({address: a}));
}
function describe(msg) {
  const list = (r) => { try { return r.address(); } catch (e) { return []; } };
  return {outgoing_id: msg.id(), subject: msg.subject(), to: list(msg.toRecipients), cc: list(msg.ccRecipients), bcc: list(msg.bccRecipients)};
}
// Errors that concern every message, not one: Automation not allowed
// (-1743), Mail not answering (-1712), Mail not running (-600). They stop
// the script so Timbre can say what to do.
function fatal(e) {
  const n = e && e.errorNumber;
  return n === -1743 || n === -1712 || n === -600 || n === -609;
}
function errorText(e) { return String(e && e.message ? e.message : e); }
// A batch stops at an error that concerns every message, returning what
// was done before it, so Timbre still records (and can undo) that.
function stop(out, e) { return {items: out, fatal: {error: errorText(e), number: (e && e.errorNumber) || null}}; }
function run(argv) {
  try { return JSON.stringify({ok: true, result: main(readInput())}); }
  catch (e) { return JSON.stringify({ok: false, error: errorText(e), number: (e && e.errorNumber) || null}); }
}
"#;

pub const VERIFY: &str = r#"
function main(i) { const m = message(i.loc, i.id); return {id: m.id(), subject: m.subject(), message_id: m.messageId()}; }
"#;

pub const DRAFT_NEW: &str = r#"
function main(i) {
  const msg = Mail.OutgoingMessage({subject: i.subject || '', content: i.body || '', visible: true});
  Mail.outgoingMessages.push(msg);
  addRecipients(msg, i);
  if (i.sender) { try { msg.sender = i.sender; } catch (e) {} }
  try { msg.save(); } catch (e) {}
  return describe(msg);
}
"#;

pub const DRAFT_REPLY: &str = r#"
function main(i) {
  const orig = message(i.loc, i.id, i.alt);
  const r = Mail.reply(orig, {openingWindow: true, replyToAll: !!i.reply_all});
  r.content = i.body;
  addRecipients(r, {cc: i.cc, bcc: i.bcc});
  try { r.save(); } catch (e) {}
  return describe(r);
}
"#;

pub const DRAFT_FORWARD: &str = r#"
function main(i) {
  const orig = message(i.loc, i.id, i.alt);
  const f = Mail.forward(orig, {openingWindow: true});
  addRecipients(f, i);
  let added = false;
  if (i.body) { try { const cur = f.content(); f.content = i.body + '\n\n' + cur; added = true; } catch (e) {} }
  try { f.save(); } catch (e) {}
  const d = describe(f);
  d.text_added = added;
  return d;
}
"#;

pub const MARK: &str = r#"
function main(i) {
  const out = [];
  for (const it of i.items) {
    try {
      const m = message(it.loc, it.id, it.alt);
      if (i.action === 'read') m.readStatus = true;
      else if (i.action === 'unread') m.readStatus = false;
      else if (i.action === 'flag') { m.flaggedStatus = true; if (typeof i.color === 'number') m.flagIndex = i.color; }
      else if (i.action === 'unflag') m.flaggedStatus = false;
      out.push({id: it.id, ok: true});
    } catch (e) { if (fatal(e)) return stop(out, e); out.push({id: it.id, ok: false, error: errorText(e)}); }
  }
  return {items: out, fatal: null};
}
"#;

pub const MOVE: &str = r#"
function main(i) {
  const out = [];
  for (const it of i.items) {
    try {
      const [m, viaAlt] = find(it.loc, it.id, it.alt);
      if (viaAlt && JSON.stringify(it.alt) === JSON.stringify(it.dest)) throw new Error('already there');
      let mid = null;
      try { mid = m.messageId(); } catch (e) { if (fatal(e)) throw e; }
      Mail.move(m, {to: mailbox(it.dest)});
      out.push({id: it.id, ok: true, message_id: mid});
    } catch (e) { if (fatal(e)) return stop(out, e); out.push({id: it.id, ok: false, error: errorText(e)}); }
  }
  return {items: out, fatal: null};
}
"#;

/// Whether Mail has a mailbox, trying `tries` times half a second apart:
/// Mail lists a mailbox made on the server a moment later, and one made
/// elsewhere (on the provider's website) before its database has it.
pub const FIND_MAILBOX: &str = r#"
function main(i) {
  const tries = i.tries || 40;
  for (let k = 0; k < tries; k++) {
    if (k > 0) delay(0.5);
    try { mailbox(i.loc); return {found: true}; } catch (e) { if (fatal(e)) throw e; }
  }
  throw new Error('Mail does not list the mailbox');
}
"#;

/// Make a mailbox at the top of an account, as Mail's own New Mailbox does:
/// the `make` told to the account itself, the form that reaches IMAP
/// servers. Mail names an IMAP account's mailboxes from its path prefix, so
/// the bare name "Archive" lands where the server keeps the account's other
/// mailboxes (inside INBOX at La Poste and Orange); made inside INBOX by
/// script, Mail kept it on the Mac alone. Some servers' accounts refuse it
/// (La Poste: -10000, from JavaScript's `push()` too), and the caller then
/// says how to make it once. The account and the name are arguments, never
/// script text.
const MAKE_MAILBOX: &str = r#"
on run argv
	set accountId to item 1 of argv
	set boxName to item 2 of argv
	tell application "Mail"
		tell account id accountId
			make new mailbox with properties {name:boxName}
		end tell
	end tell
	return "ok"
end run
"#;

/// Ask Mail to make a mailbox at the top of an account.
pub async fn make_mailbox(account_id: &str, name: &str, timeout: Duration) -> Result<()> {
    if let Some(why) = unavailable() {
        bail!("{why}");
    }
    let child = tokio::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(MAKE_MAILBOX)
        .arg(account_id)
        .arg(name)
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("could not start osascript: {e}"))?;
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => bail!("{NO_ANSWER} within {}s.", timeout.as_secs()),
    };
    if out.status.success() {
        return Ok(());
    }
    bail!("{}", script_error(&String::from_utf8_lossy(&out.stderr)))
}

/// How Mail reaches an account's server: what Timbre needs to make and
/// delete mailboxes there itself (never the password, which Mail keeps).
pub const ACCOUNT_SERVER: &str = r#"
function main(i) {
  const a = account(i.loc);
  if (!a) throw new Error('On My Mac has no server');
  const g = (f) => { try { const v = f(); return v === undefined ? null : v; } catch (e) { if (fatal(e)) throw e; return null; } };
  return {name: g(() => a.name()), type: g(() => String(a.accountType())), server: g(() => a.serverName()),
          port: g(() => a.port()), ssl: g(() => a.usesSsl()), user: g(() => a.userName())};
}
"#;

/// Ask Mail to synchronize an account with its server, as Mailbox >
/// Synchronize does: it reads the account's list of mailboxes again, after
/// one was made or deleted there. Mail goes on in the background past the
/// timeout. The account id is an argument, never script.
const SYNC_ACCOUNT: &str = r#"
on run argv
	with timeout of 20 seconds
		tell application "Mail" to synchronize with account id (item 1 of argv)
	end timeout
	return "ok"
end run
"#;

/// Ask Mail to synchronize an account (see [`SYNC_ACCOUNT`]).
pub async fn sync_account(account_id: &str) -> Result<()> {
    if let Some(why) = unavailable() {
        bail!("{why}");
    }
    let child = tokio::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(SYNC_ACCOUNT)
        .arg(account_id)
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("could not start osascript: {e}"))?;
    let out = match tokio::time::timeout(Duration::from_secs(30), child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => return Ok(()),
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    // -1712: Mail took longer than the timeout and goes on by itself.
    if out.status.success() || stderr.contains("-1712") {
        return Ok(());
    }
    bail!("{}", script_error(&stderr))
}

/// A window asking for a password; with a button opening the page where
/// the provider makes app passwords, when it takes only those. Gives up
/// after ten minutes. The text and labels are arguments, never script.
const ASK_PASSWORD: &str = r#"
on run argv
	set {msg, okText, cancelText, linkText, linkURL} to {item 1 of argv, item 2 of argv, item 3 of argv, item 4 of argv, item 5 of argv}
	if linkText is "" then
		set theButtons to {cancelText, okText}
	else
		set theButtons to {cancelText, linkText, okText}
	end if
	repeat
		tell me to activate
		try
			set r to display dialog msg with title "Timbre" default answer "" with hidden answer buttons theButtons default button okText cancel button cancelText with icon note giving up after 600
		on error number -128
			return "cancel"
		end try
		if gave up of r then return "cancel"
		if button returned of r is okText then return "ok:" & (text returned of r)
		open location linkURL
	end repeat
end run
"#;

/// Ask the user for a password in a window of Timbre's: `Some` with what
/// they typed, `None` if they closed it. What is typed goes from the window
/// to Timbre on a pipe, and nowhere else.
pub async fn ask_password(text: &str, ok: &str, cancel: &str, link: Option<(&str, &str)>) -> Result<Option<String>> {
    if let Some(why) = unavailable() {
        bail!("{why}");
    }
    let (link_text, link_url) = link.unwrap_or(("", ""));
    let child = tokio::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(ASK_PASSWORD)
        .args([text, ok, cancel, link_text, link_url])
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("could not start osascript: {e}"))?;
    let out = match tokio::time::timeout(Duration::from_secs(660), child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => return Ok(None),
    };
    let said = String::from_utf8(out.stdout).map_err(|_| anyhow!("the window's answer was not text"))?;
    let said = said.strip_suffix('\n').unwrap_or(&said);
    if let Some(typed) = said.strip_prefix("ok:") {
        return Ok(Some(typed.to_string()));
    }
    if said == "cancel" {
        return Ok(None);
    }
    bail!("{}", script_error(&String::from_utf8_lossy(&out.stderr)))
}

/// The user's Mac speaks French: Timbre's windows do too.
pub fn user_reads_french() -> bool {
    static FRENCH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FRENCH.get_or_init(|| {
        if unavailable().is_some() {
            return false;
        }
        // The first of the preferred languages: ( "fr-FR", "en-FR" )
        std::process::Command::new("/usr/bin/defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
            .ok()
            .and_then(|o| {
                let text = String::from_utf8_lossy(&o.stdout).into_owned();
                text.split(|c: char| c == '"' || c == ',' || c == '(' || c.is_whitespace())
                    .find(|w| !w.is_empty())
                    .map(|w| w.to_ascii_lowercase().starts_with("fr"))
            })
            .unwrap_or(false)
    })
}

/// What an AppleScript run by osascript failed with, said as for JXA:
/// "12:345: execution error: Mail got an error: … (-10000)".
fn script_error(stderr: &str) -> String {
    let number = stderr.rsplit_once('(').and_then(|(_, n)| n.trim().trim_end_matches(')').parse::<i64>().ok());
    let msg = stderr.split_once("execution error:").map_or(stderr, |(_, m)| m).trim();
    failure(&serde_json::json!({"error": msg, "number": number}))
}

pub const MOVE_BACK: &str = r#"
function main(i) {
  const out = [];
  for (const it of i.items) {
    try {
      const src = mailbox(it.now_in);
      let hits = src.messages.whose({messageId: it.message_id})();
      if (hits.length === 0) hits = src.messages.whose({messageId: '<' + it.message_id + '>'})();
      if (hits.length === 0) throw new Error('message not found where it was moved');
      // Copies with the same Message-ID: the one moved last has the highest id.
      let pick = hits[0];
      for (const h of hits) { try { if (h.id() > pick.id()) pick = h; } catch (e) {} }
      Mail.move(pick, {to: mailbox(it.back_to)});
      out.push({message_id: it.message_id, ok: true});
    } catch (e) { if (fatal(e)) return stop(out, e); out.push({message_id: it.message_id, ok: false, error: errorText(e)}); }
  }
  return {items: out, fatal: null};
}
"#;

pub const SELECTION: &str = r#"
function main(i) {
  if (!Mail.running()) throw new Error('Mail is not open, so nothing is selected. Ask the user to open Mail and select the messages.');
  let sel = [];
  try { sel = Mail.selection(); } catch (e) { if (fatal(e)) throw e; return []; }
  const out = [];
  for (let k = 0; k < Math.min(sel.length, 50); k++) {
    try { out.push({id: sel[k].id(), subject: sel[k].subject()}); } catch (e) {}
  }
  return out;
}
"#;

pub const SEND_OUTGOING: &str = r#"
function main(i) {
  const msg = Mail.outgoingMessages.byId(i.outgoing_id);
  // Read strictly: a recipient list that cannot be read is not an empty one.
  const all = [].concat(msg.toRecipients.address(), msg.ccRecipients.address(), msg.bccRecipients.address())
    .map(x => String(x).trim().toLowerCase());
  if (all.length === 0) throw new Error('the draft has no recipient any more');
  for (const a of all) { if (i.allowed.indexOf(a) < 0) throw new Error('recipient changed since preparation: ' + a); }
  for (const a of i.allowed) { if (all.indexOf(a) < 0) throw new Error('recipient removed since preparation: ' + a); }
  if (String(msg.subject()) !== String(i.subject)) throw new Error('the subject changed since preparation');
  const d = describe(msg);
  msg.send();
  return {sent: true, to: d.to, cc: d.cc, bcc: d.bcc, subject: d.subject};
}
"#;

/// How the errors begin when Mail may have acted without saying so.
const NO_ANSWER: &str = "Mail did not answer";

const NOT_ALLOWED: &str = "macOS has not allowed Timbre to act in Mail (Automation). Call open_permission_settings \
with pane \"automation\" and ask the user to switch on Mail under Timbre, then try again.";

/// Why Mail cannot be asked from here: not on a Mac, or in a test, which
/// must never drive the Mail of the Mac it runs on.
pub fn unavailable() -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        Some("Mail.app scripting is only available on macOS")
    } else if cfg!(test) {
        Some("Mail is not driven from tests")
    } else {
        None
    }
}

/// Run one JXA script with `input` as JSON on stdin.
pub async fn run(script: &str, input: &Value, timeout: Duration) -> Result<Value> {
    if let Some(why) = unavailable() {
        bail!("{why}");
    }
    use tokio::io::AsyncWriteExt;
    let source = format!("{PRELUDE}\n{script}");
    let mut child = tokio::process::Command::new("/usr/bin/osascript")
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(source)
        // osascript writes results in the locale's encoding: force UTF-8.
        .env("LANG", "en_US.UTF-8")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("could not start osascript: {e}"))?;
    let payload = serde_json::to_vec(input)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&payload).await?;
        stdin.shutdown().await?;
    }
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => bail!(
            "{NO_ANSWER} within {}s. The action may or may not have been applied: check Mail before retrying.",
            timeout.as_secs()
        ),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stdout.trim().is_empty() {
        if stderr.contains("-1743") || stderr.contains("Not authorized") {
            bail!("{NOT_ALLOWED}");
        }
        bail!("Mail scripting failed: {}", stderr.trim());
    }
    let v: Value =
        serde_json::from_str(stdout.trim()).map_err(|e| anyhow!("unexpected reply from Mail: {e}: {stdout}"))?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        bail!("{}", failure(&v))
    }
}

/// What a script that failed said, as a sentence telling what to do.
fn failure(v: &Value) -> String {
    let msg = v.get("error").and_then(Value::as_str).unwrap_or("unknown error");
    match v.get("number").and_then(Value::as_i64) {
        Some(-1743) => NOT_ALLOWED.to_string(),
        Some(-600 | -609) => "Mail is not running: ask the user to open Mail, then try again.".into(),
        Some(-1712) => format!(
            "{NO_ANSWER} in time (busy?). The action may or may not have been applied: check Mail before retrying."
        ),
        _ => format!("Mail refused: {msg}"),
    }
}

/// Why a batch stopped before its end.
#[derive(Debug, Clone)]
pub struct Stop {
    /// What to tell Claude.
    pub text: String,
    /// The message Mail was at when it stopped may have been acted on.
    pub maybe_applied: bool,
    /// Mail gave no account of the batch at all: any of it may have been
    /// done.
    pub unreported: bool,
}

/// The items a batch script went through and, when it stopped early on an
/// error that concerns every message (permission, Mail closed), why.
pub fn batch(v: &Value) -> (Vec<Value>, Option<Stop>) {
    let items = v.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let stop = v.get("fatal").filter(|f| f.is_object()).map(|f| Stop {
        text: failure(f),
        maybe_applied: f.get("number").and_then(Value::as_i64) == Some(-1712),
        unreported: false,
    });
    (items, stop)
}

/// Run a batch script: the items Mail went through, and why it stopped
/// early if it did.
pub async fn run_batch(script: &str, input: &Value, timeout: Duration) -> (Vec<Value>, Option<Stop>) {
    match run(script, input, timeout).await {
        Ok(r) => batch(&r),
        Err(e) => {
            let text = e.to_string();
            let maybe_applied = text.starts_with(NO_ANSWER);
            (vec![], Some(Stop { text, maybe_applied, unreported: maybe_applied }))
        }
    }
}

/// Open a message in Mail via its `message:` URL (no Automation needed).
pub fn open_message_url(message_id: &str) -> Result<()> {
    if let Some(why) = unavailable() {
        bail!("{why}");
    }
    let mut enc = String::new();
    for b in format!("<{message_id}>").bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'.'
            | b'-'
            | b'_'
            | b'~'
            | b'@'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'=' => enc.push(b as char),
            other => enc.push_str(&format!("%{other:02X}")),
        }
    }
    let status = std::process::Command::new("/usr/bin/open").arg(format!("message://{enc}")).status()?;
    if !status.success() {
        bail!("Mail could not open the message");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn applescript_errors_read_like_jxa_ones() {
        let e = script_error("12:345: execution error: Mail got an error: AppleEvent handler failed. (-10000)\n");
        assert_eq!(e, "Mail refused: Mail got an error: AppleEvent handler failed. (-10000)");
        assert!(
            script_error("0:1: execution error: Not authorized to send Apple events to Mail. (-1743)")
                .contains("open_permission_settings")
        );
        assert!(script_error("execution error: Mail isn't running. (-600)").contains("not running"));
    }

    #[test]
    fn a_batch_stopped_by_mail_keeps_what_was_done() {
        let r = json!({"items": [{"id": 1, "ok": true, "message_id": "<a@x>"}],
                       "fatal": {"error": "Not authorized", "number": -1743}});
        let (items, stop) = batch(&r);
        assert_eq!(items.len(), 1);
        let stop = stop.unwrap();
        assert!(stop.text.contains("open_permission_settings") && !stop.maybe_applied);
        let (items, stop) = batch(&json!({"items": [{"id": 1, "ok": true}], "fatal": null}));
        assert_eq!(items.len(), 1);
        assert!(stop.is_none());
        let closed = batch(&json!({"items": [], "fatal": {"error": "x", "number": -600}})).1.unwrap();
        assert!(closed.text.contains("not running"), "{}", closed.text);
        let busy = batch(&json!({"items": [], "fatal": {"error": "x", "number": -1712}})).1.unwrap();
        assert!(busy.maybe_applied && !busy.unreported);
        let other = batch(&json!({"items": [], "fatal": {"error": "boom"}})).1.unwrap();
        assert_eq!(other.text, "Mail refused: boom");
    }

    /// Run JavaScript with Node, which reads the same language as JavaScript
    /// for Automation; None where Node is missing (the test is then skipped).
    fn node(tag: &str, args: &[&str], source: &str) -> Option<std::process::Output> {
        let dir = std::env::temp_dir().join(format!("timbre-js-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("script.js");
        std::fs::write(&file, source).unwrap();
        let out = std::process::Command::new("node").args(args).arg(&file).output();
        let _ = std::fs::remove_dir_all(&dir);
        match out {
            Ok(o) => Some(o),
            Err(e) => {
                eprintln!("skipped: no Node to read the scripts with ({e})");
                None
            }
        }
    }

    #[test]
    fn every_script_parses() {
        for (name, script) in [
            ("VERIFY", VERIFY),
            ("DRAFT_NEW", DRAFT_NEW),
            ("DRAFT_REPLY", DRAFT_REPLY),
            ("DRAFT_FORWARD", DRAFT_FORWARD),
            ("MARK", MARK),
            ("MOVE", MOVE),
            ("FIND_MAILBOX", FIND_MAILBOX),
            ("MOVE_BACK", MOVE_BACK),
            ("SELECTION", SELECTION),
            ("SEND_OUTGOING", SEND_OUTGOING),
            ("ACCOUNT_SERVER", ACCOUNT_SERVER),
        ] {
            let Some(out) = node("parse", &["--check"], &format!("{PRELUDE}\n{script}")) else { return };
            assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }

    /// Mail's scripting stood in for: an account's mailboxes by name at each
    /// level, or all of them by their path from the account's prefix on.
    const FAKE_MAIL: &str = r#"
const ObjC = {import() {}};
const $ = {};
function delay() {}
function list(kids) {
  const f = () => kids;
  f.byName = (n) => kids.find((k) => k.label === n) || {name() { throw new Error('no ' + n); }, mailboxes: list([])};
  f.whose = (q) => () => kids.filter((k) => k.label === q.name);
  return f;
}
function box(label, tag, kids) { return {label, tag, name: () => label, mailboxes: list(kids || [])}; }
const ACCOUNTS = {
  // La Poste as Mail names it: from the prefix INBOX on, folders inside folders.
  LP: box('Laposte', 'lp', [box('INBOX', 'lp-inbox'), box('OUTBOX', 'lp-sent'), box('Important', 'lp-important'),
    box('Archives', 'lp-archives', [box('2009', 'lp-2009'), box('Passé', 'lp-passe')])]),
  // The same, each mailbox listed by its whole path.
  LF: box('Orange', 'lf', [box('INBOX', 'lf-inbox'), box('Archives', 'lf-archives'), box('Archives/2009', 'lf-2009')]),
  GM: box('Bob', 'gm', [box('INBOX', 'gm-inbox'), box('[Gmail]', 'gm-gmail', [box('Messages envoyés', 'gm-sent')])]),
  // iCloud: a folder inside the inbox, and another of the same name at the top.
  IC: box('iCloud', 'ic', [box('INBOX', 'ic-inbox', [box('Clients', 'ic-inbox-clients')]), box('Clients', 'ic-clients')]),
};
const accounts = () => Object.values(ACCOUNTS);
accounts.byId = (id) => ACCOUNTS[id] || {name() { throw new Error('no account ' + id); }};
accounts.byName = (n) => accounts().find((a) => a.label === n) || {name() { throw new Error('no account ' + n); }};
function Application() { return {accounts, mailboxes: list([])}; }
"#;

    /// Mail names an INBOX-prefixed account's mailboxes from the prefix on,
    /// and may write accents differently from its database: each is found,
    /// and never another mailbox that merely shares a name.
    #[test]
    fn mailboxes_are_found_as_mail_names_them() {
        let cases = r#"
const cases = [
  ['LP', ['INBOX', 'Archives', '2009']],
  ['LP', ['INBOX', 'Archives', 'Passé']],
  ['LP', ['INBOX', 'OUTBOX']],
  ['LP', ['INBOX']],
  ['LP', ['INBOX', 'Archives']],
  ['LF', ['INBOX', 'Archives', '2009']],
  ['GM', ['[Gmail]', 'Messages envoyés']],
  ['IC', ['INBOX', 'Clients']],
  ['IC', ['Clients']],
  ['LP', ['INBOX', 'Ailleurs', 'Important']],
  ['LP', ['INBOX', 'Nulle part']],
];
console.log(JSON.stringify(cases.map(([id, path]) => {
  try { return mailbox({account_id: id, account: '', path, local: false}).tag; } catch (e) { return 'error: ' + e.message; }
})));
"#;
        let Some(out) = node("find", &[], &format!("{FAKE_MAIL}\n{PRELUDE}\n{cases}")) else { return };
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let found: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(
            found,
            [
                "lp-2009",
                "lp-passe",
                "lp-sent",
                "lp-inbox",
                "lp-archives",
                "lf-2009",
                "gm-sent",
                "ic-inbox-clients",
                "ic-clients",
                // Only a name no other mailbox has, as before.
                "lp-important",
                "error: Mailbox not found in Mail: INBOX/Nulle part",
            ]
        );
    }
}
