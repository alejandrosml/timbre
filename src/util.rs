//! Small helpers shared across modules: time formatting, URI encoding,
//! text clipping and invisible-character handling.

use chrono::{DateTime, Local, TimeZone, Utc};

/// Seconds between the Unix epoch (1970) and the Mac absolute epoch (2001).
pub const MAC_EPOCH_OFFSET: i64 = 978_307_200;

/// Format a Unix timestamp as local ISO-8601 with minutes and offset,
/// e.g. `2026-11-03T16:23+01:00`. Compact on purpose: it is read by a model.
pub fn iso_local(ts: i64) -> Option<String> {
    if ts <= 0 {
        return None;
    }
    let dt: DateTime<Local> = Local.timestamp_opt(ts, 0).single()?;
    Some(dt.format("%Y-%m-%dT%H:%M%:z").to_string())
}

/// Parse a user-supplied date or datetime into a Unix timestamp.
/// Accepts `YYYY-MM-DD`, RFC 3339, or `YYYY-MM-DDTHH:MM` (local time).
pub fn parse_when(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M") {
        return Local.from_local_datetime(&nd).earliest().map(|d| d.timestamp());
    }
    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return Local.from_local_datetime(&nd).earliest().map(|d| d.timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let nd = d.and_hms_opt(0, 0, 0)?;
        return Local.from_local_datetime(&nd).earliest().map(|d| d.timestamp());
    }
    None
}

pub fn now_unix() -> i64 {
    Utc::now().timestamp()
}

/// Percent-encode a filesystem path for use in an SQLite `file:` URI.
pub fn sqlite_uri(path: &std::path::Path, query: &str) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len() + 16);
    out.push_str("file:");
    for b in raw.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    if !query.is_empty() {
        out.push('?');
        out.push_str(query);
    }
    out
}

/// Percent-decode a URL path component (mailbox URLs in the Envelope Index).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Clip a string to at most `max` characters on a char boundary.
/// Returns the clipped text and whether anything was cut.
pub fn clip(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((idx, _)) => (s[..idx].to_string(), true),
        None => (s.to_string(), false),
    }
}

/// Characters that are invisible when rendered but survive copy/paste:
/// zero-width spaces and joiners, bidirectional controls, the BOM and the
/// soft hyphen. They are a common way to smuggle instructions or to
/// obfuscate phishing text, so we drop them and report how many we saw.
pub fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
        | '\u{2065}'..='\u{2069}' | '\u{3164}' | '\u{FE00}'..='\u{FE0E}' | '\u{FEFF}' | '\u{FFA0}'
        | '\u{E0000}'..='\u{E0FFF}')
}

/// Remove invisible characters, returning the cleaned text and the count removed.
pub fn strip_invisible(s: &str) -> (String, usize) {
    let Some(start) = s.bytes().position(|b| b >= 0x80) else {
        return (s.to_string(), 0);
    };
    let Some(first) = s[start..].char_indices().find(|&(_, c)| is_invisible(c)).map(|(i, _)| start + i) else {
        return (s.to_string(), 0);
    };
    let mut out = String::with_capacity(s.len());
    out.push_str(&s[..first]);
    let mut removed = 0;
    for c in s[first..].chars() {
        if is_invisible(c) {
            removed += 1;
        } else {
            out.push(c);
        }
    }
    (out, removed)
}

/// Collapse runs of spaces/tabs and limit consecutive blank lines to one.
pub fn tidy_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    let mut collapsed = String::new();
    for line in s.lines() {
        collapsed.clear();
        let mut prev_space = false;
        for c in line.chars() {
            let space = c == ' ' || c == '\t' || c == '\u{00A0}';
            if space {
                if !prev_space {
                    collapsed.push(' ');
                }
            } else {
                collapsed.push(c);
            }
            prev_space = space;
        }
        let trimmed = collapsed.trim_end();
        if trimmed.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
            out.push('\n');
        } else {
            blank_run = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

/// Format a display address as `Name <addr>` or just `addr`.
pub fn fmt_addr(name: Option<&str>, addr: Option<&str>) -> String {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    let addr = addr.map(str::trim).filter(|a| !a.is_empty());
    match (name, addr) {
        (Some(n), Some(a)) if !n.eq_ignore_ascii_case(a) => format!("{n} <{a}>"),
        (_, Some(a)) => a.to_string(),
        (Some(n), None) => n.to_string(),
        (None, None) => String::new(),
    }
}

/// Domain part of an address, lower-cased.
pub fn domain_of(addr: &str) -> Option<String> {
    let at = addr.rfind('@')?;
    let d = addr[at + 1..].trim().trim_end_matches('>').to_ascii_lowercase();
    if d.is_empty() { None } else { Some(d) }
}

/// What the user adds to Full Disk Access: Timbre.app when the program runs
/// from it (`.../Timbre.app/Contents/MacOS/timbre`), else the program itself.
pub fn app_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.ancestors()
        .nth(3)
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .map(std::path::Path::to_path_buf)
        .unwrap_or(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_never_slices_inside_a_character() {
        assert_eq!(percent_decode("Promo%aé"), "Promo%aé");
        assert_eq!(percent_decode("%C3%A9t%C3%A9%2"), "été%2");
        assert_eq!(percent_decode("Sent%20Messages"), "Sent Messages");
    }

    #[test]
    fn uri_encodes_spaces() {
        let p = std::path::Path::new("/Users/a b/Library/Mail/V10/MailData/Envelope Index");
        assert_eq!(sqlite_uri(p, "mode=ro"), "file:/Users/a%20b/Library/Mail/V10/MailData/Envelope%20Index?mode=ro");
    }

    #[test]
    fn decode_percent() {
        assert_eq!(percent_decode("Sent%20Messages"), "Sent Messages");
        assert_eq!(percent_decode("%C3%89l%C3%A9ments"), "Éléments");
    }

    #[test]
    fn clip_respects_chars() {
        assert_eq!(clip("réunion", 3), ("réu".to_string(), true));
        assert_eq!(clip("abc", 5), ("abc".to_string(), false));
    }

    #[test]
    fn invisible_removed() {
        let (s, n) = strip_invisible("pay\u{200B}pal\u{202E}");
        assert_eq!(s, "paypal");
        assert_eq!(n, 2);
    }

    #[test]
    fn whitespace_tidy() {
        assert_eq!(tidy_whitespace("a  b\n\n\n\nc\t\td"), "a b\n\nc d");
    }
}
