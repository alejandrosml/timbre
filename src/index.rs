//! Timbre's own full-text index (SQLite FTS5), kept next to Mail's data.
//!
//! One row per message, keyed by the Envelope Index ROWID, so searches join
//! the index with Mail's live metadata. Text is accent-insensitive
//! (`unicode61 remove_diacritics 2`) and the body is indexed without quoted
//! history. The index is built in parallel on first run and then kept up to
//! date incrementally.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;

use crate::config::Config;
use crate::emlx::{self, Located};
use crate::mime;
use crate::norm;
use crate::quote;
use crate::store::{self, Store};
use crate::util::{clip, now_unix};

pub const SCHEMA_VERSION: &str = "3";
/// Version of what is read from each message (indexed text, attachment
/// count). Rows read by an older Timbre are read again in the background,
/// newest first, while searches keep using the index.
pub const PARSER_VERSION: i64 = 2;
/// Messages read again per transaction when the parser changed.
const REFRESH_BATCH: usize = 1000;
/// Longest body text indexed per message (characters).
const MAX_INDEXED_BODY: usize = 32_000;
const CHUNK: usize = 256;
/// How often Mail's database is checked for changes.
const POLL: Duration = Duration::from_secs(2);
/// Up to this many unindexed messages, their files are found from their
/// mailbox's folder instead of walking the whole store.
const PREDICT_LIMIT: usize = 5000;
/// Messages whose file was missing at a walk wait this long for another one.
const WALK_EVERY: i64 = 6 * 3600;
/// Messages per write transaction during a full build.
const COMMIT_EVERY: usize = 4096;

pub const PHASE_IDLE: u8 = 0;
pub const PHASE_SCANNING: u8 = 1;
pub const PHASE_INDEXING: u8 = 2;
pub const PHASE_READY: u8 = 3;
pub const PHASE_FAILED: u8 = 4;
pub const PHASE_DISABLED: u8 = 5;

#[derive(Default)]
pub struct Progress {
    pub phase: AtomicU8,
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub docs: AtomicU64,
    pub last_sync: AtomicI64,
    pub build_secs: AtomicU64,
    pub error: Mutex<Option<String>>,
    /// The index was complete once: searches use it while new or changed
    /// messages are being added.
    pub built: AtomicBool,
    /// Messages still to read again after an update, out of `refresh_total`.
    pub refresh_left: AtomicU64,
    pub refresh_total: AtomicU64,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ProgressView {
    pub state: String,
    pub indexed_messages: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_build_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Messages being read again after an update of Timbre, in percent
    /// (searches work meanwhile).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rereading_percent: Option<u64>,
    /// When new mail was last added to the index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_update: Option<String>,
}

impl Progress {
    /// Can searches use the full-text index?
    pub fn ready(&self) -> bool {
        match self.phase.load(Ordering::Relaxed) {
            PHASE_READY => true,
            PHASE_SCANNING | PHASE_INDEXING => self.built.load(Ordering::Relaxed),
            _ => false,
        }
    }

    pub fn view(&self) -> ProgressView {
        let phase = self.phase.load(Ordering::Relaxed);
        let state = match phase {
            PHASE_SCANNING => "scanning",
            PHASE_INDEXING => "indexing",
            PHASE_READY => "ready",
            PHASE_FAILED => "failed",
            PHASE_DISABLED => "disabled",
            PHASE_IDLE => "starting",
            _ => "unknown",
        };
        let total = self.total.load(Ordering::Relaxed);
        let done = self.done.load(Ordering::Relaxed);
        let secs = self.build_secs.load(Ordering::Relaxed);
        let (left, all) = (self.refresh_left.load(Ordering::Relaxed), self.refresh_total.load(Ordering::Relaxed));
        ProgressView {
            state: state.to_string(),
            indexed_messages: self.docs.load(Ordering::Relaxed),
            percent: (phase == PHASE_INDEXING && total > 0).then(|| done * 100 / total),
            last_build_seconds: (secs > 0).then_some(secs),
            error: self.error.lock().ok().and_then(|e| e.clone()),
            rereading_percent: (left > 0 && all > 0).then(|| all.saturating_sub(left) * 100 / all),
            last_update: match self.last_sync.load(Ordering::Relaxed) {
                0 => None,
                t => crate::util::iso_local(t),
            },
        }
    }

    fn fail(&self, e: &anyhow::Error) {
        self.phase.store(PHASE_FAILED, Ordering::Relaxed);
        self.set_error(Some(format!("{e:#}")));
    }

    fn set_error(&self, error: Option<String>) {
        if let Ok(mut slot) = self.error.lock() {
            *slot = error;
        }
    }
}

/// Open Timbre's index database (creating it if needed) and attach Mail's
/// Envelope Index as `mail`.
pub fn open(cfg: &Config, store: &Store, writable: bool) -> Result<Connection> {
    open_with_mode(cfg, store, writable).map(|(c, _)| c)
}

/// Like [`open`], also telling how Mail's database was attached.
pub fn open_with_mode(cfg: &Config, store: &Store, writable: bool) -> Result<(Connection, store::OpenMode)> {
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    // Readers also open read-write (then switch to query_only): a read-only
    // open of a WAL database fails when its shared-memory file is missing.
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let path = cfg.index_path();
    let conn = Connection::open_with_flags(&path, flags).with_context(|| format!("opening {}", path.display()))?;
    // Another Timbre (a second Claude window) may be writing: wait for it.
    conn.busy_timeout(Duration::from_secs(5))?;
    register_functions(&conn)?;
    if writable {
        match classify(&conn, store)? {
            IndexFile::Empty | IndexFile::Current => {}
            IndexFile::Foreign => {
                bail!("{} is not a Timbre index; move it away or set TIMBRE_DATA_DIR to another folder", path.display())
            }
            IndexFile::Outdated => {
                // Another format or another Mail store: drop Timbre's tables
                // (never the file: another process may have it open).
                conn.execute_batch(
                    "BEGIN IMMEDIATE;
                     DROP TABLE IF EXISTS fts;
                     DROP VIEW IF EXISTS fts_src;
                     DROP TABLE IF EXISTS texts;
                     DROP TABLE IF EXISTS docs;
                     DROP TABLE IF EXISTS unlocated;
                     DELETE FROM meta;
                     COMMIT;",
                )?;
                let _ = conn.execute_batch("VACUUM;"); // give the old index's space back
            }
        }
    }
    conn.execute_batch(
        "PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -65536;
         PRAGMA mmap_size = 1073741824;",
    )?;
    if writable {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA journal_size_limit = 67108864;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let current_dir = store.version_dir.to_string_lossy().to_string();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS docs(
                 id INTEGER PRIMARY KEY,
                 mailbox INTEGER NOT NULL DEFAULT 0,
                 path TEXT NOT NULL DEFAULT '',
                 partial INTEGER NOT NULL DEFAULT 0,
                 msgid TEXT,
                 auth TEXT,
                 hidden INTEGER NOT NULL DEFAULT 0,
                 status INTEGER NOT NULL DEFAULT 0,
                 -- Attachments a reader sees (not inline images); NULL: unknown.
                 attachments INTEGER,
                 -- PARSER_VERSION that read this message.
                 parser INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS docs_msgid ON docs(msgid);
             -- Messages without a file at the last walk of Mail's store.
             CREATE TABLE IF NOT EXISTS unlocated(id INTEGER PRIMARY KEY);
             CREATE INDEX IF NOT EXISTS docs_mailbox ON docs(mailbox);
             -- What was indexed, kept for snippets; the body is zstd-compressed.
             CREATE TABLE IF NOT EXISTS texts(
                 id INTEGER PRIMARY KEY,
                 subject TEXT NOT NULL,
                 people TEXT NOT NULL,
                 body BLOB,
                 attachments TEXT NOT NULL
             );
             CREATE VIEW IF NOT EXISTS fts_src AS
                 SELECT id, subject, people, timbre_unpack(body) AS body, attachments FROM texts;
             CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
                 subject, people, body, attachments,
                 content = 'fts_src', content_rowid = 'id',
                 tokenize = 'unicode61 remove_diacritics 2'
             );",
        )?;
        add_missing_columns(&conn)?;
        set_meta(&conn, "schema", SCHEMA_VERSION)?;
        set_meta(&conn, "mail_dir", &current_dir)?;
        // A build that was killed may have left its bulk-load settings.
        conn.execute_batch(
            "INSERT INTO fts(fts, rank) VALUES('automerge', 4);
             INSERT INTO fts(fts, rank) VALUES('hashsize', 1048576);",
        )?;
    }
    let mode = store::attach(&conn, &store.db_path)?;
    if !writable {
        conn.execute_batch("PRAGMA query_only = ON;")?;
    }
    Ok((conn, mode))
}

/// Columns added since schema 3 was first released: added in place, so an
/// update does not throw the index away.
fn add_missing_columns(conn: &Connection) -> Result<()> {
    let columns = |conn: &Connection| -> Result<Vec<String>> {
        let mut stmt = conn.prepare("PRAGMA main.table_info(docs)")?;
        let cols = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(cols)
    };
    let wanted = [("attachments", "INTEGER"), ("parser", "INTEGER NOT NULL DEFAULT 0")];
    if wanted.iter().all(|(c, _)| columns(conn).is_ok_and(|have| have.iter().any(|h| h == c))) {
        return Ok(());
    }
    // Another Timbre may be doing the same: one transaction, checked inside.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> Result<()> {
        let have = columns(conn)?;
        for (col, decl) in wanted {
            if !have.iter().any(|h| h == col) {
                conn.execute_batch(&format!("ALTER TABLE docs ADD COLUMN {col} {decl}"))?;
            }
        }
        Ok(())
    })();
    conn.execute_batch(if result.is_ok() { "COMMIT" } else { "ROLLBACK" })?;
    result
}

/// Is this error a damaged index file (not a database, malformed)?
pub fn damaged(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        matches!(
            c.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(f, _))
                if matches!(f.code, rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt)
        )
    })
}

/// Move a damaged index aside (never deleted: it is only Timbre's, but
/// someone may want to look at it); the next open starts a new one.
pub fn set_aside(cfg: &Config) {
    let main = cfg.index_path();
    for suffix in ["", "-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{suffix}", main.display()));
        let to = PathBuf::from(format!("{}.damaged{suffix}", main.display()));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    tracing::warn!("index: damaged file set aside as {}.damaged", main.display());
}

enum IndexFile {
    Empty,
    Current,
    Outdated,
    Foreign,
}

/// What is at the index path: nothing yet, Timbre's index (current or for
/// another format or Mail store), or some other database.
fn classify(conn: &Connection, store: &Store) -> Result<IndexFile> {
    let objects: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
    let has_meta: bool =
        conn.query_row("SELECT count(*) > 0 FROM sqlite_schema WHERE type = 'table' AND name = 'meta'", [], |r| {
            r.get(0)
        })?;
    if !has_meta {
        return Ok(if objects == 0 { IndexFile::Empty } else { IndexFile::Foreign });
    }
    let read = |key: &str| -> Result<Option<String>> {
        Ok(conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    };
    let current_dir = store.version_dir.to_string_lossy().to_string();
    let same_format = read("schema")?.as_deref() == Some(SCHEMA_VERSION);
    let same_store = read("mail_dir")?.is_none_or(|d| d == current_dir);
    Ok(if same_format && same_store { IndexFile::Current } else { IndexFile::Outdated })
}

/// zstd level 1 with one reused context per thread. On mail text it beat
/// DEFLATE level 1 on both ratio (3.5x vs 2.9x) and speed (2x) in our tests;
/// a fresh DEFLATE compressor also zeroes ~300 KB of tables per message.
pub fn pack(text: &str) -> Vec<u8> {
    thread_local! {
        static ZSTD: std::cell::RefCell<Option<zstd::bulk::Compressor<'static>>> = const { std::cell::RefCell::new(None) };
    }
    ZSTD.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = zstd::bulk::Compressor::new(1).ok();
        }
        slot.as_mut().and_then(|c| c.compress(text.as_bytes()).ok())
    })
    .unwrap_or_else(|| text.as_bytes().to_vec())
}

pub fn unpack(data: &[u8]) -> Result<String> {
    let raw = zstd::bulk::decompress(data, 4 << 20).map_err(|e| anyhow::anyhow!("corrupt index text: {e}"))?;
    Ok(String::from_utf8(raw)?)
}

/// SQL functions the index needs on every connection (`fts_src` uses them).
fn register_functions(conn: &Connection) -> Result<()> {
    use rusqlite::functions::FunctionFlags;
    use rusqlite::types::ValueRef;
    conn.create_scalar_function(
        "timbre_unpack",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| match ctx.get_raw(0) {
            ValueRef::Blob(b) => unpack(b).map(Some).map_err(|e| rusqlite::Error::UserFunctionError(e.into())),
            ValueRef::Text(t) => Ok(Some(String::from_utf8_lossy(t).into_owned())),
            _ => Ok(None),
        },
    )?;
    conn.create_scalar_function(
        "timbre_fold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        // Text that is not valid UTF-8 in Mail's database is read anyway.
        |ctx| Ok(crate::store::lossy(ctx.get_raw(0)).map(|s| norm::for_match(&s))),
    )?;
    // Weight of a message's age in the ranking: (1 + 3 / (1 + age / 60
    // days)) ^ p, from 4^p for today's mail down to about 1 for old mail.
    conn.create_scalar_function(
        "timbre_recency",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let age = ctx.get::<Option<f64>>(0)?.unwrap_or(0.0).max(0.0);
            let p = ctx.get::<f64>(1)?;
            Ok((1.0 + 3.0 / (1.0 + age / 5_184_000.0)).powf(p))
        },
    )?;
    Ok(())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional().ok().flatten()
}

/// One message ready to be written to the index.
pub struct DocRow {
    pub id: i64,
    pub mailbox: i64,
    pub path: String,
    pub partial: bool,
    pub msgid: Option<String>,
    pub auth: Option<String>,
    pub hidden: i64,
    pub status: i64,
    pub subject: String,
    pub people: String,
    pub body: String,
    /// `body`, zstd-compressed for the `texts` table.
    pub body_z: Vec<u8>,
    /// Attachment names, for the full-text index.
    pub attachments: String,
    /// How many attachments a reader sees (inline images aside).
    pub attachment_count: Option<i64>,
}

fn people_of(p: &mime::Parsed) -> String {
    let mut v = mime::fmt_list(&p.from);
    v.extend(mime::fmt_list(&p.to));
    v.extend(mime::fmt_list(&p.cc));
    v.join("; ")
}

/// Parse one message file into an index row.
pub fn build_doc(version_dir: &Path, id: i64, mailbox: i64, loc: &Located) -> DocRow {
    let rel = loc.path.strip_prefix(version_dir).unwrap_or(&loc.path).to_string_lossy().to_string();
    let mut row = DocRow {
        id,
        mailbox,
        path: rel,
        partial: loc.partial,
        msgid: None,
        auth: None,
        hidden: 0,
        status: 1,
        subject: String::new(),
        people: String::new(),
        body: String::new(),
        body_z: vec![],
        attachments: String::new(),
        attachment_count: None,
    };
    let parsed = emlx::read_message(&loc.path).ok().and_then(|raw| mime::parse_for_index(&raw));
    if let Some(p) = parsed {
        let (body, _) = clip(&quote::strip_quotes_of(&p.body, &p.subject), MAX_INDEXED_BODY);
        let names: Vec<&str> = p.attachments.iter().filter(|a| !a.inline).map(|a| a.name.as_str()).collect();
        row.status = 0;
        row.subject = norm::fold(&p.subject).into_owned();
        row.people = norm::fold(&people_of(&p)).into_owned();
        row.body = norm::for_index(&body);
        row.body_z = pack(&row.body);
        row.attachments = norm::fold(&names.join(" ")).into_owned();
        row.attachment_count = Some(names.len() as i64);
        row.msgid = p.message_id.clone();
        row.auth = (!p.auth.is_empty()).then(|| p.auth.compact());
        row.hidden = (p.hidden_chars + p.invisible_chars) as i64;
    }
    row
}

/// Mail is untrusted input: if parsing one message panics, record it as
/// unreadable (status 1, never retried) and keep indexing the rest.
pub fn build_doc_guarded(version_dir: &Path, id: i64, mailbox: i64, loc: &Located) -> DocRow {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_doc(version_dir, id, mailbox, loc))) {
        Ok(row) => row,
        Err(_) => {
            tracing::warn!("message {id}: parser panicked; skipped");
            let path = loc.path.strip_prefix(version_dir).unwrap_or(&loc.path).to_string_lossy().to_string();
            DocRow {
                id,
                mailbox,
                path,
                partial: loc.partial,
                msgid: None,
                auth: None,
                hidden: 0,
                status: 1,
                subject: String::new(),
                people: String::new(),
                body: String::new(),
                body_z: vec![],
                attachments: String::new(),
                attachment_count: None,
            }
        }
    }
}

/// Mark the calling thread as utility work (macOS QoS): indexing then yields
/// to whatever the user is doing, and still uses idle performance cores.
pub fn background_priority() {
    #[cfg(target_os = "macos")]
    // SAFETY: only changes the scheduling class of the calling thread.
    #[allow(unsafe_code)]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0);
    };
}

/// Empty the index in one transaction: docs, stored text and FTS entries
/// always agree, even if the process stops halfway.
/// How often, in a sample, the subject Timbre read in a message's file
/// differs from the one Mail gives its id: `None` with too few to compare.
fn subject_mismatch(conn: &Connection, store: &Store) -> Result<Option<f64>> {
    let sql = format!(
        "SELECT t.subject, {prefix} || COALESCE(s.subject, '') \
         FROM (SELECT id FROM docs WHERE status = 0 ORDER BY random() LIMIT 200) x \
         JOIN texts t ON t.id = x.id JOIN mail.messages m ON m.ROWID = x.id \
         LEFT JOIN mail.subjects s ON s.ROWID = m.subject",
        prefix = store.schema.subject_prefix
    );
    let key = |s: &str| norm::fold(s).chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let (mut compared, mut differ) = (0usize, 0usize);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let (ours, mail) = (key(&crate::store::text_at(row, 0)?), key(&crate::store::text_at(row, 1)?));
        if ours.is_empty() || mail.is_empty() {
            continue;
        }
        compared += 1;
        differ += usize::from(!ours.contains(&mail) && !mail.contains(&ours));
        if compared == 40 {
            break;
        }
    }
    Ok((compared >= 10).then(|| differ as f64 / compared as f64))
}

/// Did Mail number its messages anew (rebuilding its Envelope Index, the
/// usual fix for a broken Mail search)? The ids Timbre keeps would then name
/// other messages: nearly every subject Timbre read differs from the one
/// Mail now gives the id. How many differed right after the last build is
/// the baseline, so subjects Mail stores its own way never set off rebuild
/// after rebuild.
pub(crate) fn renumbered(conn: &Connection, store: &Store) -> Result<bool> {
    let Some(now) = subject_mismatch(conn, store)? else { return Ok(false) };
    let baseline: f64 = get_meta(conn, "subject_baseline").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    Ok(now > 0.6 && baseline < 0.3)
}

fn clear(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        "DELETE FROM docs; DELETE FROM texts; DELETE FROM unlocated; DELETE FROM meta WHERE key IN ('last_walk', 'built');
         INSERT INTO fts(fts) VALUES('delete-all');",
    )?;
    tx.commit()?;
    Ok(())
}

/// Every indexed document has stored text and an FTS entry, and nothing else.
fn consistent(conn: &Connection) -> Result<bool> {
    let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0));
    let indexed = count("SELECT count(*) FROM docs WHERE status = 0")?;
    let texts = count("SELECT count(*) FROM texts")?;
    let fts = count("SELECT count(*) FROM fts_docsize")?;
    let orphans = count("SELECT count(*) FROM texts WHERE id NOT IN (SELECT id FROM docs WHERE status = 0)")?;
    Ok(indexed == texts && texts == fts && orphans == 0)
}

/// Runs one indexing step, turning a panic into an error so the loop goes on.
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("{what} panicked")))
}

/// The FTS table only stores the inverted index: removing a document means
/// handing FTS5 the exact text that was indexed, read back from `texts`.
const UNINDEX: &str = "INSERT INTO fts(fts, rowid, subject, people, body, attachments) \
     SELECT 'delete', id, subject, people, timbre_unpack(body), attachments FROM texts WHERE id = ?1";

fn write_docs(conn: &mut Connection, rows: &[DocRow], replace: bool) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut ins_doc = tx.prepare_cached(
            "INSERT OR REPLACE INTO docs(id, mailbox, path, partial, msgid, auth, hidden, status, attachments, parser) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        let mut unindex = tx.prepare_cached(UNINDEX)?;
        let mut del_text = tx.prepare_cached("DELETE FROM texts WHERE id = ?1")?;
        let mut ins_text =
            tx.prepare_cached("INSERT INTO texts(id, subject, people, body, attachments) VALUES(?1, ?2, ?3, ?4, ?5)")?;
        let mut ins_fts =
            tx.prepare_cached("INSERT INTO fts(rowid, subject, people, body, attachments) VALUES(?1, ?2, ?3, ?4, ?5)")?;
        for r in rows {
            ins_doc.execute(params![
                r.id,
                r.mailbox,
                r.path,
                r.partial as i64,
                r.msgid,
                r.auth,
                r.hidden,
                r.status,
                r.attachment_count,
                PARSER_VERSION
            ])?;
            if replace {
                unindex.execute([r.id])?;
                del_text.execute([r.id])?;
            }
            if r.status == 0 {
                ins_text.execute(params![r.id, r.subject, r.people, r.body_z, r.attachments])?;
                ins_fts.execute(params![r.id, r.subject, r.people, r.body, r.attachments])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn delete_docs(conn: &mut Connection, ids: &[i64]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut unindex = tx.prepare_cached(UNINDEX)?;
        let mut del_text = tx.prepare_cached("DELETE FROM texts WHERE id = ?1")?;
        let mut del_doc = tx.prepare_cached("DELETE FROM docs WHERE id = ?1")?;
        for id in ids {
            unindex.execute([id])?;
            del_text.execute([id])?;
            del_doc.execute([id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub struct Indexer {
    cfg: Config,
    store: Arc<Store>,
    progress: Arc<Progress>,
    pending: Vec<(i64, i64, u32)>,
    last_reconcile: Instant,
}

impl Indexer {
    pub fn new(cfg: Config, store: Arc<Store>, progress: Arc<Progress>) -> Self {
        Indexer { cfg, store, progress, pending: vec![], last_reconcile: Instant::now() }
    }

    /// Full pass: index every message not yet indexed, drop deleted ones.
    pub fn full_build(&mut self, conn: &mut Connection, force: bool) -> Result<()> {
        let started = Instant::now();
        self.progress.phase.store(PHASE_SCANNING, Ordering::Relaxed);
        self.store.reload_mailboxes(conn)?;
        if force {
            clear(conn)?;
            self.progress.built.store(false, Ordering::Relaxed);
        }
        if !force && !consistent(conn)? {
            // Left by an interrupted older version: start over rather than
            // build on an index whose FTS entries and stored text disagree.
            tracing::warn!("index: inconsistent tables, rebuilding from scratch");
            clear(conn)?;
            self.progress.built.store(false, Ordering::Relaxed);
        }
        // Every mailbox is indexed, junk and trash too: search leaves them
        // out unless asked, and can find their words when asked.
        if !force && renumbered(conn, &self.store)? {
            // Mail rebuilt its Envelope Index (the usual fix for a broken
            // Mail search): its ids now name other messages.
            tracing::warn!("index: Mail numbered its messages anew, rebuilding");
            clear(conn)?;
            self.progress.built.store(false, Ordering::Relaxed);
        }
        let live = self.store.live_messages(conn)?;
        let max_live = live.iter().map(|(id, _)| *id).max().unwrap_or(0);
        let wanted: HashMap<i64, i64> = live.into_iter().collect();
        let existing: HashMap<i64, i64> = {
            let mut stmt = conn.prepare("SELECT id, status FROM docs")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let missing: Vec<(i64, i64)> = wanted
            .iter()
            .filter(|(id, _)| existing.get(id).is_none_or(|s| *s == 2))
            .map(|(id, mb)| (*id, *mb))
            .collect();
        let files = self.locate_missing(conn, &missing)?;
        let mut todo: Vec<(i64, i64, Located)> = wanted
            .iter()
            .filter(|(id, _)| existing.get(id).is_none_or(|s| *s == 2))
            .filter_map(|(id, mb)| files.get(id).map(|loc| (*id, *mb, loc.clone())))
            .collect();
        todo.sort_by_key(|(id, _, _)| std::cmp::Reverse(*id)); // newest first
        let stale: Vec<i64> = existing.keys().filter(|id| !wanted.contains_key(id)).copied().collect();

        self.progress.total.store(todo.len() as u64, Ordering::Relaxed);
        self.progress.done.store(0, Ordering::Relaxed);
        self.progress.phase.store(PHASE_INDEXING, Ordering::Relaxed);
        let version_dir = self.store.version_dir.clone();
        tracing::info!("index: {} messages to parse, scan took {:.1}s", todo.len(), started.elapsed().as_secs_f64());
        let phase = Instant::now();
        // Parsing runs on all cores while this thread writes: a bounded
        // channel keeps memory flat, and large transactions mean fewer FTS5
        // segments to merge. Merging waits for the final 'optimize'.
        // A 64 MiB pending-terms table (default 1 MiB) flushes one segment
        // per transaction instead of dozens: 2.5x faster bulk loads in tests.
        conn.execute_batch(
            "INSERT INTO fts(fts, rank) VALUES('automerge', 0);
             INSERT INTO fts(fts, rank) VALUES('hashsize', 67108864);",
        )?;
        let progress = &self.progress;
        let written = std::thread::scope(|scope| -> Result<()> {
            let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<DocRow>>(4);
            let todo = &todo;
            let version_dir = &version_dir;
            scope.spawn(move || {
                for chunk in todo.chunks(CHUNK) {
                    let rows: Vec<DocRow> =
                        chunk.par_iter().map(|(id, mb, loc)| build_doc_guarded(version_dir, *id, *mb, loc)).collect();
                    if tx.send(rows).is_err() {
                        return; // the writer failed
                    }
                }
            });
            let mut batch: Vec<DocRow> = Vec::with_capacity(COMMIT_EVERY + CHUNK);
            let write_time = std::cell::Cell::new(Duration::ZERO);
            let flush = |batch: &mut Vec<DocRow>, conn: &mut Connection| -> Result<()> {
                let t = Instant::now();
                write_docs(conn, batch, false)?;
                write_time.set(write_time.get() + t.elapsed());
                progress.done.fetch_add(batch.len() as u64, Ordering::Relaxed);
                progress.docs.fetch_add(batch.iter().filter(|r| r.status == 0).count() as u64, Ordering::Relaxed);
                batch.clear();
                Ok(())
            };
            let mut waited = Duration::ZERO;
            let mut t = Instant::now();
            for rows in rx {
                waited += t.elapsed();
                batch.extend(rows);
                if batch.len() >= COMMIT_EVERY {
                    flush(&mut batch, conn)?;
                }
                t = Instant::now();
            }
            flush(&mut batch, conn)?;
            tracing::info!(
                "index: writer busy {:.1}s, waited for parsers {:.1}s",
                write_time.get().as_secs_f64(),
                waited.as_secs_f64()
            );
            Ok(())
        });
        conn.execute_batch(
            "INSERT INTO fts(fts, rank) VALUES('automerge', 4);
             INSERT INTO fts(fts, rank) VALUES('hashsize', 1048576);",
        )?;
        written?;
        tracing::info!("index: parsed and written in {:.1}s", phase.elapsed().as_secs_f64());
        delete_docs(conn, &stale)?;
        if todo.len() > 5000 {
            // After a large build only: merging a big index costs time and
            // log space, and automerge keeps small additions in order.
            let phase = Instant::now();
            conn.execute_batch("INSERT INTO fts(fts) VALUES('optimize');")?;
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
            tracing::info!("index: optimized in {:.1}s", phase.elapsed().as_secs_f64());
        }
        set_meta(conn, "max_id", &max_live.to_string())?;
        if let Some(rate) = subject_mismatch(conn, &self.store)? {
            set_meta(conn, "subject_baseline", &format!("{rate:.2}"))?;
        }
        set_meta(conn, "built", "1")?;
        set_meta(conn, "last_full", &now_unix().to_string())?;
        self.progress.built.store(true, Ordering::Relaxed);
        self.refresh_count(conn)?;
        self.progress.build_secs.store(started.elapsed().as_secs().max(1), Ordering::Relaxed);
        self.progress.last_sync.store(now_unix(), Ordering::Relaxed);
        self.progress.phase.store(PHASE_READY, Ordering::Relaxed);
        self.last_reconcile = Instant::now();
        Ok(())
    }

    /// The right to keep the index, held as long as the file stays open.
    fn take_lock(&self) -> Option<std::fs::File> {
        std::fs::create_dir_all(&self.cfg.data_dir).ok()?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.cfg.data_dir.join("index.lock"))
            .ok()?;
        file.try_lock().ok()?;
        Some(file)
    }

    /// While another Timbre keeps the index: report its state from the file.
    fn follow(&self) {
        if let Ok(conn) = open(&self.cfg, &self.store, false) {
            let built = get_meta(&conn, "built").is_some();
            self.progress.built.store(built, Ordering::Relaxed);
            self.progress.phase.store(if built { PHASE_READY } else { PHASE_INDEXING }, Ordering::Relaxed);
            let _ = self.refresh_count(&conn);
        }
    }

    /// How many messages an older Timbre read (see [`PARSER_VERSION`]).
    fn outdated(&self, conn: &Connection) -> Result<u64> {
        let n: i64 = conn.query_row("SELECT count(*) FROM docs WHERE parser < ?1", [PARSER_VERSION], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Read again up to `limit` messages that an older Timbre read, newest
    /// first, replacing their index entries in one transaction. Returns how
    /// many were handled; 0 when none is left.
    pub fn reread_batch(&mut self, conn: &mut Connection, limit: usize) -> Result<usize> {
        let rows: Vec<(i64, i64, String, bool)> = {
            let mut stmt = conn.prepare_cached(
                "SELECT id, mailbox, path, partial FROM docs WHERE parser < ?1 ORDER BY id DESC LIMIT ?2",
            )?;
            stmt.query_map(params![PARSER_VERSION, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0))
            })?
            .collect::<rusqlite::Result<_>>()?
        };
        if rows.is_empty() {
            return Ok(0);
        }
        let version_dir = self.store.version_dir.clone();
        let roots = self.data_roots(conn)?;
        let mut found: Vec<(i64, i64, Located)> = vec![];
        let mut lost: Vec<i64> = vec![];
        for (id, mb, rel, partial) in &rows {
            let known = (!rel.is_empty())
                .then(|| version_dir.join(rel))
                .filter(|p| p.is_file())
                .map(|path| Located { path, partial: *partial });
            match known.or_else(|| roots.get(mb).and_then(|root| emlx::predict(root, *id))) {
                Some(loc) => found.push((*id, *mb, loc)),
                None => lost.push(*id),
            }
        }
        let parsed: Vec<DocRow> =
            found.par_iter().map(|(id, mb, loc)| build_doc_guarded(&version_dir, *id, *mb, loc)).collect();
        // Rows that read worse than before keep what they had.
        let (better, worse): (Vec<DocRow>, Vec<DocRow>) = parsed.into_iter().partition(|r| r.status == 0);
        lost.extend(worse.iter().map(|r| r.id));
        write_docs(conn, &better, true)?;
        // Messages whose file is gone for now keep their entry; the next
        // reconcile or build deals with them.
        let tx = conn.transaction()?;
        {
            let mut keep = tx.prepare_cached("UPDATE docs SET parser = ?1 WHERE id = ?2")?;
            for id in &lost {
                keep.execute(params![PARSER_VERSION, id])?;
            }
        }
        tx.commit()?;
        let n = rows.len();
        let left = self.progress.refresh_left.load(Ordering::Relaxed).saturating_sub(n as u64);
        self.progress.refresh_left.store(left, Ordering::Relaxed);
        Ok(n)
    }

    fn refresh_count(&self, conn: &Connection) -> Result<()> {
        let n: i64 = conn.query_row("SELECT count(*) FROM docs WHERE status = 0", [], |r| r.get(0))?;
        self.progress.docs.store(n as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Data directory of each mailbox's store, learned from indexed paths.
    /// Files of the messages not indexed yet. Walking Mail's whole store
    /// (hundreds of thousands of files, slow when the disk cache is cold) is
    /// avoided at every start: nothing missing means nothing to look for, a
    /// few missing are found from their mailbox's folder, and messages whose
    /// file was already absent at a recent walk do not trigger another one.
    fn locate_missing(&self, conn: &Connection, missing: &[(i64, i64)]) -> Result<HashMap<i64, Located>> {
        if missing.is_empty() {
            return Ok(HashMap::new());
        }
        let mut found = HashMap::new();
        let mut unfound = vec![];
        if missing.len() <= PREDICT_LIMIT {
            let roots = self.data_roots(conn)?;
            for &(id, mb) in missing {
                match roots.get(&mb).and_then(|root| emlx::predict(root, id)) {
                    Some(loc) => {
                        found.insert(id, loc);
                    }
                    None => unfound.push(id),
                }
            }
        } else {
            unfound = missing.iter().map(|(id, _)| *id).collect();
        }
        if unfound.is_empty() {
            return Ok(found);
        }
        let recent = get_meta(conn, "last_walk")
            .and_then(|s| s.parse::<i64>().ok())
            .is_some_and(|t| now_unix() - t < WALK_EVERY);
        if recent {
            let mut known = conn.prepare("SELECT 1 FROM unlocated WHERE id = ?1")?;
            let mut all_known = true;
            for id in &unfound {
                if !known.exists([id])? {
                    all_known = false;
                    break;
                }
            }
            if all_known {
                return Ok(found);
            }
        }
        let scanned = emlx::scan(&self.store.version_dir);
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM unlocated", [])?;
        {
            let mut ins = tx.prepare("INSERT OR IGNORE INTO unlocated(id) VALUES(?1)")?;
            for id in &unfound {
                match scanned.get(id) {
                    Some(loc) => {
                        found.insert(*id, loc.clone());
                    }
                    None => {
                        ins.execute([id])?;
                    }
                }
            }
        }
        tx.commit()?;
        set_meta(conn, "last_walk", &now_unix().to_string())?;
        Ok(found)
    }

    fn data_roots(&self, conn: &Connection) -> Result<HashMap<i64, PathBuf>> {
        let mut stmt = conn.prepare(
            "SELECT mailbox, path FROM docs WHERE id IN (SELECT max(id) FROM docs WHERE path != '' GROUP BY mailbox)",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = HashMap::new();
        for (mb, rel) in rows.flatten() {
            if let Some(root) = emlx::data_root(&self.store.version_dir.join(rel)) {
                out.insert(mb, root);
            }
        }
        Ok(out)
    }

    /// Incremental pass: index new messages, retry missing files, and every
    /// few minutes reconcile deletions and moves.
    pub fn sync(&mut self, conn: &mut Connection) -> Result<usize> {
        self.store.reload_mailboxes(conn)?;
        let max_id: i64 = get_meta(conn, "max_id").and_then(|s| s.parse().ok()).unwrap_or(0);
        let sql = format!(
            "SELECT m.ROWID, m.mailbox FROM mail.messages m WHERE m.ROWID > ?1 AND {} ORDER BY m.ROWID",
            self.store.schema.not_deleted
        );
        let new: Vec<(i64, i64)> = {
            let mut stmt = conn.prepare(&sql)?;
            stmt.query_map([max_id], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let mut candidates: Vec<(i64, i64, u32)> = std::mem::take(&mut self.pending);
        let mut new_max = max_id;
        for (id, mb) in new {
            new_max = new_max.max(id);
            candidates.push((id, mb, 0));
        }
        let mut indexed = 0;
        if !candidates.is_empty() {
            let roots = self.data_roots(conn)?;
            let mut found: Vec<(i64, i64, Located)> = vec![];
            for (id, mb, tries) in candidates {
                match roots.get(&mb).and_then(|root| emlx::predict(root, id)) {
                    Some(loc) => found.push((id, mb, loc)),
                    None if tries < 40 => self.pending.push((id, mb, tries + 1)),
                    None => {}
                }
            }
            if self.pending.len() > 500 {
                // Prediction is failing broadly (new mailbox, layout change): rescan.
                self.pending.clear();
                self.full_build(conn, false)?;
                return Ok(0);
            }
            let version_dir = self.store.version_dir.clone();
            let rows: Vec<DocRow> =
                found.par_iter().map(|(id, mb, loc)| build_doc_guarded(&version_dir, *id, *mb, loc)).collect();
            indexed = rows.len();
            write_docs(conn, &rows, true)?;
        }
        if new_max > max_id {
            set_meta(conn, "max_id", &new_max.to_string())?;
        }
        if self.last_reconcile.elapsed() > Duration::from_secs(600) {
            self.reconcile(conn)?;
        }
        self.refresh_count(conn)?;
        self.progress.last_sync.store(now_unix(), Ordering::Relaxed);
        Ok(indexed)
    }

    #[cfg(test)]
    pub fn reconcile_now(&mut self, conn: &mut Connection) -> Result<()> {
        self.reconcile(conn)
    }

    /// Drop index rows for deleted messages, follow moves, and pick up
    /// messages the incremental path missed (a file that came late, a
    /// message restored from the trash under its old id).
    fn reconcile(&mut self, conn: &mut Connection) -> Result<()> {
        if renumbered(conn, &self.store)? {
            tracing::warn!("index: Mail numbered its messages anew, rebuilding");
            self.pending.clear();
            return self.full_build(conn, true);
        }
        let live: HashMap<i64, i64> = self.store.live_messages(conn)?.into_iter().collect();
        let docs: HashMap<i64, i64> = {
            let mut stmt = conn.prepare("SELECT id, mailbox FROM docs")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let mut gone = vec![];
        let mut moved = vec![];
        for (id, mb) in &docs {
            match live.get(id) {
                None => gone.push(*id),
                Some(m) if m != mb => moved.push((*id, *m)),
                _ => {}
            }
        }
        delete_docs(conn, &gone)?;
        if !moved.is_empty() {
            let tx = conn.transaction()?;
            {
                let mut up = tx.prepare_cached("UPDATE docs SET mailbox = ?2, path = '' WHERE id = ?1")?;
                for (id, mb) in &moved {
                    up.execute(params![id, mb])?;
                }
            }
            tx.commit()?;
        }
        self.last_reconcile = Instant::now();
        let unlocated: HashSet<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM unlocated")?;
            stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
        };
        let pending: HashSet<i64> = self.pending.iter().map(|(id, _, _)| *id).collect();
        let missed = live.keys().any(|id| !docs.contains_key(id) && !unlocated.contains(id) && !pending.contains(id));
        if missed {
            return self.full_build(conn, false);
        }
        self.retry_unlocated(conn, &live, &docs, &unlocated)
    }

    /// Messages whose file was missing at the last walk: Mail downloads a
    /// body later, so each is looked for where its file should be at every
    /// reconcile, and the store is walked again once WALK_EVERY has passed.
    /// Rows of messages gone since are dropped.
    fn retry_unlocated(
        &mut self,
        conn: &mut Connection,
        live: &HashMap<i64, i64>,
        docs: &HashMap<i64, i64>,
        unlocated: &HashSet<i64>,
    ) -> Result<()> {
        let gone: Vec<i64> =
            unlocated.iter().filter(|id| !live.contains_key(id) || docs.contains_key(id)).copied().collect();
        if !gone.is_empty() {
            let tx = conn.transaction()?;
            {
                let mut del = tx.prepare_cached("DELETE FROM unlocated WHERE id = ?1")?;
                for id in &gone {
                    del.execute([id])?;
                }
            }
            tx.commit()?;
        }
        let late: Vec<(i64, i64)> = unlocated
            .iter()
            .filter_map(|id| live.get(id).map(|mb| (*id, *mb)))
            .filter(|(id, _)| !docs.contains_key(id))
            .collect();
        if late.is_empty() {
            return Ok(());
        }
        let mut found: Vec<(i64, i64, Located)> = vec![];
        if late.len() <= PREDICT_LIMIT {
            let roots = self.data_roots(conn)?;
            found = late
                .iter()
                .filter_map(|&(id, mb)| {
                    roots.get(&mb).and_then(|root| emlx::predict(root, id)).map(|loc| (id, mb, loc))
                })
                .collect();
        }
        if !found.is_empty() {
            let version_dir = self.store.version_dir.clone();
            let rows: Vec<DocRow> =
                found.par_iter().map(|(id, mb, loc)| build_doc_guarded(&version_dir, *id, *mb, loc)).collect();
            write_docs(conn, &rows, true)?;
            let tx = conn.transaction()?;
            {
                let mut del = tx.prepare_cached("DELETE FROM unlocated WHERE id = ?1")?;
                for (id, ..) in &found {
                    del.execute([id])?;
                }
            }
            tx.commit()?;
        }
        let walked_long_ago = get_meta(conn, "last_walk")
            .and_then(|s| s.parse::<i64>().ok())
            .is_none_or(|t| now_unix() - t >= WALK_EVERY);
        if found.len() < late.len() && walked_long_ago {
            // full_build walks the store: the last walk is old.
            return self.full_build(conn, false);
        }
        Ok(())
    }

    /// Background loop used by the MCP server.
    pub fn run_forever(mut self) {
        background_priority();
        if !self.cfg.index_enabled {
            self.progress.phase.store(PHASE_DISABLED, Ordering::Relaxed);
            return;
        }
        // One Timbre keeps the index; another (a second Claude window, an
        // update starting before the old one stopped) reads it meanwhile.
        let _lock = loop {
            match self.take_lock() {
                Some(lock) => break lock,
                None => {
                    self.follow();
                    std::thread::sleep(Duration::from_secs(15));
                }
            }
        };
        // A busy or locked file now is not a reason to give up for the session.
        let (mut conn, mut mode) = loop {
            match open_with_mode(&self.cfg, &self.store, true) {
                Ok(c) => break c,
                Err(e) => {
                    self.progress.fail(&e);
                    std::thread::sleep(Duration::from_secs(30));
                }
            }
        };
        let built = get_meta(&conn, "built").is_some();
        let _ = self.refresh_count(&conn);
        if built {
            self.progress.built.store(true, Ordering::Relaxed);
            self.progress.phase.store(PHASE_READY, Ordering::Relaxed);
        }
        if let Err(e) = guarded("index build", || self.full_build(&mut conn, false)) {
            tracing::warn!("index build failed: {e:#}");
            self.progress.fail(&e);
        }
        // Messages read by an older Timbre are read again, newest first.
        let outdated = self.outdated(&conn).unwrap_or(0);
        self.progress.refresh_total.store(outdated, Ordering::Relaxed);
        self.progress.refresh_left.store(outdated, Ordering::Relaxed);
        let mut rereading = outdated > 0;
        // Mail's commits change the attached database's data_version: polling
        // it costs a page read, so new mail is indexed within seconds instead
        // of at the next 15 s tick. Pending files are retried every 15 s, and
        // a sync still runs every minute in any case.
        let mut last_version: Option<i64> = None;
        let mut last_sync = Instant::now();
        let mut failures = 0;
        loop {
            std::thread::sleep(if rereading { Duration::from_millis(100) } else { POLL });
            if rereading {
                match guarded("index re-read", || self.reread_batch(&mut conn, REFRESH_BATCH)) {
                    Ok(0) => {
                        rereading = false;
                        self.progress.refresh_left.store(0, Ordering::Relaxed);
                        let _ = conn
                            .execute_batch("INSERT INTO fts(fts) VALUES('optimize'); PRAGMA wal_checkpoint(TRUNCATE);");
                        tracing::info!("index: messages read again");
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("index re-read failed: {e:#}");
                        rereading = false;
                        self.progress.refresh_left.store(0, Ordering::Relaxed);
                    }
                }
            }
            let version = conn.query_row("PRAGMA mail.data_version", [], |r| r.get::<_, i64>(0)).ok();
            let due = version.is_none()
                || version != last_version
                || (!self.pending.is_empty() && last_sync.elapsed() >= Duration::from_secs(15))
                || last_sync.elapsed() >= Duration::from_secs(60);
            if !due {
                continue;
            }
            last_version = version;
            last_sync = Instant::now();
            if mode == store::OpenMode::Immutable {
                // Opened while Mail was closed: switch to a live read-only
                // view as soon as Mail runs again (immutable reads would
                // then be unreliable).
                if let Ok((c, store::OpenMode::ReadOnly)) = open_with_mode(&self.cfg, &self.store, true) {
                    conn = c;
                    mode = store::OpenMode::ReadOnly;
                }
            }
            if let Err(e) = guarded("index sync", || self.sync(&mut conn)) {
                tracing::warn!("index sync failed: {e:#}");
                failures += 1;
                if failures >= 3 {
                    // Searches still use what is indexed; say that new mail is not.
                    self.progress.set_error(Some(format!("new mail is not being indexed: {e:#}")));
                }
                // Mail may have been upgraded or the connection broken: reopen.
                match open_with_mode(&self.cfg, &self.store, true) {
                    Ok((c, m)) => {
                        conn = c;
                        mode = m;
                    }
                    Err(e) => self.progress.fail(&e),
                }
            } else {
                if failures > 0 {
                    self.progress.set_error(None);
                }
                failures = 0;
                if self.progress.phase.load(Ordering::Relaxed) == PHASE_FAILED {
                    self.progress.phase.store(PHASE_READY, Ordering::Relaxed);
                }
            }
        }
    }
}

/// Finds message files: the index first, then a cached full scan.
/// When the last full scan ran, and what it found.
type ScanCache = (Instant, Arc<HashMap<i64, Located>>);

pub struct Locator {
    scan: Mutex<Option<ScanCache>>,
}

impl Locator {
    pub fn new() -> Self {
        Locator { scan: Mutex::new(None) }
    }

    /// Where the message's file is, from the index or from its mailbox's
    /// folder: no walk of Mail's store, so it is quick.
    pub fn locate_fast(&self, conn: &Connection, store: &Store, id: i64) -> Option<Located> {
        let row: Option<(String, i64, i64)> = conn
            .query_row("SELECT path, partial, mailbox FROM docs WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()
            .ok()
            .flatten();
        if let Some((rel, partial, _)) = &row
            && !rel.is_empty()
        {
            let path = store.version_dir.join(rel);
            if path.is_file() {
                return Some(Located { path, partial: *partial != 0 });
            }
        }
        // Moved, or not indexed yet: predict from a message of the same mailbox.
        let mailbox: i64 = conn
            .query_row("SELECT mailbox FROM mail.messages WHERE ROWID = ?1", [id], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
            .or(row.map(|(_, _, mb)| mb))?;
        let sample: String = conn
            .query_row("SELECT path FROM docs WHERE mailbox = ?1 AND path != '' LIMIT 1", [mailbox], |r| r.get(0))
            .optional()
            .ok()
            .flatten()?;
        let root = emlx::data_root(&store.version_dir.join(sample))?;
        emlx::predict(&root, id)
    }

    /// The slow way: a walk of Mail's whole store, at most once a minute.
    /// Call it without holding the reader, so other tools are not kept waiting.
    pub fn locate_by_scan(&self, store: &Store, id: i64) -> Option<Located> {
        self.lookup_scan(store, id, false).or_else(|| self.lookup_scan(store, id, true))
    }

    fn lookup_scan(&self, store: &Store, id: i64, refresh: bool) -> Option<Located> {
        let mut guard = self.scan.lock().unwrap_or_else(|e| e.into_inner());
        let stale = guard.as_ref().is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(60));
        if guard.is_none() || (refresh && stale) {
            *guard = Some((Instant::now(), Arc::new(emlx::scan(&store.version_dir))));
        } else if refresh {
            return None;
        }
        guard.as_ref().and_then(|(_, map)| map.get(&id).cloned()).filter(|l| l.path.is_file())
    }
}
