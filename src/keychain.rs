//! The password for a mail server, in the user's keychain.
//!
//! Timbre keeps the one the user types in an item of its own in the login
//! keychain: macOS lets Timbre alone read it without asking. It also looks
//! where earlier versions of macOS had Mail keep account passwords, the
//! login keychain, where macOS asks the user (with their session password)
//! before letting Timbre read one. Mail now keeps them in the protected part
//! of the keychain, "Local Items", which only Apple's own apps may open: no
//! dialog lets another app in.

/// What looking in the keychain found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Found {
    Password(String),
    Nothing,
    /// The keychain or the user said no; why.
    Refused(String),
}

/// The keychain item's name, as Keychain Access shows it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn service(host: &str) -> String {
    format!("Timbre IMAP ({host})")
}

#[cfg(target_os = "macos")]
mod imp {
    use security_framework::os::macos::passwords::{SecAuthenticationType, SecProtocolType, find_internet_password};
    use security_framework::passwords;

    use super::{Found, service};

    /// errSecItemNotFound
    const NOT_FOUND: i32 = -25300;

    fn found(r: security_framework::base::Result<Vec<u8>>) -> Found {
        match r {
            Ok(b) => String::from_utf8(b).map_or_else(|_| Found::Refused("not text".into()), Found::Password),
            Err(e) if e.code() == NOT_FOUND => Found::Nothing,
            Err(e) => Found::Refused(e.to_string()),
        }
    }

    pub fn saved(host: &str, user: &str) -> Found {
        found(passwords::get_generic_password(&service(host), user))
    }

    pub fn save(host: &str, user: &str, password: &str) -> Result<(), String> {
        passwords::set_generic_password(&service(host), user, password.as_bytes()).map_err(|e| e.to_string())
    }

    pub fn forget(host: &str, user: &str) {
        let _ = passwords::delete_generic_password(&service(host), user);
    }

    pub fn mails(host: &str, user: &str) -> Found {
        for protocol in [SecProtocolType::IMAPS, SecProtocolType::IMAP] {
            let r = find_internet_password(None, host, None, user, "", None, protocol, SecAuthenticationType::Any)
                .map(|(password, _item)| password.as_ref().to_vec());
            match found(r) {
                Found::Nothing => {}
                other => return other,
            }
        }
        Found::Nothing
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Found;

    pub fn saved(_: &str, _: &str) -> Found {
        Found::Nothing
    }
    pub fn save(_: &str, _: &str, _: &str) -> Result<(), String> {
        Err("the keychain is only on macOS".into())
    }
    pub fn forget(_: &str, _: &str) {}
    pub fn mails(_: &str, _: &str) -> Found {
        Found::Nothing
    }
}

// Tests never touch the keychain of the Mac they run on.

/// The password Timbre keeps for this server and user. May block while
/// macOS asks the user, when another build of Timbre saved it.
pub fn saved(host: &str, user: &str) -> Found {
    if cfg!(test) { Found::Nothing } else { imp::saved(host, user) }
}

pub fn save(host: &str, user: &str, password: &str) -> Result<(), String> {
    if cfg!(test) { Ok(()) } else { imp::save(host, user, password) }
}

pub fn forget(host: &str, user: &str) {
    if !cfg!(test) {
        imp::forget(host, user);
    }
}

/// The password Mail kept for this server and user in the login keychain,
/// as earlier versions of macOS had it. Blocks while macOS asks the user for
/// their session password.
pub fn mails(host: &str, user: &str) -> Found {
    if cfg!(test) { Found::Nothing } else { imp::mails(host, user) }
}
