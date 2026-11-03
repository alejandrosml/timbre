//! Runtime configuration.
//!
//! Values come from environment variables. When Timbre runs as a Claude
//! Desktop extension, the manifest maps each `user_config` setting to one of
//! these variables, so the user changes them in the extension's settings.

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// `~/Library/Mail`
    pub mail_root: PathBuf,
    /// `~/Library/Accounts/Accounts4.sqlite` (account UUID -> display name)
    pub accounts_db: PathBuf,
    /// Where Timbre keeps its own index and journals.
    pub data_dir: PathBuf,
    /// The only folder `save_attachment` writes to.
    pub attachments_dir: PathBuf,
    /// Hide every tool that changes anything.
    pub read_only: bool,
    /// Expose the two-step send tools. Off by default.
    pub allow_send: bool,
    /// When non-empty, sending is limited to these addresses or `@domains`.
    pub send_allowlist: Vec<String>,
    /// Hard cap on messages sent per rolling hour.
    pub max_sends_per_hour: u32,
    /// Mailboxes left out of searches unless asked for explicitly.
    pub excluded_roles: Vec<String>,
    /// Build and maintain the full-text index.
    pub index_enabled: bool,
}

fn env_str(key: &str) -> Option<String> {
    let v = std::env::var(key).ok()?;
    let v = v.trim();
    // An unset manifest placeholder arrives verbatim (`${user_config.x}`),
    // and some hosts pass the string "null": treat both as unset.
    if v.is_empty() || v.starts_with("${") || v.eq_ignore_ascii_case("null") {
        None
    } else {
        Some(v.to_string())
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    match env_str(key).map(|s| s.to_ascii_lowercase()) {
        Some(s) if ["1", "true", "yes", "on"].contains(&s.as_str()) => true,
        Some(s) if ["0", "false", "no", "off"].contains(&s.as_str()) => false,
        _ => default,
    }
}

fn expand_home(p: &str, home: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        PathBuf::from(home).join(rest)
    } else if p == "~" {
        PathBuf::from(home)
    } else {
        PathBuf::from(p)
    }
}

impl Config {
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let path = |key: &str, default: &str| -> PathBuf {
            expand_home(&env_str(key).unwrap_or_else(|| default.to_string()), &home)
        };
        let list = |key: &str| -> Vec<String> {
            env_str(key)
                .map(|s| {
                    s.split([',', ';', '\n'])
                        .map(|x| x.trim().to_ascii_lowercase())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        let excluded = {
            let l = list("TIMBRE_EXCLUDED_MAILBOXES");
            if l.is_empty() { vec!["junk".to_string(), "trash".to_string()] } else { l }
        };
        Config {
            mail_root: path("TIMBRE_MAIL_ROOT", "~/Library/Mail"),
            accounts_db: path("TIMBRE_ACCOUNTS_DB", "~/Library/Accounts/Accounts4.sqlite"),
            data_dir: path("TIMBRE_DATA_DIR", "~/Library/Application Support/Timbre"),
            attachments_dir: path("TIMBRE_ATTACHMENTS_DIR", "~/Downloads/Timbre"),
            read_only: env_bool("TIMBRE_READ_ONLY", false),
            allow_send: env_bool("TIMBRE_ALLOW_SEND", false),
            send_allowlist: list("TIMBRE_SEND_ALLOWLIST"),
            max_sends_per_hour: env_str("TIMBRE_MAX_SENDS_PER_HOUR")
                .and_then(|s| s.parse().ok())
                .unwrap_or(10)
                .min(100),
            excluded_roles: excluded,
            index_enabled: env_bool("TIMBRE_INDEX", true),
        }
    }

    pub fn index_path(&self) -> PathBuf {
        self.data_dir.join("index.sqlite")
    }

    /// True when `addr` may receive mail under the send allowlist.
    pub fn recipient_allowed(&self, addr: &str) -> bool {
        if self.send_allowlist.is_empty() {
            return true;
        }
        let a = addr.trim().to_ascii_lowercase();
        let domain = a.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
        self.send_allowlist.iter().any(|rule| {
            if let Some(d) = rule.strip_prefix('@') {
                domain == d || domain.ends_with(&format!(".{d}"))
            } else {
                *rule == a
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_rules() {
        let mut c = Config::from_env();
        c.send_allowlist = vec!["@example.com".into(), "bob@test.org".into()];
        assert!(c.recipient_allowed("Ann@Example.com"));
        assert!(c.recipient_allowed("x@mail.example.com"));
        assert!(c.recipient_allowed("bob@test.org"));
        assert!(!c.recipient_allowed("eve@evil.com"));
        assert!(!c.recipient_allowed("x@notexample.com"));
    }
}
