//! Write operations: drafts, marking, moving with undo, and two-step sending.
//!
//! Nothing here deletes mail permanently. Moves are journaled so they can be
//! undone; sending is off unless the user enables it, and then always takes
//! two calls: `prepare_send` shows exactly what will go out, `confirm_send`
//! sends that and only that.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::app::Ctx;
use crate::config::Config;
use crate::mailapp::{self, Loc};
use crate::mime;
use crate::store::{Mailbox, Role};
use crate::util::{iso_local, now_unix};

// ---------------------------------------------------------------- journals

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoItem {
    pub message_id: String,
    pub now_in: Loc,
    pub back_to: Loc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub token: String,
    pub created: i64,
    pub description: String,
    pub items: Vec<UndoItem>,
    pub undone: bool,
}

fn undo_path(cfg: &Config) -> std::path::PathBuf {
    cfg.data_dir.join("undo.json")
}

pub fn load_undo(cfg: &Config) -> Vec<UndoEntry> {
    std::fs::read(undo_path(cfg)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_undo(cfg: &Config, entries: &[UndoEntry]) {
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    if let Ok(b) = serde_json::to_vec_pretty(entries) {
        let tmp = undo_path(cfg).with_extension("json.tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(tmp, undo_path(cfg));
        }
    }
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
    pub id: i64,
    pub error: String,
}

fn loc_of(ctx: &Ctx, id: i64) -> Result<(Loc, Mailbox), String> {
    let meta = ctx.meta(id)?;
    let mb = ctx.store.mailbox(meta.mailbox).ok_or_else(|| format!("message {id} is in an unknown mailbox"))?;
    Ok((Loc::from(&mb), mb))
}

fn timeout_for(n: usize) -> Duration {
    Duration::from_secs((15 + 2 * n as u64).min(180))
}

// ---------------------------------------------------------------- drafts

#[derive(Debug, Deserialize, JsonSchema)]
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    pub note: String,
}

fn valid_addresses(list: &Option<Vec<String>>) -> Result<Vec<String>, String> {
    let mut out = vec![];
    for a in list.iter().flatten() {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        let addr = a.rsplit_once('<').map(|(_, r)| r.trim_end_matches('>')).unwrap_or(a).trim();
        if !addr.contains('@') || addr.contains(char::is_whitespace) || addr.len() > 254 {
            return Err(format!("'{a}' is not an email address"));
        }
        out.push(addr.to_string());
    }
    Ok(out)
}

fn quote_header(body: &str, date: &str, from: &str) -> String {
    let b = format!(" {} ", body.to_lowercase());
    let fr = [" bonjour", " merci", " je ", " vous ", " nous ", " cordialement", " bien à"].iter().filter(|w| b.contains(*w)).count();
    let es = [" hola", " gracias", " usted", " saludos", " estimado", " nosotros"].iter().filter(|w| b.contains(*w)).count();
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

async fn compose(ctx: &Ctx, p: &DraftParams) -> Result<serde_json::Value, String> {
    let to = valid_addresses(&p.to)?;
    let cc = valid_addresses(&p.cc)?;
    let bcc = valid_addresses(&p.bcc)?;
    let mode = p.mode.trim().to_lowercase();
    let (script, input) = match mode.as_str() {
        "new" => {
            if to.is_empty() {
                return Err("A new message needs at least one recipient in 'to'.".into());
            }
            (mailapp::DRAFT_NEW, json!({"to": to, "cc": cc, "bcc": bcc, "subject": p.subject.clone().unwrap_or_default(), "body": p.body}))
        }
        "reply" | "reply_all" => {
            let id = p.id.ok_or("'id' of the message to reply to is required")?;
            let (loc, _) = loc_of(ctx, id)?;
            let body = reply_body(ctx, id, &p.body);
            (mailapp::DRAFT_REPLY, json!({"loc": loc, "id": id, "reply_all": mode == "reply_all", "cc": cc, "bcc": bcc, "body": body}))
        }
        "forward" => {
            let id = p.id.ok_or("'id' of the message to forward is required")?;
            if to.is_empty() {
                return Err("A forward needs at least one recipient in 'to'.".into());
            }
            let (loc, _) = loc_of(ctx, id)?;
            (mailapp::DRAFT_FORWARD, json!({"loc": loc, "id": id, "to": to, "cc": cc, "bcc": bcc, "body": p.body}))
        }
        other => return Err(format!("Unknown mode '{other}': use new, reply, reply_all or forward.")),
    };
    mailapp::run(script, &input, Duration::from_secs(40)).await.map_err(|e| e.to_string())
}

fn strings(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

pub async fn create_draft(ctx: &Ctx, p: &DraftParams) -> Result<DraftResult, String> {
    let r = compose(ctx, p).await?;
    Ok(DraftResult {
        status: "draft_open_in_mail".into(),
        subject: r.get("subject").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
        to: strings(&r, "to"),
        cc: strings(&r, "cc"),
        bcc: strings(&r, "bcc"),
        note: "The draft is open in Mail for the user to review. Nothing was sent.".into(),
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
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

fn collect_failures(r: &serde_json::Value) -> (usize, Vec<Failure>) {
    let mut ok = 0;
    let mut failed = vec![];
    for item in r.as_array().into_iter().flatten() {
        if item.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            ok += 1;
        } else {
            failed.push(Failure {
                id: item.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                error: item.get("error").and_then(|v| v.as_str()).unwrap_or("failed").to_string(),
            });
        }
    }
    (ok, failed)
}

pub async fn mark(ctx: &Ctx, p: &MarkParams) -> Result<BatchResult, String> {
    if p.ids.is_empty() || p.ids.len() > 500 {
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
    let mut items = vec![];
    let mut failed = vec![];
    for id in &p.ids {
        match loc_of(ctx, *id) {
            Ok((loc, _)) => items.push(json!({"id": id, "loc": loc})),
            Err(e) => failed.push(Failure { id: *id, error: e }),
        }
    }
    let r = mailapp::run(mailapp::MARK, &json!({"items": items, "action": action, "color": color}), timeout_for(items.len()))
        .await
        .map_err(|e| e.to_string())?;
    let (done, mut f2) = collect_failures(&r);
    failed.append(&mut f2);
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

fn destination(ctx: &Ctx, src: &Mailbox, to: &str, account: Option<&str>) -> Result<Mailbox, String> {
    if let Some(role) = Role::from_alias(to) {
        if account.is_none() {
            return ctx
                .store
                .role_mailbox_for_account(&src.account_id, &src.account, role)
                .ok_or_else(|| format!("Account '{}' has no {} mailbox.", src.account, role.as_str()));
        }
    }
    let ids = ctx.store.resolve_mailboxes(to, account);
    let all: Vec<Mailbox> = ids.iter().filter_map(|i| ctx.store.mailbox(*i)).collect();
    let same: Vec<&Mailbox> = all.iter().filter(|m| m.account == src.account).collect();
    match (same.len(), all.len()) {
        (1, _) => Ok(same[0].clone()),
        (0, 1) => Ok(all[0].clone()),
        (0, 0) => Err(format!("No mailbox named '{to}'. Call list_mailboxes.")),
        _ => Err(format!("'{to}' matches several mailboxes: give 'account' or the full path.")),
    }
}

pub async fn move_mail(ctx: &Ctx, p: &MoveParams) -> Result<BatchResult, String> {
    if p.ids.is_empty() || p.ids.len() > 200 {
        return Err("Give between 1 and 200 ids.".into());
    }
    let mut items = vec![];
    let mut plan: HashMap<i64, (Loc, Loc)> = HashMap::new();
    let mut failed = vec![];
    for id in &p.ids {
        let r = loc_of(ctx, *id).and_then(|(loc, mb)| destination(ctx, &mb, &p.to, p.account.as_deref()).map(|d| (loc, mb, d)));
        match r {
            Ok((_, mb, dest)) if dest.id == mb.id => failed.push(Failure { id: *id, error: "already there".into() }),
            Ok((loc, _, dest)) => {
                let d = Loc::from(&dest);
                items.push(json!({"id": id, "loc": loc, "dest": d}));
                plan.insert(*id, (loc, d));
            }
            Err(e) => failed.push(Failure { id: *id, error: e }),
        }
    }
    if items.is_empty() {
        return Ok(BatchResult { done: 0, failed, undo_token: None, note: None });
    }
    let r = mailapp::run(mailapp::MOVE, &json!({"items": items}), timeout_for(items.len())).await.map_err(|e| e.to_string())?;
    let mut undo_items = vec![];
    for item in r.as_array().into_iter().flatten() {
        if item.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        let id = item.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        if let (Some(mid), Some((from, to))) = (item.get("message_id").and_then(|v| v.as_str()), plan.get(&id)) {
            undo_items.push(UndoItem { message_id: mid.to_string(), now_in: to.clone(), back_to: from.clone() });
        }
    }
    let (done, mut f2) = collect_failures(&r);
    failed.append(&mut f2);
    let undo_token = if undo_items.is_empty() {
        None
    } else {
        let t = token();
        let mut journal = ctx.undo.lock().unwrap_or_else(|e| e.into_inner());
        journal.push(UndoEntry {
            token: t.clone(),
            created: now_unix(),
            description: format!("moved {} message(s) to {}", undo_items.len(), p.to),
            items: undo_items,
            undone: false,
        });
        let excess = journal.len().saturating_sub(50);
        journal.drain(..excess);
        save_undo(&ctx.cfg, &journal);
        Some(t)
    };
    Ok(BatchResult { done, failed, undo_token, note: Some("Use undo with undo_token to move them back.".into()) })
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoParams {
    /// undo_token returned by move_mail; omit to undo the most recent move.
    pub token: Option<String>,
}

pub async fn undo(ctx: &Ctx, p: &UndoParams) -> Result<BatchResult, String> {
    let entry = {
        let journal = ctx.undo.lock().unwrap_or_else(|e| e.into_inner());
        let found = match &p.token {
            Some(t) => journal.iter().rev().find(|e| &e.token == t),
            None => journal.iter().rev().find(|e| !e.undone),
        };
        found.cloned().ok_or("Nothing to undo with that token.")?
    };
    if entry.undone {
        return Err("This move was already undone.".into());
    }
    let items: Vec<_> = entry.items.iter().map(|i| json!({"message_id": i.message_id, "now_in": i.now_in, "back_to": i.back_to})).collect();
    let r = mailapp::run(mailapp::MOVE_BACK, &json!({"items": items}), timeout_for(items.len() * 3)).await.map_err(|e| e.to_string())?;
    let (done, failed) = collect_failures(&r);
    {
        let mut journal = ctx.undo.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = journal.iter_mut().find(|e| e.token == entry.token) {
            e.undone = true;
        }
        save_undo(&ctx.cfg, &journal);
    }
    Ok(BatchResult { done, failed, undo_token: None, note: Some(format!("Undid: {}", entry.description)) })
}

// ---------------------------------------------------------------- sending

#[derive(Debug, Serialize, JsonSchema)]
pub struct PreparedSend {
    pub token: String,
    pub to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<String>,
    pub subject: String,
    pub expires_in_minutes: u32,
    pub next_step: String,
}

const SEND_TTL: i64 = 15 * 60;

pub async fn prepare_send(ctx: &Ctx, p: &DraftParams) -> Result<PreparedSend, String> {
    let r = compose(ctx, p).await?;
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
    st.pending.insert(t.clone(), PendingSend { outgoing_id, recipients: all, subject: subject.clone(), created: now_unix() });
    Ok(PreparedSend {
        token: t,
        to,
        cc,
        bcc,
        subject,
        expires_in_minutes: (SEND_TTL / 60) as u32,
        next_step: "Show the user the recipients, subject and text (also visible in the open Mail window). \
                    Only after the user explicitly agrees, call confirm_send with this token."
            .into(),
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
            return Err(format!("Sending limit reached ({} per hour). The draft stays open in Mail.", ctx.cfg.max_sends_per_hour));
        }
        let expected = st.pending.get(&p.token).ok_or("Unknown or already used token: call prepare_send again.")?;
        let mut given: Vec<String> = p.recipients.iter().map(|a| a.trim().to_lowercase()).collect();
        let mut want = expected.recipients.clone();
        given.sort();
        given.dedup();
        want.sort();
        want.dedup();
        if given != want {
            return Err(format!("The recipients do not match what was prepared ({}). Nothing was sent.", want.join(", ")));
        }
        let pending = st.pending.remove(&p.token).ok_or("Unknown or already used token: call prepare_send again.")?;
        if now - pending.created > SEND_TTL {
            return Err("This token expired: call prepare_send again.".into());
        }
        pending
    };
    let r = mailapp::run(
        mailapp::SEND_OUTGOING,
        &json!({"outgoing_id": pending.outgoing_id, "allowed": pending.recipients}),
        Duration::from_secs(60),
    )
    .await
    .map_err(|e| e.to_string())?;
    {
        let mut st = ctx.sends.lock().unwrap_or_else(|e| e.into_inner());
        st.sent.push_back(now_unix());
    }
    audit_send(&ctx.cfg, &pending);
    Ok(SentResult { status: "sent".into(), to: strings(&r, "to"), subject: pending.subject })
}

fn audit_send(cfg: &Config, p: &PendingSend) {
    use std::io::Write;
    let line = json!({"at": now_unix(), "to": p.recipients, "subject": p.subject});
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
            Some(Selected { id: v.get("id")?.as_i64()?, subject: v.get("subject").and_then(|s| s.as_str()).unwrap_or("").to_string() })
        })
        .collect())
}

pub fn open_in_mail(ctx: &Ctx, id: i64) -> Result<String, String> {
    let loc = {
        let conn = ctx.db();
        ctx.locator.locate(&conn, &ctx.store, id)
    }
    .ok_or_else(|| format!("The content of message {id} is not on this Mac."))?;
    let headers = crate::emlx::read_headers(&loc.path).map_err(|e| e.to_string())?;
    let mid = mime::parse(&headers).and_then(|p| p.message_id).ok_or("This message has no Message-ID to open it with.")?;
    mailapp::open_message_url(&mid).map_err(|e| e.to_string())?;
    Ok("Opened in Mail.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn colors() {
        assert_eq!(color_index("Red"), Some(0));
        assert_eq!(color_index("grey"), Some(6));
        assert_eq!(color_index("pink"), None);
    }
}
