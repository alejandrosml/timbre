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
use std::sync::{LazyLock, Mutex};
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
use crate::store::{Mailbox, Role, Store, name_key};

/// How long a tool waits for the user to answer a window before saying so.
const WAIT_FOR_USER: Duration = Duration::from_secs(30);
/// How long a password checked with the server is kept in memory.
const KEEP: Duration = Duration::from_secs(30 * 60);
/// How long Mail is given to show the change.
const MAIL_CATCHES_UP: Duration = Duration::from_secs(10);

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
async fn account(loc: &Loc) -> Result<Account, String> {
    let what = format!("'{}'", loc.account);
    let v = mailapp::run(mailapp::ACCOUNT_SERVER, &json!({ "loc": loc }), Duration::from_secs(30))
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
    let want = name_key(name);
    if want.is_empty() {
        return Err("Say which account (list_mailboxes names them).".into());
    }
    let mailboxes = store.mailboxes();
    let mut accounts: Vec<(&str, &str)> =
        mailboxes.values().filter(|m| !m.local).map(|m| (m.account_id.as_str(), m.account.as_str())).collect();
    accounts.sort_unstable();
    accounts.dedup();
    let exact: Vec<_> =
        accounts.iter().filter(|(id, n)| name_key(n) == want || id.eq_ignore_ascii_case(name.trim())).collect();
    let hits =
        if exact.is_empty() { accounts.iter().filter(|(_, n)| name_key(n).contains(&want)).collect() } else { exact };
    match hits.as_slice() {
        [(id, n)] => Ok(Loc { account_id: id.to_string(), account: n.to_string(), path: vec![], local: false }),
        [] if ["on my mac", "sur mon mac"].contains(&want.as_str()) => Err(ON_MY_MAC.into()),
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

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("internal error: {e}"))
}

async fn check(s: &Server, password: &str) -> Result<(), imap::Error> {
    let (s, password) = (s.clone(), password.to_string());
    blocking(move || imap::check(&s, &password)).await.unwrap_or_else(|e| Err(imap::Error::Network(e)))
}

/// What went wrong with the server, as Claude should tell it.
fn server_trouble(acct: &Account, e: &imap::Error) -> String {
    format!("Timbre could not do it on the server of '{}' ({}): {e}.", acct.name, acct.server.host)
}

/// The password for this account's server, checked with it: the one Timbre
/// keeps, the one Mail kept where older macOS had it, or the user's answer
/// to a window asking for it (the first time only). Waits a while for the
/// user; past that, says so, and a later call takes up the same window.
async fn password(acct: &Account) -> Result<String, String> {
    let k = key(&acct.server);
    for _ in 0..2 {
        if let Some(p) = known(&k) {
            return Ok(p);
        }
        match wait_for_password(acct, &k).await {
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
/// a while for it.
async fn wait_for_password(acct: &Account, k: &str) -> Result<Option<()>, String> {
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
    let waited = tokio::time::timeout(WAIT_FOR_USER, async {
        match rx.wait_for(|s| matches!(s, Step::Done(_))).await {
            Ok(step) => step.clone(),
            Err(_) => Step::Done(Err("internal error: the password search stopped".into())),
        }
    })
    .await;
    let step = waited.unwrap_or_else(|_| rx.borrow().clone());
    let ask_again = "Ask the user to answer it, then call this tool again (nothing was done yet).";
    match step {
        Step::Done(r) => {
            passwords().pending.remove(k);
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
        match check(s, &p).await {
            Ok(()) => {
                remember(k, p);
                return Ok(());
            }
            Err(imap::Error::Auth(_)) => {
                let (h, u) = (host.clone(), user.clone());
                blocking(move || keychain::forget(&h, &u)).await?;
                note = Note::Stale;
            }
            Err(e) => return Err(server_trouble(acct, &e)),
        }
    }
    // The one Mail kept, where older macOS had it: macOS asks the user.
    let _ = tx.send(Step::MacOsAsks);
    let (h, u) = (host.clone(), user.clone());
    if let Found::Password(p) = blocking(move || keychain::mails(&h, &u)).await? {
        match check(s, &p).await {
            Ok(()) => {
                remember(k, p);
                return Ok(());
            }
            Err(imap::Error::Auth(_)) => {}
            Err(e) => return Err(server_trouble(acct, &e)),
        }
    }
    // Ask the user, once; again if the server turns it down.
    for attempt in 0..3 {
        let _ = tx.send(Step::TimbreAsks);
        let ask = Ask::new(acct, if attempt > 0 { Note::Wrong } else { note });
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
        match check(s, &p).await {
            Ok(()) => {
                let (h, u, pw) = (host.clone(), user.clone(), p.clone());
                if let Err(e) = blocking(move || keychain::save(&h, &u, &pw)).await? {
                    tracing::warn!("the password for {host} could not be kept in the keychain: {e}");
                }
                remember(k, p);
                return Ok(());
            }
            Err(imap::Error::Auth(_)) => continue,
            Err(e) => return Err(server_trouble(acct, &e)),
        }
    }
    Err(format!("The server of '{}' turned the password down three times. Nothing was done.", acct.name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Note {
    None,
    /// The password typed was turned down.
    Wrong,
    /// The one kept no longer works.
    Stale,
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
    fn new(acct: &Account, note: Note) -> Ask {
        Ask::in_language(acct, note, mailapp::user_reads_french())
    }

    fn in_language(acct: &Account, note: Note, fr: bool) -> Ask {
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
        let mut text = match (note, fr) {
            (Note::Wrong, true) => "Le serveur a refusé ce mot de passe.\n\n".to_string(),
            (Note::Wrong, false) => "The server did not accept that password.\n\n".to_string(),
            (Note::Stale, true) => "Le mot de passe gardé n'est plus accepté.\n\n".to_string(),
            (Note::Stale, false) => "The saved password is no longer accepted.\n\n".to_string(),
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

/// The server's mailbox at this path; failing that, the one mailbox whose
/// path differs only in case.
fn find<'a>(all: &'a [Entry], path: &[String]) -> Option<&'a Entry> {
    all.iter().find(|e| same_path(&e.segments(), path)).or_else(|| {
        let key = |p: &[String]| p.iter().map(|s| s.nfc().collect::<String>().to_lowercase()).collect::<Vec<_>>();
        let want = key(path);
        let mut hits = all.iter().filter(|e| key(&e.segments()) == want);
        match (hits.next(), hits.next()) {
            (Some(e), None) => Some(e),
            _ => None,
        }
    })
}

/// Delete the mailbox at `path` on the server, if it holds no message and
/// no other mailbox, and is none the server keeps for itself.
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
    let raw = target.raw.clone();
    s.delete(&raw).map_err(refused)?;
    let _ = s.subscribe(&raw, false);
    if find(&s.list().map_err(refused)?, path).is_some() {
        return Err(format!("The server said it deleted {what} but still lists it."));
    }
    Ok(())
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
        .filter(|m| !m.local && crate::store::same_account(m, &loc.account_id, &loc.account) && !inbox(m))
        .collect();
    if others.is_empty() {
        return None;
    }
    let inside = others.iter().all(|m| m.path.len() >= 2 && m.path[0].eq_ignore_ascii_case("INBOX"));
    Some(if inside { vec!["INBOX".to_string()] } else { vec![] })
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
    if find(&all, &path).is_some_and(|e| same_path(&e.segments(), &path)) {
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

/// Sign in and run `f` on the server, off the async threads.
async fn on_server<T: Send + 'static>(
    acct: &Account,
    password: String,
    f: impl FnOnce(&mut Session) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let server = acct.server.clone();
    let k = key(&server);
    let opened = {
        let server = server.clone();
        blocking(move || imap::open(&server, &password)).await?
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

/// Whether Mail shows the mailbox at `loc`, waiting up to `wait` for it to
/// show it (`present`) or to stop showing it; none if Mail cannot be asked.
async fn mail_shows(loc: &Loc, present: bool, wait: Duration) -> Option<bool> {
    let tries = u64::try_from(wait.as_millis() / 500).unwrap_or(1).max(1);
    let input = json!({ "loc": loc, "present": present, "tries": tries });
    let v = mailapp::run(mailapp::MAILBOX_SHOWN, &input, wait + Duration::from_secs(20)).await.ok()?;
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

/// A path as Mail shows it: without the INBOX prefix some servers put
/// before every mailbox.
fn shown(path: &[String]) -> String {
    if path.len() >= 2 && path[0].eq_ignore_ascii_case("INBOX") { path[1..].join("/") } else { path.join("/") }
}

// ---------------------------------------------------------------- the tools

/// Why Mail or the provider keeps this mailbox for itself, if it does: an
/// account's inbox, drafts, sent mail, trash and junk, Gmail's own folders,
/// mail waiting to be sent, and Apple Notes kept in the account ("Notes").
pub(crate) fn kept_by_mail(m: &Mailbox) -> Option<String> {
    if m.role_strength == 2 && matches!(m.role, Role::Inbox | Role::Drafts | Role::Sent | Role::Trash | Role::Junk) {
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

pub async fn delete_mailbox(ctx: &Ctx, p: &DeleteMailboxParams) -> Result<MailboxDone, String> {
    let ids = ctx.store.resolve_mailboxes(&p.mailbox, p.account.as_deref());
    let mb = match ids.as_slice() {
        [id] => ctx.store.mailbox(*id).ok_or("That mailbox is gone from Mail. Call list_mailboxes.")?,
        [] => return delete_unlisted(ctx, p).await,
        _ => return Err(format!("'{}' matches several mailboxes: give 'account' or the full path.", p.mailbox)),
    };
    let what = format!("'{}' ({})", shown(&mb.path), mb.account);
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
                && m.path.len() > mb.path.len()
                && m.path.starts_with(&mb.path)
        })
        .count();
    if inner > 0 {
        return Err(format!("{what} holds {inner} other mailbox(es): delete those first, each once it is empty."));
    }
    let held = {
        let conn = ctx.db();
        ctx.store.mailbox_counts(&conn).map_err(|e| e.to_string())?.get(&mb.id).map_or(0, |c| c.0)
    };
    if held > 0 {
        return Err(format!(
            "{what} holds {held} message(s). Timbre deletes empty mailboxes only, so no mail is ever lost: move the \
             messages first (move_mail, which can be undone), then delete the mailbox."
        ));
    }
    let loc = Loc::from(&mb);
    let acct = account(&loc).await?;
    // Mail's database keeps some mailboxes Mail has deleted: one Mail no
    // longer shows never opens a password window.
    if mail_shows(&loc, true, Duration::ZERO).await == Some(false) && password_at_hand(&acct).await.is_none() {
        return Err(format!(
            "Mail does not show {what} any more (deleted already, or not caught up yet). Nothing was done."
        ));
    }
    let password = password(&acct).await?;
    let (path, label) = (mb.path.clone(), what.clone());
    on_server(&acct, password, move |s| delete_in(s, &path, &label)).await?;
    ask_mail_to_sync(&loc);
    let gone = mail_shows(&loc, false, MAIL_CATCHES_UP).await == Some(false);
    {
        let conn = ctx.db();
        let _ = ctx.store.reload_mailboxes(&conn);
    }
    let gmail = ctx.store.is_gmail(&mb.account_id, &mb.account) || acct.provider == Provider::Google;
    let note = match (gone, gmail) {
        (true, true) => "Deleted: Gmail removed the label (no message had it), and Mail no longer shows it.",
        (true, false) => "Deleted on the server; Mail no longer shows it.",
        (false, _) => {
            "Deleted on the server. Mail still shows it until it reads the account's list of mailboxes again, \
             which it does on its own (at the latest when it is next opened)."
        }
    };
    Ok(MailboxDone { mailbox: shown(&mb.path), account: mb.account, on_server: mb.path.join("/"), note: note.into() })
}

/// A mailbox Mail does not list (made elsewhere and not synchronized yet,
/// or kept where Mail does not look), in the account named: deleted on the
/// server with the same checks, when Timbre has the password at hand (a
/// name that matches nothing never opens a password window).
async fn delete_unlisted(ctx: &Ctx, p: &DeleteMailboxParams) -> Result<MailboxDone, String> {
    let not_found = format!("No mailbox named '{}'. Call list_mailboxes.", p.mailbox);
    let Some(name) = p.account.as_deref() else { return Err(not_found) };
    let loc = find_account(&ctx.store, name)?;
    let wanted = parts_of(&p.mailbox).map_err(|_| not_found.clone())?;
    let acct = account(&loc).await?;
    let Some(password) = password_at_hand(&acct).await else { return Err(not_found) };
    let what = format!("'{}' ({})", wanted.join("/"), loc.account);
    let path = on_server(&acct, password, move |s| {
        let all = s.list().map_err(|e| format!("{what} was not deleted: {e}."))?;
        let mut ways = vec![wanted.clone()];
        if !wanted[0].eq_ignore_ascii_case("INBOX") {
            ways.push(std::iter::once("INBOX".to_string()).chain(wanted.iter().cloned()).collect());
        }
        let mut hits: Vec<Vec<String>> = ways.iter().filter_map(|w| find(&all, w).map(Entry::segments)).collect();
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
        mailbox: shown(&path),
        account: loc.account,
        on_server: path.join("/"),
        note: "Deleted on the server (Mail did not show it).".into(),
    })
}

pub async fn create_mailbox(ctx: &Ctx, p: &CreateMailboxParams) -> Result<MailboxDone, String> {
    let wanted = parts_of(&p.mailbox)?;
    let loc = find_account(&ctx.store, &p.account)?;
    let made = make(ctx, &loc, &wanted).await?;
    Ok(made)
}

/// Make a mailbox in an account, on the server, and wait for Mail to show
/// it.
async fn make(ctx: &Ctx, loc: &Loc, wanted: &[String]) -> Result<MailboxDone, String> {
    let prefix = mail_prefix(&ctx.store, loc);
    // Where Mail will show it: from the account's prefix on.
    let path: Vec<String> = prefix.clone().unwrap_or_default().into_iter().chain(wanted.iter().cloned()).collect();
    let at = Loc { path: path.clone(), ..loc.clone() };
    let done = |path: &[String], note: &str| MailboxDone {
        mailbox: shown(path),
        account: loc.account.clone(),
        on_server: path.join("/"),
        note: note.into(),
    };
    if mail_shows(&at, true, Duration::ZERO).await == Some(true) {
        return Ok(done(&path, "It exists already."));
    }
    // Mail first: no password, and it shows the mailbox at once.
    match mailapp::make_mailbox(&loc.account_id, &wanted.join("/"), Duration::from_secs(25)).await {
        Ok(()) => {
            let shown_now = mail_shows(&at, true, MAIL_CATCHES_UP).await == Some(true);
            if shown_now {
                let conn = ctx.db();
                let _ = ctx.store.reload_mailboxes(&conn);
            }
            // Made, Mail says: never made a second time on the server.
            let note =
                if shown_now { "Made by Mail, on the server too." } else { "Mail made it; it shows it in a moment." };
            return Ok(done(&path, note));
        }
        Err(e) => tracing::info!("Mail would not make the mailbox, so the server is asked: {e:#}"),
    }
    let acct = account(loc).await?;
    let password = password(&acct).await?;
    let what = format!("'{}' ({})", wanted.join("/"), loc.account);
    let wanted = wanted.to_vec();
    let (path, existed) = on_server(&acct, password, move |s| create_in(s, &wanted, &what, prefix)).await?;
    ask_mail_to_sync(loc);
    let listed = mail_shows(&Loc { path: path.clone(), ..loc.clone() }, true, MAIL_CATCHES_UP).await == Some(true);
    if listed {
        let conn = ctx.db();
        let _ = ctx.store.reload_mailboxes(&conn);
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
/// no window asking for it.
async fn password_at_hand(acct: &Account) -> Option<String> {
    let k = key(&acct.server);
    if let Some(p) = known(&k) {
        return Some(p);
    }
    let (h, u) = (acct.server.host.clone(), acct.server.user.clone());
    match blocking(move || keychain::saved(&h, &u)).await {
        Ok(Found::Password(p)) => {
            remember(&k, p.clone());
            Some(p)
        }
        _ => None,
    }
}

/// Make a mailbox only if the password is at hand (no window): for an
/// archive Mail would not make.
pub async fn make_if_password_known(ctx: &Ctx, loc: &Loc, name: &str) -> Result<(), String> {
    let acct = account(loc).await?;
    let password = password_at_hand(&acct).await.ok_or("no password kept for this account yet")?;
    let (wanted, prefix) = (vec![name.to_string()], mail_prefix(&ctx.store, loc));
    let what = format!("'{name}' ({})", loc.account);
    let (path, _) = on_server(&acct, password, move |s| create_in(s, &wanted, &what, prefix)).await?;
    ask_mail_to_sync(loc);
    mail_shows(&Loc { path, ..loc.clone() }, true, MAIL_CATCHES_UP).await;
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
        assert_eq!(find(&entries, &path("INBOX/archives")).map(|e| e.name.as_str()), Some("INBOX/Archives"));
        // Two that differ only in case: never a guess.
        assert_eq!(find(&entries, &path("FOO")), None);
        assert_eq!(find(&entries, &path("foo")).map(|e| e.name.as_str()), Some("foo"));
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
            fake(vec![("LIST", LIST_LA_POSTE), ("STATUS", "* STATUS \"INBOX/Envoy&AOk-s\" (MESSAGES 3)\r\n$T OK\r\n")]);
        let e = delete_in(&mut s, &path("INBOX/Envoyés"), "'Envoyés'").unwrap_err();
        assert!(e.contains("3 message"), "{e}");
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
        assert!(delete_in(&mut s, &path("[Gmail]"), "'[Gmail]'").unwrap_err().contains("only to hold others"));
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
        assert_eq!((shown(&made).as_str(), existed), ("Factures & Reçus", false));
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
    }

    #[test]
    fn the_password_window_says_why_in_one_sentence() {
        let acct = |host: &str| Account {
            name: "Laposte".into(),
            server: Server::new(host, Some(993), Some(true), "a@laposte.net"),
            provider: provider(host),
        };
        let ask = Ask::in_language(&acct("imap.laposte.net"), Note::None, true);
        assert!(ask.text.contains("« Laposte »") && ask.text.contains("une seule fois") && ask.link.is_none());
        assert_eq!(ask.text.matches(". ").count() + ask.text.matches(".\n").count(), 0, "{}", ask.text);
        let ask = Ask::in_language(&acct("imap.gmail.com"), Note::Wrong, false);
        assert!(ask.text.starts_with("The server did not accept"));
        assert_eq!(ask.link, Some(("Create one…", "https://myaccount.google.com/apppasswords")));
    }
}
