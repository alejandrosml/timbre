//! End-to-end tests on the synthetic Mail store (see `fixture.rs`).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::app::{self, AttachmentParams, Ctx, ReadParams, ThreadParams};
use crate::fixture;
use crate::index::{self, Indexer, Progress};
use crate::search::{self, SearchParams};
use crate::store::{Role, Store};

struct Env {
    _dir: tempdir::Dir,
    ctx: Arc<Ctx>,
}

mod tempdir {
    pub struct Dir(pub std::path::PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    pub fn new(tag: &str) -> Dir {
        let p = std::env::temp_dir().join(format!("timbre-test-{tag}-{}", std::process::id()));
        Dir(p)
    }
}

fn setup(tag: &str, build_index: bool) -> Env {
    let dir = tempdir::new(tag);
    let cfg = fixture::build(&dir.0).expect("fixture");
    let store = Arc::new(Store::open(&cfg).expect("store"));
    let progress = Arc::new(Progress::default());
    let mut conn = index::open(&cfg, &store, true).expect("index");
    if build_index {
        Indexer::new(cfg.clone(), store.clone(), progress.clone()).full_build(&mut conn, false).expect("build");
    }
    drop(conn);
    let ctx = Arc::new(Ctx::new(cfg, store, progress).expect("ctx"));
    Env { _dir: dir, ctx }
}

fn search(env: &Env, p: SearchParams) -> search::SearchResult {
    let c = &env.ctx;
    search::search(&c.db(), &c.store, &c.cfg, &c.progress, &p).expect("search")
}

fn q(text: &str) -> SearchParams {
    SearchParams { query: Some(text.into()), ..Default::default() }
}

fn ids(r: &search::SearchResult) -> Vec<i64> {
    r.results.iter().map(|h| h.id).collect()
}

#[test]
fn store_reads_schema_accounts_and_roles() {
    let env = setup("store", false);
    let s = &env.ctx.store;
    assert_eq!(s.account_names(), vec!["iCloud".to_string()]);
    let roles: Vec<Role> = s.mailboxes().values().map(|m| m.role).collect();
    for r in [Role::Inbox, Role::Archive, Role::Junk, Role::Trash, Role::Sent] {
        assert!(roles.contains(&r), "missing {r:?}");
    }
    let (total, unread) = s.stats(&env.ctx.db()).unwrap();
    assert_eq!((total, unread), (6, 2));
}

#[test]
fn full_text_search_ranks_filters_and_skips_junk() {
    let env = setup("fts", true);
    assert!(env.ctx.progress.ready());
    assert_eq!(env.ctx.progress.docs.load(Ordering::Relaxed), 5, "junk is not indexed");

    let r = search(&env, q("facture"));
    assert_eq!(r.engine, "fulltext");
    assert_eq!(ids(&r), vec![101], "junk must be skipped by default");
    assert!(r.results[0].snippet.as_deref().unwrap_or("").contains("«facture»"));

    // Accent-insensitive both ways.
    assert_eq!(ids(&search(&env, q("reunion"))), vec![104]);
    assert_eq!(ids(&search(&env, q("RÉUNION"))), vec![104]);
    // Quoted history is not indexed: only the original matches.
    assert_eq!(ids(&search(&env, q("budget"))), vec![102]);
    // Operators.
    assert_eq!(ids(&search(&env, q("from:ann"))), vec![102]);
    assert_eq!(ids(&search(&env, q("is:unread"))), vec![101]);
    assert_eq!(ids(&search(&env, q("thursday -budget"))), vec![103]);
    assert_eq!(ids(&search(&env, q("contrat has:attachment"))), vec![1234]);
    // Attachment names are searchable.
    assert_eq!(ids(&search(&env, q("releve"))), vec![101]);
    // Explicit mailbox reaches Junk (metadata of the junk message only).
    let r = search(&env, SearchParams { mailbox: Some("junk".into()), ..Default::default() });
    assert_eq!(ids(&r), vec![105]);
    // Unknown mailbox explains itself.
    let c = &env.ctx;
    let err = search::search(
        &c.db(),
        &c.store,
        &c.cfg,
        &c.progress,
        &SearchParams { mailbox: Some("Nope".into()), ..Default::default() },
    );
    assert!(err.unwrap_err().contains("list_mailboxes"));
    // Pagination, message by message.
    let r = search(&env, SearchParams { limit: Some(2), group_threads: Some(false), ..Default::default() });
    assert_eq!(r.results.len(), 2);
    assert_eq!(r.next_offset, Some(2));
    assert_eq!(r.total, 5);
    // By conversation (the default): 102 and its reply 103 come as one result.
    let all = search(&env, SearchParams { limit: Some(10), ..Default::default() });
    let thread: Vec<_> = all.results.iter().filter(|h| h.id == 102 || h.id == 103).collect();
    assert_eq!(thread.len(), 1);
    assert_eq!(thread[0].thread_matches, Some(1));
    assert_eq!(all.results.len(), 4);
}

#[test]
fn metadata_search_while_index_builds() {
    let env = setup("meta", false);
    let r = search(&env, q("facture"));
    assert_eq!(r.engine, "metadata");
    assert_eq!(ids(&r), vec![101]);
    assert!(r.notice.is_some());
    let r = search(&env, SearchParams { include_junk_and_trash: Some(true), ..q("facture") });
    assert_eq!(ids(&r), vec![101, 105], "newest first");
    assert!(r.results[0].warning.is_none(), "no auth data without the index");
}

#[test]
fn read_mail_hides_injection_and_flags_spoofing() {
    let env = setup("read", true);
    let v = app::read_mail(&env.ctx, &ReadParams { id: 101, max_chars: None, strip_quotes: None }).unwrap();
    assert!(v.body.contains("42,50 €"), "{}", v.body);
    assert!(!v.body.contains("Ignore all previous"), "hidden text leaked");
    assert!(v.hidden_text_removed > 40);
    assert_eq!(v.attachments.len(), 1);
    assert_eq!(v.attachments[0].name, "releve.csv");
    assert!(v.body.starts_with("⟦untrusted "), "body is fenced");
    // The hidden instructions are gone from the body but reported.
    assert!(v.warnings.iter().any(|w| w.contains("hidden text contains \"ignore all previous\"")), "{:?}", v.warnings);
    assert!(v.warnings.iter().any(|w| w.contains("first message from this sender")), "{:?}", v.warnings);
    // A known correspondent is not a first contact.
    let v = app::read_mail(&env.ctx, &ReadParams { id: 102, max_chars: None, strip_quotes: None }).unwrap();
    assert!(!v.warnings.iter().any(|w| w.contains("first message")), "{:?}", v.warnings);

    let v = app::read_mail(&env.ctx, &ReadParams { id: 105, max_chars: None, strip_quotes: None }).unwrap();
    assert!(v.warnings.iter().any(|w| w.contains("DMARC")), "{:?}", v.warnings);
    assert!(v.warnings.iter().any(|w| w.contains("display name")), "{:?}", v.warnings);
    assert!(v.warnings.iter().any(|w| w.contains("other.ru")), "{:?}", v.warnings);

    let r = search(&env, SearchParams { include_junk_and_trash: Some(true), ..q("impayee") });
    assert_eq!(ids(&r), Vec::<i64>::new(), "junk is not full-text indexed");

    assert!(app::read_mail(&env.ctx, &ReadParams { id: 999_999, max_chars: None, strip_quotes: None }).is_err());
}

#[test]
fn thread_attachments_and_mailboxes() {
    let env = setup("thread", true);
    let t =
        app::get_thread(&env.ctx, &ThreadParams { id: 103, max_messages: None, max_chars_per_message: None }).unwrap();
    assert_eq!(t.count, 2);
    assert_eq!(t.messages[0].id, 102);
    assert!(t.messages[1].text.contains("\nThursday works for me.\n⟦/untrusted"), "{}", t.messages[1].text);

    let a = app::read_attachment(&env.ctx, &AttachmentParams { id: 101, index: 0, max_chars: None }).unwrap();
    assert!(a.text.contains("42,50"));
    // Partial message: the body lives next to the .partial.emlx.
    let a = app::read_attachment(&env.ctx, &AttachmentParams { id: 1234, index: 0, max_chars: None }).unwrap();
    assert!(a.text.contains("Contrat de vente"), "{}", a.text);

    let s1 = app::save_attachment(&env.ctx, &AttachmentParams { id: 101, index: 0, max_chars: None }).unwrap();
    let s2 = app::save_attachment(&env.ctx, &AttachmentParams { id: 101, index: 0, max_chars: None }).unwrap();
    assert!(s1.path.ends_with("releve.csv"));
    assert!(s2.path.ends_with("releve (1).csv"), "never overwrite: {}", s2.path);

    let m = app::list_mailboxes(&env.ctx).unwrap();
    assert_eq!(m.accounts.len(), 1);
    assert_eq!(m.accounts[0].account, "iCloud");
    assert_eq!(m.accounts[0].mailboxes[0].name, "INBOX");
    assert_eq!(m.accounts[0].mailboxes[0].unread, 1);
}

#[test]
fn incremental_sync_adds_and_removes() {
    let env = setup("sync", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());

    // A new message arrives in the inbox.
    let v10 = store.version_dir.clone();
    let mail = rusqlite::Connection::open(store.db_path.clone()).unwrap();
    let mut m = fixture::messages().remove(1);
    m.id = 1300;
    m.subject = "Kickoff";
    m.raw = "From: Bob <bob@example.com>\r\nSubject: Kickoff\r\nMessage-ID: <k1300@example.com>\r\n\r\nKickoff zebra agenda\r\n".into();
    fixture::write_message(&v10, &mail, &m).unwrap();
    let n = indexer.sync(&mut conn).unwrap();
    assert_eq!(n, 1);
    assert_eq!(ids(&search(&env, q("zebra"))), vec![1300]);

    // Index and stored text agree (FTS5 re-tokenizes the stored text).
    let check = "INSERT INTO fts(fts, rank) VALUES('integrity-check', 1)";
    conn.execute(check, []).unwrap();

    // It is deleted: a reconcile pass drops it from the index itself.
    mail.execute("UPDATE messages SET deleted = 1 WHERE ROWID = 1300", []).unwrap();
    indexer.reconcile_now(&mut conn).unwrap();
    assert!(ids(&search(&env, q("zebra"))).is_empty());
    let left: i64 = conn.query_row("SELECT count(*) FROM fts WHERE fts MATCH 'zebra'", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
    let texts: i64 = conn.query_row("SELECT count(*) FROM texts WHERE id = 1300", [], |r| r.get(0)).unwrap();
    assert_eq!(texts, 0);
    conn.execute(check, []).unwrap();
}

#[test]
fn status_reports_index_and_settings() {
    let env = setup("status", true);
    let s = app::status(&env.ctx).unwrap();
    assert_eq!(s.messages, 6);
    assert_eq!(s.index.state, "ready");
    assert!(!s.sending_enabled);
}

#[test]
fn copies_in_several_mailboxes_are_shown_once() {
    // Gmail shows one message in INBOX and in All Mail (here: Archive).
    let env = setup("dedupe", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());
    let mail = rusqlite::Connection::open(store.db_path.clone()).unwrap();
    let mut copy = fixture::messages().remove(1); // 102, INBOX, <m102@example.com>
    copy.id = 1400;
    copy.mailbox = "Archive";
    fixture::write_message(&store.version_dir, &mail, &copy).unwrap();
    assert_eq!(indexer.sync(&mut conn).unwrap(), 1);

    let r = search(&env, q("meeting"));
    let found: Vec<i64> = ids(&r).into_iter().filter(|id| *id == 102 || *id == 1400).collect();
    assert_eq!(found, vec![102], "one result, from the inbox");
    // A search limited to the archive still finds its copy.
    let r = search(&env, SearchParams { mailbox: Some("archive".into()), ..q("meeting") });
    assert_eq!(ids(&r), vec![1400]);
}

#[test]
fn people_the_user_wrote_to_rank_first() {
    let env = setup("affinity", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());
    let mail = rusqlite::Connection::open(store.db_path.clone()).unwrap();
    // Same words, same date; Ann is a correspondent (message 103 was sent to her).
    for (id, from) in [(1501, ("News Bot", "news@shop.example")), (1502, ("Ann Lee", "ann@example.com"))] {
        let mut m = fixture::messages().remove(1);
        m.id = id;
        m.from = from;
        m.subject = "Quarterly budget";
        m.conversation = id;
        m.raw = format!(
            "From: {} <{}>\r\nTo: moi@icloud.com\r\nSubject: Quarterly budget\r\nMessage-ID: <q{id}@example.com>\r\n\r\nThe quarterly budget review.\r\n",
            from.0, from.1
        );
        fixture::write_message(&store.version_dir, &mail, &m).unwrap();
    }
    assert_eq!(indexer.sync(&mut conn).unwrap(), 2);
    assert_eq!(ids(&search(&env, q("quarterly budget"))), vec![1502, 1501]);
}

fn integrity(conn: &rusqlite::Connection) {
    conn.execute("INSERT INTO fts(fts, rank) VALUES('integrity-check', 1)", []).expect("FTS index consistent");
}

#[test]
fn interrupted_rebuilds_leave_a_usable_index() {
    let env = setup("interrupt", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());

    // A forced rebuild that fails halfway through emptying the index rolls
    // back as a whole.
    conn.execute_batch("CREATE TEMP TRIGGER stop BEFORE DELETE ON main.texts BEGIN SELECT RAISE(ABORT, 'stop'); END;")
        .unwrap();
    assert!(indexer.full_build(&mut conn, true).is_err());
    conn.execute_batch("DROP TRIGGER temp.stop;").unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    integrity(&conn);

    // Tables left disagreeing (as an older version could) are detected and
    // the index is rebuilt instead of failing on every start.
    conn.execute_batch("DELETE FROM docs; DELETE FROM texts;").unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    integrity(&conn);
    assert_eq!(ids(&search(&env, q("facture"))), vec![101]);
    let automerge: i64 = conn.query_row("SELECT v FROM fts_config WHERE k = 'automerge'", [], |r| r.get(0)).unwrap();
    assert_eq!(automerge, 4);
}

#[test]
fn index_file_is_never_deleted() {
    let env = setup("indexfile", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let path = cfg.index_path();

    // An outdated format is rebuilt in place: connections already open keep
    // working on the same file.
    let reader = index::open(&cfg, &store, false).unwrap();
    {
        let w = index::open(&cfg, &store, true).unwrap();
        index::set_meta(&w, "schema", "0").unwrap();
    }
    let w = index::open(&cfg, &store, true).unwrap();
    assert_eq!(index::get_meta(&w, "schema").as_deref(), Some(index::SCHEMA_VERSION));
    index::set_meta(&w, "probe", "same file").unwrap();
    let seen: String = reader.query_row("SELECT value FROM meta WHERE key = 'probe'", [], |r| r.get(0)).unwrap();
    assert_eq!(seen, "same file");
    drop((w, reader));

    // Someone else's database at that path is refused, not deleted.
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let other = rusqlite::Connection::open(&path).unwrap();
    other.execute_batch("CREATE TABLE notes(t TEXT); INSERT INTO notes VALUES('precious');").unwrap();
    drop(other);
    let err = index::open(&cfg, &store, true).unwrap_err();
    assert!(format!("{err:#}").contains("not a Timbre index"), "{err:#}");
    let kept: i64 =
        rusqlite::Connection::open(&path).unwrap().query_row("SELECT count(*) FROM notes", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, 1);
}

#[test]
fn paging_is_stable_when_filters_thin_a_broad_query() {
    let env = setup("paging", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut ix = index::open(&cfg, &store, true).unwrap();
    let mut mail = rusqlite::Connection::open(&store.db_path).unwrap();
    let inbox: i64 = mail.query_row("SELECT ROWID FROM mailboxes WHERE url LIKE '%/INBOX'", [], |r| r.get(0)).unwrap();
    mail.execute("INSERT INTO addresses(address, comment) VALUES('rare@x.com', 'Rare')", []).unwrap();
    let rare = mail.last_insert_rowid();
    mail.execute("INSERT INTO addresses(address, comment) VALUES('bulk@y.com', 'Bulk')", []).unwrap();
    let bulk = mail.last_insert_rowid();
    let now = crate::util::now_unix();
    {
        let mt = mail.transaction().unwrap();
        let it = ix.transaction().unwrap();
        let add = |id: i64, sender: i64, subject: &str, body: &str, date: i64| {
            mt.execute("INSERT OR IGNORE INTO subjects(subject) VALUES(?1)", [subject]).unwrap();
            let s: i64 =
                mt.query_row("SELECT ROWID FROM subjects WHERE subject = ?1", [subject], |r| r.get(0)).unwrap();
            mt.execute(
                "INSERT INTO messages(ROWID, sender, subject_prefix, subject, date_sent, date_received, mailbox, flags, read, flagged, deleted, size, conversation_id)
                 VALUES(?1, ?2, '', ?3, ?4, ?4, ?5, 1, 1, 0, 0, 100, 0)",
                rusqlite::params![id, sender, s, date, inbox],
            )
            .unwrap();
            it.execute(
                "INSERT INTO docs(id, mailbox, path, status) VALUES(?1, ?2, '', 0)",
                rusqlite::params![id, inbox],
            )
            .unwrap();
            it.execute(
                "INSERT INTO texts(id, subject, people, body, attachments) VALUES(?1, ?2, '', ?3, '')",
                rusqlite::params![id, subject, index::pack(body)],
            )
            .unwrap();
            it.execute(
                "INSERT INTO fts(rowid, subject, people, body, attachments) VALUES(?1, ?2, '', ?3, '')",
                rusqlite::params![id, subject, body],
            )
            .unwrap();
        };
        for id in 20_000..20_300 {
            add(id, rare, "zork report", "monthly statement", now - 400 * 86_400);
        }
        for id in 30_000..40_000 {
            add(id, bulk, "news", "zork news filler", now - 10 * 86_400);
        }
        for id in 40_000..40_050 {
            add(id, rare, "update", "the zork thing", now - 86_400);
        }
        it.commit().unwrap();
        mt.commit().unwrap();
    }
    let mut seen = vec![];
    let mut offset = 0;
    loop {
        let p = SearchParams { limit: Some(20), offset: Some(offset), ..q("zork from:rare") };
        let r = search(&env, p);
        seen.extend(ids(&r));
        match r.next_offset {
            Some(o) => offset = o,
            None => break,
        }
    }
    let mut distinct = seen.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), seen.len(), "a page repeated results");
    assert_eq!(distinct.len(), 350, "results were skipped");
}

#[test]
fn restarts_do_not_walk_the_whole_store() {
    let env = setup("restart", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());
    let walked = |conn: &rusqlite::Connection| index::get_meta(conn, "last_walk");
    assert!(walked(&conn).is_some(), "the first build walks the store");

    // Nothing missing: no walk.
    index::set_meta(&conn, "last_walk", "12345").unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    assert_eq!(walked(&conn).as_deref(), Some("12345"));

    // A new message in a known mailbox is found from that mailbox's folder.
    let mail = rusqlite::Connection::open(store.db_path.clone()).unwrap();
    let mut m = fixture::messages().remove(1);
    m.id = 1600;
    m.raw =
        "From: Bob <bob@example.com>\r\nSubject: Okapi\r\nMessage-ID: <o1600@example.com>\r\n\r\nokapi sighting\r\n"
            .into();
    fixture::write_message(&store.version_dir, &mail, &m).unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    assert_eq!(walked(&conn).as_deref(), Some("12345"));
    assert_eq!(ids(&search(&env, q("okapi"))), vec![1600]);

    // A message whose file Mail has not downloaded: one walk, then remembered.
    let inbox: i64 = mail.query_row("SELECT ROWID FROM mailboxes WHERE url LIKE '%/INBOX'", [], |r| r.get(0)).unwrap();
    mail.execute(
        "INSERT INTO messages(ROWID, sender, subject_prefix, subject, date_sent, date_received, mailbox, flags, read, flagged, deleted, size, conversation_id)
         VALUES(1700, 1, '', 1, 1793000000, 1793000000, ?1, 0, 0, 0, 0, 10, 1700)",
        [inbox],
    )
    .unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    let after_walk = walked(&conn);
    assert_ne!(after_walk.as_deref(), Some("12345"), "an unknown missing file triggers a walk");
    let unlocated: i64 = conn.query_row("SELECT count(*) FROM unlocated WHERE id = 1700", [], |r| r.get(0)).unwrap();
    assert_eq!(unlocated, 1);
    index::set_meta(&conn, "last_walk", &(crate::util::now_unix() - 60).to_string()).unwrap();
    let before = walked(&conn);
    indexer.full_build(&mut conn, false).unwrap();
    assert_eq!(walked(&conn), before, "a known missing file does not trigger another walk");
}

#[test]
fn filters_ignore_accents_and_case() {
    let env = setup("folding", true);
    // "Reunión del comité" in Archive, from Ana.
    let r = search(
        &env,
        SearchParams { subject: Some("REUNION".into()), mailbox: Some("archive".into()), ..Default::default() },
    );
    assert_eq!(ids(&r), vec![104]);
    let r = search(&env, SearchParams { subject: Some("comite".into()), ..Default::default() });
    assert_eq!(ids(&r), vec![104]);
}
