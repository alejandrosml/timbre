//! Text normalization shared by indexing and querying.
//!
//! SQLite's `unicode61` tokenizer folds case and accents but not ligatures
//! (œ, æ), compatibility forms (ﬁ, fullwidth letters) or ß. We apply NFKC
//! plus those mappings to both the indexed text and the query, so "cœur"
//! finds "coeur" and the reverse.

use std::borrow::Cow;

use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};

/// True for characters NFKC leaves unchanged on their own. Accented Latin
/// letters and common punctuation answer without a table lookup.
fn nfkc_stable(c: char) -> bool {
    match c {
        '\u{00C0}'..='\u{0131}' | '\u{0134}'..='\u{013E}' | '\u{0141}'..='\u{0148}' | '\u{014A}'..='\u{017E}' => true,
        '\u{00A1}'..='\u{00A7}'
        | '\u{00A9}'
        | '\u{00AB}'..='\u{00AE}'
        | '\u{00B0}'
        | '\u{00B1}'
        | '\u{00B6}'
        | '\u{00B7}'
        | '\u{00BB}'
        | '\u{00BF}' => true,
        '\u{2013}' | '\u{2014}' | '\u{2018}'..='\u{2022}' | '\u{20AC}' => true,
        _ => is_nfkc_quick(std::iter::once(c)) == IsNormalized::Yes,
    }
}

fn ligature(c: char) -> Option<&'static str> {
    Some(match c {
        'œ' => "oe",
        'Œ' => "OE",
        'æ' => "ae",
        'Æ' => "AE",
        'ß' => "ss",
        'ẞ' => "SS",
        _ => return None,
    })
}

/// NFKC plus ligature folding. Borrows when nothing changes (ASCII text,
/// the common case, costs one scan).
pub fn fold(s: &str) -> Cow<'_, str> {
    let changes = |c: char| !c.is_ascii() && (ligature(c).is_some() || !nfkc_stable(c));
    let Some(first) = s.char_indices().find(|&(_, c)| changes(c)).map(|(i, _)| i) else {
        return Cow::Borrowed(s);
    };
    // Characters are normalized one at a time: composition across characters
    // (a letter followed by a combining accent) is left to the tokenizer,
    // which removes diacritics anyway.
    let mut out = String::with_capacity(s.len() + 8);
    out.push_str(&s[..first]);
    for c in s[first..].chars() {
        if c.is_ascii() {
            out.push(c);
        } else if let Some(r) = ligature(c) {
            out.push_str(r);
        } else if nfkc_stable(c) {
            out.push(c);
        } else {
            out.extend(std::iter::once(c).nfkc());
        }
    }
    Cow::Owned(out)
}

/// Text for the index: folded, links reduced to their host name, and long
/// machine tokens (base64, hex ids, tracking codes) dropped. They bloat the
/// index and never match a real query.
pub fn for_index(s: &str) -> String {
    let folded = fold(s);
    let mut out = String::with_capacity(folded.len());
    for line in folded.lines() {
        let start = out.len();
        for tok in line.split_whitespace() {
            let Some(kept) = index_token(tok) else { continue };
            if out.len() > start {
                out.push(' ');
            }
            out.push_str(&kept);
        }
        if out.len() > start {
            out.push('\n');
        }
    }
    out
}

fn index_token(tok: &str) -> Option<Cow<'_, str>> {
    let core = tok.trim_matches(|c: char| "()<>[]{}\"'«».,;:!?".contains(c));
    let has_prefix = |p: &str| core.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p));
    let after_scheme = if has_prefix("https://") {
        Some(&core[8..])
    } else if has_prefix("http://") {
        Some(&core[7..])
    } else if has_prefix("www.") {
        Some(core)
    } else {
        None
    };
    if let Some(rest) = after_scheme {
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = host.rsplit('@').next().unwrap_or_default(); // user:pass@host
        let host = host.split(':').next().unwrap_or_default(); // :port
        let host = host.get(..4).filter(|h| h.eq_ignore_ascii_case("www.")).map_or(host, |_| &host[4..]);
        return (!host.is_empty() && host.len() <= 60).then(|| Cow::Owned(host.to_string()));
    }
    drop_machine_runs(tok)
}

/// Base64, hex digests and tracking codes are long unbroken runs of ASCII
/// letters and digits; real words are not. Only those runs are dropped, so
/// `report_final_2026.xlsx` or `site.com/a/b` keep their words.
fn drop_machine_runs(tok: &str) -> Option<Cow<'_, str>> {
    const MAX_RUN: usize = 40;
    let mut run = 0;
    let long = tok.bytes().any(|c| {
        run = if c.is_ascii_alphanumeric() { run + 1 } else { 0 };
        run > MAX_RUN
    });
    if !long {
        return Some(Cow::Borrowed(tok));
    }
    let mut out = String::with_capacity(tok.len());
    let mut start: Option<usize> = None;
    for (i, c) in tok.char_indices() {
        if c.is_ascii_alphanumeric() {
            start.get_or_insert(i);
            continue;
        }
        if let Some(s) = start.take()
            && i - s <= MAX_RUN
        {
            out.push_str(&tok[s..i]);
        }
        out.push(c);
    }
    if let Some(s) = start
        && tok.len() - s <= MAX_RUN
    {
        out.push_str(&tok[s..]);
    }
    out.chars().any(char::is_alphanumeric).then_some(Cow::Owned(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_ligatures_and_compat_forms() {
        assert_eq!(fold("Cœur d\u{2019}œuvre ﬁnal Straße"), "Coeur d\u{2019}oeuvre final Strasse");
        assert_eq!(fold("ＡＢＣ"), "ABC");
        assert!(matches!(fold("déjà vu"), Cow::Borrowed(_)));
        assert!(matches!(fold("plain ascii"), Cow::Borrowed(_)));
        assert_eq!(fold("prix\u{00A0}: 3\u{2026} ½"), "prix : 3... 1\u{2044}2");
        for c in ('\u{A0}'..='\u{24F}').chain('\u{2000}'..='\u{206F}') {
            let one = c.to_string();
            let reference: String = one.nfkc().collect();
            let expected = match ligature(c) {
                Some(r) => r.to_string(),
                None => reference,
            };
            assert_eq!(fold(&one), expected, "U+{:04X}", c as u32);
        }
    }

    #[test]
    fn index_text_keeps_hosts_and_drops_noise() {
        let long = "A".repeat(46);
        let t = for_index(&format!("Voir https://tracker.example.com/x?id=1 et www.site.fr ici\n{long} fin"));
        assert_eq!(t, "Voir tracker.example.com et site.fr ici\nfin\n");
        assert_eq!(for_index("Lien (https://user@zoom.us:443/j/1?pwd=x)."), "Lien zoom.us\n");
    }

    #[test]
    fn only_machine_runs_are_dropped() {
        let code = "Z".repeat(41);
        assert_eq!(for_index(&format!("id={code}&utm=x")), "id=&utm=x\n");
        assert_eq!(
            for_index("report_final_version_2026_q3_budget_review.xlsx"),
            "report_final_version_2026_q3_budget_review.xlsx\n"
        );
        assert_eq!(
            for_index("github.com/alejandrosml/timbre/blob/main/README.md"),
            "github.com/alejandrosml/timbre/blob/main/README.md\n"
        );
        assert_eq!(for_index(&code), "");
    }

    #[test]
    fn index_text_keeps_long_words_that_are_not_codes() {
        // CJK runs have no spaces; addresses contain '@'.
        let cjk = "会議の議事録を添付しますのでご確認くださいよろしくお願いいたします会議の議事録を添付します";
        assert_eq!(for_index(cjk), format!("{cjk}\n"));
        let addr = "firstname.lastname.department@company-name.example.com";
        assert_eq!(for_index(addr), format!("{addr}\n"));
    }
}
