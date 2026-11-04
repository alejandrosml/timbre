//! Read-only access to Apple Mail's own metadata database, the
//! "Envelope Index" (`~/Library/Mail/V*/MailData/Envelope Index`).
//!
//! The database is attached to our own SQLite connection as `mail`, always
//! read-only, so a single query can join Mail's live metadata (mailbox,
//! read/flag state, dates) with Timbre's full-text index. The schema varies
//! across macOS releases, so column usage is decided by introspection with
//! documented fallbacks on the `flags` bitfield.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result, anyhow, bail};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};
use serde::Serialize;

use crate::config::Config;
use crate::util::{MAC_EPOCH_OFFSET, fmt_addr, percent_decode, sqlite_uri};

// Bits of `messages.flags` (stable since OS X 10.x; used when a dedicated
// column is missing). Colour bits 39-41 hold Mail's 0-6 `flagIndex`.
const FLAG_READ: i64 = 1 << 0;
const FLAG_DELETED: i64 = 1 << 1;
const FLAG_FLAGGED: i64 = 1 << 4;
const FLAG_ATTACH_SHIFT: i64 = 10;
const FLAG_ATTACH_MASK: i64 = 0x3F;
const FLAG_COLOR_SHIFT: i64 = 39;
const FLAG_COLOR_MASK: i64 = 0x7 << FLAG_COLOR_SHIFT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenMode {
    /// Normal read-only open; sees Mail's latest writes through its WAL.
    ReadOnly,
    /// Mail's shared-memory file was unavailable (usually Mail is closed):
    /// opened as immutable, which never writes but may miss the newest rows.
    Immutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    Other,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Inbox => "inbox",
            Role::Sent => "sent",
            Role::Drafts => "drafts",
            Role::Trash => "trash",
            Role::Junk => "junk",
            Role::Archive => "archive",
            Role::Other => "other",
        }
    }

    pub fn from_alias(s: &str) -> Option<Role> {
        match s.trim().to_lowercase().as_str() {
            "inbox" | "boîte de réception" | "reception" | "réception" | "bandeja de entrada" | "entrada" => {
                Some(Role::Inbox)
            }
            "sent" | "envoyés" | "envoyes" | "enviados" => Some(Role::Sent),
            "drafts" | "draft" | "brouillons" | "borradores" => Some(Role::Drafts),
            "trash" | "bin" | "corbeille" | "papelera" => Some(Role::Trash),
            "junk" | "spam" | "indésirables" | "indesirables" | "no deseado" => Some(Role::Junk),
            "archive" | "archives" | "archivo" | "archivados" => Some(Role::Archive),
            _ => None,
        }
    }

    /// Guess a mailbox's role from its (possibly localized) name.
    pub fn detect(path: &[String]) -> Role {
        let Some(last) = path.last() else { return Role::Other };
        let n = last.to_lowercase();
        let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let has = |p: &str| n.contains(p);
        let word = |w: &str| words.contains(&w);
        if path.len() == 1 && n == "inbox" || n == "boîte de réception" || n == "bandeja de entrada" {
            Role::Inbox
        } else if has("draft") || has("brouillon") || has("borrador") {
            Role::Drafts
        } else if has("junk")
            || has("spam")
            || has("indésirable")
            || has("indesirable")
            || has("no deseado")
            || has("quarantaine")
            || has("quarantine")
            || word("bulk")
        {
            Role::Junk
        } else if has("trash")
            || has("deleted")
            || has("corbeille")
            || has("papelera")
            || has("supprimé")
            || has("eliminad")
            || word("bin")
        {
            Role::Trash
        } else if has("sent") || has("envoy") || has("enviad") || word("outbox") {
            // Orange and La Poste name the sent mailbox OUTBOX.
            Role::Sent
        } else if has("archiv") || n == "all mail" || n == "tous les messages" || n == "todos" {
            Role::Archive
        } else {
            Role::Other
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Mailbox {
    pub id: i64,
    pub url: String,
    /// Account UUID from the mailbox URL (empty for "On My Mac").
    pub account_id: String,
    /// Human account name ("iCloud", "Gmail", ...).
    pub account: String,
    /// Path segments, e.g. `["Archive", "2024"]`.
    pub path: Vec<String>,
    pub name: String,
    pub role: Role,
    /// "On My Mac" mailbox (not under an account).
    pub local: bool,
}

impl Mailbox {
    pub fn display_path(&self) -> String {
        self.path.join("/")
    }
}

/// SQL fragments adapted to the detected schema. All use the alias `m`
/// for `mail.messages`.
#[derive(Debug, Clone)]
pub struct Schema {
    pub unread: String,
    pub flagged: String,
    pub not_deleted: String,
    pub attach_count: String,
    pub flag_color: String,
    pub date_sent: String,
    pub subject_prefix: String,
    pub size: String,
    pub conversation: String,
    pub rec_msg_col: Option<String>,
    pub rec_type_col: Option<String>,
    /// Gmail keeps each message once, in All Mail, and lists its other
    /// mailboxes (Inbox, Sent, labels) in the `labels` table: the messages
    /// column that `labels.message_id` refers to, when the table exists.
    pub labels: Option<String>,
}

pub struct Store {
    pub version_dir: PathBuf,
    pub db_path: PathBuf,
    pub schema: Schema,
    pub epoch_offset: i64,
    pub open_mode: OpenMode,
    accounts: HashMap<String, String>,
    /// The user's addresses known from the account settings (lower case).
    pub account_addresses: Vec<String>,
    mailboxes: RwLock<Arc<HashMap<i64, Mailbox>>>,
}

/// One message's metadata, straight from the Envelope Index.
#[derive(Debug, Clone)]
#[allow(dead_code)] // every column is decoded; not all are shown yet
pub struct Meta {
    pub id: i64,
    pub subject: String,
    pub from: String,
    pub from_addr: String,
    pub date: i64,
    pub date_sent: i64,
    pub unread: bool,
    pub flagged: bool,
    pub flag_color: i64,
    pub attachments: i64,
    pub mailbox: i64,
    pub size: i64,
    pub conversation: i64,
}

/// Locate `~/Library/Mail/V<n>` holding `MailData/Envelope Index`.
pub fn locate_version_dir(mail_root: &Path) -> Result<PathBuf> {
    let entries = std::fs::read_dir(mail_root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow!(
                "Cannot read {} — Timbre needs Full Disk Access (System Settings › Privacy & Security › Full Disk Access). \
                 Add this binary: {}",
                mail_root.display(),
                std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
            )
        } else {
            anyhow!("Cannot read {}: {e}", mail_root.display())
        }
    })?;
    let mut best: Option<(u32, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(num) = name.strip_prefix('V').and_then(|n| n.parse::<u32>().ok()) {
            let p = entry.path();
            if p.join("MailData").join("Envelope Index").exists() && best.as_ref().is_none_or(|(b, _)| num > *b) {
                best = Some((num, p));
            }
        }
    }
    best.map(|(_, p)| p).ok_or_else(|| anyhow!("No V*/MailData/Envelope Index under {}", mail_root.display()))
}

/// Attach the Envelope Index to `conn` as `mail`, read-only.
pub fn attach(conn: &Connection, db_path: &Path) -> Result<OpenMode> {
    let probe = |c: &Connection| -> rusqlite::Result<i64> {
        c.query_row("SELECT count(*) FROM mail.sqlite_master", [], |r| r.get(0))
    };
    let uri = sqlite_uri(db_path, "mode=ro");
    if conn.execute("ATTACH DATABASE ?1 AS mail", [&uri]).is_ok() && probe(conn).is_ok() {
        return Ok(OpenMode::ReadOnly);
    }
    let _ = conn.execute_batch("DETACH DATABASE mail");
    let uri = sqlite_uri(db_path, "mode=ro&immutable=1");
    conn.execute("ATTACH DATABASE ?1 AS mail", [&uri]).with_context(|| format!("attaching {}", db_path.display()))?;
    probe(conn).with_context(|| format!("reading {}", db_path.display()))?;
    Ok(OpenMode::Immutable)
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA mail.table_info({table})"))?;
    let cols = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cols)
}

fn detect_schema(conn: &Connection) -> Result<Schema> {
    let m = table_columns(conn, "messages")?;
    if m.is_empty() {
        bail!("Envelope Index has no messages table — unsupported Mail version");
    }
    for req in ["subject", "sender", "mailbox", "date_received"] {
        if !m.iter().any(|c| c == req) {
            bail!("Envelope Index schema not recognised: messages.{req} is missing (columns: {})", m.join(", "));
        }
    }
    let has = |c: &str| m.iter().any(|x| x == c);
    let flags = has("flags");
    let r = table_columns(conn, "recipients").unwrap_or_default();
    let rhas = |c: &str| r.iter().any(|x| x == c);
    Ok(Schema {
        unread: if has("read") {
            "(m.read = 0)".into()
        } else if flags {
            format!("((m.flags & {FLAG_READ}) = 0)")
        } else {
            "0".into()
        },
        flagged: if has("flagged") {
            "(m.flagged != 0)".into()
        } else if flags {
            format!("((m.flags & {FLAG_FLAGGED}) != 0)")
        } else {
            "0".into()
        },
        not_deleted: if has("deleted") {
            "(m.deleted = 0)".into()
        } else if flags {
            format!("((m.flags & {FLAG_DELETED}) = 0)")
        } else {
            "1".into()
        },
        attach_count: if flags {
            format!("((m.flags >> {FLAG_ATTACH_SHIFT}) & {FLAG_ATTACH_MASK})")
        } else {
            "0".into()
        },
        flag_color: if flags { format!("((m.flags & {FLAG_COLOR_MASK}) >> {FLAG_COLOR_SHIFT})") } else { "-1".into() },
        date_sent: if has("date_sent") { "m.date_sent".into() } else { "m.date_received".into() },
        subject_prefix: if has("subject_prefix") { "COALESCE(m.subject_prefix, '')".into() } else { "''".into() },
        size: if has("size") { "COALESCE(m.size, 0)".into() } else { "0".into() },
        conversation: if has("conversation_id") { "COALESCE(m.conversation_id, 0)".into() } else { "0".into() },
        rec_msg_col: if rhas("message") {
            Some("message".into())
        } else if rhas("message_id") {
            Some("message_id".into())
        } else {
            None
        },
        rec_type_col: if rhas("type") { Some("type".into()) } else { None },
        labels: {
            let l = table_columns(conn, "labels").unwrap_or_default();
            (l.iter().any(|c| c == "message_id") && l.iter().any(|c| c == "mailbox_id"))
                .then(|| labels_join(conn, has("message_id")))
        },
    })
}

/// The messages column that `labels.message_id` refers to. Mail declares
/// `REFERENCES messages(ROWID)`; without that declaration, the column that
/// matches more of a sample of labels wins.
fn labels_join(conn: &Connection, messages_have_message_id: bool) -> String {
    let sql: String = conn
        .query_row("SELECT sql FROM mail.sqlite_master WHERE type = 'table' AND name = 'labels'", [], |r| r.get(0))
        .unwrap_or_default();
    let compact: String = sql.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains("referencesmessages(rowid)") || !messages_have_message_id {
        return "m.ROWID".into();
    }
    let hits = |col: &str| {
        conn.query_row(
            &format!(
                "SELECT count(*) FROM (SELECT message_id FROM mail.labels LIMIT 200) l \
                 JOIN mail.messages m ON {col} = l.message_id"
            ),
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    };
    if hits("m.message_id") > hits("m.ROWID") { "m.message_id".into() } else { "m.ROWID".into() }
}

/// Addresses of the accounts set up on this Mac, from the account settings.
fn load_account_addresses(path: &Path) -> Vec<String> {
    let Ok(conn) =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
    else {
        return vec![];
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT DISTINCT lower(trim(ZUSERNAME)) FROM ZACCOUNT WHERE ZUSERNAME LIKE '%_@_%'")
    else {
        return vec![];
    };
    stmt.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

fn load_accounts(path: &Path) -> HashMap<String, String> {
    let Ok(conn) =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
    else {
        return HashMap::new();
    };
    let sql = "SELECT a.ZIDENTIFIER,
                      COALESCE(NULLIF(p.ZACCOUNTDESCRIPTION, ''), NULLIF(a.ZACCOUNTDESCRIPTION, ''),
                               NULLIF(p.ZUSERNAME, ''), NULLIF(a.ZUSERNAME, ''))
               FROM ZACCOUNT a LEFT JOIN ZACCOUNT p ON a.ZPARENTACCOUNT = p.Z_PK
               WHERE a.ZIDENTIFIER IS NOT NULL";
    let mut out = HashMap::new();
    if let Ok(mut stmt) = conn.prepare(sql)
        && let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))
    {
        for (id, name) in rows.flatten() {
            if let (Some(id), Some(name)) = (id, name) {
                out.insert(id.to_uppercase(), name);
            }
        }
    }
    out
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Parse a mailbox URL such as `imap://<UUID>/INBOX`, `ews://<UUID>/Inbox`
/// or `local:///Drafts`.
pub fn parse_mailbox_url(id: i64, url: &str, accounts: &HashMap<String, String>) -> Mailbox {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let (netloc, path) = rest.split_once('/').unwrap_or((rest, ""));
    let segments: Vec<String> = path.split('/').filter(|s| !s.is_empty()).map(percent_decode).collect();
    let local = scheme == "local" || netloc.is_empty();
    let account_id = if looks_like_uuid(netloc) { netloc.to_uppercase() } else { String::new() };
    let account = if local {
        "On My Mac".to_string()
    } else if !account_id.is_empty() {
        accounts.get(&account_id).cloned().unwrap_or_else(|| account_id.clone())
    } else {
        // Older form: user%40host@server
        let user = netloc.rsplit_once('@').map(|(u, _)| u).unwrap_or(netloc);
        percent_decode(user)
    };
    let name = segments.last().cloned().unwrap_or_else(|| url.to_string());
    let role = Role::detect(&segments);
    Mailbox { id, url: url.to_string(), account_id, account, path: segments, name, role, local }
}

impl Store {
    /// Open Mail's store: locate it, attach it to a scratch connection to
    /// learn the schema, and load accounts and mailboxes.
    pub fn open(cfg: &Config) -> Result<Store> {
        let version_dir = locate_version_dir(&cfg.mail_root)?;
        let db_path = version_dir.join("MailData").join("Envelope Index");
        let conn = Connection::open_in_memory()?;
        let open_mode = attach(&conn, &db_path)?;
        let schema = detect_schema(&conn)?;
        let max_date: Option<f64> = conn.query_row("SELECT MAX(date_received) FROM mail.messages", [], |r| r.get(0))?;
        let epoch_offset = if max_date.unwrap_or(0.0) > 1.0e9 { 0 } else { MAC_EPOCH_OFFSET };
        let accounts = load_accounts(&cfg.accounts_db);
        let account_addresses = load_account_addresses(&cfg.accounts_db);
        let store = Store {
            version_dir,
            db_path,
            schema,
            epoch_offset,
            open_mode,
            accounts,
            account_addresses,
            mailboxes: RwLock::new(Arc::new(HashMap::new())),
        };
        store.reload_mailboxes(&conn)?;
        Ok(store)
    }

    pub fn reload_mailboxes(&self, conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("SELECT ROWID, url FROM mail.mailboxes")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))?;
        let mut map = HashMap::new();
        for (id, url) in rows.flatten() {
            map.insert(id, parse_mailbox_url(id, url.as_deref().unwrap_or(""), &self.accounts));
        }
        *self.mailboxes.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(map);
        Ok(())
    }

    pub fn mailboxes(&self) -> Arc<HashMap<i64, Mailbox>> {
        self.mailboxes.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn mailbox(&self, id: i64) -> Option<Mailbox> {
        self.mailboxes().get(&id).cloned()
    }

    pub fn account_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.mailboxes().values().map(|m| m.account.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Convert a stored date to Unix seconds.
    pub fn to_unix(&self, v: i64) -> i64 {
        if v == 0 { 0 } else { v + self.epoch_offset }
    }

    /// Convert Unix seconds to the stored representation.
    pub fn to_db(&self, unix: i64) -> i64 {
        unix - self.epoch_offset
    }

    /// Column list for [`Store::row_meta`], alias `m` for messages, `s` for
    /// subjects, `a` for the sender address.
    pub fn meta_columns(&self) -> String {
        let s = &self.schema;
        format!(
            "m.ROWID, {prefix} || COALESCE(s.subject, ''), COALESCE(a.comment, ''), COALESCE(a.address, ''), \
             COALESCE(m.date_received, 0), COALESCE({sent}, 0), {unread}, {flagged}, {color}, {attach}, \
             m.mailbox, {size}, {conv}",
            prefix = s.subject_prefix,
            sent = s.date_sent,
            unread = s.unread,
            flagged = s.flagged,
            color = s.flag_color,
            attach = s.attach_count,
            size = s.size,
            conv = s.conversation,
        )
    }

    pub const META_JOINS: &'static str =
        "LEFT JOIN mail.subjects s ON s.ROWID = m.subject LEFT JOIN mail.addresses a ON a.ROWID = m.sender";

    /// Read a [`Meta`] from the columns produced by [`Store::meta_columns`],
    /// starting at `at`.
    pub fn row_meta(&self, row: &Row<'_>, at: usize) -> rusqlite::Result<Meta> {
        let name: String = row.get(at + 2)?;
        let addr: String = row.get(at + 3)?;
        let date: f64 = row.get::<_, Option<f64>>(at + 4)?.unwrap_or(0.0);
        let sent: f64 = row.get::<_, Option<f64>>(at + 5)?.unwrap_or(0.0);
        Ok(Meta {
            id: row.get(at)?,
            subject: row.get(at + 1)?,
            from: fmt_addr(Some(&name), Some(&addr)),
            from_addr: addr,
            date: self.to_unix(date as i64),
            date_sent: self.to_unix(sent as i64),
            unread: row.get::<_, i64>(at + 6)? != 0,
            flagged: row.get::<_, i64>(at + 7)? != 0,
            flag_color: row.get(at + 8)?,
            attachments: row.get(at + 9)?,
            mailbox: row.get(at + 10)?,
            size: row.get(at + 11)?,
            conversation: row.get(at + 12)?,
        })
    }

    pub fn get_meta(&self, conn: &Connection, id: i64) -> Result<Option<Meta>> {
        let sql =
            format!("SELECT {} FROM mail.messages m {} WHERE m.ROWID = ?1", self.meta_columns(), Self::META_JOINS);
        Ok(conn.query_row(&sql, [id], |r| self.row_meta(r, 0)).optional()?)
    }

    pub fn get_metas(&self, conn: &Connection, ids: &[i64]) -> Result<Vec<Meta>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let list = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {} FROM mail.messages m {} WHERE m.ROWID IN ({list})",
            self.meta_columns(),
            Self::META_JOINS
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| self.row_meta(r, 0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut by_id: HashMap<i64, Meta> = rows.into_iter().map(|m| (m.id, m)).collect();
        Ok(ids.iter().filter_map(|i| by_id.remove(i)).collect())
    }

    /// Recipients (to, cc) from the database, used when no `.emlx` is at hand.
    pub fn recipients(&self, conn: &Connection, id: i64) -> Result<(Vec<String>, Vec<String>)> {
        let Some(col) = &self.schema.rec_msg_col else { return Ok((vec![], vec![])) };
        let type_col = self.schema.rec_type_col.as_deref().map(|t| format!("r.{t}")).unwrap_or_else(|| "0".into());
        let sql = format!(
            "SELECT {type_col}, COALESCE(ra.comment, ''), COALESCE(ra.address, '')
             FROM mail.recipients r LEFT JOIN mail.addresses ra ON ra.ROWID = r.address
             WHERE r.{col} = ?1 ORDER BY r.ROWID"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut to = vec![];
        let mut cc = vec![];
        for row in
            stmt.query_map([id], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        {
            let (t, name, addr) = row?;
            if addr.is_empty() {
                continue;
            }
            let s = fmt_addr(Some(&name), Some(&addr));
            match t {
                Some(1) => cc.push(s),
                Some(0) | None => to.push(s),
                _ => {}
            }
        }
        Ok((to, cc))
    }

    /// Messages of the same Mail conversation, oldest first.
    pub fn conversation_ids(&self, conn: &Connection, id: i64, limit: usize) -> Result<Vec<i64>> {
        if self.schema.conversation == "0" {
            return Ok(vec![id]);
        }
        let conv: Option<i64> = conn
            .query_row(
                &format!("SELECT {} FROM mail.messages m WHERE m.ROWID = ?1", self.schema.conversation),
                [id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(conv) = conv.filter(|c| *c != 0) else { return Ok(vec![id]) };
        let sql = format!(
            "SELECT m.ROWID FROM mail.messages m WHERE {} = ?1 AND {} ORDER BY m.date_received ASC LIMIT ?2",
            self.schema.conversation, self.schema.not_deleted
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids = stmt
            .query_map(rusqlite::params![conv, limit as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(if ids.is_empty() { vec![id] } else { ids })
    }

    /// SQL condition on a message (alias `m`): it is stored in one of `ids`
    /// or, for Gmail, carries one of them as a label.
    pub fn in_mailboxes(&self, ids: &[i64]) -> String {
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        match &self.schema.labels {
            Some(col) => format!(
                "(m.mailbox IN ({list}) OR EXISTS (SELECT 1 FROM mail.labels l \
                 WHERE l.message_id = {col} AND l.mailbox_id IN ({list})))"
            ),
            None => format!("m.mailbox IN ({list})"),
        }
    }

    /// The mailbox to show for a message: for Gmail, its inbox or most
    /// telling label rather than All Mail.
    pub fn display_mailbox(&self, conn: &Connection, id: i64, stored: i64) -> i64 {
        let Some(col) = &self.schema.labels else { return stored };
        let sql = format!(
            "SELECT l.mailbox_id FROM mail.labels l JOIN mail.messages m ON {col} = l.message_id WHERE m.ROWID = ?1"
        );
        let Ok(mut stmt) = conn.prepare_cached(&sql) else { return stored };
        let Ok(labels) = stmt.query_map([id], |r| r.get::<_, i64>(0)).map(|rows| rows.flatten().collect::<Vec<_>>())
        else {
            return stored;
        };
        let mailboxes = self.mailboxes();
        let rank = |mb: i64| match mailboxes.get(&mb) {
            Some(m) => match m.role {
                Role::Inbox => 0,
                Role::Sent => 1,
                // The user's own labels, not Gmail's Important or Starred.
                Role::Other if !m.path.first().is_some_and(|p| p.starts_with('[')) => 2,
                Role::Drafts => 3,
                Role::Junk | Role::Trash => 4,
                _ => 5,
            },
            None => 6,
        };
        labels.into_iter().chain([stored]).min_by_key(|&mb| (rank(mb), mb != stored)).unwrap_or(stored)
    }

    /// (total, unread) per mailbox id, Gmail labels included.
    pub fn mailbox_counts(&self, conn: &Connection) -> Result<HashMap<i64, (i64, i64)>> {
        let (unread, live) = (&self.schema.unread, &self.schema.not_deleted);
        let sql = match &self.schema.labels {
            Some(col) => format!(
                "SELECT mb, COUNT(*), SUM(u) FROM (
                    SELECT m.mailbox AS mb, CASE WHEN {unread} THEN 1 ELSE 0 END AS u FROM mail.messages m WHERE {live}
                    UNION ALL
                    SELECT l.mailbox_id, CASE WHEN {unread} THEN 1 ELSE 0 END FROM mail.labels l
                    JOIN mail.messages m ON {col} = l.message_id WHERE {live} AND l.mailbox_id != m.mailbox
                 ) GROUP BY mb"
            ),
            None => format!(
                "SELECT m.mailbox, COUNT(*), SUM(CASE WHEN {unread} THEN 1 ELSE 0 END) FROM mail.messages m \
                 WHERE {live} GROUP BY m.mailbox"
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?.unwrap_or(0)))
        })?;
        Ok(rows.flatten().map(|(id, t, u)| (id, (t, u))).collect())
    }

    /// Resolve a mailbox name, path or role alias to mailbox ids.
    /// Matching is case-insensitive on the full path or the leaf name.
    pub fn resolve_mailboxes(&self, spec: &str, account: Option<&str>) -> Vec<i64> {
        let spec = spec.trim();
        let role = Role::from_alias(spec);
        let lower = spec.to_lowercase();
        let acct = account.map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty());
        let mut out: Vec<i64> = self
            .mailboxes()
            .values()
            .filter(|m| {
                acct.as_ref()
                    .is_none_or(|a| m.account.to_lowercase().contains(a.as_str()) || m.account_id.to_lowercase() == *a)
            })
            .filter(|m| {
                role.is_some_and(|r| m.role == r)
                    || m.display_path().to_lowercase() == lower
                    || m.name.to_lowercase() == lower
            })
            .map(|m| m.id)
            .collect();
        out.sort_unstable();
        out
    }

    /// Mailbox ids whose role is in `roles` (e.g. junk, trash).
    pub fn mailboxes_with_roles(&self, roles: &[String]) -> Vec<i64> {
        self.mailboxes().values().filter(|m| roles.iter().any(|r| r == m.role.as_str())).map(|m| m.id).collect()
    }

    /// Accounts' mailbox with a given role (first match), e.g. Trash of an account.
    pub fn role_mailbox_for_account(&self, account_id: &str, account: &str, role: Role) -> Option<Mailbox> {
        let mut candidates: Vec<Mailbox> = self
            .mailboxes()
            .values()
            .filter(|m| {
                m.role == role && ((!account_id.is_empty() && m.account_id == account_id) || m.account == account)
            })
            .cloned()
            .collect();
        candidates.sort_by_key(|m| m.path.len());
        candidates.into_iter().next()
    }

    /// Live message ids (not deleted) with their mailbox.
    pub fn live_messages(&self, conn: &Connection) -> Result<Vec<(i64, i64)>> {
        let sql = format!("SELECT m.ROWID, m.mailbox FROM mail.messages m WHERE {}", self.schema.not_deleted);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// (messages received from `addr` other than `exclude`, messages the user
    /// sent to `addr`), to spot a first contact.
    pub fn sender_history(&self, conn: &Connection, addr: &str, exclude: i64) -> Result<(i64, i64)> {
        let from: i64 = conn.query_row(
            "SELECT count(*) FROM mail.messages m JOIN mail.addresses a ON a.ROWID = m.sender
             WHERE a.address = ?1 COLLATE NOCASE AND m.ROWID != ?2",
            rusqlite::params![addr, exclude],
            |r| r.get(0),
        )?;
        let sent = self.mailboxes_with_roles(&["sent".to_string()]);
        let to = match &self.schema.rec_msg_col {
            Some(col) if !sent.is_empty() => {
                let in_sent = self.in_mailboxes(&sent);
                conn.query_row(
                    &format!(
                        "SELECT count(*) FROM mail.recipients r JOIN mail.addresses ra ON ra.ROWID = r.address
                         JOIN mail.messages m ON m.ROWID = r.{col} WHERE ra.address = ?1 COLLATE NOCASE AND {in_sent}"
                    ),
                    [addr],
                    |r| r.get(0),
                )?
            }
            _ => 0,
        };
        Ok((from, to))
    }

    pub fn stats(&self, conn: &Connection) -> Result<(i64, i64)> {
        let sql = format!(
            "SELECT COUNT(*), SUM(CASE WHEN {} THEN 1 ELSE 0 END) FROM mail.messages m WHERE {}",
            self.schema.unread, self.schema.not_deleted
        );
        Ok(conn.query_row(&sql, [], |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mailbox_urls() {
        let mut acc = HashMap::new();
        acc.insert("62653857-BF50-4534-AB81-8F72E0FCEDF5".to_string(), "iCloud".to_string());
        let m = parse_mailbox_url(1, "imap://62653857-bf50-4534-ab81-8f72e0fcedf5/Archive/2024", &acc);
        assert_eq!(m.account, "iCloud");
        assert_eq!(m.path, vec!["Archive", "2024"]);
        assert_eq!(m.name, "2024");
        assert_eq!(m.role, Role::Other);
        let m = parse_mailbox_url(2, "imap://62653857-BF50-4534-AB81-8F72E0FCEDF5/INBOX", &acc);
        assert_eq!(m.role, Role::Inbox);
        let m = parse_mailbox_url(3, "local:///Drafts", &acc);
        assert!(m.local);
        assert_eq!(m.role, Role::Drafts);
        let m = parse_mailbox_url(4, "imap://62653857-BF50-4534-AB81-8F72E0FCEDF5/Sent%20Messages", &acc);
        assert_eq!(m.role, Role::Sent);
        assert_eq!(m.name, "Sent Messages");
    }

    #[test]
    fn detects_localized_roles() {
        let r = |s: &str| Role::detect(&[s.to_string()]);
        assert_eq!(r("Corbeille"), Role::Trash);
        assert_eq!(r("Courrier indésirable"), Role::Junk);
        assert_eq!(r("Éléments envoyés"), Role::Sent);
        assert_eq!(r("Deleted Messages"), Role::Trash);
        assert_eq!(r("Papelera"), Role::Trash);
        assert_eq!(r("Brouillons"), Role::Drafts);
        assert_eq!(r("Archive"), Role::Archive);
        assert_eq!(r("Combined"), Role::Other);
        // Orange, Wanadoo and La Poste.
        assert_eq!(Role::detect(&["INBOX".into(), "OUTBOX".into()]), Role::Sent);
        assert_eq!(Role::detect(&["INBOX".into(), "QUARANTAINE".into()]), Role::Junk);
        assert_eq!(Role::detect(&["INBOX".into(), "DRAFT".into()]), Role::Drafts);
    }
}
