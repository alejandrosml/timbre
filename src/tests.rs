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
    // Pagination.
    let r = search(&env, SearchParams { limit: Some(2), ..Default::default() });
    assert_eq!(r.results.len(), 2);
    assert_eq!(r.next_offset, Some(2));
    assert_eq!(r.total, 5);
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

    // It is deleted: a reconcile pass drops it.
    mail.execute("UPDATE messages SET deleted = 1 WHERE ROWID = 1300", []).unwrap();
    indexer.reconcile_now(&mut conn).unwrap();
    assert!(ids(&search(&env, q("zebra"))).is_empty());
}

#[test]
fn status_reports_index_and_settings() {
    let env = setup("status", true);
    let s = app::status(&env.ctx).unwrap();
    assert_eq!(s.messages, 6);
    assert_eq!(s.index.state, "ready");
    assert!(!s.sending_enabled);
}
