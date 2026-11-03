//! Text normalization shared by indexing and querying.
//!
//! SQLite's `unicode61` tokenizer folds case and accents but not ligatures
//! (œ, æ), compatibility forms (ﬁ, fullwidth letters) or ß. We apply NFKC
//! plus those mappings to both the indexed text and the query, so "cœur"
//! finds "coeur" and the reverse.

use std::borrow::Cow;

use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfkc_quick};

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
    if s.is_ascii() {
        return Cow::Borrowed(s);
    }
    let nfkc = is_nfkc_quick(s.chars()) == IsNormalized::Yes;
    if nfkc && !s.chars().any(|c| ligature(c).is_some()) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    let mut push = |c: char| match ligature(c) {
        Some(r) => out.push_str(r),
        None => out.push(c),
    };
    if nfkc {
        s.chars().for_each(&mut push);
    } else {
        s.nfkc().for_each(&mut push);
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
    if tok.len() > 40 && tok.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=_-.%:&?#~".contains(&b)) {
        return None;
    }
    Some(Cow::Borrowed(tok))
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
    }

    #[test]
    fn index_text_keeps_hosts_and_drops_noise() {
        let long = "A".repeat(46);
        let t = for_index(&format!("Voir https://tracker.example.com/x?id=1 et www.site.fr ici\n{long} fin"));
        assert_eq!(t, "Voir tracker.example.com et site.fr ici\nfin\n");
        assert_eq!(for_index("Lien (https://user@zoom.us:443/j/1?pwd=x)."), "Lien zoom.us\n");
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
