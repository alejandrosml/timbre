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
    setup_with(tag, build_index, |_| {})
}

/// Like `setup`, with a chance to change the synthetic store first.
/// Mail keeps a folder for each mailbox it has: so does every test store,
/// once `prepare` added its mailboxes.
fn setup_with(tag: &str, build_index: bool, prepare: impl FnOnce(&crate::config::Config)) -> Env {
    let dir = tempdir::new(tag);
    let cfg = fixture::build(&dir.0).expect("fixture");
    prepare(&cfg);
    fixture::mail_folders(&cfg).expect("folders");
    setup_from(dir, cfg, build_index)
}

/// The test environment over a store already made.
fn setup_from(dir: tempdir::Dir, cfg: crate::config::Config, build_index: bool) -> Env {
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
    let (total, unread) = s.stats(&env.ctx.db(), &[]).unwrap();
    assert_eq!((total, unread), (6, 2));
    let skip = s.mailboxes_with_roles(&env.ctx.cfg.excluded_roles);
    assert_eq!(s.stats(&env.ctx.db(), &skip).unwrap(), (6, 1), "unread junk is not counted");
}

#[test]
fn full_text_search_ranks_filters_and_skips_junk() {
    let env = setup("fts", true);
    assert!(env.ctx.progress.ready());
    assert_eq!(env.ctx.progress.docs.load(Ordering::Relaxed), 6, "every mailbox is indexed, junk too");

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

    // Junk is indexed but left out of searches unless asked for.
    assert_eq!(ids(&search(&env, q("impayee"))), Vec::<i64>::new());
    let r = search(&env, SearchParams { include_junk_and_trash: Some(true), ..q("impayee") });
    assert_eq!(ids(&r), vec![105], "words in junk are found when asked");
    assert_eq!(ids(&search(&env, q("in:junk impayee"))), vec![105]);

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
fn attachment_counts_come_from_the_messages() {
    let env = setup("attachments", true);
    let with = |h: bool| {
        let p = SearchParams { has_attachments: Some(h), group_threads: Some(false), ..Default::default() };
        let mut r = ids(&search(&env, p));
        r.sort();
        r
    };
    // Mail says 63 (unknown) or counts inline images: Timbre counts what it read.
    assert_eq!(with(true), vec![101, 1234]);
    assert_eq!(with(false), vec![102, 103, 104]);
    let r = search(&env, SearchParams { group_threads: Some(false), ..Default::default() });
    let count = |id: i64| r.results.iter().find(|h| h.id == id).map(|h| h.attachments);
    assert_eq!((count(101), count(102), count(1234)), (Some(1), Some(0), Some(1)));
    let t =
        app::get_thread(&env.ctx, &ThreadParams { id: 103, max_messages: None, max_chars_per_message: None }).unwrap();
    assert!(t.messages.iter().all(|m| m.attachments == 0), "{:?}", t.messages);
}

#[test]
fn an_older_index_is_read_again_without_losing_search() {
    let env = setup("reread", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    // As left by Timbre 0.5.1: no attachment count, no parser version.
    {
        let conn = index::open(&cfg, &store, true).unwrap();
        conn.execute_batch("ALTER TABLE docs DROP COLUMN attachments; ALTER TABLE docs DROP COLUMN parser;").unwrap();
    }
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let outdated: i64 =
        conn.query_row("SELECT count(*) FROM docs WHERE parser < ?1", [index::PARSER_VERSION], |r| r.get(0)).unwrap();
    assert_eq!(outdated, 6, "the columns are added in place, rows marked as outdated");
    assert_eq!(ids(&search(&env, q("facture"))), vec![101], "searches work before the re-read");
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());
    let mut rounds = 0;
    while indexer.reread_batch(&mut conn, 2).unwrap() > 0 {
        rounds += 1;
        assert_eq!(ids(&search(&env, q("facture"))), vec![101], "and during it");
    }
    assert_eq!(rounds, 3, "6 messages, 2 at a time");
    let (left, counted): (i64, i64) = conn
        .query_row("SELECT sum(parser < ?1), sum(attachments IS NOT NULL) FROM docs", [index::PARSER_VERSION], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((left, counted), (0, 6));
    let fts: i64 = conn.query_row("SELECT count(*) FROM fts_docsize", [], |r| r.get(0)).unwrap();
    assert_eq!(fts, 6, "each message is in the full-text index once");
    assert_eq!(ids(&search(&env, q("reunion"))), vec![104]);
}

#[test]
fn an_index_read_by_the_previous_parser_is_read_again() {
    let env = setup("reread-previous", true);
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut conn = index::open(&cfg, &store, true).unwrap();
    // As left by Timbre 0.5.7, before HTML charsets were read as Mail reads them.
    conn.execute("UPDATE docs SET parser = ?1", [index::PARSER_VERSION - 1]).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());
    let mut read = 0;
    loop {
        let n = indexer.reread_batch(&mut conn, 4).unwrap();
        if n == 0 {
            break;
        }
        read += n;
        assert_eq!(ids(&search(&env, q("facture"))), vec![101], "searches work during the re-read");
    }
    assert_eq!(read, 6, "every message is read again once");
    let left: i64 =
        conn.query_row("SELECT count(*) FROM docs WHERE parser < ?1", [index::PARSER_VERSION], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
    let fts: i64 = conn.query_row("SELECT count(*) FROM fts_docsize", [], |r| r.get(0)).unwrap();
    assert_eq!(fts, 6, "each message is in the full-text index once");
}

#[test]
fn words_left_out_are_left_out() {
    let env = setup("negation", true);
    let all = |query: &str| {
        let mut r = ids(&search(&env, SearchParams { group_threads: Some(false), ..q(query) }));
        r.sort();
        r
    };
    // Only words to leave out, with the index ready.
    assert_eq!(all("is:unread -facture"), Vec::<i64>::new(), "101 is the unread one, and it is an invoice");
    assert_eq!(all("-budget"), vec![101, 103, 104, 1234]);
    assert_eq!(all("from:ann OR from:orange"), vec![101, 102]);
    assert_eq!(all("-from:ann has:attachment"), vec![101, 1234]);
    // Before the index exists: the same logic on subjects and senders.
    let env = setup("negation-noindex", false);
    let r = search(&env, SearchParams { group_threads: Some(false), ..q("facture -orange") });
    assert_eq!(r.engine, "metadata");
    assert_eq!(ids(&r), Vec::<i64>::new(), "Orange's invoice is left out");
    let mut r = ids(&search(&env, SearchParams { group_threads: Some(false), ..q("contrat OR reunion") }));
    r.sort();
    assert_eq!(r, vec![104, 1234]);
}

#[test]
fn chinese_and_japanese_words_are_found() {
    let env = setup_with("cjk", false, |cfg| {
        let mail = envelope_index(cfg);
        let msg = fixture::Msg {
            id: 7001,
            mailbox: "INBOX",
            from: ("田中", "tanaka@example.jp"),
            to: "moi@icloud.com",
            subject: "議事録",
            date: fixture::T0 - 100,
            read: true,
            conversation: 0,
            partial: false,
            raw: "From: tanaka@example.jp\r\nTo: moi@icloud.com\r\nSubject: =?utf-8?b?6K2w5LqL6Yyy?=\r\n\
                  Content-Type: text/plain; charset=utf-8\r\n\r\n会議の議事録を添付します。よろしくお願いします。\r\n"
                .to_string(),
        };
        fixture::write_message(&cfg.mail_root.join("V10"), &mail, &msg).unwrap();
    });
    let cfg = env.ctx.cfg.clone();
    let store = env.ctx.store.clone();
    let mut conn = index::open(&cfg, &store, true).unwrap();
    Indexer::new(cfg, store, env.ctx.progress.clone()).full_build(&mut conn, false).unwrap();
    for word in ["会議", "議事録", "添付"] {
        let r = search(&env, q(word));
        assert_eq!(ids(&r), vec![7001], "{word}");
    }
    let r = search(&env, q("会議"));
    assert!(r.results[0].snippet.as_deref().is_some_and(|s| s.contains("«会議»")), "{:?}", r.results[0].snippet);
    assert_eq!(ids(&search(&env, q("会社"))), Vec::<i64>::new());
}

#[test]
fn emlxpart_bodies_are_decoded() {
    use crate::mime::decode_part_body;
    assert_eq!(decode_part_body(b"SGVsbG8gVGltYnJl\r\n", Some("base64")), b"Hello Timbre");
    assert_eq!(decode_part_body(b"caf=C3=A9", Some("quoted-printable")), "café".as_bytes());
    // Already binary: kept as is.
    assert_eq!(decode_part_body(&[0x25, 0x50, 0xff, 0x00], Some("base64")), vec![0x25, 0x50, 0xff, 0x00]);
}

#[test]
fn deleted_messages_are_not_acted_on() {
    let env = setup_with("deleted", true, |cfg| {
        envelope_index(cfg).execute("UPDATE messages SET deleted = 1 WHERE ROWID = 102", []).unwrap();
    });
    let m = env.ctx.meta(102).unwrap();
    assert!(m.deleted);
    let err = crate::runtime().unwrap().block_on(crate::actions::mark(
        &env.ctx,
        &crate::actions::MarkParams { ids: vec![102], action: "read".into(), color: None },
    ));
    let r = err.unwrap();
    assert_eq!(r.done, 0);
    assert!(r.failed[0].error.contains("deleted"), "{:?}", r.failed[0].error);
}

/// Skipping: a folder of the user's named with a junk word is searched;
/// Gmail's sent mail (stored in All Mail, labelled Sent) is skipped with "sent".
#[test]
fn what_search_skips() {
    let env = setup_with("skips", true, |cfg| {
        let mail = envelope_index(cfg);
        mail.execute_batch(
            "INSERT INTO mailboxes(url) VALUES('imap://62653857-BF50-4534-AB81-8F72E0FCEDF5/Old%20Spam');
             CREATE TABLE labels (message_id INTEGER NOT NULL, mailbox_id INTEGER NOT NULL,
                 PRIMARY KEY (message_id, mailbox_id)) WITHOUT ROWID;
             -- 102 stored in Archive (as in All Mail), labelled Sent only; 1234 labelled Sent and INBOX.
             UPDATE messages SET mailbox = (SELECT ROWID FROM mailboxes WHERE url LIKE '%/Archive') WHERE ROWID IN (102, 1234);
             INSERT INTO labels SELECT 102, ROWID FROM mailboxes WHERE url LIKE '%/Sent%20Messages';
             INSERT INTO labels SELECT 1234, ROWID FROM mailboxes WHERE url LIKE '%/Sent%20Messages' OR url LIKE '%/INBOX';
             UPDATE messages SET mailbox = (SELECT ROWID FROM mailboxes WHERE url LIKE '%/Old%20Spam') WHERE ROWID = 104;",
        )
        .unwrap();
    });
    let store = &env.ctx.store;
    let count = |skip_roles: &[&str], id: i64| -> i64 {
        let roles: Vec<String> = skip_roles.iter().map(|r| r.to_string()).collect();
        let skip = store.skipped_mailboxes(&roles);
        env.ctx
            .db()
            .query_row(
                &format!(
                    "SELECT count(*) FROM mail.messages m WHERE m.ROWID = ?1 AND {}",
                    store.outside_mailboxes(&skip)
                ),
                [id],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(count(&["junk", "trash"], 104), 1, "\"Old Spam\" is the user's folder: searched");
    assert_eq!(count(&["junk", "trash", "sent"], 102), 0, "Gmail sent mail, labelled Sent only: skipped");
    assert_eq!(count(&["junk", "trash", "sent"], 1234), 1, "also labelled Inbox: kept");
    assert_eq!(count(&["junk", "trash"], 102), 1);
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

/// Paging by conversation: what one page showed, the next does not show again.
#[test]
fn pages_do_not_repeat_conversations() {
    let env = setup_with("pages", true, |cfg| {
        let mail = envelope_index(cfg);
        // 30 messages in 6 conversations, interleaved in time.
        let inbox: i64 =
            mail.query_row("SELECT ROWID FROM mailboxes WHERE url LIKE '%/INBOX'", [], |r| r.get(0)).unwrap();
        mail.execute("INSERT INTO subjects(subject) VALUES('rapport')", []).unwrap();
        let subj = mail.last_insert_rowid();
        for i in 0..30 {
            mail.execute(
                "INSERT INTO messages(ROWID, sender, subject_prefix, subject, date_sent, date_received, mailbox, flags, read, flagged, deleted, size, conversation_id)
                 VALUES(?1, 1, '', ?2, ?3, ?3, ?4, 1, 1, 0, 0, 100, ?5)",
                rusqlite::params![5000 + i, subj, crate::util::now_unix() - i * 3600, inbox, 900 + i % 6],
            )
            .unwrap();
        }
    });
    let mut seen = vec![];
    let mut offset = 0;
    loop {
        let r = search(
            &env,
            SearchParams {
                limit: Some(2),
                offset: Some(offset),
                subject: Some("rapport".into()),
                ..Default::default()
            },
        );
        seen.extend(ids(&r));
        match r.next_offset {
            Some(o) => offset = o,
            None => break,
        }
    }
    assert_eq!(seen.len(), 6, "one result per conversation, over all pages: {seen:?}");
    // An offset past the end (a stale next_offset, a guess) is an empty page.
    for offset in [6, 29, 30, 500, 100_000] {
        let r = search(
            &env,
            SearchParams {
                limit: Some(5),
                offset: Some(offset),
                subject: Some("rapport".into()),
                ..Default::default()
            },
        );
        assert!(r.results.is_empty() && r.next_offset.is_none(), "offset {offset}: {:?} {:?}", ids(&r), r.next_offset);
        let r = search(
            &env,
            SearchParams { limit: Some(5), offset: Some(offset), query: Some("rapport".into()), ..Default::default() },
        );
        assert!(r.next_offset.is_none_or(|o| o > offset), "offset {offset}");
    }
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

/// MCP clients such as Claude Desktop reject a result missing a property
/// that the output schema lists as required. A field left out when empty
/// must therefore also be optional in the schema: `#[serde(default, ...)]`.
#[test]
fn fields_left_out_are_optional_in_schemas() {
    let sources = [
        ("actions.rs", include_str!("actions.rs")),
        ("app.rs", include_str!("app.rs")),
        ("degraded.rs", include_str!("degraded.rs")),
        ("mime.rs", include_str!("mime.rs")),
        ("search.rs", include_str!("search.rs")),
        ("server.rs", include_str!("server.rs")),
    ];
    let mut checked = 0;
    for (file, text) in sources {
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if !line.starts_with("#[serde(") || !line.contains("skip_serializing_if") {
                continue;
            }
            checked += 1;
            // Option fields are optional in schemas without help.
            if line.contains("Option::is_none") {
                continue;
            }
            assert!(line.contains("default"), "{file}:{}: add `default` to {line}", n + 1);
        }
    }
    assert!(checked > 20, "the scan found the attributes ({checked})");
}

fn envelope_index(cfg: &crate::config::Config) -> rusqlite::Connection {
    rusqlite::Connection::open(cfg.mail_root.join("V10/MailData/Envelope Index")).expect("envelope index")
}

/// When Mail rebuilds its Envelope Index, its ids name other messages: the
/// index is rebuilt, once.
#[test]
fn mail_renumbering_its_messages_rebuilds_the_index() {
    let env = setup_with("renumber", false, |cfg| {
        let conn = envelope_index(cfg);
        for k in 0..12 {
            let m = fixture::Msg {
                id: 300 + k,
                mailbox: "INBOX",
                from: ("Shop", "news@shop.example"),
                to: "me@example.com",
                subject: [
                    "Alpha", "Bravo", "Charlie", "Delta", "Echo", "Foxtrot", "Golf", "Hotel", "India", "Juliett",
                    "Kilo", "Lima",
                ][k as usize],
                date: fixture::T0 + k,
                read: true,
                conversation: 300 + k,
                partial: false,
                raw: format!("From: news@shop.example\r\nSubject: Letter {k}\r\n\r\nBody {k}\r\n"),
            };
            fixture::write_message(&cfg.mail_root.join("V10"), &conn, &m).unwrap();
        }
    });
    let ctx = &env.ctx;
    let store = &ctx.store;
    let mut conn = index::open(&ctx.cfg, store, true).unwrap();
    let mut indexer = Indexer::new(ctx.cfg.clone(), store.clone(), ctx.progress.clone());
    // The subjects in the files ("Letter 3") and in Mail ("Delta") differ
    // from the start: that is how this Mail stores them, not a renumbering.
    indexer.full_build(&mut conn, false).unwrap();
    assert!(!index::renumbered(&conn, store).unwrap());
    // Same subjects in files and in Mail, then Mail numbers anew.
    let ev = envelope_index(&ctx.cfg);
    ev.execute_batch(
        "UPDATE subjects SET subject = 'Letter ' || (SELECT m.ROWID - 300 FROM messages m WHERE m.subject = subjects.ROWID)
         WHERE ROWID IN (SELECT subject FROM messages WHERE ROWID >= 300);",
    )
    .unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'subject_baseline'", []).unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    assert!(!index::renumbered(&conn, store).unwrap());
    ev.execute_batch("UPDATE messages SET subject = (SELECT subject FROM messages n WHERE n.ROWID = 300 + (messages.ROWID - 299) % 12) WHERE ROWID >= 300;")
        .unwrap();
    assert!(index::renumbered(&conn, store).unwrap());
    // A row the rebuild must replace: proof the index was cleared.
    conn.execute("UPDATE texts SET subject = 'stale' WHERE id = 305", []).unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    let subject: String = conn.query_row("SELECT subject FROM texts WHERE id = 305", [], |r| r.get(0)).unwrap();
    assert_eq!(subject, "Letter 5");
    // The files still disagree with Mail's new numbering here: the baseline
    // keeps that from rebuilding again and again.
    assert!(!index::renumbered(&conn, store).unwrap());
}

/// Gmail keeps each message once, in All Mail; its Inbox, Sent and labels
/// are rows of the `labels` table.
#[test]
fn gmail_labels_count_as_mailboxes() {
    let env = setup_with("labels", true, |cfg| {
        envelope_index(cfg)
            .execute_batch(
                "CREATE TABLE labels (message_id INTEGER NOT NULL REFERENCES messages(ROWID) ON DELETE CASCADE,
                     mailbox_id INTEGER NOT NULL REFERENCES mailboxes(ROWID) ON DELETE CASCADE,
                     PRIMARY KEY (message_id, mailbox_id)) WITHOUT ROWID;
                 INSERT INTO addresses(address, comment) VALUES('zoe@example.com', 'Zoé');
                 INSERT INTO recipients(message, address, type, position) VALUES(104, last_insert_rowid(), 0, 1);
                 INSERT INTO labels SELECT 104, ROWID FROM mailboxes
                     WHERE url LIKE '%/INBOX' OR url LIKE '%/Sent%20Messages';",
            )
            .unwrap();
    });
    let store = &env.ctx.store;
    assert_eq!(store.schema.labels.as_deref(), Some("m.ROWID"));
    // 104 is stored in Archive and labelled Inbox: found, shown and counted there.
    let inbox = search(&env, SearchParams { mailbox: Some("inbox".into()), ..Default::default() });
    assert!(ids(&inbox).contains(&104), "{:?}", ids(&inbox));
    let r = search(&env, q("in:inbox comite"));
    assert_eq!(ids(&r), vec![104]);
    assert_eq!(r.results[0].mailbox, "INBOX");
    let m = app::read_mail(&env.ctx, &ReadParams { id: 104, max_chars: None, strip_quotes: None }).unwrap();
    assert_eq!(m.mailbox, "INBOX");
    let counts = store.mailbox_counts(&env.ctx.db()).unwrap();
    let only = |role: &str| store.mailboxes_with_roles(&[role.to_string()])[0];
    assert_eq!(counts[&only("inbox")].0, 4, "3 stored + 1 labelled");
    assert_eq!(counts[&only("archive")].0, 1, "104 is still stored in Archive");
    assert_eq!(counts[&only("sent")].0, 2, "103 stored + 104 labelled");
    // Labelled Sent: its recipients count as people the user wrote to.
    assert_eq!(store.sender_history(&env.ctx.db(), "zoe@example.com", 0).unwrap(), (0, 1));
}

/// Gmail keeps labels on trashed mail: the trash still wins.
#[test]
fn trash_wins_over_gmail_labels() {
    let env = setup_with("trashlabels", true, |cfg| {
        envelope_index(cfg)
            .execute_batch(
                "CREATE TABLE labels (message_id INTEGER NOT NULL REFERENCES messages(ROWID) ON DELETE CASCADE,
                     mailbox_id INTEGER NOT NULL REFERENCES mailboxes(ROWID) ON DELETE CASCADE,
                     PRIMARY KEY (message_id, mailbox_id)) WITHOUT ROWID;
                 -- 102 is stored in the inbox but labelled Trash; 104 in Archive, labelled Inbox.
                 INSERT INTO labels SELECT 102, ROWID FROM mailboxes WHERE url LIKE '%/Deleted%20Messages' OR url LIKE '%/Trash';
                 INSERT INTO labels SELECT 104, ROWID FROM mailboxes WHERE url LIKE '%/INBOX';",
            )
            .unwrap();
    });
    let all = |p: SearchParams| ids(&search(&env, SearchParams { group_threads: Some(false), ..p }));
    assert!(!all(SearchParams::default()).contains(&102), "labelled Trash: skipped");
    assert!(all(SearchParams { include_junk_and_trash: Some(true), ..Default::default() }).contains(&102));
    // It is shown, and acted on, where it really is: in the trash.
    let stored = env.ctx.meta(102).unwrap().mailbox;
    let shown = env.ctx.store.display_mailbox(&env.ctx.db(), 102, stored);
    assert_eq!(env.ctx.store.mailbox(shown).map(|m| m.role), Some(Role::Trash));
    // With archives skipped too, an archived message labelled Inbox stays.
    let mut cfg = env.ctx.cfg.clone();
    cfg.excluded_roles.push("archive".into());
    let skip = env.ctx.store.mailboxes_with_roles(&cfg.excluded_roles);
    let n: i64 = env
        .ctx
        .db()
        .query_row(
            &format!(
                "SELECT count(*) FROM mail.messages m WHERE m.ROWID = 104 AND {}",
                env.ctx.store.outside_mailboxes(&skip)
            ),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

/// Mail people send themselves must not rank as mail from a correspondent.
#[test]
fn own_addresses_are_not_correspondents() {
    let env = setup_with("own", true, |cfg| {
        // The reply 103 also went to the user's own address.
        envelope_index(cfg)
            .execute_batch(
                "INSERT INTO addresses(address, comment) VALUES('Moi@iCloud.com', '');
                 INSERT INTO recipients(message, address, type, position) VALUES(103, last_insert_rowid(), 1, 1);",
            )
            .unwrap();
    });
    let conn = env.ctx.db();
    assert!(search::refresh_correspondents(&conn, &env.ctx.store).unwrap());
    let correspondent = |addr: &str| -> bool {
        conn.query_row(
            "SELECT count(*) FROM temp.correspondents c JOIN mail.addresses a ON a.ROWID = c.addr WHERE lower(a.address) = ?1",
            [addr],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    };
    assert!(correspondent("ann@example.com"));
    assert!(!correspondent("moi@icloud.com"));
}

/// A recent message outranks an old one that repeats the words, as in a
/// mailbox where years-old messages carry "Factures" in the subject and
/// "Facture" in every attachment name.
#[test]
fn recent_mail_outranks_old_repetitions() {
    let now = crate::util::now_unix();
    let env = setup_with("recency", true, |cfg| {
        let conn = envelope_index(cfg);
        let v10 = cfg.mail_root.join("V10");
        let add = |id: i64, from: (&'static str, &'static str), subject: &str, date: i64, body: &str, extra: &str| {
            let raw = format!(
                "From: {} <{}>\r\nTo: moi@icloud.com\r\nSubject: {subject}\r\nMessage-ID: <m{id}@example.com>\r\n\
                 MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n\
                 --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n{extra}--b--\r\n",
                from.0, from.1
            );
            let m = fixture::Msg {
                id,
                mailbox: "Archive",
                from,
                to: "moi@icloud.com",
                subject: Box::leak(subject.to_string().into_boxed_str()),
                date,
                read: true,
                conversation: id,
                partial: false,
                raw,
            };
            fixture::write_message(&v10, &conn, &m).unwrap();
        };
        let attachment = |name: &str| {
            format!(
                "--b\r\nContent-Type: text/plain; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\n\r\nx\r\n"
            )
        };
        let year = 365 * 86_400;
        add(
            900,
            ("Moi", "moi@icloud.com"),
            "Factures",
            fixture::T0 - 13 * year,
            "Factures",
            &(attachment("Facture 4.doc") + &attachment("Facture 5.doc")),
        );
        add(
            901,
            ("EDF", "service@edf.fr"),
            "Votre facture est disponible",
            now - 3 * 86_400,
            "Bonjour, votre facture d'electricite du mois est disponible dans votre espace client. \
             Le montant sera preleve le 15 du mois. Merci de votre confiance, le service clients.",
            "",
        );
        // The rest of a mailbox, older: invoices, a few plurals, other mail.
        let from = ("Fournisseur", "compta@example.com");
        for i in 0..380 {
            let (subject, body) = match i % 19 {
                0..=2 => {
                    (format!("Facture {i}"), "Veuillez trouver la facture du trimestre, payable sous trente jours.")
                }
                3 => (format!("Vos factures {i}"), "Vos factures du semestre sont disponibles en ligne."),
                _ => {
                    (format!("Nouvelles {i}"), "Les nouvelles de la semaine, le programme et les rendez-vous du club.")
                }
            };
            add(3000 + i, from, &subject, fixture::T0 - (1 + i % 9) * year, body, "");
        }
    });
    let found = ids(&search(&env, q("facture")));
    let pos = |id: i64| found.iter().position(|&x| x == id).unwrap_or_else(|| panic!("{id} missing from {found:?}"));
    // The two recent invoices (October's from Orange, this week's from EDF)
    // come first, before the 2013 message that repeats the word.
    let mut top = found[..2].to_vec();
    top.sort_unstable();
    assert_eq!(top, vec![101, 901], "{found:?}");
    assert!(pos(900) > 1);
}

/// Parse every file under TIMBRE_FUZZ_DIR (real mail, e.g. a maildir) and
/// report panics and slow messages: `TIMBRE_FUZZ_DIR=… cargo test parse_real_mail -- --ignored`.
#[test]
#[ignore]
fn parse_real_mail() {
    let Ok(dir) = std::env::var("TIMBRE_FUZZ_DIR") else { return };
    let limit: usize = std::env::var("TIMBRE_FUZZ_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let (mut n, mut panics, mut slow) = (0, vec![], vec![]);
    let started = std::time::Instant::now();
    for entry in walkdir::WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        if n >= limit {
            break;
        }
        let Ok(mut raw) = std::fs::read(entry.path()) else { continue };
        if entry.path().extension().is_some_and(|e| e == "emlx") {
            let Ok(m) = crate::emlx::read_message(entry.path()) else { continue };
            raw = m;
        }
        n += 1;
        let t = std::time::Instant::now();
        let r = std::panic::catch_unwind(|| {
            if let Some(p) = crate::mime::parse(&raw) {
                let _ = crate::quote::strip_quotes_of(&p.body, &p.subject);
                let _ = crate::mime::warnings(&p);
                for i in 0..p.attachments.len() {
                    let _ = crate::mime::attachment_bytes(&raw, i);
                }
            }
        });
        if r.is_err() {
            panics.push(entry.path().display().to_string());
        }
        if t.elapsed() > std::time::Duration::from_secs(1) {
            slow.push(format!("{} {:?}", entry.path().display(), t.elapsed()));
        }
    }
    eprintln!("parsed {n} messages in {:?}; {} panics; {} slow", started.elapsed(), panics.len(), slow.len());
    for p in panics.iter().take(20) {
        eprintln!("panic: {p}");
    }
    for s in slow.iter().take(20) {
        eprintln!("slow: {s}");
    }
    assert!(panics.is_empty());
}

// ---------------------------------------------------------------- moves

/// A Gmail account and a La Poste one, beside the fixture's iCloud.
const GMAIL: &str = "0A0A0A0A-1111-4E5F-9A8B-00000000000A";
const LAPOSTE: &str = "0B0B0B0B-1111-4E5F-9A8B-00000000000B";

/// 101 is a Gmail message stored in All Mail and labelled Inbox, 102 is in
/// the Gmail trash, 104 archived in Gmail (All Mail only), 1234 in La
/// Poste's inbox, whose mailboxes all live inside INBOX.
fn gmail_and_laposte(cfg: &crate::config::Config) {
    let c = envelope_index(cfg);
    for p in ["INBOX", "%5BGmail%5D/Tous%20les%20messages", "%5BGmail%5D/Corbeille", "%5BGmail%5D/Spam", "Perso"] {
        c.execute("INSERT INTO mailboxes(url) VALUES(?1)", [format!("imap://{GMAIL}/{p}")]).unwrap();
    }
    for p in ["INBOX", "INBOX/DRAFT", "INBOX/OUTBOX", "INBOX/TRASH", "INBOX/QUARANTAINE"] {
        c.execute("INSERT INTO mailboxes(url) VALUES(?1)", [format!("imap://{LAPOSTE}/{p}")]).unwrap();
    }
    let mb = |p: &str| format!("(SELECT ROWID FROM mailboxes WHERE url = 'imap://{p}')");
    c.execute_batch(&format!(
        "CREATE TABLE labels (message_id INTEGER NOT NULL REFERENCES messages(ROWID) ON DELETE CASCADE,
             mailbox_id INTEGER NOT NULL REFERENCES mailboxes(ROWID) ON DELETE CASCADE,
             PRIMARY KEY (message_id, mailbox_id)) WITHOUT ROWID;
         UPDATE messages SET mailbox = {all} WHERE ROWID IN (101, 104);
         INSERT INTO labels VALUES(101, {inbox});
         UPDATE messages SET mailbox = {bin} WHERE ROWID = 102;
         UPDATE messages SET mailbox = {lp} WHERE ROWID = 1234;",
        all = mb(&format!("{GMAIL}/%5BGmail%5D/Tous%20les%20messages")),
        inbox = mb(&format!("{GMAIL}/INBOX")),
        bin = mb(&format!("{GMAIL}/%5BGmail%5D/Corbeille")),
        lp = mb(&format!("{LAPOSTE}/INBOX")),
    ))
    .unwrap();
}

fn move_mail(env: &Env, ids: Vec<i64>, to: &str) -> Result<crate::actions::BatchResult, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(crate::actions::move_mail(&env.ctx, &crate::actions::MoveParams { ids, to: to.into(), account: None }))
}

/// Mail, asked by a script, cannot take a Gmail message out of the inbox or
/// a label (it comes straight back), and Gmail deletes for good what goes
/// from its trash to All Mail: those moves are refused, with what to do.
#[test]
fn gmail_moves_mail_cannot_make_are_refused() {
    let env = setup_with("gmailmoves", false, gmail_and_laposte);
    let refused = |id: i64, to: &str, why: &str| {
        let r = move_mail(&env, vec![id], to).unwrap();
        assert_eq!((r.done, r.failed.len(), r.undo_token.is_none()), (0, 1, true), "{id} to {to}");
        assert!(r.failed[0].error.contains(why), "{id} to {to}: {}", r.failed[0].error);
    };
    refused(101, "archive", "Archive button");
    refused(101, "Perso", "Archive button");
    refused(104, "inbox", "Archive button");
    refused(102, "archive", "delete it for good");
    refused(102, "junk", "delete it for good");
    refused(102, "Perso", "delete it for good");
    // To the trash, and out of it to the inbox, Mail can: here only Mail
    // itself is missing (tests never drive it).
    let no_mail = crate::mailapp::unavailable().unwrap();
    for (id, to) in [(101, "trash"), (101, "junk"), (104, "trash"), (102, "inbox")] {
        let e = move_mail(&env, vec![id], to).unwrap_err();
        assert!(e.contains(no_mail), "{id} to {to}: {e}");
    }
    // The fixture's iCloud is no Gmail: archiving is planned as before.
    let e = move_mail(&env, vec![103], "archive").unwrap_err();
    assert!(e.contains(no_mail), "{e}");
}

/// Undo takes a Gmail message out of the trash the only way Mail can: to
/// the inbox (it keeps its labels). What Gmail keeps in All Mail cannot be
/// told apart from where it was moved, and is left alone.
#[test]
fn gmail_undo_goes_through_the_inbox() {
    let env = setup_with("gmailundo", false, gmail_and_laposte);
    let store = &env.ctx.store;
    let loc = |account: &str, path: &[&str]| crate::mailapp::Loc {
        account_id: account.to_string(),
        account: account.to_string(),
        path: path.iter().map(|p| p.to_string()).collect(),
        local: false,
    };
    let inbox = loc(GMAIL, &["INBOX"]);
    let bin = loc(GMAIL, &["[Gmail]", "Corbeille"]);
    let all = loc(GMAIL, &["[Gmail]", "Tous les messages"]);
    let perso = loc(GMAIL, &["Perso"]);
    let icloud = loc(fixture::ACCOUNT, &["INBOX"]);
    let undo = |now_in: &crate::mailapp::Loc, back_to: &crate::mailapp::Loc| {
        crate::actions::gmail_undo(store, now_in, back_to)
    };
    // Out of the trash: through the inbox, whether it came from there or
    // from a label (Gmail kept the label).
    for back_to in [&inbox, &perso, &all] {
        let (to, note) = undo(&bin, back_to).unwrap();
        assert_eq!(to.path, vec!["INBOX"]);
        assert!(note.unwrap().contains("All Mail"));
    }
    assert!(undo(&all, &bin).unwrap_err().contains("All Mail"));
    assert!(undo(&bin, &loc(GMAIL, &["[Gmail]", "Spam"])).unwrap_err().contains("delete it for good"));
    // Back into the trash, Mail can; out of the inbox to a label, not.
    assert_eq!(undo(&inbox, &bin).unwrap(), (bin.clone(), None));
    assert!(undo(&inbox, &perso).unwrap_err().contains("Archive button"));
    // Another account: Mail copies it there; Gmail may keep its own.
    assert!(undo(&inbox, &icloud).unwrap().1.unwrap().contains("own copy"));
    // No Gmail involved: back where it was.
    assert_eq!(undo(&icloud, &loc(fixture::ACCOUNT, &["Archive"])).unwrap().1, None);
}

/// An IMAP account without an archive mailbox gets one, as Mail's Archive
/// button makes it: "Archive", beside the account's own mailboxes.
#[test]
fn a_missing_archive_is_made_beside_the_accounts_mailboxes() {
    let env = setup_with("newarchive", false, gmail_and_laposte);
    let store = &env.ctx.store;
    let inbox_of = |account: &str| {
        store.mailboxes().values().find(|m| m.account_id == account && m.path == ["INBOX"]).cloned().unwrap()
    };
    let archive_of = |account: &str| {
        let inbox = inbox_of(account);
        crate::actions::new_archive(store, &inbox.account_id, &inbox.account)
    };
    let laposte = archive_of(LAPOSTE).unwrap();
    assert_eq!(
        (laposte.path.clone(), laposte.role, laposte.local),
        (vec!["INBOX".into(), "Archive".into()], Role::Archive, false)
    );
    // Mailboxes at the top: the archive too.
    assert_eq!(archive_of(fixture::ACCOUNT).unwrap().path, vec!["Archive"]);
    // Gmail's archive is All Mail; On My Mac has none to make.
    assert!(archive_of(GMAIL).is_none());
    assert!(crate::actions::new_archive(store, "", "On My Mac").is_none());
    // Archiving from La Poste makes the mailbox first; Mail is never driven
    // from tests, so nothing is moved and the result says why.
    let r = move_mail(&env, vec![1234], "archive").unwrap();
    assert_eq!((r.done, r.undo_token.is_none()), (0, true));
    let why = &r.failed[0].error;
    let no_mail = crate::mailapp::unavailable().unwrap();
    assert!(why.contains("archive mailbox for") && why.contains(no_mail) && why.contains("Nothing was moved"), "{why}");
}

/// A mailbox Mail creates after Timbre read the list ("Recovered Messages")
/// is named in results, not left blank.
#[test]
fn a_new_mailbox_is_named_in_results() {
    let env = setup("newmailbox", true);
    let c = envelope_index(&env.ctx.cfg);
    c.execute("INSERT INTO mailboxes(url) VALUES(?1)", ["local:///Recovered%20Messages%20(iCloud)"]).unwrap();
    c.execute("UPDATE messages SET mailbox = last_insert_rowid() WHERE ROWID = 101", []).unwrap();
    drop(c);
    let r = search(&env, q("facture"));
    let hit = r.results.iter().find(|h| h.id == 101).expect("101 found");
    assert_eq!((hit.mailbox.as_str(), hit.account.as_str()), ("Recovered Messages (iCloud)", "On My Mac"));
}

/// La Poste's mailboxes as Orange's are too: folders inside an "Archives"
/// folder, all inside INBOX; and Gmail's sent mail, its name written by
/// Mail's database with a combining accent. 1234 is filed in
/// "INBOX/Archives/Important", 103 sent from La Poste, 104 sent from Gmail.
fn folders_inside_inbox(cfg: &crate::config::Config) {
    gmail_and_laposte(cfg);
    let c = envelope_index(cfg);
    for p in ["INBOX/Archives", "INBOX/Archives/2009", "INBOX/Archives/Important"] {
        c.execute("INSERT INTO mailboxes(url) VALUES(?1)", [format!("imap://{LAPOSTE}/{p}")]).unwrap();
    }
    let sent = format!("{GMAIL}/%5BGmail%5D/Messages%20envoye%CC%81s");
    c.execute("INSERT INTO mailboxes(url) VALUES(?1)", [format!("imap://{sent}")]).unwrap();
    let mb = |p: &str| format!("(SELECT ROWID FROM mailboxes WHERE url = 'imap://{p}')");
    c.execute_batch(&format!(
        "UPDATE messages SET mailbox = {important} WHERE ROWID = 1234;
         UPDATE messages SET mailbox = {outbox} WHERE ROWID = 103;
         INSERT INTO labels VALUES(104, {sent});",
        important = mb(&format!("{LAPOSTE}/INBOX/Archives/Important")),
        outbox = mb(&format!("{LAPOSTE}/INBOX/OUTBOX")),
        sent = mb(&sent),
    ))
    .unwrap();
}

/// Mailboxes are found as Mail shows them: from the INBOX prefix on at La
/// Poste and Orange, with or without accents however they are written, and
/// a folder with the folders inside it. An inbox is searched alone: inside
/// La Poste's are its sent mail and trash.
#[test]
fn mailboxes_are_found_as_mail_shows_them() {
    let env = setup_with("mailnames", false, folders_inside_inbox);
    let found = |p: SearchParams| {
        let mut v = ids(&search(&env, p));
        v.sort_unstable();
        v
    };
    let named = |mailbox: &str, account: &str| {
        found(SearchParams { mailbox: Some(mailbox.into()), account: Some(account.into()), ..Default::default() })
    };
    assert_eq!(named("Archives/Important", LAPOSTE), vec![1234]);
    assert_eq!(named("INBOX/Archives/Important", LAPOSTE), vec![1234]);
    // The account's archive, a folder that holds others, with them.
    assert_eq!(named("archive", LAPOSTE), vec![1234]);
    assert_eq!(named("Archives", LAPOSTE), vec![1234]);
    assert!(named("inbox", LAPOSTE).is_empty(), "the inbox alone, not the mailboxes inside it");
    assert_eq!(named("sent", LAPOSTE), vec![103]);
    // Gmail's sent mail is its sent mail, however its name is written.
    for name in ["sent", "Messages envoyés", "messages envoyes", "[Gmail]/Messages envoyés"] {
        assert!(named(name, GMAIL).contains(&104), "{name}");
    }
    // Left out with the folders inside it.
    let mut p = q("-in:Archives");
    p.account = Some(LAPOSTE.into());
    assert_eq!(found(p), vec![103]);
    // Listed: the archive, empty though it holds folders, and the sent mail.
    let list = app::list_mailboxes(&env.ctx).unwrap();
    let role = |account: &str, name: &str| {
        let a = list.accounts.iter().find(|a| a.account == account).expect("account listed");
        let m = a.mailboxes.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("{name} listed"));
        (m.role.clone(), m.total)
    };
    assert_eq!(role(LAPOSTE, "INBOX/Archives"), (Some("archive".into()), 0));
    assert_eq!(role(GMAIL, "[Gmail]/Messages envoyés").0.as_deref(), Some("sent"));
    // A move to a folder named as Mail shows it goes to Mail (never driven
    // from tests) instead of failing to find the folder.
    let e = move_mail(&env, vec![1234], "Archives/2009").unwrap_err();
    assert!(e.contains(crate::mailapp::unavailable().unwrap()), "{e}");
    // A mailbox is made where Mail shows the account's mailboxes: inside
    // INBOX at La Poste, at the top at Gmail.
    let loc = |id: &str| crate::mailapp::Loc { account_id: id.into(), account: id.into(), path: vec![], local: false };
    assert_eq!(crate::mailboxes::mail_prefix(&env.ctx.store, &loc(LAPOSTE)), Some(vec!["INBOX".to_string()]));
    assert_eq!(crate::mailboxes::mail_prefix(&env.ctx.store, &loc(GMAIL)), Some(vec![]));
}

// ---------------------------------------------------------------- first bug review

fn add_account(cfg: &crate::config::Config, pk: i64, id: &str, name: &str, user: &str) {
    rusqlite::Connection::open(&cfg.accounts_db)
        .unwrap()
        .execute("INSERT INTO ZACCOUNT VALUES(?1, ?2, ?3, ?4, NULL)", rusqlite::params![pk, id, name, user])
        .unwrap();
}

fn add_mailboxes(cfg: &crate::config::Config, account: &str, paths: &[&str]) {
    let c = envelope_index(cfg);
    for p in paths {
        c.execute("INSERT INTO mailboxes(url) VALUES(?1)", [format!("imap://{account}/{p}")]).unwrap();
    }
}

/// A label of the user's whose name holds "spam" is no junk: a message in
/// the inbox that has it is shown, searched and moved as in the inbox.
#[test]
fn a_label_named_like_spam_is_not_the_junk() {
    let env = setup_with("weakjunk", true, |cfg| {
        gmail_and_laposte(cfg);
        add_mailboxes(cfg, GMAIL, &["Spam%20reports"]);
        envelope_index(cfg)
            .execute(
                &format!(
                    "INSERT INTO labels VALUES(101, (SELECT ROWID FROM mailboxes WHERE url = 'imap://{GMAIL}/Spam%20reports'))"
                ),
                [],
            )
            .unwrap();
    });
    let store = &env.ctx.store;
    let stored = env.ctx.meta(101).unwrap().mailbox;
    let shown = store.display_mailbox(&env.ctx.db(), 101, stored);
    assert_eq!(store.mailbox(shown).unwrap().path, ["INBOX"]);
    assert!(ids(&search(&env, SearchParams { group_threads: Some(false), ..Default::default() })).contains(&101));
    // Trashed from the inbox: Gmail's rules let it, so Mail is asked (it
    // cannot be here).
    let e = move_mail(&env, vec![101], "trash").unwrap_err();
    assert!(e.contains("only available on macOS"), "{e}");
}

/// "archive" is the account's archive, never a folder of the user's that
/// holds the word: with none, the archive is to be made.
#[test]
fn the_archive_is_never_a_folder_holding_the_word() {
    let env = setup_with("weakarchive", false, |cfg| {
        gmail_and_laposte(cfg);
        add_mailboxes(cfg, LAPOSTE, &["INBOX/Archives%20clients%202019"]);
    });
    assert!(env.ctx.store.role_mailbox_for_account(LAPOSTE, LAPOSTE, Role::Archive).is_none());
    let r = move_mail(&env, vec![1234], "archive").unwrap();
    assert_eq!(r.done, 0);
    assert!(r.failed[0].error.contains("archive mailbox"), "{:?}", r.failed);
    // Listed as a folder of the user's, with no role.
    let list = app::list_mailboxes(&env.ctx).unwrap();
    let lp = list.accounts.iter().find(|a| a.account == LAPOSTE).unwrap();
    let folder = lp.mailboxes.iter().find(|m| m.name == "INBOX/Archives clients 2019").unwrap();
    assert_eq!(folder.role, None);
}

/// Mail's database with no message yet when Timbre starts (a new Mac, Mail
/// rebuilding it): dates are read as Unix times, as Mail writes them.
#[test]
fn dates_are_right_when_mail_had_no_message_at_start() {
    let env = setup_with("emptyatstart", false, |cfg| {
        envelope_index(cfg).execute_batch("CREATE TABLE keep AS SELECT * FROM messages; DELETE FROM messages;").unwrap()
    });
    envelope_index(&env.ctx.cfg).execute_batch("INSERT INTO messages SELECT * FROM keep;").unwrap();
    assert_eq!(env.ctx.store.epoch_offset, 0);
    assert_eq!(env.ctx.meta(101).unwrap().date, fixture::messages()[0].date);
}

/// A subject that is not valid UTF-8 in Mail's database is read anyway:
/// searches, threads, reading and the index never fail on it.
#[test]
fn text_that_is_not_utf8_never_fails_a_search() {
    let env = setup_with("badutf8", false, |cfg| {
        envelope_index(cfg)
            .execute_batch(
                "UPDATE subjects SET subject = CAST(x'50726f6a6563748e' AS TEXT)
                 WHERE ROWID = (SELECT subject FROM messages WHERE ROWID = 102);",
            )
            .unwrap()
    });
    let c = &env.ctx;
    let mut conn = index::open(&c.cfg, &c.store, true).unwrap();
    Indexer::new(c.cfg.clone(), c.store.clone(), c.progress.clone()).full_build(&mut conn, false).unwrap();
    assert!(ids(&search(&env, q("budget"))).contains(&102));
    assert!(!ids(&search(&env, SearchParams::default())).is_empty());
    assert!(!ids(&search(&env, SearchParams { subject: Some("project".into()), ..Default::default() })).is_empty());
    assert!(app::get_thread(c, &ThreadParams { id: 103, max_messages: None, max_chars_per_message: None }).is_ok());
    let m = app::read_mail(c, &ReadParams { id: 102, max_chars: None, strip_quotes: None }).unwrap();
    assert!(m.subject.starts_with("Project"), "{}", m.subject);
}

/// The account alone, by name or by its id, as with a mailbox: the
/// account so named, not every account whose name holds it.
#[test]
fn accounts_are_matched_alike_everywhere() {
    const PRO: &str = "0C0C0C0C-1111-4E5F-9A8B-00000000000C";
    let env = setup_with("accounts", true, |cfg| {
        gmail_and_laposte(cfg);
        add_account(cfg, 10, GMAIL, "Gmail", "bob@gmail.com");
        add_account(cfg, 11, PRO, "Gmail Pro", "pro@gmail.com");
        add_mailboxes(cfg, PRO, &["INBOX", "Factures"]);
    });
    let store = &env.ctx.store;
    let by_id = search(&env, SearchParams { account: Some(fixture::ACCOUNT.into()), ..Default::default() });
    assert!(!ids(&by_id).is_empty(), "by its id");
    assert_eq!(store.accounts_named("gmail").len(), 1);
    assert_eq!(store.accounts_named("pro").first().map(|a| a.1.as_str()), Some("Gmail Pro"));
    assert!(store.resolve_mailboxes("Factures", Some("Gmail")).is_empty(), "not Gmail Pro's");
    assert_eq!(store.resolve_mailboxes("Factures", Some("Gmail Pro")).len(), 1);
    assert!(matches!(crate::mailboxes::mailbox_to_delete(store, "Factures", Some("Gmail")), Ok(None)));
}

/// What delete_mailbox takes for the mailbox named: never one standing for
/// a role's name, nor one whose name merely starts the same.
#[test]
fn the_mailbox_to_delete_is_the_one_named() {
    let env = setup_with("todelete", false, |cfg| {
        gmail_and_laposte(cfg);
        add_mailboxes(cfg, LAPOSTE, &["INBOX/Archive", "INBOX/Archives%202023", "INBOX/Projets", "INBOX/projets"]);
    });
    let pick = |spec: &str| {
        crate::mailboxes::mailbox_to_delete(&env.ctx.store, spec, Some(LAPOSTE)).map(|m| m.map(|m| m.path.join("/")))
    };
    assert_eq!(pick("Archives"), Ok(None));
    assert_eq!(pick("Archive"), Ok(Some("INBOX/Archive".into())));
    assert_eq!(pick("INBOX/Archive"), Ok(Some("INBOX/Archive".into())));
    assert_eq!(pick("archives 2023"), Ok(Some("INBOX/Archives 2023".into())));
    // Exactly as written; two differing in case alone: the one written.
    assert_eq!(pick("Projets"), Ok(Some("INBOX/Projets".into())));
    assert_eq!(pick("projets"), Ok(Some("INBOX/projets".into())));
    assert!(pick("PROJETS").is_err(), "{:?}", pick("PROJETS"));
}

/// Rows Mail's database keeps for mailboxes Mail deleted (their folder is
/// gone from the Mac, no message is in them): not listed, not found by
/// name, never a role's mailbox. An empty mailbox Mail has stays, and so
/// does one that holds mail, and every mailbox of an account whose folders
/// are not where Timbre looks.
#[test]
fn leftover_rows_of_deleted_mailboxes_are_left_out() {
    let dir = tempdir::new("leftovers");
    let cfg = fixture::build(&dir.0).unwrap();
    gmail_and_laposte(&cfg);
    // An old trash row Gmail renamed since, older than the live one.
    let c = envelope_index(&cfg);
    c.execute_batch(&format!(
        "UPDATE mailboxes SET ROWID = 900 WHERE url = 'imap://{GMAIL}/%5BGmail%5D/Corbeille';
         UPDATE messages SET mailbox = 900 WHERE ROWID = 102;
         INSERT INTO mailboxes(ROWID, url) VALUES(800, 'imap://{GMAIL}/%5BGmail%5D/Trash');"
    ))
    .unwrap();
    add_mailboxes(&cfg, GMAIL, &["Timbre%20essai", "Notes", "Vide"]);
    c.execute(
        &format!("INSERT INTO labels VALUES(101, (SELECT ROWID FROM mailboxes WHERE url = 'imap://{GMAIL}/Perso'))"),
        [],
    )
    .unwrap();
    fixture::mail_folders(&cfg).unwrap();
    let v10 = cfg.mail_root.join("V10");
    for gone in ["Timbre essai.mbox", "Notes.mbox", "[Gmail].mbox/Trash.mbox", "Perso.mbox"] {
        std::fs::remove_dir_all(v10.join(GMAIL).join(gone)).unwrap();
    }
    // La Poste's folders are elsewhere: nothing is taken for a leftover there.
    std::fs::remove_dir_all(v10.join(LAPOSTE)).unwrap();
    let env = setup_from(dir, cfg, false);
    let store = &env.ctx.store;
    let named =
        |p: &str| store.mailboxes().values().find(|m| m.account_id == GMAIL && m.display_path() == p).unwrap().id;
    let leftovers = store.leftovers();
    assert!(leftovers.contains(&named("Timbre essai")) && leftovers.contains(&named("Notes")));
    assert!(leftovers.contains(&800));
    assert!(!leftovers.contains(&named("Vide")), "empty, but Mail has it");
    assert!(!leftovers.contains(&named("Perso")), "a label points to it");
    assert!(!store.mailboxes().values().any(|m| m.account_id == LAPOSTE && leftovers.contains(&m.id)));
    // Named alone, such a row is still found (Mail may not have made a new
    // mailbox's folder yet): Mail says, when acting, whether it is there.
    assert_eq!(store.resolve_mailboxes("Timbre essai", None), vec![named("Timbre essai")]);
    assert_eq!(store.role_mailbox_for_account(GMAIL, GMAIL, Role::Trash).map(|m| m.id), Some(900));
    let list = app::list_mailboxes(&env.ctx).unwrap();
    let gmail: Vec<&str> = list
        .accounts
        .iter()
        .filter(|a| a.account == GMAIL)
        .flat_map(|a| a.mailboxes.iter().map(|m| m.name.as_str()))
        .collect();
    assert!(gmail.contains(&"Vide") && gmail.contains(&"Perso"), "{gmail:?}");
    assert!(!gmail.contains(&"Timbre essai") && !gmail.contains(&"Notes"), "{gmail:?}");
}

/// A message whose file Mail writes after the walk found it missing is
/// indexed at the next reconcile, without a restart.
#[test]
fn a_message_whose_file_comes_late_is_indexed() {
    let env = setup("latefile", true);
    let (cfg, store) = (env.ctx.cfg.clone(), env.ctx.store.clone());
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());
    let mail = rusqlite::Connection::open(store.db_path.clone()).unwrap();
    let mut m = fixture::messages().remove(1);
    m.id = 1700;
    m.raw =
        "From: Bob <bob@example.com>\r\nSubject: Okapi\r\nMessage-ID: <o1700@example.com>\r\n\r\nokapi sighting\r\n"
            .into();
    fixture::write_message(&store.version_dir, &mail, &m).unwrap();
    let file = walkdir::WalkDir::new(store.version_dir.join(fixture::ACCOUNT))
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name() == "1700.emlx")
        .unwrap()
        .into_path();
    let bytes = std::fs::read(&file).unwrap();
    std::fs::remove_file(&file).unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    assert!(ids(&search(&env, q("okapi"))).is_empty());
    std::fs::write(&file, bytes).unwrap();
    indexer.reconcile_now(&mut conn).unwrap();
    assert_eq!(ids(&search(&env, q("okapi"))), vec![1700]);
    let left: i64 = conn.query_row("SELECT count(*) FROM unlocated", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 0);
}

/// Words with nothing to search for are said to be left out, and alone
/// find nothing rather than every message. `-subject:` leaves out what has
/// the word in its subject only.
#[test]
fn queries_say_what_they_cannot_search() {
    let env = setup("edgequeries", true);
    for nothing in ["€", "@", "🎉", "- €"] {
        let r = search(&env, q(nothing));
        assert_eq!(r.total, 0, "{nothing}");
        assert!(r.notice.as_deref().is_some_and(|n| n.contains("no letter or digit")), "{nothing}: {:?}", r.notice);
    }
    let r = search(&env, q("project €"));
    assert!(!ids(&r).is_empty() && r.notice.is_some());
    let unthreaded = |text: &str| ids(&search(&env, SearchParams { group_threads: Some(false), ..q(text) }));
    // "moved" is in 102's text, not its subject.
    assert!(unthreaded("project -subject:moved").contains(&102));
    assert!(!unthreaded("project -subject:meeting").contains(&102));
}

#[test]
fn ids_given_twice_are_acted_on_once() {
    assert_eq!(crate::actions::unique_for_tests(&[3, 1, 3, 2, 1]), vec![3, 1, 2]);
}

// ---------------------------------------------------------------- second bug review

/// Folders of the user's named like an account's own (made after it, as
/// when imported) never take the role's moves: the account's own, oldest,
/// do. A row Mail keeps for a deleted mailbox comes last, deep or not.
#[test]
fn the_accounts_own_mailboxes_keep_their_roles() {
    let env = setup_with("ownroles", false, |cfg| {
        gmail_and_laposte(cfg);
        // Named alike by another app, made after; or before Mail's own (as
        // the rows of a Mac's iCloud may come in any order).
        add_mailboxes(cfg, fixture::ACCOUNT, &["Trash", "Spam", "Sent", "Archives"]);
        envelope_index(cfg)
            .execute_batch(&format!(
                "UPDATE mailboxes SET ROWID = 9001 WHERE url = 'imap://{a}/Deleted%20Messages';
                 UPDATE mailboxes SET ROWID = 9002 WHERE url = 'imap://{a}/Sent%20Messages';",
                a = fixture::ACCOUNT
            ))
            .unwrap();
        add_mailboxes(cfg, LAPOSTE, &["Archive", "INBOX/Archive"]);
    });
    let store = &env.ctx.store;
    let role = |account: &str, r: Role| store.role_mailbox_for_account(account, account, r).map(|m| m.display_path());
    assert_eq!(role(fixture::ACCOUNT, Role::Trash).as_deref(), Some("Deleted Messages"));
    assert_eq!(role(fixture::ACCOUNT, Role::Junk).as_deref(), Some("Junk"));
    assert_eq!(role(fixture::ACCOUNT, Role::Sent).as_deref(), Some("Sent Messages"));
    assert_eq!(role(fixture::ACCOUNT, Role::Archive).as_deref(), Some("Archive"));
    // La Poste's top-level "Archive" is a row Mail keeps, its folder gone.
    std::fs::remove_dir_all(store.version_dir.join(LAPOSTE).join("Archive.mbox")).unwrap();
    store.refresh_leftovers_now(&env.ctx.db());
    assert_eq!(role(LAPOSTE, Role::Archive).as_deref(), Some("INBOX/Archive"));
}

/// "archive" with the account named means that account's archive, as
/// without: never a folder of the user's holding the word.
#[test]
fn a_role_with_an_account_named_is_that_accounts_own() {
    let env = setup_with("roleaccount", false, |cfg| {
        gmail_and_laposte(cfg);
        add_mailboxes(cfg, LAPOSTE, &["INBOX/Archives%20clients%202019"]);
    });
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let p = crate::actions::MoveParams { ids: vec![1234], to: "archive".into(), account: Some(LAPOSTE.into()) };
    let r = rt.block_on(crate::actions::move_mail(&env.ctx, &p)).unwrap();
    assert_eq!(r.done, 0);
    assert!(r.failed[0].error.contains("archive mailbox"), "{:?}", r.failed);
    let p = crate::actions::MoveParams { ids: vec![1234], to: "archive".into(), account: Some("nowhere".into()) };
    assert!(rt.block_on(crate::actions::move_mail(&env.ctx, &p)).unwrap().failed[0].error.contains("No account"));
}

/// A mailbox at the top and one of the same name inside the inbox (iCloud):
/// the name alone is the one at the top, the full path the other.
#[test]
fn a_mailbox_at_the_top_is_named_by_its_name() {
    let env = setup_with("topname", false, |cfg| {
        add_mailboxes(cfg, fixture::ACCOUNT, &["Clients", "INBOX/Clients"]);
    });
    let store = &env.ctx.store;
    let path_of =
        |ids: Vec<i64>| ids.iter().filter_map(|i| store.mailbox(*i)).map(|m| m.display_path()).collect::<Vec<_>>();
    assert_eq!(path_of(store.resolve_mailboxes("Clients", None)), ["Clients"]);
    assert_eq!(path_of(store.resolve_mailboxes("INBOX/Clients", None)), ["INBOX/Clients"]);
    let pick = |spec: &str| {
        crate::mailboxes::mailbox_to_delete(store, spec, Some(fixture::ACCOUNT)).map(|m| m.map(|m| m.display_path()))
    };
    assert_eq!(pick("Clients"), Ok(Some("Clients".into())));
    assert_eq!(pick("INBOX/Clients"), Ok(Some("INBOX/Clients".into())));
}

/// A mailbox made again under the row Mail kept is found at once, before
/// the next look for leftovers.
#[test]
fn a_mailbox_made_again_is_found_at_once() {
    let env = setup_with("madeagain", false, |cfg| add_mailboxes(cfg, fixture::ACCOUNT, &["Clients"]));
    let store = &env.ctx.store;
    let folder = store.version_dir.join(fixture::ACCOUNT).join("Clients.mbox");
    std::fs::remove_dir_all(&folder).unwrap();
    store.refresh_leftovers_now(&env.ctx.db());
    let clients = || store.mailbox(store.resolve_mailboxes("Clients", None)[0]).unwrap();
    assert!(store.is_leftover(&clients()));
    std::fs::create_dir_all(&folder).unwrap();
    assert!(!store.is_leftover(&clients()));
    // Mail saying it shows one whose folder it has not made: listed.
    std::fs::remove_dir_all(&folder).unwrap();
    assert!(store.is_leftover(&clients()));
    store.note_mail_said(clients().id, true);
    assert!(!store.is_leftover(&clients()));
    let list = app::list_mailboxes(&env.ctx).unwrap();
    assert!(list.accounts.iter().flat_map(|a| &a.mailboxes).any(|m| m.name == "Clients"));
}

/// Without a token, undo takes this Timbre's latest move, or one made
/// before it started; never another Claude window's, made since.
#[test]
fn undo_without_a_token_is_this_windows() {
    let env = setup("undoowner", false);
    let other = crate::actions::UndoEntry {
        token: "t-other".into(),
        created: crate::util::now_unix() + 60,
        description: "moved 1 message(s) to archive".into(),
        items: vec![],
        undone: false,
        owner: std::process::id().wrapping_add(1),
    };
    std::fs::create_dir_all(&env.ctx.cfg.data_dir).unwrap();
    std::fs::write(env.ctx.cfg.data_dir.join("undo.json"), serde_json::to_vec(&vec![other]).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let r = rt.block_on(crate::actions::undo(&env.ctx, &crate::actions::UndoParams { token: None }));
    assert_eq!(r.unwrap_err(), "Nothing to undo with that token.");
    // With its token, it is found (and has nothing left to move back).
    let r = rt.block_on(crate::actions::undo(&env.ctx, &crate::actions::UndoParams { token: Some("t-other".into()) }));
    assert!(r.is_ok() || r.as_ref().is_err_and(|e| !e.contains("Nothing to undo")), "{r:?}");
}

/// Files that never come (bodies Mail does not download): the store is
/// walked again less and less often.
#[test]
fn walks_for_files_that_never_come_slow_down() {
    let env = setup("neverfile", true);
    let (cfg, store) = (env.ctx.cfg.clone(), env.ctx.store.clone());
    let mut conn = index::open(&cfg, &store, true).unwrap();
    let mut indexer = Indexer::new(cfg.clone(), store.clone(), env.ctx.progress.clone());
    let mail = rusqlite::Connection::open(store.db_path.clone()).unwrap();
    let mut m = fixture::messages().remove(1);
    m.id = 1701;
    fixture::write_message(&store.version_dir, &mail, &m).unwrap();
    let file = walkdir::WalkDir::new(store.version_dir.join(fixture::ACCOUNT))
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name() == "1701.emlx")
        .unwrap()
        .into_path();
    std::fs::remove_file(&file).unwrap();
    indexer.full_build(&mut conn, false).unwrap();
    let every = |conn: &rusqlite::Connection| index::get_meta(conn, "late_walk_every");
    let long_ago = (crate::util::now_unix() - 7 * 3600).to_string();
    index::set_meta(&conn, "last_walk", &long_ago).unwrap();
    indexer.reconcile_now(&mut conn).unwrap();
    assert_eq!(every(&conn).as_deref(), Some("43200"), "found nothing: twice as long");
    index::set_meta(&conn, "last_walk", &long_ago).unwrap();
    indexer.reconcile_now(&mut conn).unwrap();
    assert_eq!(index::get_meta(&conn, "last_walk").as_deref(), Some(long_ago.as_str()), "not due yet");
}

/// A symbol that folds to letters is searched like them.
#[test]
fn symbols_that_fold_to_letters_are_searched() {
    let env = setup("tm", true);
    let r = search(&env, q("™"));
    assert!(r.notice.as_deref().is_none_or(|n| !n.contains("no letter or digit")), "{:?}", r.notice);
}

/// Mail rebuilding its database puts a new file in its place: Timbre reads
/// the new one, not the file it opened.
#[test]
fn a_database_mail_replaced_is_read_anew() {
    let env = setup("newdb", false);
    let path = env.ctx.store.db_path.clone();
    let copy = path.with_extension("new");
    std::fs::copy(&path, &copy).unwrap();
    rusqlite::Connection::open(&copy)
        .unwrap()
        .execute(
            "UPDATE subjects SET subject = 'Rebuilt by Mail' WHERE ROWID = (SELECT subject FROM messages WHERE ROWID = 101)",
            [],
        )
        .unwrap();
    assert_ne!(env.ctx.meta(101).unwrap().subject, "Rebuilt by Mail");
    std::fs::rename(&copy, &path).unwrap();
    assert_eq!(env.ctx.meta(101).unwrap().subject, "Rebuilt by Mail");
}

/// Each message of a thread carries the warnings read_mail gives it.
#[test]
fn a_thread_warns_as_reading_does() {
    let env = setup("threadwarn", true);
    let c = &env.ctx;
    for id in [101, 102, 104, 1234] {
        let t = app::get_thread(c, &ThreadParams { id, max_messages: None, max_chars_per_message: None }).unwrap();
        for m in &t.messages {
            let read = app::read_mail(c, &ReadParams { id: m.id, max_chars: None, strip_quotes: None }).unwrap();
            assert_eq!(m.warnings, read.warnings, "message {}", m.id);
            assert_eq!(m.hidden_text_removed, read.hidden_text_removed, "message {}", m.id);
        }
    }
}

/// A mailbox Timbre made a moment ago, before Mail's database lists it, is
/// listed and mail can be filed into it.
#[test]
fn a_mailbox_just_made_can_be_filed_into() {
    let env = setup_with("justmade", false, gmail_and_laposte);
    let loc = crate::mailapp::Loc {
        account_id: LAPOSTE.into(),
        account: LAPOSTE.into(),
        path: vec!["INBOX".into(), "Tout neuf".into()],
        local: false,
    };
    env.ctx.store.note_made(loc);
    let list = app::list_mailboxes(&env.ctx).unwrap();
    let lp = list.accounts.iter().find(|a| a.account == LAPOSTE).unwrap();
    assert!(lp.mailboxes.iter().any(|m| m.name == "INBOX/Tout neuf"), "{:?}", lp.mailboxes);
    // Planned and handed to Mail (which cannot run here), not "no mailbox".
    let e = move_mail(&env, vec![1234], "Tout neuf").unwrap_err();
    assert!(e.contains("only available on macOS"), "{e}");
}
