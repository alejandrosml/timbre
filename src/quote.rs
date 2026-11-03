//! Strip quoted replies and signatures before indexing.
//!
//! Earlier messages of a thread are indexed on their own, so indexing the
//! quoted history again only adds noise (every reply would match every old
//! term) and size. Forwarded content is kept: it is the substance of a
//! forward. The full text is still shown when a message is read.

fn is_reply_header(line: &str, next: Option<&str>) -> bool {
    let l = line.trim();
    let lower = l.to_lowercase();
    let joined = match next {
        Some(n) => format!("{} {}", lower, n.trim().to_lowercase()),
        None => lower.clone(),
    };
    let starts = |p: &str| lower.starts_with(p);
    // "On <date>, <name> wrote:" (possibly wrapped onto two lines)
    if starts("on ") && (lower.ends_with("wrote:") || (joined.ends_with("wrote:") && joined.len() < 300)) {
        return true;
    }
    // "Le <date>, <nom> a écrit :"
    if starts("le ")
        && (lower.ends_with("a écrit :")
            || lower.ends_with("a écrit:")
            || joined.ends_with("a écrit :")
            || joined.ends_with("a écrit:"))
    {
        return true;
    }
    // "El <fecha>, <nombre> escribió:"
    if starts("el ") && (lower.ends_with("escribió:") || joined.ends_with("escribió:")) {
        return true;
    }
    matches!(
        lower.as_str(),
        "-----original message-----"
            | "-----message d'origine-----"
            | "-----mensaje original-----"
            | "----- original message -----"
    )
}

fn is_outlook_block(lines: &[&str], i: usize) -> bool {
    // A separator line followed by From:/De : and Sent:/Envoyé :/Enviado:
    let l = lines[i].trim();
    let sep = l.len() >= 20 && l.chars().all(|c| c == '_' || c == '-');
    let at = if sep { i + 1 } else { i };
    let get = |k: usize| lines.get(k).map(|s| s.trim().to_lowercase()).unwrap_or_default();
    let from = get(at);
    let from_ok = from.starts_with("from:") || from.starts_with("de :") || from.starts_with("de:");
    if !from_ok {
        return false;
    }
    (at + 1..=at + 3).any(|k| {
        let s = get(k);
        s.starts_with("sent:")
            || s.starts_with("envoyé :")
            || s.starts_with("envoyé:")
            || s.starts_with("enviado:")
            || s.starts_with("date:")
            || s.starts_with("date :")
            || s.starts_with("fecha:")
    })
}

/// Return the new content of a message: quoted history and signature removed.
pub fn strip_quotes(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let lower = t.to_lowercase();
        if lower.contains("forwarded message")
            || lower.contains("message transféré")
            || lower.contains("message réexpédié")
            || lower.contains("mensaje reenviado")
        {
            // A forward: what follows is the substance, keep all of it.
            out.extend_from_slice(&lines[i..]);
            break;
        }
        if is_reply_header(line, lines.get(i + 1).copied()) || is_outlook_block(&lines, i) {
            break;
        }
        if t.starts_with('>') {
            continue;
        }
        if *line == "-- " || (line.trim() == "--" && lines.len() - i <= 15) {
            break;
        }
        out.push(line);
    }
    let result = out.join("\n").trim().to_string();
    if result.is_empty() { text.trim().to_string() } else { result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_english_reply() {
        let t = "Sounds good.\n\nOn Mon, Nov 3, 2026 at 10:00 AM Ann <ann@x.com> wrote:\n> old text\n> more";
        assert_eq!(strip_quotes(t), "Sounds good.");
    }

    #[test]
    fn strips_wrapped_french_reply() {
        let t = "D'accord pour mardi.\n\nLe 3 nov. 2026 à 10:00, Marie Dupont <marie@x.fr>\na écrit :\n> ancien";
        assert_eq!(strip_quotes(t), "D'accord pour mardi.");
    }

    #[test]
    fn strips_spanish_and_outlook() {
        assert_eq!(strip_quotes("Vale.\nEl lun, 3 nov 2026, Ana escribió:\n> hola"), "Vale.");
        let o = "Merci\n________________________________\nDe : Paul\nEnvoyé : lundi\nObjet : x\nancien";
        assert_eq!(strip_quotes(o), "Merci");
    }

    #[test]
    fn keeps_forwards_and_signature_cut() {
        let t = "FYI\n\nBegin forwarded message:\n\nFrom: Bob\nDate: 3 Nov 2026\nSubject: Plan\n\nThe plan is...";
        assert!(strip_quotes(t).contains("The plan is"));
        assert_eq!(strip_quotes("Hello\n-- \nAlex\nCEO"), "Hello");
    }

    #[test]
    fn falls_back_when_everything_is_quoted() {
        assert_eq!(strip_quotes("> only quoted"), "> only quoted");
    }
}
