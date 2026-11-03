//! Timbre's own full-text index (SQLite FTS5), kept next to Mail's data.
//!
//! One row per message, keyed by the Envelope Index ROWID, so searches join
//! the index with Mail's live metadata. Text is accent-insensitive
//! (`unicode61 remove_diacritics 2`) and the body is indexed without quoted
//! history. The index is built in parallel on first run and then kept up to
//! date incrementally.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;

use crate::config::Config;
use crate::emlx::{self, Located};
use crate::mime;
use crate::quote;
use crate::store::{self, Store};
use crate::util::{clip, now_unix};

pub const SCHEMA_VERSION: &str = "1";
/// Longest body text indexed per message (characters).
const MAX_INDEXED_BODY: usize = 32_000;
const CHUNK: usize = 256;

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
}

impl Progress {
    pub fn ready(&self) -> bool {
        self.phase.load(Ordering::Relaxed) == PHASE_READY
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
        ProgressView {
            state: state.to_string(),
            indexed_messages: self.docs.load(Ordering::Relaxed),
            percent: (phase == PHASE_INDEXING && total > 0).then(|| done * 100 / total),
            last_build_seconds: (secs > 0).then_some(secs),
            error: self.error.lock().ok().and_then(|e| e.clone()),
        }
    }

    fn fail(&self, e: &anyhow::Error) {
        self.phase.store(PHASE_FAILED, Ordering::Relaxed);
        if let Ok(mut slot) = self.error.lock() {
            *slot = Some(format!("{e:#}"));
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
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -65536;
         PRAGMA mmap_size = 268435456;",
    )?;
    if writable {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let version: Option<String> = conn.query_row("SELECT value FROM meta WHERE key='schema'", [], |r| r.get(0)).optional()?;
        let mail_dir: Option<String> = conn.query_row("SELECT value FROM meta WHERE key='mail_dir'", [], |r| r.get(0)).optional()?;
        let current_dir = store.version_dir.to_string_lossy().to_string();
        if version.as_deref() != Some(SCHEMA_VERSION) || mail_dir.as_deref().is_some_and(|d| d != current_dir) {
            conn.execute_batch("DROP TABLE IF EXISTS docs; DROP TABLE IF EXISTS fts; DELETE FROM meta;")?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS docs(
                 id INTEGER PRIMARY KEY,
                 mailbox INTEGER NOT NULL DEFAULT 0,
                 path TEXT NOT NULL DEFAULT '',
                 partial INTEGER NOT NULL DEFAULT 0,
                 msgid TEXT,
                 auth TEXT,
                 hidden INTEGER NOT NULL DEFAULT 0,
                 status INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS docs_msgid ON docs(msgid);
             CREATE INDEX IF NOT EXISTS docs_mailbox ON docs(mailbox);
             CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
                 subject, people, body, attachments,
                 tokenize = 'unicode61 remove_diacritics 2',
                 prefix = '2 3'
             );",
        )?;
        set_meta(&conn, "schema", SCHEMA_VERSION)?;
        set_meta(&conn, "mail_dir", &current_dir)?;
    }
    let mode = store::attach(&conn, &store.db_path)?;
    if !writable {
        conn.execute_batch("PRAGMA query_only = ON;")?;
    }
    Ok((conn, mode))
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
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
    pub attachments: String,
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
        attachments: String::new(),
    };
    let parsed = emlx::read_message(&loc.path).ok().and_then(|raw| mime::parse(&raw));
    if let Some(p) = parsed {
        let (body, _) = clip(&quote::strip_quotes(&p.body), MAX_INDEXED_BODY);
        row.status = 0;
        row.subject = p.subject.clone();
        row.people = people_of(&p);
        row.body = body;
        row.attachments = p.attachments.iter().filter(|a| !a.inline).map(|a| a.name.as_str()).collect::<Vec<_>>().join(" ");
        row.msgid = p.message_id.clone();
        row.auth = (!p.auth.is_empty()).then(|| p.auth.compact());
        row.hidden = (p.hidden_chars + p.invisible_chars) as i64;
    }
    row
}

fn write_docs(conn: &mut Connection, rows: &[DocRow], replace: bool) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut ins_doc = tx.prepare_cached(
            "INSERT OR REPLACE INTO docs(id, mailbox, path, partial, msgid, auth, hidden, status) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        let mut del_fts = tx.prepare_cached("DELETE FROM fts WHERE rowid = ?1")?;
        let mut ins_fts = tx.prepare_cached("INSERT INTO fts(rowid, subject, people, body, attachments) VALUES(?1, ?2, ?3, ?4, ?5)")?;
        for r in rows {
            ins_doc.execute(params![r.id, r.mailbox, r.path, r.partial as i64, r.msgid, r.auth, r.hidden, r.status])?;
            if replace {
                del_fts.execute([r.id])?;
            }
            if r.status == 0 {
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
        let mut d1 = tx.prepare_cached("DELETE FROM docs WHERE id = ?1")?;
        let mut d2 = tx.prepare_cached("DELETE FROM fts WHERE rowid = ?1")?;
        for id in ids {
            d1.execute([id])?;
            d2.execute([id])?;
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

    fn excluded_mailboxes(&self) -> HashSet<i64> {
        self.store.mailboxes_with_roles(&self.cfg.excluded_roles).into_iter().collect()
    }

    /// Full pass: index every message not yet indexed, drop deleted ones.
    pub fn full_build(&mut self, conn: &mut Connection, force: bool) -> Result<()> {
        let started = Instant::now();
        self.progress.phase.store(PHASE_SCANNING, Ordering::Relaxed);
        self.store.reload_mailboxes(conn)?;
        if force {
            conn.execute_batch("DELETE FROM docs; DELETE FROM fts;")?;
        }
        let excluded = self.excluded_mailboxes();
        let live = self.store.live_messages(conn)?;
        let max_live = live.iter().map(|(id, _)| *id).max().unwrap_or(0);
        let wanted: HashMap<i64, i64> = live.into_iter().filter(|(_, mb)| !excluded.contains(mb)).collect();
        let files = emlx::scan(&self.store.version_dir);
        let existing: HashMap<i64, i64> = {
            let mut stmt = conn.prepare("SELECT id, status FROM docs")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
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
        for chunk in todo.chunks(CHUNK) {
            let rows: Vec<DocRow> = chunk.par_iter().map(|(id, mb, loc)| build_doc(&version_dir, *id, *mb, loc)).collect();
            write_docs(conn, &rows, false)?;
            self.progress.done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            self.progress.docs.fetch_add(rows.iter().filter(|r| r.status == 0).count() as u64, Ordering::Relaxed);
        }
        delete_docs(conn, &stale)?;
        if !todo.is_empty() {
            conn.execute_batch("INSERT INTO fts(fts) VALUES('optimize');")?;
        }
        set_meta(conn, "max_id", &max_live.to_string())?;
        set_meta(conn, "built", "1")?;
        set_meta(conn, "last_full", &now_unix().to_string())?;
        self.refresh_count(conn)?;
        self.progress.build_secs.store(started.elapsed().as_secs().max(1), Ordering::Relaxed);
        self.progress.last_sync.store(now_unix(), Ordering::Relaxed);
        self.progress.phase.store(PHASE_READY, Ordering::Relaxed);
        self.last_reconcile = Instant::now();
        Ok(())
    }

    fn refresh_count(&self, conn: &Connection) -> Result<()> {
        let n: i64 = conn.query_row("SELECT count(*) FROM docs WHERE status = 0", [], |r| r.get(0))?;
        self.progress.docs.store(n as u64, Ordering::Relaxed);
        Ok(())
    }

    /// Data directory of each mailbox's store, learned from indexed paths.
    fn data_roots(&self, conn: &Connection) -> Result<HashMap<i64, PathBuf>> {
        let mut stmt = conn.prepare("SELECT mailbox, path FROM docs WHERE id IN (SELECT max(id) FROM docs WHERE path != '' GROUP BY mailbox)")?;
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
        let excluded = self.excluded_mailboxes();
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
            if !excluded.contains(&mb) {
                candidates.push((id, mb, 0));
            }
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
            let rows: Vec<DocRow> = found.par_iter().map(|(id, mb, loc)| build_doc(&version_dir, *id, *mb, loc)).collect();
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

    /// Drop index rows for deleted messages and follow moves.
    fn reconcile(&mut self, conn: &mut Connection) -> Result<()> {
        let excluded = self.excluded_mailboxes();
        let live: HashMap<i64, i64> = self.store.live_messages(conn)?.into_iter().collect();
        let docs: Vec<(i64, i64)> = {
            let mut stmt = conn.prepare("SELECT id, mailbox FROM docs")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
        };
        let mut gone = vec![];
        let mut moved = vec![];
        for (id, mb) in docs {
            match live.get(&id) {
                None => gone.push(id),
                Some(m) if excluded.contains(m) => gone.push(id),
                Some(m) if *m != mb => moved.push((id, *m)),
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
        Ok(())
    }

    /// Background loop used by the MCP server.
    pub fn run_forever(mut self) {
        if !self.cfg.index_enabled {
            self.progress.phase.store(PHASE_DISABLED, Ordering::Relaxed);
            return;
        }
        let (mut conn, mut mode) = match open_with_mode(&self.cfg, &self.store, true) {
            Ok(c) => c,
            Err(e) => {
                self.progress.fail(&e);
                return;
            }
        };
        let built = get_meta(&conn, "built").is_some();
        let _ = self.refresh_count(&conn);
        if built {
            self.progress.phase.store(PHASE_READY, Ordering::Relaxed);
        }
        if let Err(e) = self.full_build(&mut conn, false) {
            tracing::warn!("index build failed: {e:#}");
            self.progress.fail(&e);
        }
        loop {
            std::thread::sleep(Duration::from_secs(15));
            if mode == store::OpenMode::Immutable {
                // Opened while Mail was closed: switch to a live read-only
                // view as soon as Mail runs again (immutable reads would
                // then be unreliable).
                if let Ok((c, store::OpenMode::ReadOnly)) = open_with_mode(&self.cfg, &self.store, true) {
                    conn = c;
                    mode = store::OpenMode::ReadOnly;
                }
            }
            if let Err(e) = self.sync(&mut conn) {
                tracing::warn!("index sync failed: {e:#}");
                // Mail may have been upgraded or the connection broken: reopen.
                match open_with_mode(&self.cfg, &self.store, true) {
                    Ok((c, m)) => {
                        conn = c;
                        mode = m;
                    }
                    Err(e) => self.progress.fail(&e),
                }
            } else if self.progress.phase.load(Ordering::Relaxed) == PHASE_FAILED {
                self.progress.phase.store(PHASE_READY, Ordering::Relaxed);
            }
        }
    }
}

/// Finds message files: the index first, then a cached full scan.
pub struct Locator {
    scan: Mutex<Option<(Instant, Arc<HashMap<i64, Located>>)>>,
}

impl Locator {
    pub fn new() -> Self {
        Locator { scan: Mutex::new(None) }
    }

    pub fn locate(&self, conn: &Connection, store: &Store, id: i64) -> Option<Located> {
        let row: Option<(String, i64, i64)> = conn
            .query_row("SELECT path, partial, mailbox FROM docs WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .optional()
            .ok()
            .flatten();
        if let Some((rel, partial, _)) = &row {
            if !rel.is_empty() {
                let path = store.version_dir.join(rel);
                if path.is_file() {
                    return Some(Located { path, partial: *partial != 0 });
                }
            }
        }
        self.from_scan(store, id, false).or_else(|| self.from_scan(store, id, true))
    }

    fn from_scan(&self, store: &Store, id: i64, refresh: bool) -> Option<Located> {
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
