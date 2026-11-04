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
    // An attribution names a date or an address: "Le client nous a écrit :"
    // is a sentence of the message, not the start of a quote.
    let attribution = joined.chars().any(|c| c.is_ascii_digit()) || joined.contains('@');
    // "On <date>, <name> wrote:" (possibly wrapped onto two lines)
    if starts("on ") && attribution && (lower.ends_with("wrote:") || (joined.ends_with("wrote:") && joined.len() < 300))
    {
        return true;
    }
    // "Le <date>, <nom> a écrit :"
    if starts("le ")
        && attribution
        && (lower.ends_with("a écrit :")
            || lower.ends_with("a écrit:")
            || joined.ends_with("a écrit :")
            || joined.ends_with("a écrit:"))
    {
        return true;
    }
    // "El <fecha>, <nombre> escribió:"
    if starts("el ") && attribution && (lower.ends_with("escribió:") || joined.ends_with("escribió:")) {
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

/// The header block Outlook puts above the message it quotes: an optional
/// separator line, "From:"/"De :", then "Sent:"/"Envoyé :" or an address,
/// and "To:"/"Subject:". A booking's "From: Paris / To: Madrid / Date:" is
/// not one: no address, no "Sent:", no "Subject:".
fn is_outlook_block(lines: &[&str], i: usize) -> bool {
    let l = lines[i].trim();
    let sep = l.len() >= 20 && l.chars().all(|c| c == '_' || c == '-');
    let at = if sep { i + 1 } else { i };
    let get = |k: usize| lines.get(k).map(|s| s.trim().to_lowercase()).unwrap_or_default();
    let from = get(at);
    let from_ok = ["from:", "de :", "de:", "von:"].iter().any(|p| from.starts_with(p));
    if !from_ok {
        return false;
    }
    let next: Vec<String> = (at + 1..=at + 4).map(get).collect();
    let has = |keys: &[&str]| next.iter().any(|s| keys.iter().any(|k| s.starts_with(k)));
    let sent = has(&["sent:", "envoyé :", "envoyé:", "enviado:", "enviado el:", "gesendet:"]);
    let dated = has(&["date:", "date :", "fecha:", "datum:"]);
    let subject = has(&["subject:", "objet :", "objet:", "asunto:", "betreff:"]);
    let address = from.contains('@');
    (sent && (subject || address || sep)) || (dated && subject && (address || sep))
}

/// Subjects of forwarded messages, in the languages mail is written in.
fn is_forward_subject(subject: &str) -> bool {
    let s = subject.trim_start().to_lowercase();
    // English, French, Spanish, German, Portuguese, Italian, Dutch, Nordic, Polish.
    ["fwd:", "fw:", "tr:", "rv:", "wg:", "enc:", "i:", "doorst:", "vs:", "vb:", "pd:"]
        .iter()
        .any(|p| s.starts_with(p) || s.starts_with(&p.replace(':', " :")))
}

fn starts_ci(s: &str, prefix: &str) -> bool {
    s.as_bytes().get(..prefix.len()).is_some_and(|h| h.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Cheap tests that rule out most lines before the exact (allocating) ones.
fn may_be_forward(t: &str) -> bool {
    ["essag", "ESSAG", "ensaj", "ENSAJ", "ensag", "ENSAG", "achricht", "ACHRICHT", "ericht", "ERICHT"]
        .iter()
        .any(|n| t.contains(n))
}

/// The line that starts a forwarded message, in the languages mail
/// programs write it in (lower-cased `t`).
fn is_forward_marker(lower: &str) -> bool {
    [
        "forwarded message",
        "message transféré",
        "message réexpédié",
        "mensaje reenviado",
        "weitergeleitete nachricht",
        "weitergeleiteten nachricht",
        "messaggio inoltrato",
        "mensagem encaminhada",
        "mensagem reencaminhada",
        "doorgestuurd bericht",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

fn may_be_header(t: &str) -> bool {
    starts_ci(t, "on ")
        || starts_ci(t, "le ")
        || starts_ci(t, "el ")
        || t.starts_with("--")
        || t.starts_with("__")
        || starts_ci(t, "from:")
        || starts_ci(t, "de:")
        || starts_ci(t, "de :")
}

/// The new content of a message with this subject: a forward is kept
/// whole (what was forwarded is its substance), other messages lose their
/// quoted history and signature.
pub fn strip_quotes_of(text: &str, subject: &str) -> String {
    if is_forward_subject(subject) { text.trim().to_string() } else { strip_quotes(text) }
}

/// Return the new content of a message: quoted history and signature removed.
pub fn strip_quotes(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let quoted = |k: usize| lines.get(k).is_some_and(|l| l.trim_start().starts_with('>'));
    let mut skip_header = false;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with('>') {
            continue;
        }
        if std::mem::take(&mut skip_header) {
            continue; // second line of a wrapped attribution
        }
        let lower = if may_be_forward(t) { t.to_lowercase() } else { String::new() };
        if is_forward_marker(&lower) {
            // A forward: what follows is the substance, keep all of it.
            out.extend_from_slice(&lines[i..]);
            break;
        }
        if may_be_header(t) && is_reply_header(line, lines.get(i + 1).copied()) {
            // Answers written between quoted lines are kept: when quoted
            // lines follow the attribution, only those are dropped.
            let first = (i + 1..lines.len()).find(|&k| !lines[k].trim().is_empty());
            let wrapped = first.is_some_and(|k| !quoted(k) && lines[k].trim().len() < 80 && quoted(k + 1));
            if first.is_some_and(quoted) || wrapped {
                skip_header = wrapped;
                continue;
            }
            break;
        }
        if may_be_header(t) && is_outlook_block(&lines, i) {
            break;
        }
        // "-- " starts a signature (the trailing space is often trimmed on
        // the way: then only near the end, as "--" may separate content).
        if *line == "-- " || (line.trim() == "--" && lines.len() - i <= 40) {
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
    fn keeps_what_is_not_a_quote() {
        // A forward from Outlook keeps what it forwards.
        let fwd = "Pour info.\n________________________________\nDe : Banque <info@banque.fr>\nEnvoyé : lundi\nObjet : Prélèvement\n\nLe prélèvement de 42 € sera fait le 5.";
        assert!(strip_quotes_of(fwd, "TR: Prélèvement").contains("42 €"));
        assert_eq!(strip_quotes_of(fwd, "Re: Prélèvement"), "Pour info.");
        assert!(strip_quotes_of(fwd, "I: Prelievo").contains("42 €"));
        assert!(strip_quotes_of(fwd, "WG : Lastschrift").contains("42 €"));
        for marker in [
            "---------- Weitergeleitete Nachricht ---------",
            "Anfang der weitergeleiteten Nachricht:",
            "---------- Messaggio inoltrato ---------",
            "Início da mensagem reencaminhada:",
            "Begin doorgestuurd bericht:",
        ] {
            // Quoted lines after the marker are kept only if it is taken for
            // a forward.
            let text = format!("FYI\n{marker}\n> Von: Bank <info@bank.de>\n> Betrag 42 €");
            assert!(strip_quotes(&text).contains("42 €"), "{marker}");
        }
        // A booking's own From:/To:/Date: lines are its content.
        let booking = "Your trip\nFrom: Paris (CDG)\nTo: Madrid (MAD)\nDate: 12 Dec 2026\nHotel Gran Via, total 420 €";
        assert!(strip_quotes(booking).contains("Hotel Gran Via"));
        // A sentence ending in "a écrit :" is not an attribution.
        let t = "Le client nous a écrit :\nil veut un remboursement de 30 €.";
        assert!(strip_quotes(t).contains("remboursement"));
        // Answers between quoted lines stay.
        let inline = "See below.\n\nOn Mon, 3 Nov 2026, Ann <a@x.fr> wrote:\n> Can you come Friday?\nYes, at 10.\n> And the budget?\nApproved.";
        let s = strip_quotes(inline);
        assert!(s.contains("Yes, at 10.") && s.contains("Approved.") && !s.contains("Can you"), "{s}");
        // Spanish Outlook.
        let es = "Gracias.\nDe: Ana <ana@x.es>\nEnviado el: lunes\nPara: Juan\nAsunto: Hola\nviejo";
        assert_eq!(strip_quotes(es), "Gracias.");
    }

    #[test]
    fn falls_back_when_everything_is_quoted() {
        assert_eq!(strip_quotes("> only quoted"), "> only quoted");
    }
}
