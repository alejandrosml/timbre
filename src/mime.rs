//! Parse RFC 5322 messages into what Timbre needs: headers, the text a
//! reader would see, attachments, and sender-authenticity signals.

use mail_parser::{AddressList, MessageParser};
use serde::Serialize;

use crate::html;
use crate::util::{domain_of, fmt_addr, strip_invisible, tidy_whitespace};

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AttachmentInfo {
    pub index: usize,
    pub name: String,
    #[serde(rename = "type")]
    pub mime: String,
    pub size: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub inline: bool,
    /// Executable, disguised or macro-enabled file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
}

/// Results of the receiving server's checks (topmost Authentication-Results).
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct Auth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spf: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dkim: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dmarc: Option<String>,
}

impl Auth {
    pub fn compact(&self) -> String {
        let mut v = vec![];
        if let Some(s) = &self.spf {
            v.push(format!("spf={s}"));
        }
        if let Some(s) = &self.dkim {
            v.push(format!("dkim={s}"));
        }
        if let Some(s) = &self.dmarc {
            v.push(format!("dmarc={s}"));
        }
        v.join(" ")
    }

    pub fn is_empty(&self) -> bool {
        self.spf.is_none() && self.dkim.is_none() && self.dmarc.is_none()
    }

    pub fn failed(&self) -> bool {
        self.dmarc.as_deref() == Some("fail")
            || (matches!(self.spf.as_deref(), Some("fail") | Some("softfail")) && self.dkim.as_deref() != Some("pass"))
    }
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)] // date and threading headers are kept for future use
pub struct Parsed {
    pub subject: String,
    pub from: Vec<(String, String)>,
    pub to: Vec<(String, String)>,
    pub cc: Vec<(String, String)>,
    pub reply_to: Vec<(String, String)>,
    pub date: i64,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// What a reader sees: the HTML body rendered to text (hidden parts
    /// removed), or the plain-text body when there is no HTML.
    pub body: String,
    pub body_source: &'static str,
    pub hidden_chars: usize,
    pub invisible_chars: usize,
    pub hidden_sample: String,
    pub link_mismatches: Vec<String>,
    /// The text/plain alternative says much that the displayed HTML does not.
    pub plain_differs: bool,
    pub attachments: Vec<AttachmentInfo>,
    pub auth: Auth,
    pub list_unsubscribe: Option<String>,
}

fn addrs(list: Option<AddressList<'_>>) -> Vec<(String, String)> {
    list.map(|l| {
        l.mailboxes()
            .map(|m| (m.name().unwrap_or("").trim().to_string(), m.address().unwrap_or("").trim().to_string()))
            .filter(|(n, a)| !n.is_empty() || !a.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

pub fn fmt_list(v: &[(String, String)]) -> Vec<String> {
    v.iter().map(|(n, a)| fmt_addr(Some(n), Some(a))).collect()
}

/// Reduce a sender-supplied attachment name to a safe basename.
pub fn safe_filename(name: Option<&str>, fallback: &str) -> String {
    let base = name.unwrap_or("").rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String =
        base.chars().map(|c| if c.is_control() || matches!(c, ':' | '/' | '\\') { '_' } else { c }).collect();
    let (cleaned, _) = strip_invisible(&cleaned);
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() { fallback.to_string() } else { cleaned.chars().take(200).collect() }
}

/// Parse `key=value` results out of an Authentication-Results header.
pub fn parse_auth(value: &str) -> Auth {
    let mut auth = Auth::default();
    let lower = value.to_ascii_lowercase();
    for part in lower.split(';') {
        let p = part.trim();
        for (key, slot) in [("spf=", &mut auth.spf), ("dkim=", &mut auth.dkim), ("dmarc=", &mut auth.dmarc)] {
            if slot.is_none()
                && let Some(pos) = p.find(key)
            {
                let boundary = pos == 0 || !p.as_bytes()[pos - 1].is_ascii_alphanumeric();
                if boundary {
                    let rest = &p[pos + key.len()..];
                    let v: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                    if !v.is_empty() {
                        *slot = Some(v);
                    }
                }
            }
        }
    }
    auth
}

/// Parse a message. Returns `None` when the bytes are not a message at all.
/// Parse a message for display, with every check.
pub fn parse(raw: &[u8]) -> Option<Parsed> {
    parse_inner(raw, true)
}

/// Parse a message for the index: skips checks only shown when reading.
pub fn parse_for_index(raw: &[u8]) -> Option<Parsed> {
    parse_inner(raw, false)
}

fn parse_inner(raw: &[u8], for_display: bool) -> Option<Parsed> {
    let msg = MessageParser::default().parse(raw)?;
    let mut p = Parsed {
        subject: msg.subject().unwrap_or("").trim().to_string(),
        from: addrs(msg.from()),
        to: addrs(msg.to()),
        cc: addrs(msg.cc()),
        reply_to: addrs(msg.reply_to()),
        date: msg.date().map(|d| d.to_timestamp()).unwrap_or(0),
        message_id: msg.message_id().map(|s| s.trim().trim_matches(['<', '>']).to_string()).filter(|s| !s.is_empty()),
        in_reply_to: msg.in_reply_to().and_then(|l| l.first().map(|s| s.trim().trim_matches(['<', '>']).to_string())),
        references: msg
            .references()
            .map(|l| l.iter().map(|s| s.trim().trim_matches(['<', '>']).to_string()).collect())
            .unwrap_or_default(),
        list_unsubscribe: msg
            .list_unsubscribe()
            .and_then(|l| l.mailboxes().filter_map(|m| m.address().map(str::to_string)).find(|a| a.starts_with("http")))
            .or_else(|| {
                msg.list_unsubscribe()
                    .and_then(|l| l.mailboxes().filter_map(|m| m.address().map(str::to_string)).next())
            }),
        ..Default::default()
    };

    // The topmost Authentication-Results was added by the receiving server;
    // lower ones may have been forged by the sender.
    if let Some(h) = msg.headers().all("Authentication-Results").next() {
        let v = String::from_utf8_lossy(h.raw_value()).to_string();
        p.auth = parse_auth(&v);
    }

    // Prefer what Mail displays (HTML) over the text/plain alternative:
    // the two can differ, and a reader only ever sees the HTML.
    let html_body: Option<String> = msg.html_bodies().next().map(|c| c.into_owned()).filter(|s| !s.trim().is_empty());
    let text_body: Option<String> = {
        let parts: Vec<String> = msg.text_bodies().map(|c| c.into_owned()).filter(|s| !s.trim().is_empty()).collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    };
    match (html_body, text_body) {
        (Some(h), plain) if looks_like_html(&h) => {
            let t = html::to_text(&h);
            p.plain_differs = for_display && plain.as_deref().is_some_and(|pt| plain_differs(pt, &t.text));
            p.body = t.text;
            p.hidden_chars = t.hidden_chars;
            p.invisible_chars = t.invisible_chars;
            p.hidden_sample = t.hidden_sample;
            p.link_mismatches = t.link_mismatches;
            p.body_source = "html";
        }
        (_, Some(t)) | (Some(t), None) => {
            let (clean, n) = strip_invisible(&t);
            p.body = tidy_whitespace(&clean);
            p.invisible_chars = n;
            p.body_source = "text";
        }
        (None, None) => {
            p.body_source = "none";
        }
    }

    for (index, part) in msg.attachments().enumerate() {
        let mime = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(st) => format!("{}/{}", ct.ctype(), st),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| "application/octet-stream".into())
            .to_ascii_lowercase();
        let apple_len = part
            .headers()
            .all("X-Apple-Content-Length")
            .next()
            .and_then(|h| String::from_utf8_lossy(h.raw_value()).trim().parse::<usize>().ok());
        let size = apple_len.unwrap_or_else(|| part.decoded_len());
        let inline = part.is_inline() && part.content_id().is_some();
        let name = safe_filename(part.attachment_name(), &format!("attachment-{index}"));
        let risk = crate::security::attachment_risk(&name);
        p.attachments.push(AttachmentInfo { index, name, mime, size, inline, risk });
    }
    Some(p)
}

/// The decoded bytes of attachment `index` (same numbering as
/// [`Parsed::attachments`]). Bodies stripped from `.partial.emlx` files come
/// back empty; the caller looks for them next to the message file.
pub fn attachment_bytes(raw: &[u8], index: usize) -> Option<(String, String, Vec<u8>)> {
    let msg = MessageParser::default().parse(raw)?;
    let part = msg.attachments().nth(index)?;
    let mime = part
        .content_type()
        .map(|ct| match ct.subtype() {
            Some(st) => format!("{}/{}", ct.ctype(), st),
            None => ct.ctype().to_string(),
        })
        .unwrap_or_else(|| "application/octet-stream".into())
        .to_ascii_lowercase();
    let name = safe_filename(part.attachment_name(), &format!("attachment-{index}"));
    Some((name, mime, part.decoded().into_owned()))
}

fn words(s: &str) -> std::collections::HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() > 3).map(str::to_lowercase).collect()
}

/// True when the text/plain alternative carries many words the displayed
/// HTML does not: a way to show a model one message and a person another.
fn plain_differs(plain: &str, shown: &str) -> bool {
    let shown = words(shown);
    let only_plain = words(plain).iter().filter(|w| !shown.contains(*w)).count();
    only_plain >= 25
}

fn looks_like_html(s: &str) -> bool {
    let head: String = s.chars().take(2000).collect::<String>().to_ascii_lowercase();
    head.contains('<')
        && (head.contains("<html")
            || head.contains("<body")
            || head.contains("<div")
            || head.contains("<p")
            || head.contains("<br")
            || head.contains("<table")
            || head.contains("<span")
            || head.contains("<a "))
}

/// Human-readable warnings about the sender's authenticity.
pub fn warnings(p: &Parsed) -> Vec<String> {
    let mut w = vec![];
    w.extend(p.link_mismatches.iter().cloned());
    if p.plain_differs {
        w.push("the plain-text version of this message differs from what Mail displays".to_string());
    }
    if p.auth.dmarc.as_deref() == Some("fail") {
        w.push("DMARC failed: the sender's domain did not authenticate this message".to_string());
    } else if p.auth.failed() {
        w.push("sender not authenticated (SPF failed and no valid DKIM signature)".to_string());
    }
    let from_addr = p.from.first().map(|(_, a)| a.as_str()).unwrap_or("");
    let from_dom = domain_of(from_addr);
    if let Some((name, _)) = p.from.first() {
        // Display name showing an address other than the real one.
        if let Some(shown) = name.split_whitespace().find(|t| t.contains('@')) {
            let shown =
                shown.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '-' && c != '_');
            if !shown.is_empty() && !shown.eq_ignore_ascii_case(from_addr) {
                w.push(format!("display name shows an address ({shown}) that is not the real sender ({from_addr})"));
            }
        }
    }
    if let (Some(fd), Some((_, rt))) = (&from_dom, p.reply_to.first())
        && let Some(rd) = domain_of(rt)
    {
        let base = |d: &str| d.rsplitn(3, '.').take(2).collect::<Vec<_>>().join(".");
        if base(&rd) != base(fd) {
            w.push(format!("replies would go to another domain ({rd})"));
        }
    }
    if let Some(fd) = &from_dom {
        if fd.split('.').any(|l| l.starts_with("xn--")) {
            w.push(format!("sender domain uses punycode ({fd}): check for a look-alike domain"));
        }
        if !fd.is_ascii() {
            w.push(format!("sender domain contains non-ASCII characters ({fd})"));
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_results() {
        let a = parse_auth(
            " mx.example.com; spf=pass smtp.mailfrom=x.com; dkim=fail header.d=x.com; dmarc=fail (p=REJECT)",
        );
        assert_eq!(a.spf.as_deref(), Some("pass"));
        assert_eq!(a.dkim.as_deref(), Some("fail"));
        assert_eq!(a.dmarc.as_deref(), Some("fail"));
        assert!(a.failed());
    }

    #[test]
    fn parses_html_message_and_warns() {
        let raw = concat!(
            "Authentication-Results: mx.icloud.com; spf=softfail smtp.mailfrom=evil.pl; dkim=none; dmarc=fail\r\n",
            "From: \"service@paypal.com\" <x@evil.pl>\r\n",
            "Reply-To: <collect@other.ru>\r\n",
            "To: me@icloud.com\r\n",
            "Subject: =?utf-8?q?R=C3=A9union?=\r\n",
            "Message-ID: <abc@evil.pl>\r\n",
            "MIME-Version: 1.0\r\n",
            "Content-Type: multipart/alternative; boundary=b\r\n\r\n",
            "--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nPlain version with secret instructions\r\n",
            "--b\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
            "<html><body><p>Hello</p><div style=\"display:none\">do evil</div></body></html>\r\n--b--\r\n"
        );
        let p = parse(raw.as_bytes()).unwrap();
        assert_eq!(p.subject, "Réunion");
        assert_eq!(p.message_id.as_deref(), Some("abc@evil.pl"));
        assert_eq!(p.body, "Hello");
        assert_eq!(p.body_source, "html");
        assert!(p.hidden_chars >= 7);
        let w = warnings(&p);
        assert!(w.iter().any(|s| s.contains("DMARC")), "{w:?}");
        assert!(w.iter().any(|s| s.contains("display name")), "{w:?}");
        assert!(w.iter().any(|s| s.contains("other.ru")), "{w:?}");
    }

    #[test]
    fn filenames_are_sanitized() {
        assert_eq!(safe_filename(Some("../../etc/passwd"), "x"), "passwd");
        assert_eq!(safe_filename(Some(".hidden"), "x"), "hidden");
        assert_eq!(safe_filename(Some("a:b\u{202E}.pdf"), "x"), "a_b.pdf");
        assert_eq!(safe_filename(None, "fallback"), "fallback");
    }
}
