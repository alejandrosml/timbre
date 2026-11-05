//! Making and deleting mailboxes.
//!
//! Mail makes a mailbox when a script asks it to, at least at some
//! providers: Timbre asks Mail first, which needs no password and shows
//! the mailbox at once. Mail deletes none: its scripting answers
//! "AppleEvent handler failed" (-10000) for every account on current
//! macOS. Its database is no way round: it is Mail's copy of what the
//! server holds, and Mail would bring the mailbox back at its next check.
//! So Timbre deletes, and makes what Mail would not, on the server (IMAP),
//! as Mail's own Mailbox menu does, then has Mail synchronize.
//!
//! That takes the account's password, which Mail keeps where no other app
//! may read it. The first time the user makes or deletes a mailbox in an
//! account, and only then, a window of Timbre asks for it in one sentence;
//! once the server accepts it, it stays in the user's keychain, readable by
//! Timbre alone. Claude never sees it.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;
use unicode_normalization::UnicodeNormalization;

use crate::app::Ctx;
use crate::imap::{self, Entry, Server, Session};
use crate::keychain::{self, Found};
use crate::mailapp::{self, Loc};
use crate::store::{Mailbox, Role, Store, account_key};

/// How long a tool waits for the user to answer a window before saying so.
const WAIT_FOR_USER: Duration = Duration::from_secs(30);
/// How long a password checked with the server is kept in memory.
const KEEP: Duration = Duration::from_secs(30 * 60);
/// How long Mail is given to show the change.
const MAIL_CATCHES_UP: Duration = Duration::from_secs(10);
/// How long a tool call may take in all: Claude waits about a minute.
const CALL_BUDGET: Duration = Duration::from_secs(50);
/// Time kept for the server's part once the password is there.
const SERVER_TIME: Duration = Duration::from_secs(15);
/// How long a look in the keychain may take when no window may open.
const AT_HAND: Duration = Duration::from_secs(5);
/// How long a session signed in to check a password waits for the
/// operation that follows.
const READY_FOR: Duration = Duration::from_secs(60);

/// What a deletion whose answer was lost says: done, perhaps.
const MAYBE_DELETED: &str = "may have been deleted";

/// When a tool call must be done by.
#[derive(Debug, Clone, Copy)]
struct Deadline(Instant);

impl Deadline {
    fn new() -> Deadline {
        Deadline(Instant::now() + CALL_BUDGET)
    }

    fn left(self) -> Duration {
        self.0.saturating_duration_since(Instant::now())
    }

    /// `d`, or what the call has left if that is less.
    fn cap(self, d: Duration) -> Duration {
        d.min(self.left())
    }
}

// ---------------------------------------------------------------- params

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteMailboxParams {
    /// The mailbox (folder, or Gmail label) to delete: its name or path, as
    /// list_mailboxes or Mail shows it.
    pub mailbox: String,
    /// Its account, when the name exists in several accounts.
    pub account: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateMailboxParams {
    /// The mailbox to make, as Mail would show its path: "Receipts", or
    /// "Projects/2026" for 2026 inside Projects.
    pub mailbox: String,
    /// The account to make it in, as list_mailboxes names it.
    pub account: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct MailboxDone {
    /// As Mail shows it.
    pub mailbox: String,
    pub account: String,
    /// Its path on the server.
    pub on_server: String,
    pub note: String,
}

// ---------------------------------------------------------------- accounts

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Google,
    Apple,
    Yahoo,
    Microsoft,
    Other,
}

/// Who runs a mail server, by its name: some take only an app password
/// over IMAP, Microsoft only its own sign-in.
pub fn provider(host: &str) -> Provider {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let under = |d: &str| h == d || h.ends_with(&format!(".{d}"));
    if under("gmail.com") || under("googlemail.com") || under("google.com") {
        Provider::Google
    } else if under("me.com") || under("icloud.com") || under("mac.com") {
        Provider::Apple
    } else if under("yahoo.com") || under("aol.com") || under("ymail.com") {
        Provider::Yahoo
    } else if under("office365.com") || under("outlook.com") || under("hotmail.com") || under("live.com") {
        Provider::Microsoft
    } else {
        Provider::Other
    }
}

/// An account as Mail reaches its server.
#[derive(Debug, Clone)]
pub struct Account {
    /// Its name in Mail.
    pub name: String,
    pub server: Server,
    pub provider: Provider,
}

const IN_MAIL: &str = "The user can do it in Mail: select the mailbox, then Mailbox > Delete Mailbox (or Mailbox > \
                       New Mailbox to make one).";

/// How Mail reaches this account's server: what Timbre needs to act there.
async fn account(loc: &Loc, deadline: Deadline) -> Result<Account, String> {
    let what = format!("'{}'", loc.account);
    let v = mailapp::run(mailapp::ACCOUNT_SERVER, &json!({ "loc": loc }), deadline.cap(Duration::from_secs(20)))
        .await
        .map_err(|e| e.to_string())?;
    let field = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or("").trim().to_string();
    let (kind, host, user) = (field("type").to_lowercase(), field("server"), field("user"));
    let name = Some(field("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| loc.account.clone());
    if kind == "pop" {
        return Err(format!(
            "{what} is a POP account: its mailboxes are kept by Mail on this Mac, where Mail lets no other app \
             make or delete one. {IN_MAIL}"
        ));
    }
    if kind != "imap" && kind != "icloud" {
        return Err(format!(
            "{what} is not an IMAP account (Exchange?): Timbre makes and deletes mailboxes over IMAP only. {IN_MAIL}"
        ));
    }
    if host.is_empty() || user.is_empty() {
        return Err(format!("Mail does not say which server {what} uses. {IN_MAIL}"));
    }
    let provider = provider(&host);
    if provider == Provider::Microsoft {
        return Err(format!(
            "Microsoft lets mail apps into {what} only with its own sign-in, which Mail keeps to itself: Timbre \
             cannot make or delete mailboxes there. {IN_MAIL}"
        ));
    }
    let port = v.get("port").and_then(serde_json::Value::as_i64);
    let ssl = v.get("ssl").and_then(serde_json::Value::as_bool);
    Ok(Account { name, server: Server::new(&host, port, ssl, &user), provider })
}

/// An account by the name the user gave, as a place for Mail's scripting.
fn find_account(store: &Store, name: &str) -> Result<Loc, String> {
    let want = account_key(name);
    if want.is_empty() {
        return Err("Say which account (list_mailboxes names them).".into());
    }
    let mailboxes = store.mailboxes();
    let mut accounts: Vec<(&str, &str)> =
        mailboxes.values().filter(|m| !m.local).map(|m| (m.account_id.as_str(), m.account.as_str())).collect();
    accounts.sort_unstable();
    accounts.dedup();
    let exact: Vec<_> =
        accounts.iter().filter(|(id, n)| account_key(n) == want || id.eq_ignore_ascii_case(name.trim())).collect();
    let hits = if exact.is_empty() {
        accounts.iter().filter(|(_, n)| account_key(n).contains(&want)).collect()
    } else {
        exact
    };
    match hits.as_slice() {
        [(id, n)] => Ok(Loc { account_id: id.to_string(), account: n.to_string(), path: vec![], local: false }),
        [] if ["onmymac", "surmonmac"].contains(&want.as_str()) => Err(ON_MY_MAC.into()),
        [] => {
            let mut names: Vec<&str> = accounts.iter().map(|(_, n)| *n).collect();
            names.dedup();
            Err(format!("No account named '{name}'. Accounts: {}.", names.join(", ")))
        }
        _ => Err(format!(
            "'{name}' matches several accounts ({}): give its full name.",
            hits.iter().map(|(_, n)| *n).collect::<Vec<_>>().join(", ")
        )),
    }
}

const ON_MY_MAC: &str = "On My Mac mailboxes are kept by Mail alone, with no server Timbre could ask, and Mail lets \
                         no other app make or delete one.";

// ---------------------------------------------------------------- passwords

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Looking in the keychain.
    Looking,
    /// macOS asks the user whether Timbre may read the password Mail kept.
    MacOsAsks,
    /// Timbre's window asks the user for the password.
    TimbreAsks,
    /// A password found or typed is being tried on the server.
    Checking,
    Done(Result<(), String>),
}

#[derive(Default)]
struct Passwords {
    /// Checked with the server, and when.
    known: HashMap<String, (String, Instant)>,
    /// Being found, or asked of the user; when it began, or ended.
    pending: HashMap<String, (watch::Receiver<Step>, Instant)>,
}

/// How long the outcome of a search that failed (the user closed the
/// window) is told to the calls that follow; past that, a new call asks
/// again.
const OUTCOME_KEPT: Duration = Duration::from_secs(90);

static PASSWORDS: LazyLock<Mutex<Passwords>> = LazyLock::new(Mutex::default);

fn passwords() -> std::sync::MutexGuard<'static, Passwords> {
    PASSWORDS.lock().unwrap_or_else(|e| e.into_inner())
}

fn key(s: &Server) -> String {
    format!("{}\u{1f}{}\u{1f}{}", s.user, s.host.to_ascii_lowercase(), s.port)
}

fn known(k: &str) -> Option<String> {
    let mut p = passwords();
    match p.known.get(k) {
        Some((pw, at)) if at.elapsed() < KEEP => Some(pw.clone()),
        Some(_) => {
            p.known.remove(k);
            None
        }
        None => None,
    }
}

fn remember(k: &str, password: String) {
    passwords().known.insert(k.to_string(), (password, Instant::now()));
}

fn forget(k: &str) {
    passwords().known.remove(k);
}

/// Sessions signed in when a password was checked, for the operation that
/// follows: one sign-in instead of two, as servers limit how many they take.
/// A signed-in session, the password it signed in with, and when.
type Ready = (Session, String, Instant);

static READY: LazyLock<Mutex<HashMap<String, Ready>>> = LazyLock::new(Mutex::default);

fn keep_ready(k: &str, session: Session, password: &str) {
    let older = READY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(k.to_string(), (session, password.to_string(), Instant::now()));
    drop(older);
}

/// The session signed in with this password a moment ago, if there is one.
fn take_ready(k: &str, password: &str) -> Option<Session> {
    let (session, pw, at) = READY.lock().unwrap_or_else(|e| e.into_inner()).remove(k)?;
    (pw == password && at.elapsed() < READY_FOR).then_some(session)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("internal error: {e}"))
}

/// Sign in with a password: the session, kept for what follows.
async fn check(s: &Server, password: &str) -> Result<Session, imap::Error> {
    let (s, password) = (s.clone(), password.to_string());
    blocking(move || imap::open(&s, &password)).await.unwrap_or_else(|e| Err(imap::Error::Network(e)))
}

/// What went wrong with the server, as Claude should tell it.
fn server_trouble(acct: &Account, e: &imap::Error) -> String {
    format!("Timbre could not do it on the server of '{}' ({}): {e}.", acct.name, acct.server.host)
}

/// The password for this account's server, checked with it: the one Timbre
/// keeps, the one Mail kept where older macOS had it, or the user's answer
/// to a window asking for it (the first time only). Waits a while for the
/// user; past that, says so, and a later call takes up the same window.
async fn password(acct: &Account, deadline: Deadline) -> Result<String, String> {
    let k = key(&acct.server);
    for _ in 0..2 {
        if let Some(p) = known(&k) {
            return Ok(p);
        }
        match wait_for_password(acct, &k, deadline).await {
            // Found, but forgotten from memory since (the user came back
            // long after): look again, the keychain has it now.
            Ok(None) => continue,
            Ok(Some(())) => {}
            Err(e) => return Err(e),
        }
    }
    known(&k).ok_or_else(|| "internal error: the password was not kept".into())
}

/// Start looking for the password (or join the search under way) and wait
/// a while for it: as long as the call can, keeping time for the server.
async fn wait_for_password(acct: &Account, k: &str, deadline: Deadline) -> Result<Option<()>, String> {
    let mut rx = {
        let mut p = passwords();
        let stale = p
            .pending
            .get(k)
            .is_some_and(|(rx, at)| matches!(*rx.borrow(), Step::Done(_)) && at.elapsed() > OUTCOME_KEPT);
        if stale {
            p.pending.remove(k);
        }
        match p.pending.get(k) {
            Some((rx, _)) => rx.clone(),
            None => {
                let (tx, rx) = watch::channel(Step::Looking);
                p.pending.insert(k.to_string(), (rx.clone(), Instant::now()));
                tokio::spawn(find_password(acct.clone(), k.to_string(), tx));
                rx
            }
        }
    };
    let wait = deadline.cap(WAIT_FOR_USER).min(deadline.left().saturating_sub(SERVER_TIME));
    let waited = tokio::time::timeout(wait, async {
        match rx.wait_for(|s| matches!(s, Step::Done(_))).await {
            Ok(step) => step.clone(),
            Err(_) => Step::Done(Err("internal error: the password search stopped".into())),
        }
    })
    .await;
    let step = waited.unwrap_or_else(|_| rx.borrow().clone());
    let ask_again = "Ask the user to answer it, then call this tool again (nothing was done yet).";
    if wait.is_zero() && matches!(step, Step::Looking | Step::Checking) {
        return Err(format!(
            "Mail was slow to answer about '{}', so this call ran out of time before the password step. Call this \
             tool again (nothing was done yet).",
            acct.name
        ));
    }
    match step {
        Step::Done(r) => {
            {
                // This search's outcome only: a new one may have begun since.
                let mut p = passwords();
                if p.pending.get(k).is_some_and(|(other, _)| other.same_channel(&rx)) {
                    p.pending.remove(k);
                }
            }
            r?;
            Ok(known(k).map(|_| ()))
        }
        Step::TimbreAsks => Err(format!(
            "A window of Timbre on the Mac asks the user for the password of the '{}' account, once: Timbre keeps it \
             in the keychain for next time. {ask_again}",
            acct.name
        )),
        Step::MacOsAsks => Err(format!(
            "macOS asks the user, in a window on the Mac, whether Timbre may use the password Mail saved for '{}' \
             (with their Mac's login password; \"Always Allow\" spares them the question next time). {ask_again}",
            acct.name
        )),
        Step::Checking => Err(format!(
            "Timbre is trying the password of '{}' on its server ({}), which is slow to answer. Call this tool \
             again in a moment (nothing was done yet).",
            acct.name, acct.server.host
        )),
        Step::Looking => Err(format!(
            "Timbre is still looking for the password of '{}' in the keychain; macOS may be asking the user about \
             it. {ask_again}",
            acct.name
        )),
    }
}

async fn find_password(acct: Account, k: String, tx: watch::Sender<Step>) {
    let r = find_password_steps(&acct, &k, &tx).await;
    if let Some(entry) = passwords().pending.get_mut(&k) {
        entry.1 = Instant::now();
    }
    let _ = tx.send(Step::Done(r));
}

async fn find_password_steps(acct: &Account, k: &str, tx: &watch::Sender<Step>) -> Result<(), String> {
    let s = &acct.server;
    let (host, user) = (s.host.clone(), s.user.clone());
    let mut note = Note::None;
    // The one Timbre keeps.
    let (h, u) = (host.clone(), user.clone());
    if let Found::Password(p) = blocking(move || keychain::saved(&h, &u)).await? {
        let _ = tx.send(Step::Checking);
        match check(s, &p).await {
            Ok(session) => {
                keep_ready(k, session, &p);
                remember(k, p);
                return Ok(());
            }
            // Turned down: changed since. Any other refusal leaves it kept
            // (the server in trouble, or a message for the user).
            Err(imap::Error::Auth(_)) => {
                let (h, u) = (host.clone(), user.clone());
                blocking(move || keychain::forget(&h, &u)).await?;
                note = Note::Stale;
            }
            // The server's word for the user: the window asking shows it.
            Err(imap::Error::Alert(why)) => note = Note::Alert(why),
            Err(e) => return Err(server_trouble(acct, &e)),
        }
    }
    // The one Mail kept, where older macOS had it: macOS asks the user.
    let _ = tx.send(Step::MacOsAsks);
    let (h, u) = (host.clone(), user.clone());
    if let Found::Password(p) = blocking(move || keychain::mails(&h, &u)).await? {
        let _ = tx.send(Step::Checking);
        match check(s, &p).await {
            Ok(session) => {
                keep_ready(k, session, &p);
                remember(k, p);
                return Ok(());
            }
            Err(imap::Error::Auth(_)) => {}
            // Gmail asking for an app password instead of the account's own:
            // the window asking shows it, with the button that makes one.
            Err(imap::Error::Alert(why)) => note = Note::Alert(why),
            Err(e) => return Err(server_trouble(acct, &e)),
        }
    }
    // Ask the user, once; again if the server turns it down.
    let fr = blocking(mailapp::user_reads_french).await.unwrap_or(false);
    for _ in 0..3 {
        let _ = tx.send(Step::TimbreAsks);
        let ask = Ask::in_language(acct, &note, fr);
        let typed = mailapp::ask_password(&ask.text, ask.ok, ask.cancel, ask.link)
            .await
            .map_err(|e| format!("Timbre could not show its password window on the Mac ({e}). Nothing was done."))?;
        let Some(p) = typed.filter(|p| !p.is_empty()) else {
            return Err(format!(
                "The user closed the password window for '{}'. Nothing was done; it is asked again only if they \
                 make or delete a mailbox in this account again.",
                acct.name
            ));
        };
        let _ = tx.send(Step::Checking);
        match check(s, &p).await {
            Ok(session) => {
                let (h, u, pw) = (host.clone(), user.clone(), p.clone());
                if let Err(e) = blocking(move || keychain::save(&h, &u, &pw)).await? {
                    tracing::warn!("the password for {host} could not be kept in the keychain: {e}");
                }
                keep_ready(k, session, &p);
                remember(k, p);
                return Ok(());
            }
            Err(imap::Error::Auth(_)) => note = Note::Wrong,
            // The server's word for the user (Gmail asking for an app
            // password): shown in the window asking again.
            Err(imap::Error::Alert(why)) => note = Note::Alert(why),
            Err(e) => return Err(server_trouble(acct, &e)),
        }
    }
    let said = match &note {
        Note::Alert(why) => format!(" (it says: {})", imap::alert_text(why)),
        _ => String::new(),
    };
    Err(format!("The server of '{}' turned the password down three times{said}. Nothing was done.", acct.name))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Note {
    None,
    /// The password typed was turned down.
    Wrong,
    /// The one kept no longer works.
    Stale,
    /// The server's message for the user, about the password typed.
    Alert(String),
}

/// The window asking for a password: one sentence on why, and that it is
/// asked only once; a button to the provider's page where an app password
/// is needed.
struct Ask {
    text: String,
    ok: &'static str,
    cancel: &'static str,
    link: Option<(&'static str, &'static str)>,
}

impl Ask {
    fn in_language(acct: &Account, note: &Note, fr: bool) -> Ask {
        let (name, user) = (&acct.name, &acct.server.user);
        let app_password = match (acct.provider, fr) {
            (Provider::Google, true) => Some("un mot de passe d'application Google"),
            (Provider::Google, false) => Some("a Google app password"),
            (Provider::Apple, true) => Some("un mot de passe pour app Apple"),
            (Provider::Apple, false) => Some("an Apple app-specific password"),
            (Provider::Yahoo, true) => Some("un mot de passe d'application Yahoo"),
            (Provider::Yahoo, false) => Some("a Yahoo app password"),
            _ => None,
        };
        // What the server said, as it said it: short, on one line.
        let said = |why: &str| -> String {
            let t: String = imap::alert_text(why).chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
            t.chars().take(200).collect()
        };
        let mut text = match (note, fr) {
            (Note::Wrong, true) => "Le serveur a refusé ce mot de passe.\n\n".to_string(),
            (Note::Wrong, false) => "The server did not accept that password.\n\n".to_string(),
            (Note::Stale, true) => "Le mot de passe gardé n'est plus accepté.\n\n".to_string(),
            (Note::Stale, false) => "The saved password is no longer accepted.\n\n".to_string(),
            (Note::Alert(why), true) => format!("Le serveur répond : « {} »\n\n", said(why)),
            (Note::Alert(why), false) => format!("The server says: “{}”\n\n", said(why)),
            (Note::None, _) => String::new(),
        };
        text.push_str(&match (app_password, fr) {
            (None, true) => format!(
                "Pour créer ou supprimer des dossiers dans « {name} », Timbre a besoin du mot de passe de ce compte \
                 ({user}) : saisissez-le une seule fois, il restera dans votre trousseau."
            ),
            (None, false) => format!(
                "To create or delete folders in “{name}”, Timbre needs this account's password ({user}): enter it \
                 once, and it stays in your keychain."
            ),
            (Some(what), true) => format!(
                "Pour créer ou supprimer des dossiers dans « {name} », Timbre a besoin de {what} pour ce compte \
                 ({user}) : créez-le avec le bouton ci-dessous, collez-le ici une seule fois, il restera dans votre \
                 trousseau."
            ),
            (Some(what), false) => format!(
                "To create or delete folders in “{name}”, Timbre needs {what} for this account ({user}): create one \
                 with the button below, paste it here once, and it stays in your keychain."
            ),
        });
        let url = match acct.provider {
            Provider::Google => Some("https://myaccount.google.com/apppasswords"),
            Provider::Apple => Some("https://account.apple.com"),
            Provider::Yahoo => Some("https://login.yahoo.com/account/security"),
            _ => None,
        };
        let label = if fr { "Créer le mot de passe…" } else { "Create one…" };
        Ask { text, ok: "OK", cancel: if fr { "Annuler" } else { "Cancel" }, link: url.map(|u| (label, u)) }
    }
}

// ---------------------------------------------------------------- the server side

/// Two paths name the same mailbox: part for part, accents composed, INBOX
/// in any case (IMAP's one case-insensitive name).
fn same_path(a: &[String], b: &[String]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).enumerate().all(|(i, (x, y))| {
            let (x, y): (String, String) = (x.nfc().collect(), y.nfc().collect());
            if i == 0 && x.eq_ignore_ascii_case("INBOX") { y.eq_ignore_ascii_case("INBOX") } else { x == y }
        })
}

/// The server's mailbox at exactly this path. "Clients" and "clients" are
/// two mailboxes on most servers.
fn find<'a>(all: &'a [Entry], path: &[String]) -> Option<&'a Entry> {
    all.iter().find(|e| same_path(&e.segments(), path))
}

/// The server's mailbox at a path the user typed: exactly, else the one
/// whose path differs only in case. Several so: which, to be given in full.
fn find_typed<'a>(all: &'a [Entry], path: &[String]) -> Result<Option<&'a Entry>, Vec<&'a Entry>> {
    if let Some(e) = find(all, path) {
        return Ok(Some(e));
    }
    let key = |p: &[String]| p.iter().map(|s| s.nfc().collect::<String>().to_lowercase()).collect::<Vec<_>>();
    let want = key(path);
    let hits: Vec<&Entry> = all.iter().filter(|e| key(&e.segments()) == want).collect();
    match hits.len() {
        0 => Ok(None),
        1 => Ok(Some(hits[0])),
        _ => Err(hits),
    }
}

/// Why Mail or the provider keeps a mailbox of the server, by its path:
/// what servers that flag nothing (La Poste, Orange, older ones) name the
/// account's drafts, sent mail, trash and junk, Gmail's own folders, mail
/// being sent, Apple Notes.
fn kept_on_server(path: &[String]) -> Option<String> {
    let (role, role_strength) = Role::detect_scored(path);
    let m = Mailbox {
        id: 0,
        url: String::new(),
        account_id: "server".into(),
        account: String::new(),
        path: path.to_vec(),
        name: path.last().cloned().unwrap_or_default(),
        role,
        role_strength,
        local: false,
    };
    kept_by_mail(&m)
}

/// Delete the mailbox at `path` on the server, if it holds no message and
/// no other mailbox, and is none the server, the provider or Mail keeps for
/// itself.
fn delete_in(s: &mut Session, path: &[String], what: &str) -> Result<(), String> {
    let refused = |e: imap::Error| format!("{what} was not deleted: {e}.");
    let all = s.list().map_err(refused)?;
    let Some(target) = find(&all, path) else {
        return Err(format!(
            "The server does not have {what} (deleted elsewhere already?). Mail drops it at its next check."
        ));
    };
    let segments = target.segments();
    if segments.len() == 1 && segments[0].eq_ignore_ascii_case("INBOX") {
        return Err(format!("{what} is the account's inbox: it cannot be deleted."));
    }
    if let Some(why) = kept_on_server(&segments) {
        return Err(format!("{what} cannot be deleted: {why}."));
    }
    if target.has("\\noselect") || target.has("\\nonexistent") {
        return Err(format!("{what} is a folder the server keeps only to hold others: it is not deleted."));
    }
    if let Some(role) = target.special_use() {
        return Err(format!("{what} cannot be deleted: the server keeps it as the account's {role} mailbox."));
    }
    let inner = all
        .iter()
        .filter(|e| e.segments().len() > segments.len() && same_path(&e.segments()[..segments.len()], &segments))
        .count();
    if inner > 0 {
        return Err(format!(
            "{what} holds {inner} other mailbox(es) on the server: delete those first, each once it is empty."
        ));
    }
    let held = s.messages(&target.raw).map_err(refused)?;
    if held > 0 {
        return Err(format!(
            "{what} holds {held} message(s) on the server. Timbre deletes empty mailboxes only, so no mail is ever \
             lost: move the messages first (move_mail, which can be undone), then delete the mailbox."
        ));
    }
    // IMAP has no "delete if empty": a message filed into it in the instant
    // between the count and the deletion would go with it. Only a server's
    // own filter could, into a mailbox the user is deleting; Gmail keeps
    // such messages in All Mail anyway.
    let raw = target.raw.clone();
    match s.delete(&raw) {
        Ok(()) => {}
        // Answered no: not deleted.
        Err(e @ imap::Error::Refused(_)) => return Err(refused(e)),
        // Asked, and the answer lost: the server may have done it.
        Err(e) => {
            return Err(format!(
                "{what} {MAYBE_DELETED}: the server's answer did not come ({e}). Call list_mailboxes in a moment \
                 to know."
            ));
        }
    }
    let _ = s.subscribe(&raw, false);
    match s.list() {
        Ok(after) if find(&after, path).is_some() => {
            Err(format!("The server said it deleted {what} but still lists it."))
        }
        // Deleted, the server said so; its list could not be read again.
        _ => Ok(()),
    }
}

/// Where the server keeps an account's mailboxes ("INBOX" at La Poste and
/// Orange, which Mail does not show), and its hierarchy delimiter.
fn layout(s: &mut Session, all: &[Entry]) -> Result<(Vec<String>, char), imap::Error> {
    let ns = s.namespace()?;
    let delimiter = ns.as_ref().and_then(|n| n.1).or_else(|| all.iter().find_map(|e| e.delimiter)).unwrap_or('/');
    let prefix = match ns {
        Some((p, _)) => p.split(delimiter).filter(|x| !x.is_empty()).map(str::to_string).collect(),
        // No NAMESPACE: inside INBOX when every other mailbox is.
        None => {
            let others: Vec<Vec<String>> = all
                .iter()
                .map(Entry::segments)
                .filter(|p| !(p.len() == 1 && p[0].eq_ignore_ascii_case("INBOX")))
                .collect();
            if !others.is_empty() && others.iter().all(|p| p.len() > 1 && p[0].eq_ignore_ascii_case("INBOX")) {
                vec!["INBOX".to_string()]
            } else {
                vec![]
            }
        }
    };
    Ok((prefix, delimiter))
}

/// The parts of a mailbox path as the user wrote it, or why it cannot be a
/// mailbox name.
fn parts_of(name: &str) -> Result<Vec<String>, String> {
    let parts: Vec<String> = name.split('/').map(|p| p.trim().nfc().collect::<String>()).collect();
    if parts.iter().any(String::is_empty) {
        return Err(format!("'{name}' is not a mailbox name: no part of it may be empty."));
    }
    if parts.len() > 10 || parts.iter().any(|p| p.chars().count() > 200) {
        return Err(format!("'{name}' is too long for a mailbox name."));
    }
    if let Some(p) = parts.iter().find(|p| p.chars().any(|c| c.is_control() || c == '*' || c == '%')) {
        return Err(format!("'{p}' holds a character mail servers do not take in mailbox names."));
    }
    if parts.len() == 1 && parts[0].eq_ignore_ascii_case("INBOX") {
        return Err("Every account has its inbox already.".into());
    }
    Ok(parts)
}

/// Where Mail shows an account's mailboxes: inside INBOX (its path prefix,
/// at La Poste and Orange) when every one it lists but the inbox is there,
/// at the top when one is not; none when it lists the inbox alone. Mail
/// shows nothing outside its prefix, so a mailbox made there must follow
/// it, whatever the server's NAMESPACE says.
pub(crate) fn mail_prefix(store: &Store, loc: &Loc) -> Option<Vec<String>> {
    let mailboxes = store.mailboxes();
    let inbox = |m: &Mailbox| m.path.len() == 1 && m.path[0].eq_ignore_ascii_case("INBOX");
    let others: Vec<&Mailbox> = mailboxes
        .values()
        .filter(|m| {
            !m.local
                && crate::store::same_account(m, &loc.account_id, &loc.account)
                && !inbox(m)
                && !store.is_leftover(m)
        })
        .collect();
    if others.is_empty() {
        return None;
    }
    let inside = others.iter().all(|m| m.path.len() >= 2 && m.path[0].eq_ignore_ascii_case("INBOX"));
    Some(if inside { vec!["INBOX".to_string()] } else { vec![] })
}

/// Whether Mail shows this account's mailboxes from INBOX on (La Poste,
/// Orange), at the top (false), or it is not known (None): where Mail's
/// scripting finds a mailbox of Timbre's paths.
pub(crate) fn prefixed(store: &Store, loc: &Loc) -> Option<bool> {
    mail_prefix(store, loc).map(|p| !p.is_empty())
}

/// Make the mailbox the user named (`wanted`, as Mail shows paths) on the
/// server, where Mail shows the account's mailboxes (`mail_prefix`) or,
/// when that is not known, where the server keeps them. Returns its path on
/// the server, and whether it was there already.
fn create_in(
    s: &mut Session,
    wanted: &[String],
    what: &str,
    mail_prefix: Option<Vec<String>>,
) -> Result<(Vec<String>, bool), String> {
    let refused = |e: imap::Error| format!("{what} was not made: {e}.");
    let all = s.list().map_err(refused)?;
    let (server_prefix, delimiter) = layout(s, &all).map_err(refused)?;
    let prefix = mail_prefix.unwrap_or(server_prefix);
    let mut path = wanted.to_vec();
    // A path given with the server's prefix ("INBOX/Projects") keeps it.
    if !(path.len() > prefix.len() && same_path(&path[..prefix.len()], &prefix)) {
        path = prefix.iter().cloned().chain(path).collect();
    }
    if let Some(p) = path.iter().find(|p| p.contains(delimiter)) {
        return Err(format!(
            "'{p}' holds '{delimiter}', which this server puts between a mailbox and the ones inside it: choose a \
             name without it."
        ));
    }
    if path.first().is_some_and(|p| p.starts_with('[')) {
        return Err(format!("{what} would be inside the provider's own folders, which it keeps for itself."));
    }
    if find(&all, &path).is_some() {
        return Ok((path, true));
    }
    let raw = imap::mutf7_encode(&path.join(&delimiter.to_string()));
    s.create(&raw).map_err(refused)?;
    let _ = s.subscribe(&raw, true);
    if find(&s.list().map_err(refused)?, &path).is_none() {
        return Err(format!("The server said it made {what} but does not list it."));
    }
    Ok((path, false))
}

/// Sign in (or take the session the password check left signed in) and run
/// `f` on the server, off the async threads, within the call's time: past
/// it, the call says so (the server may still finish in a moment).
async fn on_server<T: Send + 'static>(
    acct: &Account,
    password: String,
    deadline: Deadline,
    f: impl FnOnce(&mut Session) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let time = deadline.left().max(Duration::from_secs(5));
    match tokio::time::timeout(time, signed_in(acct, password, f)).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "The server of '{}' ({}) is slow to answer: what was asked may still be done in a moment. Call \
             list_mailboxes shortly to see.",
            acct.name, acct.server.host
        )),
    }
}

async fn signed_in<T: Send + 'static>(
    acct: &Account,
    password: String,
    f: impl FnOnce(&mut Session) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let server = acct.server.clone();
    let k = key(&server);
    let opened = match take_ready(&k, &password) {
        Some(session) => Ok(session),
        None => {
            let server = server.clone();
            blocking(move || imap::open(&server, &password)).await?
        }
    };
    let mut session = match opened {
        Ok(s) => s,
        Err(e @ imap::Error::Auth(_)) => {
            // Changed since: forget it, so the next try asks again.
            forget(&k);
            let (h, u) = (server.host.clone(), server.user.clone());
            blocking(move || keychain::forget(&h, &u)).await?;
            return Err(format!(
                "{} Call this tool again: Timbre then asks the user for the password anew.",
                server_trouble(acct, &e)
            ));
        }
        Err(e) => return Err(server_trouble(acct, &e)),
    };
    blocking(move || {
        let r = f(&mut session);
        session.logout();
        r
    })
    .await?
}

/// Whether Mail shows the mailbox at exactly `loc`, waiting up to `wait`
/// for it to show it (`present`) or to stop showing it; none if Mail
/// cannot be asked, or the call has no time left for it.
async fn mail_shows(store: &Store, loc: &Loc, present: bool, wait: Duration, deadline: Deadline) -> Option<bool> {
    if deadline.left() < Duration::from_secs(3) {
        return None;
    }
    let wait = deadline.cap(wait);
    let tries = u64::try_from(wait.as_millis() / 500).unwrap_or(1).max(1);
    let input = json!({ "loc": loc, "present": present, "tries": tries, "prefixed": prefixed(store, loc) });
    let v = mailapp::run(mailapp::MAILBOX_SHOWN, &input, deadline.cap(wait + Duration::from_secs(10))).await.ok()?;
    v.get("found").and_then(serde_json::Value::as_bool)
}

/// Ask Mail to synchronize the account, without waiting: Mail may take its
/// time, and the change is made already.
fn ask_mail_to_sync(loc: &Loc) {
    let (id, account) = (loc.account_id.clone(), loc.account.clone());
    tokio::spawn(async move {
        if let Err(e) = mailapp::sync_account(&id).await {
            tracing::warn!("Mail was not asked to synchronize {account}: {e:#}");
        }
    });
}

/// A path as Mail shows it: from INBOX on where the account keeps every
/// mailbox inside it (La Poste, Orange), whole where it does not (iCloud's
/// "INBOX/Clients" is a mailbox inside the inbox).
fn shown(path: &[String], prefixed: Option<bool>) -> String {
    if prefixed != Some(false) && path.len() >= 2 && path[0].eq_ignore_ascii_case("INBOX") {
        path[1..].join("/")
    } else {
        path.join("/")
    }
}

/// Read Mail's list of mailboxes again, off the async threads.
async fn reload(ctx: &Arc<Ctx>) {
    let c = ctx.clone();
    let _ = blocking(move || {
        let conn = c.db();
        let _ = c.store.reload_mailboxes(&conn);
        c.store.refresh_leftovers_now(&conn);
    })
    .await;
}

// ---------------------------------------------------------------- the tools

/// Why Mail or the provider keeps this mailbox for itself, if it does: an
/// account's inbox, drafts, sent mail, trash and junk, Gmail's own folders,
/// mail waiting to be sent, and Apple Notes kept in the account ("Notes").
/// An archive is not kept: Mail makes it again when it archives.
pub(crate) fn kept_by_mail(m: &Mailbox) -> Option<String> {
    // Where an account keeps those: at the top, inside INBOX on servers
    // that keep every mailbox there, or among Gmail's own. "Clients/Sent"
    // is a folder of the user's.
    let level = m.path.len() == 1
        || (m.path.len() == 2 && (m.path[0].eq_ignore_ascii_case("INBOX") || m.path[0].starts_with('[')));
    if level
        && m.role_strength == 2
        && matches!(m.role, Role::Inbox | Role::Drafts | Role::Sent | Role::Trash | Role::Junk)
    {
        return Some(format!("it is the account's {} mailbox", m.role.as_str()));
    }
    if crate::store::gmail_folder(m) || (!m.local && m.path.len() == 1 && m.path[0].starts_with('[')) {
        return Some("the provider keeps it for itself".into());
    }
    let top = m.path.len() == 1 || (m.path.len() == 2 && m.path[0].eq_ignore_ascii_case("INBOX"));
    let name = m.name.to_lowercase();
    if top && matches!(name.as_str(), "outbox" | "sendlater") {
        return Some("it holds mail being sent".into());
    }
    if top && name == "notes" {
        return Some("Apple Notes may keep the account's notes in it".into());
    }
    None
}

/// The mailbox Mail lists that the user means to delete: the one at that
/// path; else the one at that path from Mail's INBOX prefix on, in an
/// account that keeps every mailbox inside INBOX (La Poste's "Archives");
/// else one whose path differs only in case or accents, the same ways;
/// else the one of that name; in the account so named, or, failing that,
/// whose name holds the one given. Never another mailbox standing for a
/// role's name ("Archives" is not the account's "Archive"), nor iCloud's
/// "INBOX/Clients" for its "Clients". None when Mail lists none.
pub(crate) fn mailbox_to_delete(store: &Store, spec: &str, account: Option<&str>) -> Result<Option<Mailbox>, String> {
    let accounts = account.filter(|a| !account_key(a).is_empty()).map(|a| store.accounts_named(a));
    if accounts.as_ref().is_some_and(Vec::is_empty) {
        return Ok(None);
    }
    let mailboxes = store.mailboxes();
    let candidates: Vec<&Mailbox> = mailboxes
        .values()
        .filter(|m| accounts.as_ref().is_none_or(|a| a.iter().any(|(id, n)| crate::store::same_account(m, id, n))))
        .filter(|m| !store.is_leftover(m))
        .collect();
    // Whether each account shows its mailboxes from INBOX on.
    let mut inside: HashMap<(bool, String, String), bool> = HashMap::new();
    for m in &candidates {
        inside
            .entry((m.local, m.account_id.clone(), m.account.clone()))
            .or_insert_with(|| prefixed(store, &Loc::from(*m)) == Some(true));
    }
    let want: Vec<String> = spec.trim().trim_matches('/').split('/').map(|p| p.trim().nfc().collect()).collect();
    let from_inbox = |m: &Mailbox| -> Option<Vec<String>> {
        let prefixed = inside.get(&(m.local, m.account_id.clone(), m.account.clone())).copied().unwrap_or(false);
        (prefixed && m.path.len() >= 2 && m.path[0].eq_ignore_ascii_case("INBOX")).then(|| m.path[1..].to_vec())
    };
    let folded = |p: &[String]| crate::store::name_key(&p.join("/"));
    let want_folded = folded(&want);
    let rules: [&dyn Fn(&Mailbox) -> bool; 5] = [
        &|m| same_path(&m.path, &want),
        &|m| from_inbox(m).is_some_and(|p| same_path(&p, &want)),
        &|m| folded(&m.path) == want_folded,
        &|m| from_inbox(m).is_some_and(|p| folded(&p) == want_folded),
        &|m| want.len() == 1 && crate::store::name_key(&m.name) == want_folded,
    ];
    for rule in rules {
        let hits: Vec<&&Mailbox> = candidates.iter().filter(|m| rule(m)).collect();
        match hits.as_slice() {
            [] => continue,
            [one] => return Ok(Some((**one).clone())),
            many => {
                let mut names: Vec<String> =
                    many.iter().map(|m| format!("{} ({})", m.display_path(), m.account)).collect();
                names.sort();
                return Err(format!(
                    "'{spec}' matches several mailboxes ({}): give 'account' or the full path.",
                    names.join(", ")
                ));
            }
        }
    }
    Ok(None)
}

pub async fn delete_mailbox(ctx: &Arc<Ctx>, p: &DeleteMailboxParams) -> Result<MailboxDone, String> {
    let deadline = Deadline::new();
    let Some(mb) = mailbox_to_delete(&ctx.store, &p.mailbox, p.account.as_deref())? else {
        return delete_unlisted(ctx, p, deadline).await;
    };
    let loc = Loc::from(&mb);
    let fix = prefixed(&ctx.store, &loc);
    let what = format!("'{}' ({})", shown(&mb.path, fix), mb.account);
    if mb.local {
        return Err(format!("{what}: {ON_MY_MAC} {IN_MAIL}"));
    }
    if let Some(why) = kept_by_mail(&mb) {
        return Err(format!("{what} cannot be deleted: {why}."));
    }
    let inner = ctx
        .store
        .mailboxes()
        .values()
        .filter(|m| {
            !m.local
                && crate::store::same_account(m, &mb.account_id, &mb.account)
                && !ctx.store.is_leftover(m)
                && m.path.len() > mb.path.len()
                && m.path.starts_with(&mb.path)
        })
        .count();
    if inner > 0 {
        return Err(format!("{what} holds {inner} other mailbox(es): delete those first, each once it is empty."));
    }
    let held = {
        let (c, id) = (ctx.clone(), mb.id);
        blocking(move || c.store.messages_in(&c.db(), id)).await?.map_err(|e| e.to_string())?
    };
    if held > 0 {
        return Err(format!(
            "{what} holds {held} message(s). Timbre deletes empty mailboxes only, so no mail is ever lost: move the \
             messages first (move_mail, which can be undone), then delete the mailbox."
        ));
    }
    let acct = account(&loc, deadline).await?;
    // Mail's database keeps some mailboxes Mail has deleted: one Mail no
    // longer shows never opens a password window.
    if mail_shows(&ctx.store, &loc, true, Duration::ZERO, deadline).await == Some(false)
        && password_at_hand(&acct).await.is_none()
    {
        return Err(format!(
            "Mail does not show {what} any more (deleted already, or not caught up yet). Nothing was done."
        ));
    }
    let password = password(&acct, deadline).await?;
    let (path, label) = (mb.path.clone(), what.clone());
    if let Err(e) = on_server(&acct, password, deadline, move |s| delete_in(s, &path, &label)).await {
        // Perhaps done: Mail is asked to look, so list_mailboxes tells.
        if e.contains(MAYBE_DELETED) {
            ask_mail_to_sync(&loc);
        }
        return Err(e);
    }
    ask_mail_to_sync(&loc);
    let gone = mail_shows(&ctx.store, &loc, false, MAIL_CATCHES_UP, deadline).await == Some(false);
    reload(ctx).await;
    let gmail = ctx.store.is_gmail(&mb.account_id, &mb.account) || acct.provider == Provider::Google;
    let note = match (gone, gmail) {
        (true, true) => "Deleted: Gmail removed the label (no message had it), and Mail no longer shows it.",
        (true, false) => "Deleted on the server; Mail no longer shows it.",
        (false, _) => {
            "Deleted on the server. Mail keeps showing it until Mail is next opened, which removes it by itself: \
             nothing else to do (quitting and reopening Mail shows it gone now)."
        }
    };
    Ok(MailboxDone {
        mailbox: shown(&mb.path, fix),
        account: mb.account,
        on_server: mb.path.join("/"),
        note: note.into(),
    })
}

/// A mailbox Mail does not list (made elsewhere and not synchronized yet,
/// or kept where Mail does not look), in the account named: deleted on the
/// server with the same checks, when Timbre has the password at hand (a
/// name that matches nothing never opens a password window).
async fn delete_unlisted(ctx: &Arc<Ctx>, p: &DeleteMailboxParams, deadline: Deadline) -> Result<MailboxDone, String> {
    let not_found = format!("No mailbox named '{}'. Call list_mailboxes.", p.mailbox);
    let Some(name) = p.account.as_deref() else { return Err(not_found) };
    let loc = find_account(&ctx.store, name)?;
    let wanted = parts_of(&p.mailbox).map_err(|_| not_found.clone())?;
    let acct = account(&loc, deadline).await?;
    let Some(password) = password_at_hand(&acct).await else { return Err(not_found) };
    let what = format!("'{}' ({})", wanted.join("/"), loc.account);
    let path = on_server(&acct, password, deadline, move |s| {
        let all = s.list().map_err(|e| format!("{what} was not deleted: {e}."))?;
        let mut ways = vec![wanted.clone()];
        if !wanted[0].eq_ignore_ascii_case("INBOX") {
            ways.push(std::iter::once("INBOX".to_string()).chain(wanted.iter().cloned()).collect());
        }
        let mut hits: Vec<Vec<String>> = vec![];
        for way in &ways {
            match find_typed(&all, way) {
                Ok(Some(e)) => hits.push(e.segments()),
                Ok(None) => {}
                Err(several) => {
                    return Err(format!(
                        "{what} matches {} on the server, which differ in case: give the name exactly.",
                        several.iter().map(|e| e.name.clone()).collect::<Vec<_>>().join(" and ")
                    ));
                }
            }
        }
        hits.dedup();
        match hits.as_slice() {
            [one] => delete_in(s, one, &what).map(|()| one.clone()),
            [] => Err(not_found),
            _ => Err(format!(
                "{what} matches {} on the server: give the full path.",
                hits.iter().map(|h| h.join("/")).collect::<Vec<_>>().join(" and ")
            )),
        }
    })
    .await?;
    ask_mail_to_sync(&loc);
    Ok(MailboxDone {
        mailbox: shown(&path, prefixed(&ctx.store, &loc)),
        account: loc.account,
        on_server: path.join("/"),
        note: "Deleted on the server (Mail did not show it).".into(),
    })
}

pub async fn create_mailbox(ctx: &Arc<Ctx>, p: &CreateMailboxParams) -> Result<MailboxDone, String> {
    let deadline = Deadline::new();
    let wanted = parts_of(&p.mailbox)?;
    let loc = find_account(&ctx.store, &p.account)?;
    make(ctx, &loc, &wanted, deadline).await
}

/// Make a mailbox in an account, on the server, and wait for Mail to show
/// it.
async fn make(ctx: &Arc<Ctx>, loc: &Loc, wanted: &[String], deadline: Deadline) -> Result<MailboxDone, String> {
    let prefix = mail_prefix(&ctx.store, loc);
    let fix = prefixed(&ctx.store, loc);
    let pre = prefix.clone().unwrap_or_default();
    // As Mail names it, from the account's prefix on: "INBOX/Projets" given
    // for La Poste is its "Projets".
    let rel: Vec<String> = if !pre.is_empty() && wanted.len() > pre.len() && same_path(&wanted[..pre.len()], &pre) {
        wanted[pre.len()..].to_vec()
    } else {
        wanted.to_vec()
    };
    // Where Mail will show it.
    let path: Vec<String> = pre.iter().cloned().chain(rel.iter().cloned()).collect();
    let at = Loc { path: path.clone(), ..loc.clone() };
    let done = |path: &[String], note: &str| MailboxDone {
        mailbox: shown(path, fix),
        account: loc.account.clone(),
        on_server: path.join("/"),
        note: note.into(),
    };
    if mail_shows(&ctx.store, &at, true, Duration::ZERO, deadline).await == Some(true) {
        return Ok(done(&path, "It exists already."));
    }
    // Mail first: no password, and it shows the mailbox at once.
    match mailapp::make_mailbox(&loc.account_id, &rel.join("/"), deadline.cap(Duration::from_secs(25))).await {
        Ok(()) => {
            let shown_now = mail_shows(&ctx.store, &at, true, MAIL_CATCHES_UP, deadline).await == Some(true);
            if shown_now {
                reload(ctx).await;
            }
            // Made, Mail says: never made a second time on the server.
            let note =
                if shown_now { "Made by Mail, on the server too." } else { "Mail made it; it shows it in a moment." };
            return Ok(done(&path, note));
        }
        // No answer in time: Mail may still be making it, so neither the
        // server nor a password window is tried meanwhile.
        Err(e) if mailapp::no_answer(&e) => {
            if mail_shows(&ctx.store, &at, true, MAIL_CATCHES_UP, deadline).await == Some(true) {
                reload(ctx).await;
                return Ok(done(&path, "Made by Mail, on the server too."));
            }
            return Err(format!(
                "Mail did not answer in time while making '{}' in '{}': it may still be making it. Call \
                 list_mailboxes in a moment to see; nothing else was done.",
                rel.join("/"),
                loc.account
            ));
        }
        Err(e) => tracing::info!("Mail would not make the mailbox, so the server is asked: {e:#}"),
    }
    let acct = account(loc, deadline).await?;
    let password = password(&acct, deadline).await?;
    let what = format!("'{}' ({})", rel.join("/"), loc.account);
    let (path, existed) = on_server(&acct, password, deadline, move |s| create_in(s, &rel, &what, prefix)).await?;
    ask_mail_to_sync(loc);
    let listed = mail_shows(&ctx.store, &Loc { path: path.clone(), ..loc.clone() }, true, MAIL_CATCHES_UP, deadline)
        .await
        == Some(true);
    if listed {
        reload(ctx).await;
    }
    let note = match (existed, listed) {
        (true, true) => "It exists already.",
        (false, true) => "Made on the server; Mail shows it.",
        (true, false) => {
            "It exists already on the server. Mail shows it once it reads the account's list of mailboxes again, \
             which it does on its own (at the latest when it is next opened)."
        }
        (false, false) => {
            "Made on the server. Mail shows it once it reads the account's list of mailboxes again, which it does \
             on its own (at the latest when it is next opened)."
        }
    };
    Ok(done(&path, note))
}

/// The password for this account's server if Timbre has it at hand, with
/// no window asking for it. macOS may ask the user before letting Timbre
/// read its item (one another build saved, a locked keychain): not waited
/// for long.
async fn password_at_hand(acct: &Account) -> Option<String> {
    let k = key(&acct.server);
    if let Some(p) = known(&k) {
        return Some(p);
    }
    // One look at a time: a look still waiting on macOS (and the user) is
    // joined, never doubled with a second question on the screen. What it
    // finds is kept for the calls that follow.
    let mut rx = {
        let mut looks = AT_HAND_LOOKS.lock().unwrap_or_else(|e| e.into_inner());
        match looks.get(&k) {
            Some(rx) => rx.clone(),
            None => {
                let (tx, rx) = watch::channel(false);
                looks.insert(k.clone(), rx.clone());
                let (h, u, k) = (acct.server.host.clone(), acct.server.user.clone(), k.clone());
                tokio::task::spawn_blocking(move || {
                    if let Found::Password(p) = keychain::saved(&h, &u) {
                        remember(&k, p);
                    }
                    AT_HAND_LOOKS.lock().unwrap_or_else(|e| e.into_inner()).remove(&k);
                    let _ = tx.send(true);
                });
                rx
            }
        }
    };
    let _ = tokio::time::timeout(AT_HAND, rx.wait_for(|done| *done)).await;
    known(&k)
}

/// Looks in the keychain under way, for calls that open no window.
static AT_HAND_LOOKS: LazyLock<Mutex<HashMap<String, watch::Receiver<bool>>>> = LazyLock::new(Mutex::default);

/// Make a mailbox only if the password is at hand (no window): for an
/// archive Mail would not make.
pub async fn make_if_password_known(ctx: &Ctx, loc: &Loc, name: &str) -> Result<(), String> {
    let deadline = Deadline(Instant::now() + Duration::from_secs(40));
    let acct = account(loc, deadline).await?;
    let password = password_at_hand(&acct).await.ok_or("no password kept for this account yet")?;
    let (wanted, prefix) = (vec![name.to_string()], mail_prefix(&ctx.store, loc));
    let what = format!("'{name}' ({})", loc.account);
    let (path, _) = on_server(&acct, password, deadline, move |s| create_in(s, &wanted, &what, prefix)).await?;
    ask_mail_to_sync(loc);
    mail_shows(&ctx.store, &Loc { path, ..loc.clone() }, true, MAIL_CATCHES_UP, deadline).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap::testing::fake;

    fn path(p: &str) -> Vec<String> {
        p.split('/').map(str::to_string).collect()
    }

    const LIST_LA_POSTE: &str = "* LIST (\\HasChildren) \"/\" INBOX\r\n\
        * LIST (\\HasNoChildren) \"/\" \"INBOX/Archives\"\r\n\
        * LIST (\\HasChildren) \"/\" \"INBOX/Projets\"\r\n\
        * LIST (\\HasNoChildren) \"/\" \"INBOX/Projets/2026\"\r\n\
        * LIST (\\HasNoChildren) \"/\" \"INBOX/Envoy&AOk-s\"\r\n\
        * LIST (\\HasNoChildren \\Sent) \"/\" \"INBOX/OUTBOX\"\r\n\
        $T OK done\r\n";

    #[test]
    fn providers_by_server_name() {
        assert_eq!(provider("imap.gmail.com"), Provider::Google);
        assert_eq!(provider("p52-imap.mail.me.com"), Provider::Apple);
        assert_eq!(provider("imap.mail.yahoo.com"), Provider::Yahoo);
        assert_eq!(provider("outlook.office365.com"), Provider::Microsoft);
        assert_eq!(provider("imap.laposte.net"), Provider::Other);
        assert_eq!(provider("notgmail.com"), Provider::Other);
    }

    #[test]
    fn paths_match_as_servers_name_them() {
        assert!(same_path(&path("inbox/Envoyés"), &path("INBOX/Envoye\u{301}s")));
        assert!(!same_path(&path("INBOX/archives"), &path("INBOX/Archives")));
        let entries = vec![
            Entry {
                raw: b"INBOX/Archives".to_vec(),
                name: "INBOX/Archives".into(),
                delimiter: Some('/'),
                attrs: vec![],
            },
            Entry { raw: b"Foo".to_vec(), name: "Foo".into(), delimiter: Some('/'), attrs: vec![] },
            Entry { raw: b"foo".to_vec(), name: "foo".into(), delimiter: Some('/'), attrs: vec![] },
        ];
        // What Mail lists is found at exactly its path: "INBOX/archives"
        // is another mailbox than "INBOX/Archives" on most servers.
        assert_eq!(find(&entries, &path("inbox/Archives")).map(|e| e.name.as_str()), Some("INBOX/Archives"));
        assert_eq!(find(&entries, &path("INBOX/archives")), None);
        assert_eq!(find(&entries, &path("foo")).map(|e| e.name.as_str()), Some("foo"));
        // What the user typed may differ in case, when that leaves no doubt.
        let typed = |p: &str| find_typed(&entries, &path(p)).map(|e| e.map(|e| e.name.clone()));
        assert_eq!(typed("INBOX/archives"), Ok(Some("INBOX/Archives".into())));
        assert!(typed("FOO").is_err(), "two that differ only in case: never a guess");
        assert_eq!(typed("Bar"), Ok(None));
    }

    #[test]
    fn accounts_are_named_as_the_user_writes_them() {
        assert_eq!(account_key("La Poste"), account_key("Laposte"));
        assert_eq!(account_key(" iCloud "), "icloud");
        assert_eq!(account_key("Sur mon Mac"), "surmonmac");
    }

    #[test]
    fn mailbox_names_are_checked() {
        assert_eq!(parts_of(" Projets / 2026 ").unwrap(), ["Projets", "2026"]);
        assert!(parts_of("a//b").is_err());
        assert!(parts_of("inbox").is_err());
        assert!(parts_of("50%").is_err());
        assert_eq!(parts_of("Re\u{301}sumés").unwrap(), ["Résumés"]);
    }

    #[test]
    fn an_empty_mailbox_is_deleted_on_the_server() {
        let (mut s, server) = fake(vec![
            ("LIST", LIST_LA_POSTE),
            ("STATUS \"INBOX/Archives\" (MESSAGES)", "* STATUS \"INBOX/Archives\" (MESSAGES 0)\r\n$T OK\r\n"),
            ("DELETE \"INBOX/Archives\"", "$T OK gone\r\n"),
            ("UNSUBSCRIBE \"INBOX/Archives\"", "$T OK\r\n"),
            ("LIST", "* LIST (\\HasChildren) \"/\" INBOX\r\n$T OK\r\n"),
        ]);
        delete_in(&mut s, &path("INBOX/Archives"), "'Archives'").unwrap();
        assert_eq!(server.join().unwrap().len(), 5);
    }

    #[test]
    fn what_holds_mail_or_mailboxes_or_is_kept_is_never_deleted() {
        let (mut s, _) =
            fake(vec![("LIST", LIST_LA_POSTE), ("STATUS", "* STATUS \"INBOX/Archives\" (MESSAGES 3)\r\n$T OK\r\n")]);
        let e = delete_in(&mut s, &path("INBOX/Archives"), "'Archives'").unwrap_err();
        assert!(e.contains("3 message"), "{e}");
        let (mut s, _) = fake(vec![("LIST", LIST_LA_POSTE)]);
        let e = delete_in(&mut s, &path("INBOX/Envoyés"), "'Envoyés'").unwrap_err();
        assert!(e.contains("sent mailbox"), "{e}");
        let (mut s, _) = fake(vec![("LIST", LIST_LA_POSTE)]);
        let e = delete_in(&mut s, &path("INBOX/Projets"), "'Projets'").unwrap_err();
        assert!(e.contains("1 other mailbox"), "{e}");
        let (mut s, _) = fake(vec![("LIST", LIST_LA_POSTE)]);
        let e = delete_in(&mut s, &path("INBOX/OUTBOX"), "'OUTBOX'").unwrap_err();
        assert!(e.contains("sent"), "{e}");
        let (mut s, _) = fake(vec![("LIST", LIST_LA_POSTE)]);
        assert!(delete_in(&mut s, &path("INBOX"), "'INBOX'").unwrap_err().contains("inbox"));
        let (mut s, _) = fake(vec![("LIST", LIST_LA_POSTE)]);
        assert!(delete_in(&mut s, &path("INBOX/Nope"), "'Nope'").unwrap_err().contains("does not have"));
        let (mut s, _) = fake(vec![("LIST", "* LIST (\\Noselect \\HasChildren) \"/\" \"[Gmail]\"\r\n$T OK\r\n")]);
        assert!(delete_in(&mut s, &path("[Gmail]"), "'[Gmail]'").unwrap_err().contains("provider"));
        // Servers that flag nothing: Mail's and the provider's mailboxes are
        // known by their names (nothing is asked of the server about them).
        let plain = "* LIST () \"/\" INBOX\r\n* LIST () \"/\" \"INBOX/Drafts\"\r\n* LIST () \"/\" \"INBOX/OUTBOX\"\r\n\
            * LIST () \"/\" Notes\r\n* LIST () \"/\" \"INBOX/QUARANTAINE\"\r\n$T OK\r\n";
        for kept in ["INBOX/Drafts", "INBOX/OUTBOX", "Notes", "INBOX/QUARANTAINE"] {
            let (mut s, server) = fake(vec![("LIST", plain)]);
            let e = delete_in(&mut s, &path(kept), "'x'").unwrap_err();
            assert!(e.contains("cannot be deleted"), "{kept}: {e}");
            drop(s);
            assert_eq!(server.join().unwrap().len(), 1, "{kept}: only LIST was sent");
        }
    }

    /// Mail's "Clients" when the server has only "clients": another
    /// mailbox, never counted nor deleted.
    #[test]
    fn a_mailbox_differing_in_case_is_left_alone() {
        let (mut s, server) = fake(vec![("LIST", "* LIST () \"/\" INBOX\r\n* LIST () \"/\" clients\r\n$T OK\r\n")]);
        let e = delete_in(&mut s, &path("Clients"), "'Clients'").unwrap_err();
        assert!(e.contains("does not have"), "{e}");
        drop(s);
        assert_eq!(server.join().unwrap().len(), 1, "no STATUS, no DELETE");
    }

    /// The server's answer to DELETE lost: it may have deleted it, and the
    /// user is told so, not that nothing happened.
    #[test]
    fn a_lost_answer_to_delete_is_told_as_such() {
        let (mut s, _server) = fake(vec![
            ("LIST", LIST_LA_POSTE),
            ("STATUS \"INBOX/Archives\" (MESSAGES)", "* STATUS \"INBOX/Archives\" (MESSAGES 0)\r\n$T OK\r\n"),
        ]);
        let e = delete_in(&mut s, &path("INBOX/Archives"), "'Archives'").unwrap_err();
        assert!(e.contains("may have been deleted"), "{e}");
        // Answered no: not deleted, and said so.
        let (mut s, _server) = fake(vec![
            ("LIST", LIST_LA_POSTE),
            ("STATUS \"INBOX/Archives\" (MESSAGES)", "* STATUS \"INBOX/Archives\" (MESSAGES 0)\r\n$T OK\r\n"),
            ("DELETE", "$T NO [INUSE] busy\r\n"),
        ]);
        let e = delete_in(&mut s, &path("INBOX/Archives"), "'Archives'").unwrap_err();
        assert!(e.contains("was not deleted") && e.contains("INUSE"), "{e}");
    }

    #[test]
    fn a_call_keeps_to_its_time() {
        let d = Deadline(Instant::now() + Duration::from_secs(5));
        assert!(d.cap(Duration::from_secs(30)) <= Duration::from_secs(5));
        assert_eq!(d.cap(Duration::ZERO), Duration::ZERO);
        let past = Deadline(Instant::now());
        assert_eq!(past.left(), Duration::ZERO);
    }

    #[test]
    fn a_mailbox_is_made_where_the_server_keeps_them() {
        // NAMESPACE says INBOX/: "Factures" is made as INBOX/Factures.
        let (mut s, server) = fake(vec![
            ("LIST", LIST_LA_POSTE),
            ("NAMESPACE", "* NAMESPACE ((\"INBOX/\" \"/\")) NIL NIL\r\n$T OK\r\n"),
            ("CREATE \"INBOX/Factures &- Re&AOc-us\"", "$T OK made\r\n"),
            ("SUBSCRIBE", "$T OK\r\n"),
            ("LIST", "* LIST () \"/\" \"INBOX/Factures &- Re&AOc-us\"\r\n$T OK\r\n"),
        ]);
        let (made, existed) = create_in(&mut s, &path("Factures & Reçus"), "'x'", None).unwrap();
        assert_eq!((shown(&made, Some(true)).as_str(), existed), ("Factures & Reçus", false));
        // Inside an inbox that keeps no other mailbox (iCloud): shown whole.
        assert_eq!(shown(&path("INBOX/Clients"), Some(false)), "INBOX/Clients");
        assert_eq!(server.join().unwrap().len(), 5);
        // Already there (given with its prefix): nothing is made.
        let (mut s, _) =
            fake(vec![("LIST", LIST_LA_POSTE), ("NAMESPACE", "* NAMESPACE ((\"INBOX/\" \"/\")) NIL NIL\r\n$T OK\r\n")]);
        assert_eq!(create_in(&mut s, &path("INBOX/Archives"), "'x'", None).unwrap(), (path("INBOX/Archives"), true));
        // Mail shows the account's mailboxes inside INBOX while the server's
        // NAMESPACE says the top: Mail's way wins, or Mail would not show it.
        let (mut s, _) = fake(vec![
            ("LIST", LIST_LA_POSTE),
            ("NAMESPACE", "* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n$T OK\r\n"),
            ("CREATE \"INBOX/Essai\"", "$T OK\r\n"),
            ("SUBSCRIBE", "$T OK\r\n"),
            ("LIST", "* LIST () \"/\" \"INBOX/Essai\"\r\n$T OK\r\n"),
        ]);
        let made = create_in(&mut s, &path("Essai"), "'Essai'", Some(path("INBOX"))).unwrap();
        assert_eq!(made, (path("INBOX/Essai"), false));
    }

    #[test]
    fn without_namespace_the_layout_is_inferred() {
        // Gmail-like: mailboxes at the top; a '.' server refuses a '.' in a name.
        let list = "* LIST () \".\" INBOX\r\n* LIST () \".\" Work\r\n$T OK\r\n";
        let (mut s, _) = fake_no_namespace(vec![("LIST", list)]);
        let e = create_in(&mut s, &path("v1.2"), "'v1.2'", None).unwrap_err();
        assert!(e.contains("'.'"), "{e}");
        let (mut s, _) = fake_no_namespace(vec![
            ("LIST", list),
            ("CREATE \"Perso\"", "$T OK\r\n"),
            ("SUBSCRIBE", "$T OK\r\n"),
            ("LIST", "* LIST () \".\" Perso\r\n$T OK\r\n"),
        ]);
        assert_eq!(create_in(&mut s, &path("Perso"), "'Perso'", Some(vec![])).unwrap(), (path("Perso"), false));
    }

    fn fake_no_namespace(script: Vec<(&'static str, &'static str)>) -> (Session, std::thread::JoinHandle<Vec<String>>) {
        crate::imap::testing::fake_with("IMAP4rev1 AUTH=PLAIN", script)
    }

    #[test]
    fn mailboxes_mail_or_the_provider_keep_are_named() {
        let mb = |path: &[&str]| {
            let p: Vec<String> = path.iter().map(|s| s.to_string()).collect();
            crate::store::parse_mailbox_url(
                1,
                &format!("imap://ABCDEF01-2345-6789-ABCD-EF0123456789/{}", p.join("/")),
                &HashMap::new(),
            )
        };
        assert!(kept_by_mail(&mb(&["INBOX"])).is_some());
        assert!(kept_by_mail(&mb(&["Sent Messages"])).is_some());
        assert!(kept_by_mail(&mb(&["[Gmail]", "Important"])).is_some());
        assert!(kept_by_mail(&mb(&["Notes"])).is_some());
        assert!(kept_by_mail(&mb(&["INBOX", "Archives"])).is_none());
        assert!(kept_by_mail(&mb(&["Projets"])).is_none());
        assert!(kept_by_mail(&mb(&["INBOX", "Trash"])).is_some());
        // A folder of the user's named like one, inside another.
        assert!(kept_by_mail(&mb(&["Clients", "Sent"])).is_none());
        assert!(kept_by_mail(&mb(&["Clients", "Werbung"])).is_none());
    }

    #[test]
    fn the_password_window_says_why_in_one_sentence() {
        let acct = |host: &str| Account {
            name: "Laposte".into(),
            server: Server::new(host, Some(993), Some(true), "a@laposte.net"),
            provider: provider(host),
        };
        let ask = Ask::in_language(&acct("imap.laposte.net"), &Note::None, true);
        assert!(ask.text.contains("« Laposte »") && ask.text.contains("une seule fois") && ask.link.is_none());
        assert_eq!(ask.text.matches(". ").count() + ask.text.matches(".\n").count(), 0, "{}", ask.text);
        let ask = Ask::in_language(&acct("imap.gmail.com"), &Note::Wrong, false);
        assert!(ask.text.starts_with("The server did not accept"));
        assert_eq!(ask.link, Some(("Create one…", "https://myaccount.google.com/apppasswords")));
        // The server's own word for the user, shown as it said it.
        let alert = Note::Alert("[ALERT] Application-specific password required: https://x (Failure)".into());
        let ask = Ask::in_language(&acct("imap.gmail.com"), &alert, true);
        assert!(
            ask.text.starts_with("Le serveur répond : « Application-specific password required: https://x (Failure) »"),
            "{}",
            ask.text
        );
    }
}
