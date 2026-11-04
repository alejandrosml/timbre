//! Knowing when a newer Timbre exists.
//!
//! At most once a day, Timbre asks GitHub for the number of its latest
//! release, through macOS's own curl: the request carries nothing about the
//! user or their mail. `mail_status` then reports a newer version, and the
//! first search after that mentions it once, so Claude can tell the user.
//! `TIMBRE_CHECK_UPDATES=0` (the extension's "Check for updates" setting)
//! turns it off.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Config;

const LATEST: &str = "https://api.github.com/repos/alejandrosml/timbre/releases/latest";
const DAY: i64 = 86_400;

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct UpdateView {
    /// The newer version.
    pub version: String,
    /// Where to download it.
    pub url: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    checked: i64,
    latest: Option<String>,
    url: Option<String>,
}

static FOUND: Mutex<Option<UpdateView>> = Mutex::new(None);
static TOLD: AtomicBool = AtomicBool::new(false);

/// A newer release than this one, if the last check found one.
pub fn available() -> Option<UpdateView> {
    FOUND.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The newer release, the first time it is asked for in this session.
pub fn notice_once() -> Option<String> {
    let update = available()?;
    if TOLD.swap(true, Ordering::Relaxed) {
        return None;
    }
    Some(format!(
        "Timbre {} is available (this is {}): {} — mention it to the user once.",
        update.version,
        env!("CARGO_PKG_VERSION"),
        update.url
    ))
}

/// Check in the background, from the cache when it is less than a day old.
pub fn start(cfg: &Config) {
    if !cfg.check_updates {
        return;
    }
    let path = cfg.data_dir.join("latest-release.json");
    let _ = std::thread::Builder::new().name("timbre-updates".into()).spawn(move || {
        // Let Claude's own start-up go first.
        std::thread::sleep(Duration::from_secs(20));
        loop {
            check(&path);
            std::thread::sleep(Duration::from_secs(DAY as u64));
        }
    });
}

fn check(path: &Path) {
    let now = crate::util::now_unix();
    let mut cache: Cache = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if now - cache.checked >= DAY || cache.checked > now {
        if let Some((tag, url)) = fetch() {
            cache.latest = Some(tag);
            cache.url = Some(url);
        }
        cache.checked = now;
        let _ = write_atomically(path, &serde_json::to_vec(&cache).unwrap_or_default());
    }
    if let (Some(latest), Some(url)) = (&cache.latest, &cache.url)
        && newer(latest, env!("CARGO_PKG_VERSION"))
    {
        let version = latest.trim_start_matches('v').to_string();
        *FOUND.lock().unwrap_or_else(|e| e.into_inner()) = Some(UpdateView { version, url: url.clone() });
    }
}

fn fetch() -> Option<(String, String)> {
    let out = std::process::Command::new("/usr/bin/curl")
        .args(["-fsSL", "--max-time", "10", "-H", "Accept: application/vnd.github+json", "-A"])
        .arg(concat!("timbre/", env!("CARGO_PKG_VERSION")))
        .arg(LATEST)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        tracing::info!("update check failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let tag = v.get("tag_name")?.as_str()?.to_string();
    let url = v.get("html_url")?.as_str()?.to_string();
    url.starts_with("https://github.com/alejandrosml/timbre/").then_some((tag, url))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp: PathBuf = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Whether version `a` (a tag such as "v0.5.1") is newer than `b`.
fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<Vec<u64>> {
        let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
        core.split('.').map(|p| p.parse().ok()).collect()
    };
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => x > y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("v0.5.1", "0.5.0"));
        assert!(newer("v0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("v0.5.0", "0.5.0"));
        assert!(!newer("v0.4.9", "0.5.0"));
        assert!(!newer("v0.6.0-beta", "0.6.0"));
        assert!(!newer("nightly", "0.5.0"));
    }

    #[test]
    fn a_fresh_cache_is_used_without_asking_github() {
        let dir = std::env::temp_dir().join(format!("timbre-updates-{}", std::process::id()));
        let path = dir.join("latest-release.json");
        let cache = Cache {
            checked: crate::util::now_unix(),
            latest: Some("v99.0.0".into()),
            url: Some("https://github.com/alejandrosml/timbre/releases/tag/v99.0.0".into()),
        };
        write_atomically(&path, &serde_json::to_vec(&cache).unwrap()).unwrap();
        check(&path);
        let found = available().expect("a newer release");
        assert_eq!(found.version, "99.0.0");
        assert!(notice_once().unwrap().contains("Timbre 99.0.0 is available"));
        assert!(notice_once().is_none(), "said once per session");
        let _ = std::fs::remove_dir_all(dir);
    }
}
