//! Text extraction from attachments, with size limits.

use crate::{html, mime};

/// Largest attachment Timbre will try to read as text.
const MAX_INPUT: usize = 25 * 1024 * 1024;

fn ext(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn decode_text(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.trim_start_matches('\u{FEFF}').to_string();
    }
    // Not UTF-8: Windows-1252 is the usual suspect for European mail.
    let (cow, _, _) = encoding_rs::WINDOWS_1252.decode(bytes);
    cow.into_owned()
}

pub fn to_text(name: &str, mime_type: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_INPUT {
        return Err(format!(
            "This attachment is too large to read as text ({} MB). Use save_attachment.",
            bytes.len() / 1_048_576
        ));
    }
    let e = ext(name);
    let m = mime_type.to_ascii_lowercase();
    let texty = m.starts_with("text/")
        || matches!(
            m.as_str(),
            "application/json" | "application/xml" | "application/csv" | "application/x-yaml" | "application/yaml"
        )
        || matches!(
            e.as_str(),
            "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "yaml" | "yml" | "ics" | "vcf" | "log" | "ini" | "toml"
        );
    if m == "text/html" || matches!(e.as_str(), "html" | "htm") {
        return Ok(html::to_text(&decode_text(bytes)).text);
    }
    if m == "message/rfc822" || e == "eml" {
        let p = mime::parse(bytes).ok_or("could not parse the attached message")?;
        let mut out = String::new();
        if let Some((n, a)) = p.from.first() {
            out.push_str(&format!("From: {}\n", crate::util::fmt_addr(Some(n), Some(a))));
        }
        out.push_str(&format!("Subject: {}\n\n{}", p.subject, p.body));
        return Ok(out);
    }
    if texty {
        let (clean, _) = crate::util::strip_invisible(&decode_text(bytes));
        return Ok(clean);
    }
    Err(format!(
        "Reading text from '{name}' ({mime_type}) is not supported yet. Use save_attachment to save it to the attachments folder."
    ))
}
