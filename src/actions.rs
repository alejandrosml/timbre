//! Write operations: drafts, marking, moving with undo, and two-step sending.
//!
//! Nothing here deletes mail permanently. Moves are journaled so they can be
//! undone; sending is off unless the user enables it, and then always takes
//! two calls: `prepare_send` shows exactly what will go out, `confirm_send`
//! sends that and only that.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::Ctx;
use crate::config::Config;
use crate::mailapp::{self, Loc};
use crate::mime;
use crate::store::{Mailbox, Role, Store};
use crate::util::{iso_local, now_unix};

// ---------------------------------------------------------------- journals

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoItem {
    pub message_id: String,
    pub now_in: Loc,
    pub back_to: Loc,
    /// Moved back already (an undo can partly fail and be retried).
    #[serde(default)]
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub token: String,
    pub created: i64,
    pub description: String,
    pub items: Vec<UndoItem>,
    pub undone: bool,
    /// The Timbre (process) that made the move: an undo without a token
    /// takes this Timbre's latest move, never another Claude window's.
    #[serde(default)]
    pub owner: u32,
}

/// When this Timbre started: moves made before, by any, are the user's
/// earlier ones, which an undo without a token may take as before.
static STARTED: std::sync::OnceLock<i64> = std::sync::OnceLock::new();

fn undo_path(cfg: &Config) -> std::path::PathBuf {
    cfg.data_dir.join("undo.json")
}

pub fn load_undo(cfg: &Config) -> Vec<UndoEntry> {
    STARTED.get_or_init(now_unix);
    read_undo(cfg).unwrap_or_default()
}

/// The journal on disk; None when there is none, or it cannot be read.
fn read_undo(cfg: &Config) -> Option<Vec<UndoEntry>> {
    std::fs::read(undo_path(cfg)).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

fn save_undo(cfg: &Config, entries: &[UndoEntry]) {
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    if let Ok(b) = serde_json::to_vec_pretty(entries) {
        // A file of this process's own: two Timbres never write the same one.
        let tmp = cfg.data_dir.join(format!("undo.json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(tmp, undo_path(cfg));
        }
    }
}

/// Another Timbre (a second Claude window) may keep the journal too: it is
/// read again under a lock both take, so neither loses the other's moves.
/// The lock goes with the file handle.
fn lock_journal(cfg: &Config) -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(cfg.data_dir.join("undo.lock"))
        .ok()?;
    f.lock().ok()?;
    Some(f)
}

pub struct PendingSend {
    pub outgoing_id: i64,
    pub recipients: Vec<String>,
    pub subject: String,
    pub created: i64,
}

#[derive(Default)]
pub struct SendState {
    pub pending: HashMap<String, PendingSend>,
    pub sent: VecDeque<i64>,
}

fn token() -> String {
    let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(now_unix());
    format!("{:016x}{:08x}", a, h.finish() as u32)
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Failure {
    /// The message id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// The message's Message-ID header (undo_move failures).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub error: String,
}

impl Failure {
    fn of(id: i64, error: impl Into<String>) -> Failure {
        Failure { id: Some(id), message_id: None, error: error.into() }
    }
}

/// Where a message is, for acting on it in Mail: the mailbox the user sees
/// it in (for Gmail its inbox or label, not All Mail where Mail stores it)
/// and, when different, the stored one.
struct Source {
    shown: Mailbox,
    stored: Mailbox,
    /// The Message-ID Timbre read in its file: Mail's scripts check that
    /// the message they find by id is still that one.
    mid: Option<String>,
}

impl Source {
    fn json(&self, id: i64) -> serde_json::Value {
        let alt = (self.stored.id != self.shown.id).then(|| Loc::from(&self.stored));
        json!({"id": id, "loc": Loc::from(&self.shown), "alt": alt, "mid": self.mid})
    }
}

fn source_of(ctx: &Ctx, id: i64) -> Result<Source, String> {
    let meta = ctx.meta(id)?;
    if meta.deleted {
        return Err(format!("Message {id} was deleted in Mail: there is nothing to act on."));
    }
    let conn = ctx.db();
    let stored =
        ctx.store.mailbox_fresh(&conn, meta.mailbox).ok_or_else(|| format!("message {id} is in an unknown mailbox"))?;
    let shown_id = ctx.store.display_mailbox(&conn, id, meta.mailbox);
    let shown = ctx.store.mailbox_fresh(&conn, shown_id).unwrap_or_else(|| stored.clone());
    let mid: Option<String> = conn
        .query_row("SELECT msgid FROM main.docs WHERE id = ?1", [id], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten()
        .map(|m| m.trim().trim_matches(['<', '>']).trim().to_string())
        .filter(|m| !m.is_empty());
    Ok(Source { shown, stored, mid })
}

fn timeout_for(n: usize) -> Duration {
    Duration::from_secs((15 + 2 * n as u64).min(180))
}

/// Database and file work, off the async threads: a long search holding
/// the reader must not stall the server's few threads.
async fn blocking<T: Send + 'static>(ctx: &Arc<Ctx>, f: impl FnOnce(&Ctx) -> T + Send + 'static) -> Result<T, String> {
    let ctx = ctx.clone();
    tokio::task::spawn_blocking(move || f(&ctx)).await.map_err(|e| format!("internal error: {e}"))
}

/// Each id once, in the order given: an id given twice is acted on once.
fn unique(ids: &[i64]) -> Vec<i64> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

#[cfg(test)]
pub fn unique_for_tests(ids: &[i64]) -> Vec<i64> {
    unique(ids)
}

// ---------------------------------------------------------------- drafts

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct DraftParams {
    /// "new", "reply", "reply_all" or "forward".
    pub mode: String,
    /// Message to reply to or forward (required except for "new").
    pub id: Option<i64>,
    /// Recipients for "new" and "forward". Replies go to the original sender(s).
    pub to: Option<Vec<String>>,
    pub cc: Option<Vec<String>>,
    pub bcc: Option<Vec<String>>,
    /// Subject for "new" (replies and forwards keep Mail's subject).
    pub subject: Option<String>,
    /// Plain-text body you wrote. For replies the quoted original is added below.
    pub body: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DraftResult {
    pub status: String,
    pub subject: String,
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    pub note: String,
}

/// Split a recipient field on commas and semicolons that are not inside
/// quotes or angle brackets: "Jean <j@x.fr>, \"Doe, Ann\" <a@y.fr>".
fn split_recipients(s: &str) -> Vec<&str> {
    let (mut out, mut start, mut quoted, mut angle) = (vec![], 0, false, false);
    for (i, c) in s.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' if !quoted && !angle => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out.into_iter().map(str::trim).filter(|p| !p.is_empty()).collect()
}

/// One plain address `local@domain`, nothing that Mail could read as two.
fn plain_address(a: &str) -> Option<String> {
    let (local, domain) = a.split_once('@')?;
    let bad = |c: char| c.is_whitespace() || c.is_control() || "<>(),;:\"[]\\@".contains(c);
    let ok = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && a.len() <= 254
        && !local.chars().any(bad)
        && !domain.chars().any(bad);
    ok.then(|| a.to_string())
}

fn valid_addresses(list: &Option<Vec<String>>) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = vec![];
    for entry in list.iter().flatten() {
        for part in split_recipients(entry) {
            let addr = match part.rsplit_once('<') {
                Some((_, r)) => r.trim_end().strip_suffix('>').unwrap_or(r).trim(),
                None => part,
            };
            let addr = plain_address(addr).ok_or_else(|| format!("'{part}' is not an email address"))?;
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&addr)) {
                out.push(addr);
            }
        }
    }
    Ok(out)
}

fn quote_header(body: &str, date: &str, from: &str) -> String {
    let b = format!(" {} ", body.to_lowercase());
    let fr = [" bonjour", " merci", " je ", " vous ", " nous ", " cordialement", " bien à"]
        .iter()
        .filter(|w| b.contains(*w))
        .count();
    let es =
        [" hola", " gracias", " usted", " saludos", " estimado", " nosotros"].iter().filter(|w| b.contains(*w)).count();
    if fr >= 1 && fr >= es {
        format!("Le {date}, {from} a écrit :")
    } else if es >= 1 {
        format!("El {date}, {from} escribió:")
    } else {
        format!("On {date}, {from} wrote:")
    }
}

/// The reply text: what the model wrote, then the original, quoted.
/// Built here so no clipboard or Accessibility permission is needed.
fn reply_body(ctx: &Ctx, id: i64, body: &str) -> String {
    let meta = ctx.meta(id).ok();
    let original = ctx.raw(id).ok().and_then(|(raw, _)| mime::parse(&raw)).map(|p| p.body).unwrap_or_default();
    let mut out = body.trim_end().to_string();
    if let Some(m) = meta {
        out.push_str("\n\n");
        out.push_str(&quote_header(body, &iso_local(m.date).unwrap_or_default().replace('T', " "), &m.from));
        out.push('\n');
        for line in original.lines().take(400) {
            out.push_str("> ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The draft Mail made, and the files that go out with it (a forward
/// keeps the original's attachments).
async fn compose(ctx: &Arc<Ctx>, p: &DraftParams) -> Result<(serde_json::Value, Vec<String>), String> {
    let p = p.clone();
    let (script, input, files) = blocking(ctx, move |ctx| draft_input(ctx, &p)).await??;
    let made = mailapp::run(script, &input, Duration::from_secs(40)).await.map_err(|e| e.to_string())?;
    Ok((made, files))
}

/// The script and its input for a draft, and the files a forward sends on:
/// the message replied to or forwarded is read here, off the async threads.
fn draft_input(ctx: &Ctx, p: &DraftParams) -> Result<(&'static str, serde_json::Value, Vec<String>), String> {
    let to = valid_addresses(&p.to)?;
    let cc = valid_addresses(&p.cc)?;
    let bcc = valid_addresses(&p.bcc)?;
    let mode = p.mode.trim().to_lowercase();
    let mut files: Vec<String> = vec![];
    let (script, input) = match mode.as_str() {
        "new" => {
            if to.is_empty() {
                return Err("A new message needs at least one recipient in 'to'.".into());
            }
            (
                mailapp::DRAFT_NEW,
                json!({"to": to, "cc": cc, "bcc": bcc, "subject": p.subject.clone().unwrap_or_default(), "body": p.body}),
            )
        }
        "reply" | "reply_all" => {
            let id = p.id.ok_or("'id' of the message to reply to is required")?;
            let mut input = source_of(ctx, id)?.json(id);
            input["reply_all"] = json!(mode == "reply_all");
            input["cc"] = json!(cc);
            input["bcc"] = json!(bcc);
            input["body"] = json!(reply_body(ctx, id, &p.body));
            (mailapp::DRAFT_REPLY, input)
        }
        "forward" => {
            let id = p.id.ok_or("'id' of the message to forward is required")?;
            if to.is_empty() {
                return Err("A forward needs at least one recipient in 'to'.".into());
            }
            let mut input = source_of(ctx, id)?.json(id);
            input["to"] = json!(to);
            input["cc"] = json!(cc);
            input["bcc"] = json!(bcc);
            // Mail drops a forward's attachments when a script replaces its
            // text: with attachments, the text is left for the user to type.
            files = ctx
                .raw(id)
                .ok()
                .and_then(|(raw, _)| mime::parse(&raw))
                .map(|m| m.attachments.iter().filter(|a| !a.inline).map(|a| a.name.clone()).collect())
                .unwrap_or_default();
            input["body"] = json!(if files.is_empty() { p.body.as_str() } else { "" });
            (mailapp::DRAFT_FORWARD, input)
        }
        other => return Err(format!("Unknown mode '{other}': use new, reply, reply_all or forward.")),
    };
    Ok((script, input, files))
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub async fn create_draft(ctx: &Arc<Ctx>, p: &DraftParams) -> Result<DraftResult, String> {
    let (r, _) = compose(ctx, p).await?;
    let mut note = "The draft is open in Mail for the user to review. Nothing was sent.".to_string();
    let text_left_out = p.mode.trim().eq_ignore_ascii_case("forward")
        && !p.body.trim().is_empty()
        && r.get("text_added").and_then(|v| v.as_bool()) == Some(false);
    if text_left_out {
        note.push_str(
            " The forwarded message has attachments, which Mail would drop if Timbre changed the text: \
             ask the user to type the text above the forwarded message in the open draft.",
        );
    }
    Ok(DraftResult {
        status: "draft_open_in_mail".into(),
        subject: r.get("subject").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
        to: strings(&r, "to"),
        cc: strings(&r, "cc"),
        bcc: strings(&r, "bcc"),
        note,
    })
}

// ---------------------------------------------------------------- mark

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkParams {
    /// Message ids (1-500).
    pub ids: Vec<i64>,
    /// "read", "unread", "flag" or "unflag".
    pub action: String,
    /// Flag colour for "flag": red, orange, yellow, green, blue, purple or gray.
    pub color: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BatchResult {
    pub done: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<Failure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undo_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn color_index(c: &str) -> Option<i64> {
    ["red", "orange", "yellow", "green", "blue", "purple", "gray"]
        .iter()
        .position(|x| *x == c.trim().to_lowercase().replace("grey", "gray"))
        .map(|i| i as i64)
}

fn collect_failures(items: &[serde_json::Value]) -> (usize, Vec<Failure>) {
    let mut ok = 0;
    let mut failed = vec![];
    for item in items {
        if item.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            ok += 1;
        } else {
            failed.push(Failure {
                id: item.get("id").and_then(|v| v.as_i64()),
                message_id: item.get("message_id").and_then(|v| v.as_str()).map(str::to_string),
                error: item.get("error").and_then(|v| v.as_str()).unwrap_or("failed").to_string(),
            });
        }
    }
    (ok, failed)
}

/// Messages per call to Mail: a slow Mail stops one batch, not all of
/// them, and what was done is known.
const CHUNK: usize = 25;

/// How long Mail may take to make a mailbox on the server, then to list it.
const MAKE_TIMEOUT: Duration = Duration::from_secs(90);
const FIND_TIMEOUT: Duration = Duration::from_secs(40);

pub async fn mark(ctx: &Arc<Ctx>, p: &MarkParams) -> Result<BatchResult, String> {
    let ids = unique(&p.ids);
    if ids.is_empty() || ids.len() > 500 {
        return Err("Give between 1 and 500 ids.".into());
    }
    let action = p.action.trim().to_lowercase();
    if !["read", "unread", "flag", "unflag"].contains(&action.as_str()) {
        return Err("action must be read, unread, flag or unflag".into());
    }
    let color = match &p.color {
        Some(c) => Some(color_index(c).ok_or_else(|| format!("unknown colour '{c}'"))?),
        None => None,
    };
    let (items, mut failed) = blocking(ctx, move |ctx| {
        let (mut items, mut failed) = (vec![], vec![]);
        for id in ids {
            match source_of(ctx, id) {
                Ok(src) => items.push((id, src.json(id))),
                Err(e) => failed.push(Failure::of(id, e)),
            }
        }
        (items, failed)
    })
    .await?;
    let mut done = 0;
    let mut stopped: Option<String> = None;
    let mut chunks = items.chunks(CHUNK);
    for chunk in chunks.by_ref() {
        let batch: Vec<_> = chunk.iter().map(|(_, v)| v.clone()).collect();
        let input = json!({"items": batch, "action": action, "color": color});
        let (tried, stop) = mailapp::run_batch(mailapp::MARK, &input, timeout_for(batch.len())).await;
        let (n, mut f) = collect_failures(&tried);
        done += n;
        failed.append(&mut f);
        if let Some(stop) = stop {
            if done == 0 && failed.is_empty() {
                return Err(stop.text);
            }
            failed.extend(chunk[tried.len().min(chunk.len())..].iter().map(|(id, _)| Failure::of(*id, &stop.text)));
            stopped = Some(stop.text);
            break;
        }
    }
    let why = stopped.unwrap_or_default();
    failed.extend(chunks.flatten().map(|(id, _)| Failure::of(*id, format!("not tried: {why}"))));
    Ok(BatchResult { done, failed, undo_token: None, note: None })
}

// ---------------------------------------------------------------- move + undo

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveParams {
    /// Message ids (1-200).
    pub ids: Vec<i64>,
    /// Destination: archive, trash, inbox or junk (each message's own account),
    /// or a mailbox name or path from list_mailboxes.
    pub to: String,
    /// Account of the destination mailbox, when its name exists in several accounts.
    pub account: Option<String>,
}

/// Where a move goes.
struct Target {
    mailbox: Mailbox,
    /// The account has no archive mailbox: Mail creates it first, as its
    /// Archive button does.
    create: bool,
}

fn destination(ctx: &Ctx, src: &Mailbox, to: &str, account: Option<&str>) -> Result<Target, String> {
    let found = |mailbox: Mailbox| Target { mailbox, create: false };
    if let Some(role) = Role::from_alias(to) {
        // The role's mailbox of each message's own account, or of the
        // account named: as Mail or the provider names it, never a folder
        // of the user's that holds the word.
        let (account_id, name) = match account.filter(|a| !crate::store::account_key(a).is_empty()) {
            None => (src.account_id.clone(), src.account.clone()),
            Some(a) => match ctx.store.accounts_named(a).as_slice() {
                [(id, name)] => (id.clone(), name.clone()),
                [] => {
                    return Err(format!("No account named '{a}'. Accounts: {}.", ctx.store.account_names().join(", ")));
                }
                many => {
                    let names: Vec<&str> = many.iter().map(|(_, n)| n.as_str()).collect();
                    return Err(format!("'{a}' matches several accounts ({}): give its full name.", names.join(", ")));
                }
            },
        };
        if let Some(m) = ctx.store.role_mailbox_for_account(&account_id, &name, role) {
            return Ok(found(m));
        }
        if role == Role::Archive
            && let Some(mailbox) = new_archive(&ctx.store, &account_id, &name)
        {
            return Ok(Target { mailbox, create: true });
        }
        return Err(format!("Account '{name}' has no {} mailbox.", role.as_str()));
    }
    let ids = ctx.store.resolve_mailboxes(to, account);
    let all: Vec<Mailbox> = ids.iter().filter_map(|i| ctx.store.mailbox(*i)).collect();
    let same: Vec<&Mailbox> =
        all.iter().filter(|m| crate::store::same_account(m, &src.account_id, &src.account)).collect();
    match (same.len(), all.len()) {
        (1, _) => Ok(found(same[0].clone())),
        (0, 1) => Ok(found(all[0].clone())),
        (0, 0) => {
            // Made by Timbre a moment ago (create_mailbox), before Mail's
            // database lists it: Mail finds it by its path.
            let account_id = match account.map(|a| ctx.store.accounts_named(a)) {
                Some(named) if named.len() == 1 => named[0].0.clone(),
                Some(_) => String::new(),
                None => src.account_id.clone(),
            };
            ctx.store
                .made_lately_named(to, &account_id)
                .map(|loc| found(mailbox_at(&loc)))
                .ok_or_else(|| format!("No mailbox named '{to}'. Call list_mailboxes."))
        }
        _ => Err(format!("'{to}' matches several mailboxes: give 'account' or the full path.")),
    }
}

/// A mailbox known by its place alone (Mail's database does not list it yet).
fn mailbox_at(loc: &Loc) -> Mailbox {
    Mailbox {
        id: -2,
        url: String::new(),
        account_id: loc.account_id.clone(),
        account: loc.account.clone(),
        name: loc.path.last().cloned().unwrap_or_default(),
        path: loc.path.clone(),
        role: Role::Other,
        role_strength: 0,
        local: loc.local,
    }
}

/// The archive mailbox Mail's Archive button makes for an IMAP account that
/// has none: "Archive", at the top of the account as Mail shows it. Its
/// path is where the server will keep it, beside the account's own
/// mailboxes: inside INBOX on servers that keep everything there
/// ("INBOX/DRAFT", "INBOX/TRASH" at La Poste and Orange). None where Mail
/// would not make one: On My Mac, Exchange, Gmail (whose All Mail is the
/// archive).
pub(crate) fn new_archive(store: &Store, account_id: &str, account: &str) -> Option<Mailbox> {
    let mailboxes = store.mailboxes();
    let any = mailboxes.values().find(|m| crate::store::same_account(m, account_id, account))?;
    if any.local || !any.url.starts_with("imap://") || store.is_gmail(account_id, account) {
        return None;
    }
    let loc = Loc { account_id: account_id.to_string(), account: account.to_string(), path: vec![], local: false };
    let mut path = crate::mailboxes::mail_prefix(store, &loc).unwrap_or_default();
    path.push("Archive".to_string());
    Some(Mailbox {
        id: -1,
        url: String::new(),
        account_id: account_id.to_string(),
        account: account.to_string(),
        name: "Archive".into(),
        path,
        role: Role::Archive,
        role_strength: 2,
        local: false,
    })
}

// What Mail does when a script moves a Gmail message, as tried on a real
// account (macOS 26, November 2026). Mail keeps each Gmail message once, in
// All Mail, and shows its labels (Inbox…) as mailboxes; a scripted move
// takes the message from All Mail, never from the label it is shown under:
// - to the Trash or Spam it goes, keeping its labels;
// - from there to the inbox it comes out, labels kept, but lands in All
//   Mail: Gmail gives it no Inbox label;
// - out of the inbox or a label (to the archive, another label) nothing
//   happens: the message is back within seconds. Only Mail's Archive button
//   takes a message out of the Gmail inbox;
// - from the Trash or Spam to All Mail, Gmail deletes it for good (Mail
//   keeps a copy in "Recovered Messages", on the Mac only).
const GMAIL_STAYS: &str = "Gmail: by script, Mail can only move a message to the trash or junk, and out of them to \
All Mail; it cannot make this move (a message taken out of the inbox comes straight back). Gmail itself can (its \
website or app, or a Gmail connector), and Mail's Archive button (Message > Archive) archives.";
const GMAIL_DELETES: &str = "Gmail: out of the trash or junk, Mail can only move a message to the inbox by script; \
anywhere else, Gmail would delete it for good.";
const GMAIL_TRASHED: &str = "Gmail: undo_move takes them out of the trash or junk with their labels, but Mail \
cannot put a Gmail message back in the inbox by script: those from the inbox will be in All Mail (the archive).";
const GMAIL_RESCUED: &str = "Gmail: out of the trash or junk with their labels, but in All Mail (the archive), not \
the inbox: Mail cannot put a Gmail message in the inbox by script. Mail may still list them in the inbox for a while.";
const GMAIL_UNDONE: &str = "Gmail: out of the trash or junk with their labels, but Mail cannot put a Gmail message \
back in the inbox by script: those that were in the inbox are in All Mail (the archive). Mail may still list them in \
the inbox for a while.";
const GMAIL_ELSEWHERE: &str = "Gmail may keep its own copy of messages moved to another account.";
const GMAIL_ALL_MAIL: &str = "Gmail keeps every message in All Mail, so Timbre cannot tell whether this one is \
still where it was moved: it is left where it is.";

/// Gmail's own trash or spam, never a label of the user's named so.
fn thrown(m: &Mailbox) -> bool {
    crate::store::gmail_bin(m)
}

/// Whether Mail can make this move of a Gmail message, from the mailbox it
/// is shown in: Ok with a note to pass on, or Err with why not. Moves that
/// involve no Gmail account are not concerned.
pub(crate) fn gmail_move(store: &Store, from: &Mailbox, to: &Mailbox) -> Result<Option<&'static str>, String> {
    if !store.is_gmail(&from.account_id, &from.account) {
        return Ok(None);
    }
    if !crate::store::same_account(to, &from.account_id, &from.account) {
        return Ok(Some(GMAIL_ELSEWHERE));
    }
    match (thrown(from), thrown(to), to.role) {
        (false, true, _) => Ok(Some(GMAIL_TRASHED)),
        (true, false, Role::Inbox) => Ok(Some(GMAIL_RESCUED)),
        (true, ..) => Err(GMAIL_DELETES.into()),
        (false, false, _) => Err(GMAIL_STAYS.into()),
    }
}

/// Where an undo can send a message back, for Gmail: out of the trash or
/// junk only through the inbox (it keeps its labels, so one that was under
/// a label is back there; one from the inbox lands in All Mail).
pub(crate) fn gmail_undo(store: &Store, now_in: &Loc, back_to: &Loc) -> Result<(Loc, Option<&'static str>), String> {
    if now_in.local || !store.is_gmail(&now_in.account_id, &now_in.account) {
        return Ok((back_to.clone(), None));
    }
    let same = !back_to.local && back_to.account_id == now_in.account_id;
    if !same {
        return Ok((back_to.clone(), Some(GMAIL_ELSEWHERE)));
    }
    // The role of each place, and whether it is Gmail's own trash or spam.
    let place = |l: &Loc| {
        let (role, strength) = store
            .mailbox_at(&l.account_id, &l.account, &l.path)
            .map_or_else(|| Role::detect_scored(&l.path), |m| (m.role, m.role_strength));
        let system = l.path.first().is_some_and(|p| p.trim_start().starts_with('['));
        (role, strength == 2 && system && matches!(role, Role::Junk | Role::Trash))
    };
    let ((from, from_bin), (_, to_bin)) = (place(now_in), place(back_to));
    if from_bin {
        if to_bin {
            return Err(GMAIL_DELETES.into());
        }
        let inbox = store
            .role_mailbox_for_account(&now_in.account_id, &now_in.account, Role::Inbox)
            .ok_or_else(|| format!("Account '{}' has no inbox to bring it back through.", now_in.account))?;
        return Ok((Loc::from(&inbox), Some(GMAIL_UNDONE)));
    }
    if from == Role::Archive {
        return Err(GMAIL_ALL_MAIL.into());
    }
    if to_bin {
        return Ok((back_to.clone(), None));
    }
    Err(GMAIL_STAYS.into())
}

/// The moves to ask Mail for, worked out from Timbre's copy of Mail's
/// database, the moves refused and why, notes to pass on, and archives to
/// make first (with where Mail shows them).
struct Plan {
    items: Vec<Planned>,
    failed: Vec<Failure>,
    notes: Vec<&'static str>,
    make: Vec<(Loc, String, Option<bool>)>,
}

fn plan_moves(ctx: &Ctx, ids: &[i64], to: &str, account: Option<&str>) -> Plan {
    let mut plan = Plan { items: vec![], failed: vec![], notes: vec![], make: vec![] };
    for &id in ids {
        let r = source_of(ctx, id).and_then(|src| destination(ctx, &src.shown, to, account).map(|d| (src, d)));
        match r {
            Ok((src, dest)) if dest.mailbox.id == src.shown.id => plan.failed.push(Failure::of(id, "already there")),
            Ok((src, dest)) => match gmail_move(&ctx.store, &src.shown, &dest.mailbox) {
                Err(why) => plan.failed.push(Failure::of(id, why)),
                Ok(note) => {
                    if let Some(n) = note
                        && !plan.notes.contains(&n)
                    {
                        plan.notes.push(n);
                    }
                    let to = Loc::from(&dest.mailbox);
                    let prefixed = crate::mailboxes::prefixed(&ctx.store, &to);
                    if dest.create && !plan.make.iter().any(|(l, ..)| *l == to) {
                        plan.make.push((to.clone(), dest.mailbox.name.clone(), prefixed));
                    }
                    let mut v = src.json(id);
                    v["dest"] = json!(to);
                    v["dest_prefixed"] = json!(prefixed);
                    plan.items.push((id, v, Loc::from(&src.shown), to));
                }
            },
            Err(e) => plan.failed.push(Failure::of(id, e)),
        }
    }
    plan
}

pub async fn move_mail(ctx: &Arc<Ctx>, p: &MoveParams) -> Result<BatchResult, String> {
    let ids = unique(&p.ids);
    if ids.is_empty() || ids.len() > 200 {
        return Err("Give between 1 and 200 ids.".into());
    }
    let (to_spec, account) = (p.to.clone(), p.account.clone());
    let Plan { mut items, mut failed, mut notes, make } =
        blocking(ctx, move |ctx| plan_moves(ctx, &ids, &to_spec, account.as_deref())).await?;
    // An archive to make first, in its own call: Mail asks the server, which
    // can take a while, and nothing is moved unless it is there.
    for (loc, name, prefixed) in make {
        let find = |tries: u32| json!({"loc": loc, "prefixed": prefixed, "tries": tries});
        // Mail may have it already (made on the provider's website) before
        // its database, which Timbre reads, does.
        let has = mailapp::run(mailapp::FIND_MAILBOX, &find(1), FIND_TIMEOUT).await.is_ok();
        let made = if has {
            Ok(())
        } else {
            match mailapp::make_mailbox(&loc.account_id, &name, MAKE_TIMEOUT).await {
                Ok(()) => mailapp::run(mailapp::FIND_MAILBOX, &find(40), FIND_TIMEOUT).await.map(|_| ()),
                // Mail refuses some accounts (La Poste): made on the server
                // instead, when Timbre has the password already.
                Err(e) if !mailapp::no_answer(&e) => {
                    match crate::mailboxes::make_if_password_known(ctx, &loc, &name).await {
                        Ok(()) => mailapp::run(mailapp::FIND_MAILBOX, &find(40), FIND_TIMEOUT).await.map(|_| ()),
                        Err(_) => Err(e),
                    }
                }
                // Mail did not answer in time: it may still be making it.
                Err(e) => match mailapp::run(mailapp::FIND_MAILBOX, &find(20), FIND_TIMEOUT).await {
                    Ok(_) => Ok(()),
                    Err(_) => Err(e),
                },
            }
        };
        if let Err(e) = made {
            let why = format!(
                "Mail could not make an archive mailbox for '{}' ({}). Nothing was moved. Call create_mailbox \
                 with mailbox \"{}\" and account \"{}\" (the first time, a window on the Mac asks the user for \
                 the account's password, once), then archive again.",
                loc.account,
                e.to_string().trim_end_matches('.'),
                name,
                loc.account
            );
            items.retain(|(id, .., to)| {
                let keep = *to != loc;
                if !keep {
                    failed.push(Failure::of(*id, why.clone()));
                }
                keep
            });
        }
    }
    if items.is_empty() {
        return Ok(BatchResult { done: 0, failed, undo_token: None, note: None });
    }
    // The Message-IDs Timbre read, so each batch is journaled before Mail is
    // asked: if Mail stops half-way, what it moved can still be undone.
    let known = {
        let ids: Vec<i64> = items.iter().map(|(id, ..)| *id).collect();
        blocking(ctx, move |ctx| message_ids(ctx, &ids)).await?
    };
    let token = token();
    {
        let t = token.clone();
        journal(ctx, true, move |journal| {
            journal.push(UndoEntry {
                token: t,
                created: now_unix(),
                description: String::new(),
                items: vec![],
                undone: false,
                owner: std::process::id(),
            });
            let excess = journal.len().saturating_sub(50);
            journal.drain(..excess);
        })
        .await?;
    }
    let (mut done, mut unjournaled) = (0, 0);
    let mut stopped: Option<String> = None;
    let mut chunks = items.chunks(CHUNK);
    for chunk in chunks.by_ref() {
        let planned: Vec<UndoItem> = chunk
            .iter()
            .filter_map(|(id, _, from, to)| {
                known.get(id).map(|mid| UndoItem {
                    message_id: mid.clone(),
                    now_in: to.clone(),
                    back_to: from.clone(),
                    done: false,
                })
            })
            .collect();
        let t = token.clone();
        let before = journal(ctx, true, move |journal| match journal.iter_mut().find(|e| e.token == t) {
            Some(e) => {
                let before = e.items.len();
                e.items.extend(planned);
                before
            }
            None => 0,
        })
        .await?;
        let batch: Vec<_> = chunk.iter().map(|(_, v, _, _)| v.clone()).collect();
        let (tried, stop) = mailapp::run_batch(mailapp::MOVE, &json!({"items": batch}), timeout_for(batch.len())).await;
        let (n, keep, mut f, u) = settle(chunk, &tried, stop.as_ref(), &known);
        done += n;
        unjournaled += u;
        failed.append(&mut f);
        // This batch's planned moves give way to what Mail said of them;
        // earlier batches' stay as they are.
        let t = token.clone();
        journal(ctx, true, move |journal| {
            if let Some(e) = journal.iter_mut().find(|e| e.token == t) {
                e.items.truncate(before);
                e.items.extend(keep);
            }
        })
        .await?;
        if let Some(stop) = stop {
            stopped = Some(stop.text);
            break;
        }
    }
    if let Some(why) = &stopped {
        failed.extend(chunks.flatten().map(|(id, ..)| Failure::of(*id, format!("not tried: {why}"))));
    }
    // The entry stays only if it holds something to undo.
    let to = p.to.clone();
    let undo_token = journal(ctx, true, move |journal| {
        let pos = journal.iter().position(|e| e.token == token)?;
        if journal[pos].items.is_empty() {
            journal.remove(pos);
            return None;
        }
        journal[pos].description = format!("moved {} message(s) to {to}", journal[pos].items.len());
        Some(token)
    })
    .await?;
    if done == 0
        && undo_token.is_none()
        && let Some(e) = stopped
    {
        return Err(e);
    }
    if done == 0 {
        notes.clear();
    }
    if undo_token.is_some() {
        notes.push("To move them back, call undo_move with this undo_token.");
    }
    if unjournaled > 0 {
        notes.push(
            "Some moved messages have no Message-ID, which undo_move needs to find them: those stay where they are.",
        );
    }
    let note = (!notes.is_empty()).then(|| notes.join(" "));
    Ok(BatchResult { done, failed, undo_token, note })
}

type Planned = (i64, serde_json::Value, Loc, Loc);

/// What a batch of moves did, from what Mail said of each message: how
/// many moved, the moves to journal (those done, and those Mail may have
/// done before it stopped answering), and the failures.
fn settle(
    chunk: &[Planned],
    tried: &[serde_json::Value],
    stop: Option<&mailapp::Stop>,
    known: &HashMap<i64, String>,
) -> (usize, Vec<UndoItem>, Vec<Failure>, usize) {
    let (mut done, mut keep, mut failed, mut unjournaled) = (0, vec![], vec![], 0);
    for (pos, (id, _, from, to)) in chunk.iter().enumerate() {
        let reported = tried.iter().find(|t| t.get("id").and_then(|v| v.as_i64()) == Some(*id));
        let item = |mid: &str| UndoItem {
            message_id: mid.trim().trim_matches(['<', '>']).trim().to_string(),
            now_in: to.clone(),
            back_to: from.clone(),
            done: false,
        };
        match reported {
            Some(t) if t.get("ok").and_then(|v| v.as_bool()) == Some(true) => {
                done += 1;
                // A message without a Message-ID cannot be found again: its
                // move is not journaled (undo would take another one).
                let mid = t
                    .get("message_id")
                    .and_then(|v| v.as_str())
                    .filter(|m| !m.trim().trim_matches(['<', '>']).trim().is_empty())
                    .or(known.get(id).map(String::as_str));
                match mid {
                    Some(mid) => keep.push(item(mid)),
                    None => unjournaled += 1,
                }
            }
            Some(t) => failed.push(Failure::of(*id, t.get("error").and_then(|v| v.as_str()).unwrap_or("failed"))),
            None => {
                let maybe = stop.is_some_and(|s| s.unreported || (s.maybe_applied && pos == tried.len()));
                let why = stop.map_or("Mail stopped answering", |s| s.text.as_str());
                match known.get(id) {
                    Some(mid) if maybe => {
                        keep.push(item(mid));
                        failed.push(Failure::of(
                            *id,
                            format!("{why} It may have been moved: undo_move with the undo_token moves it back if so."),
                        ));
                    }
                    _ => failed.push(Failure::of(*id, why)),
                }
            }
        }
    }
    (done, keep, failed, unjournaled)
}

/// The undo journal as it is on disk (another Timbre may have changed it),
/// read, and changed and saved when `save`, under the lock both take.
async fn journal<T: Send + 'static>(
    ctx: &Arc<Ctx>,
    save: bool,
    f: impl FnOnce(&mut Vec<UndoEntry>) -> T + Send + 'static,
) -> Result<T, String> {
    blocking(ctx, move |ctx| with_journal(ctx, save, f)).await
}

fn with_journal<T>(ctx: &Ctx, save: bool, f: impl FnOnce(&mut Vec<UndoEntry>) -> T) -> T {
    let mut journal = ctx.undo.lock().unwrap_or_else(|e| e.into_inner());
    let _lock = lock_journal(&ctx.cfg);
    if let Some(on_disk) = read_undo(&ctx.cfg) {
        *journal = on_disk;
    }
    let out = f(&mut journal);
    if save {
        save_undo(&ctx.cfg, &journal);
    }
    out
}

/// The Message-IDs Timbre read in these messages (without angle brackets).
fn message_ids(ctx: &Ctx, ids: &[i64]) -> HashMap<i64, String> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    let conn = ctx.db();
    let Ok(mut stmt) = conn.prepare(&format!("SELECT id, msgid FROM docs WHERE msgid IS NOT NULL AND id IN ({list})"))
    else {
        return HashMap::new();
    };
    stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map(|rows| {
            rows.flatten()
                .map(|(id, m)| (id, m.trim().trim_matches(['<', '>']).trim().to_string()))
                .filter(|(_, m)| !m.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoParams {
    /// undo_token returned by move_mail; omit to undo the most recent move.
    pub token: Option<String>,
}

pub async fn undo(ctx: &Arc<Ctx>, p: &UndoParams) -> Result<BatchResult, String> {
    let token = p.token.clone();
    let entry = journal(ctx, false, move |journal| {
        let found = match &token {
            Some(t) => journal.iter().rev().find(|e| &e.token == t),
            // Without a token, only the latest move, never an older one the
            // user has forgotten about; this Timbre's, or one made before it
            // started, never another Claude window's.
            None => {
                let (me, started) = (std::process::id(), *STARTED.get_or_init(now_unix));
                journal.iter().rev().find(|e| e.owner == me || e.created < started)
            }
        };
        found.cloned()
    })
    .await?
    .ok_or("Nothing to undo with that token.")?;
    if entry.undone {
        return Err(if p.token.is_some() {
            "This move was already undone.".into()
        } else {
            "The latest move was already undone. To undo an earlier one, give its undo_token.".into()
        });
    }
    let todo: Vec<&UndoItem> = entry.items.iter().filter(|i| !i.done).collect();
    let mut back: Vec<String> = vec![];
    let mut failed = vec![];
    let mut notes: Vec<&str> = vec![];
    // Where each goes back, the way Mail can take it (Gmail).
    let mut routed = vec![];
    for i in &todo {
        if i.message_id.trim().is_empty() {
            failed.push(Failure { id: None, message_id: None, error: "no Message-ID to find it by".into() });
            continue;
        }
        match gmail_undo(&ctx.store, &i.now_in, &i.back_to) {
            Ok((to, note)) => {
                if let Some(n) = note
                    && !notes.contains(&n)
                {
                    notes.push(n);
                }
                let (now_prefixed, back_prefixed) =
                    (crate::mailboxes::prefixed(&ctx.store, &i.now_in), crate::mailboxes::prefixed(&ctx.store, &to));
                routed.push(json!({"message_id": i.message_id, "now_in": i.now_in, "now_prefixed": now_prefixed,
                                   "back_to": to, "back_prefixed": back_prefixed}));
            }
            Err(why) => failed.push(Failure { id: None, message_id: Some(i.message_id.clone()), error: why }),
        }
    }
    for items in routed.chunks(CHUNK) {
        let items = items.to_vec();
        let (tried, stop) =
            mailapp::run_batch(mailapp::MOVE_BACK, &json!({"items": items}), timeout_for(items.len() * 3)).await;
        for item in &tried {
            if item.get("ok").and_then(|v| v.as_bool()) == Some(true)
                && let Some(mid) = item.get("message_id").and_then(|v| v.as_str())
            {
                back.push(mid.to_string());
            }
        }
        failed.append(&mut collect_failures(&tried).1);
        if let Some(stop) = stop {
            if back.is_empty() && failed.is_empty() {
                return Err(stop.text);
            }
            failed.push(Failure { id: None, message_id: None, error: stop.text });
            break;
        }
    }
    let (t, moved_back) = (entry.token.clone(), back.clone());
    let complete = journal(ctx, true, move |journal| {
        let mut complete = false;
        if let Some(e) = journal.iter_mut().find(|e| e.token == t) {
            for mid in &moved_back {
                if let Some(item) = e.items.iter_mut().find(|i| !i.done && &i.message_id == mid) {
                    item.done = true;
                }
            }
            e.undone = e.items.iter().all(|i| i.done);
            complete = e.undone;
        }
        complete
    })
    .await?;
    // Those Mail cannot move back stay as they are: trying again changes nothing.
    let refused = todo.len() - routed.len();
    let retry = !complete && back.len() + refused < todo.len();
    let mut note = if complete {
        format!("Undid: {}", entry.description)
    } else if retry {
        format!(
            "Partly undone ({} of {}). Call undo_move again with undo_token {} to retry the others.",
            back.len(),
            todo.len(),
            entry.token
        )
    } else if back.is_empty() {
        "Nothing was moved back: see why in failed.".to_string()
    } else {
        format!("Partly undone ({} of {}): the others cannot be moved back, see why in failed.", back.len(), todo.len())
    };
    if !back.is_empty() {
        for n in notes {
            note.push(' ');
            note.push_str(n);
        }
    }
    let undo_token = retry.then(|| entry.token.clone());
    Ok(BatchResult { done: back.len(), failed, undo_token, note: Some(note) })
}

// ---------------------------------------------------------------- sending

#[derive(Debug, Serialize, JsonSchema)]
pub struct PreparedSend {
    pub token: String,
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    pub subject: String,
    /// Files that go out with it: a forward sends the original's on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<String>,
    pub expires_in_minutes: u32,
    pub next_step: String,
}

const SEND_TTL: i64 = 15 * 60;

pub async fn prepare_send(ctx: &Arc<Ctx>, p: &DraftParams) -> Result<PreparedSend, String> {
    let (r, attachments) = compose(ctx, p).await?;
    // Never a forward without the text the user agreed to.
    let text_left_out = p.mode.trim().eq_ignore_ascii_case("forward")
        && !p.body.trim().is_empty()
        && r.get("text_added").and_then(|v| v.as_bool()) == Some(false);
    if text_left_out {
        return Err("This forward has attachments, which Mail drops when Timbre sets its text: it cannot go out \
                    with your text. The draft is open in Mail: the user can type the text and send it, or call \
                    prepare_send again with an empty body to forward it as it is."
            .into());
    }
    let (to, cc, bcc) = (strings(&r, "to"), strings(&r, "cc"), strings(&r, "bcc"));
    let all: Vec<String> = to.iter().chain(&cc).chain(&bcc).map(|a| a.to_lowercase()).collect();
    if all.is_empty() {
        return Err("The message has no recipient.".into());
    }
    if let Some(bad) = all.iter().find(|a| !ctx.cfg.recipient_allowed(a)) {
        return Err(format!(
            "'{bad}' is not in the allowed recipients for sending. The draft stays open in Mail for the user to send manually."
        ));
    }
    let outgoing_id = r.get("outgoing_id").and_then(|v| v.as_i64()).ok_or("Mail did not return the draft id")?;
    let subject = r.get("subject").and_then(|s| s.as_str()).unwrap_or_default().to_string();
    let t = token();
    let mut st = ctx.sends.lock().unwrap_or_else(|e| e.into_inner());
    st.pending.retain(|_, v| now_unix() - v.created < SEND_TTL);
    st.pending
        .insert(t.clone(), PendingSend { outgoing_id, recipients: all, subject: subject.clone(), created: now_unix() });
    Ok(PreparedSend {
        token: t,
        to,
        cc,
        bcc,
        subject,
        next_step: format!(
            "Show the user the recipients, subject{} and text (also visible in the open Mail window). Only after \
             the user explicitly agrees, call confirm_send with this token.",
            if attachments.is_empty() { "" } else { ", the attached files" }
        ),
        attachments,
        expires_in_minutes: (SEND_TTL / 60) as u32,
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfirmParams {
    /// Token returned by prepare_send, after the user agreed to send.
    pub token: String,
    /// Every recipient (to, cc and bcc) exactly as returned by prepare_send,
    /// so the approval prompt shows who will receive the message.
    pub recipients: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SentResult {
    pub status: String,
    pub to: Vec<String>,
    pub subject: String,
}

pub async fn confirm_send(ctx: &Ctx, p: &ConfirmParams) -> Result<SentResult, String> {
    let pending = {
        let mut st = ctx.sends.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        while st.sent.front().is_some_and(|t| now - t > 3600) {
            st.sent.pop_front();
        }
        if st.sent.len() as u32 >= ctx.cfg.max_sends_per_hour {
            return Err(format!(
                "Sending limit reached ({} per hour). The draft stays open in Mail.",
                ctx.cfg.max_sends_per_hour
            ));
        }
        let expected = st.pending.get(&p.token).ok_or("Unknown or already used token: call prepare_send again.")?;
        let mut given: Vec<String> = p.recipients.iter().map(|a| a.trim().to_lowercase()).collect();
        let mut want = expected.recipients.clone();
        given.sort();
        given.dedup();
        want.sort();
        want.dedup();
        if given != want {
            return Err(format!(
                "The recipients do not match what was prepared ({}). Nothing was sent.",
                want.join(", ")
            ));
        }
        let pending = st.pending.remove(&p.token).ok_or("Unknown or already used token: call prepare_send again.")?;
        if now - pending.created > SEND_TTL {
            return Err("This token expired: call prepare_send again.".into());
        }
        // Counted with the check, before Mail answers: two sends at once
        // cannot both take the last place, and one that times out may
        // still have gone out.
        st.sent.push_back(now);
        pending
    };
    let r = mailapp::run(
        mailapp::SEND_OUTGOING,
        &json!({"outgoing_id": pending.outgoing_id, "allowed": pending.recipients, "subject": pending.subject}),
        Duration::from_secs(60),
    )
    .await;
    audit_send(&ctx.cfg, &pending, r.as_ref().err().map(|e| e.to_string()));
    let r = r.map_err(|e| e.to_string())?;
    Ok(SentResult { status: "sent".into(), to: strings(&r, "to"), subject: pending.subject })
}

fn audit_send(cfg: &Config, p: &PendingSend, error: Option<String>) {
    use std::io::Write;
    let line = json!({"at": now_unix(), "to": p.recipients, "subject": p.subject, "error": error});
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(cfg.data_dir.join("sent.log")) {
        let _ = writeln!(f, "{line}");
    }
}

// ---------------------------------------------------------------- misc

#[derive(Debug, Serialize, JsonSchema)]
pub struct Selected {
    pub id: i64,
    pub subject: String,
}

pub async fn selected(_ctx: &Ctx) -> Result<Vec<Selected>, String> {
    let r = mailapp::run(mailapp::SELECTION, &json!({}), Duration::from_secs(15)).await.map_err(|e| e.to_string())?;
    Ok(r.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            Some(Selected {
                id: v.get("id")?.as_i64()?,
                subject: v.get("subject").and_then(|s| s.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect())
}

pub fn open_in_mail(ctx: &Ctx, id: i64) -> Result<String, String> {
    let loc = {
        let conn = ctx.db();
        ctx.locator.locate_fast(&conn, &ctx.store, id)
    }
    .or_else(|| ctx.locator.locate_by_scan(&ctx.store, id))
    .ok_or_else(|| format!("The content of message {id} is not on this Mac."))?;
    let headers = crate::emlx::read_headers(&loc.path).map_err(|e| e.to_string())?;
    let mid =
        mime::parse(&headers).and_then(|p| p.message_id).ok_or("This message has no Message-ID to open it with.")?;
    mailapp::open_message_url(&mid).map_err(|e| e.to_string())?;
    Ok("Opened in Mail.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_are_journaled_even_when_mail_stops() {
        let loc =
            |p: &str| Loc { account_id: "A".into(), account: "iCloud".into(), path: vec![p.into()], local: false };
        let chunk: Vec<Planned> = (1..=4).map(|id| (id, json!({}), loc("INBOX"), loc("Archive"))).collect();
        let known: HashMap<i64, String> = (1..=4).map(|id| (id, format!("m{id}@x"))).collect();
        let ids = |keep: &[UndoItem]| keep.iter().map(|i| i.message_id.clone()).collect::<Vec<_>>();
        // All moved: Mail's own Message-IDs are kept.
        let tried: Vec<_> =
            (1..=4).map(|id| json!({"id": id, "ok": true, "message_id": format!("<M{id}@x>")})).collect();
        let (done, keep, failed, _) = settle(&chunk, &tried, None, &known);
        assert_eq!(
            (done, ids(&keep), failed.len()),
            (4, vec!["M1@x".into(), "M2@x".into(), "M3@x".into(), "M4@x".into()], 0)
        );
        // No Message-ID anywhere: moved, never journaled under an empty one
        // (undo would take any message without one), and said so.
        let bare: Vec<Planned> = vec![(9, json!({}), loc("INBOX"), loc("Archive"))];
        let tried = [json!({"id": 9, "ok": true, "message_id": " <> "})];
        let (done, keep, _, unjournaled) = settle(&bare, &tried, None, &HashMap::new());
        assert_eq!((done, keep.len(), unjournaled), (1, 0, 1));
        // Mail busy at the third: the first two moved, the third may have.
        let busy =
            mailapp::Stop { text: "Mail did not answer in time.".into(), maybe_applied: true, unreported: false };
        let tried =
            vec![json!({"id": 1, "ok": true, "message_id": "m1@x"}), json!({"id": 2, "ok": false, "error": "gone"})];
        let (done, keep, failed, _) = settle(&chunk, &tried, Some(&busy), &known);
        assert_eq!((done, ids(&keep)), (1, vec!["m1@x".to_string(), "m3@x".to_string()]));
        assert_eq!(failed.len(), 3);
        assert!(failed[1].error.contains("may have been moved"));
        // No answer at all: any of them may have moved.
        let silent =
            mailapp::Stop { text: "Mail did not answer within 23s.".into(), maybe_applied: true, unreported: true };
        let (done, keep, _, _) = settle(&chunk, &[], Some(&silent), &known);
        assert_eq!((done, keep.len()), (0, 4));
        // Permission refused: nothing moved, nothing to undo.
        let refused = mailapp::Stop { text: "not allowed".into(), maybe_applied: false, unreported: false };
        let (done, keep, failed, _) = settle(&chunk, &[], Some(&refused), &known);
        assert_eq!((done, keep.len(), failed.len()), (0, 0, 4));
    }

    #[test]
    fn quote_header_language() {
        assert!(quote_header("Bonjour, merci pour votre message", "d", "f").starts_with("Le d, f a écrit"));
        assert!(quote_header("Hola, gracias", "d", "f").starts_with("El d"));
        assert!(quote_header("Thanks!", "d", "f").starts_with("On d"));
    }

    #[test]
    fn addresses_validated() {
        assert_eq!(valid_addresses(&Some(vec!["Ann <ann@x.com>".into()])).unwrap(), vec!["ann@x.com"]);
        assert!(valid_addresses(&Some(vec!["not an address".into()])).is_err());
        // Several in one field, quoted names with commas: none is lost.
        assert_eq!(
            valid_addresses(&Some(vec!["Jean <jean@x.fr>, \"Doe, Ann\" <ann@y.fr>; bob@z.org".into()])).unwrap(),
            vec!["jean@x.fr", "ann@y.fr", "bob@z.org"]
        );
        // Nothing Mail could read as two addresses, or as something else.
        for bad in ["evil@x.com@corp.fr", "a b@x.fr", "ann@localhost", "<>@x.fr", "@x.fr", "ann@.fr", "a\"b@x.fr"] {
            assert!(valid_addresses(&Some(vec![bad.into()])).is_err(), "{bad}");
        }
    }

    #[test]
    fn colors() {
        assert_eq!(color_index("Red"), Some(0));
        assert_eq!(color_index("grey"), Some(6));
        assert_eq!(color_index("pink"), None);
    }
}
