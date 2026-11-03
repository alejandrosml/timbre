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
  const full = loc.path.join('/');
  let found = null;
  try { const mb = container.mailboxes.byName(full); mb.name(); found = mb; } catch (e) {}
  if (!found) {
    try { let cur = container; for (const p of loc.path) { cur = cur.mailboxes.byName(p); } cur.name(); found = cur; } catch (e) {}
  }
  if (!found) {
    const leaf = loc.path[loc.path.length - 1];
    const hits = container.mailboxes.whose({name: leaf})();
    if (hits.length > 0) found = hits[0];
  }
  if (!found) throw new Error('Mailbox not found in Mail: ' + (loc.account ? loc.account + ' / ' : '') + full);
  _mb[key] = found;
  return found;
}
function message(loc, id) {
  const m = mailbox(loc).messages.byId(id);
  m.id();
  return m;
}
function addRecipients(msg, i) {
  for (const a of (i.to || [])) msg.toRecipients.push(Mail.ToRecipient({address: a}));
  for (const a of (i.cc || [])) msg.ccRecipients.push(Mail.CcRecipient({address: a}));
  for (const a of (i.bcc || [])) msg.bccRecipients.push(Mail.BccRecipient({address: a}));
}
function describe(msg) {
  const list = (r) => { try { return r.address(); } catch (e) { return []; } };
  return {outgoing_id: msg.id(), subject: msg.subject(), to: list(msg.toRecipients), cc: list(msg.ccRecipients), bcc: list(msg.bccRecipients)};
}
function run(argv) {
  try { return JSON.stringify({ok: true, result: main(readInput())}); }
  catch (e) { return JSON.stringify({ok: false, error: String(e && e.message ? e.message : e), number: (e && e.errorNumber) || null}); }
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
  const orig = message(i.loc, i.id);
  const r = Mail.reply(orig, {openingWindow: true, replyToAll: !!i.reply_all});
  r.content = i.body;
  addRecipients(r, {cc: i.cc, bcc: i.bcc});
  try { r.save(); } catch (e) {}
  return describe(r);
}
"#;

pub const DRAFT_FORWARD: &str = r#"
function main(i) {
  const orig = message(i.loc, i.id);
  const f = Mail.forward(orig, {openingWindow: true});
  addRecipients(f, i);
  if (i.body) { try { const cur = f.content(); f.content = i.body + '\n\n' + cur; } catch (e) {} }
  try { f.save(); } catch (e) {}
  return describe(f);
}
"#;

pub const MARK: &str = r#"
function main(i) {
  const out = [];
  for (const it of i.items) {
    try {
      const m = message(it.loc, it.id);
      if (i.action === 'read') m.readStatus = true;
      else if (i.action === 'unread') m.readStatus = false;
      else if (i.action === 'flag') { m.flaggedStatus = true; if (typeof i.color === 'number') m.flagIndex = i.color; }
      else if (i.action === 'unflag') m.flaggedStatus = false;
      out.push({id: it.id, ok: true});
    } catch (e) { out.push({id: it.id, ok: false, error: String(e && e.message ? e.message : e)}); }
  }
  return out;
}
"#;

pub const MOVE: &str = r#"
function main(i) {
  const out = [];
  for (const it of i.items) {
    try {
      const m = message(it.loc, it.id);
      let mid = null;
      try { mid = m.messageId(); } catch (e) {}
      Mail.move(m, {to: mailbox(it.dest)});
      out.push({id: it.id, ok: true, message_id: mid});
    } catch (e) { out.push({id: it.id, ok: false, error: String(e && e.message ? e.message : e)}); }
  }
  return out;
}
"#;

pub const MOVE_BACK: &str = r#"
function main(i) {
  const out = [];
  for (const it of i.items) {
    try {
      const src = mailbox(it.now_in);
      const hits = src.messages.whose({messageId: it.message_id})();
      if (hits.length === 0) throw new Error('message not found where it was moved');
      Mail.move(hits[0], {to: mailbox(it.back_to)});
      out.push({message_id: it.message_id, ok: true});
    } catch (e) { out.push({message_id: it.message_id, ok: false, error: String(e && e.message ? e.message : e)}); }
  }
  return out;
}
"#;

pub const SELECTION: &str = r#"
function main(i) {
  let sel = [];
  try { sel = Mail.selection(); } catch (e) { return []; }
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
  const d = describe(msg);
  const all = [].concat(d.to, d.cc, d.bcc).map(x => String(x).toLowerCase());
  for (const a of all) { if (i.allowed.indexOf(a) < 0) throw new Error('recipient changed since preparation: ' + a); }
  msg.send();
  return {sent: true, to: d.to, cc: d.cc, bcc: d.bcc, subject: d.subject};
}
"#;

/// Run one JXA script with `input` as JSON on stdin.
pub async fn run(script: &str, input: &Value, timeout: Duration) -> Result<Value> {
    if !cfg!(target_os = "macos") {
        bail!("Mail.app scripting is only available on macOS");
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
            "Mail did not answer within {}s. The action may or may not have been applied: check Mail before retrying.",
            timeout.as_secs()
        ),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stdout.trim().is_empty() {
        if stderr.contains("-1743") || stderr.contains("Not authorized") {
            bail!(
                "macOS has not allowed Timbre to control Mail. Allow it in System Settings › Privacy & Security › Automation."
            );
        }
        bail!("Mail scripting failed: {}", stderr.trim());
    }
    let v: Value =
        serde_json::from_str(stdout.trim()).map_err(|e| anyhow!("unexpected reply from Mail: {e}: {stdout}"))?;
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        let msg = v.get("error").and_then(Value::as_str).unwrap_or("unknown error");
        let num = v.get("number").and_then(Value::as_i64);
        if num == Some(-1743) {
            bail!(
                "macOS has not allowed Timbre to control Mail. Allow it in System Settings › Privacy & Security › Automation."
            );
        }
        bail!("Mail refused: {msg}")
    }
}

/// Open a message in Mail via its `message:` URL (no Automation needed).
pub fn open_message_url(message_id: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("opening Mail is only available on macOS");
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
