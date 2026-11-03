//! HTML e-mail to plain text, without the parts a reader never sees.
//!
//! Hidden text is a classic carrier for prompt injection and phishing
//! tricks: `display:none` blocks, zero-size fonts, white-on-white text,
//! off-screen positioning, MSO-only content. We render what a person would
//! see in Mail and count what we dropped, so the caller can report it.

use std::collections::HashSet;

use ego_tree::NodeRef;
use scraper::{Html, Node};

use crate::util::{strip_invisible, tidy_whitespace};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HtmlText {
    pub text: String,
    /// Characters of text removed because the HTML hid them.
    pub hidden_chars: usize,
    /// Zero-width / bidi control characters removed.
    pub invisible_chars: usize,
    /// The beginning of the hidden text, kept only to scan it for injections.
    pub hidden_sample: String,
    /// Links whose visible text shows one domain but which lead to another.
    pub link_mismatches: Vec<String>,
    /// The HTML was nested too deep to lay out safely: its text is withheld
    /// (counted as hidden) because styles could not be checked.
    pub degraded: bool,
}

/// Host of a URL or of a bare domain shown as link text, lower-cased.
fn host_of(s: &str) -> Option<String> {
    let t = s.trim().trim_end_matches(['/', '.', ',', ')']).to_ascii_lowercase();
    let rest = t.split_once("://").map(|(_, r)| r).unwrap_or(&t);
    let host = rest.split(['/', '?', '#', ':']).next()?.trim_start_matches("www.");
    let looks = host.contains('.')
        && !host.contains(char::is_whitespace)
        && host.len() <= 253
        && host.rsplit('.').next().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()));
    looks.then(|| host.to_string())
}

/// Registrable-ish part of a host: the last two labels (three for common
/// second-level suffixes like co.uk).
pub fn base_domain(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host.to_string();
    }
    let two = format!("{}.{}", labels[n - 2], labels[n - 1]);
    if ["co.uk", "com.au", "co.jp", "com.br", "com.es", "gouv.fr", "co.nz", "com.mx", "org.uk"].contains(&two.as_str())
    {
        format!("{}.{}", labels[n - 3], two)
    } else {
        two
    }
}

const SKIP: &[&str] = &[
    "script", "style", "head", "title", "noscript", "template", "svg", "math", "object", "embed", "iframe", "frame",
    "frameset", "canvas", "audio", "video", "map", "select", "option", "textarea", "meta", "link", "base", "input",
];

const BLOCK: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "aside",
    "nav",
    "main",
    "blockquote",
    "pre",
    "address",
    "center",
    "table",
    "tbody",
    "thead",
    "tfoot",
    "ul",
    "ol",
    "dl",
    "dt",
    "dd",
    "figure",
    "figcaption",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "form",
    "fieldset",
    "details",
    "summary",
];

/// Classes and ids hidden by `<style>` rules such as `.preheader{display:none}`.
#[derive(Default)]
struct HideRules {
    classes: HashSet<String>,
    ids: HashSet<String>,
}

fn compact(style: &str) -> String {
    style.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase()
}

fn num_after(s: &str, key: &str) -> Option<f64> {
    let i = s.find(key)? + key.len();
    let rest = &s[i..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn decl_value<'a>(s: &'a str, prop: &str) -> Option<&'a str> {
    // `s` is a compacted declaration list; match the property at a boundary.
    let mut from = 0;
    while let Some(pos) = s[from..].find(prop) {
        let at = from + pos;
        let boundary = at == 0 || matches!(s.as_bytes()[at - 1], b';' | b'{' | b'"' | b'\'');
        let after = at + prop.len();
        if boundary && s[after..].starts_with(':') {
            let rest = &s[after + 1..];
            let end = rest.find(';').unwrap_or(rest.len());
            return Some(rest[..end].trim_end_matches("!important"));
        }
        from = after;
    }
    None
}

fn is_whiteish(v: &str) -> bool {
    matches!(v, "#fff" | "#ffffff" | "white" | "rgb(255,255,255)" | "#fefefe" | "#fdfdfd")
}

/// Does this inline style (or style rule body) hide the element?
pub fn style_hides(style: &str) -> bool {
    let s = compact(style);
    if s.contains("display:none") || s.contains("visibility:hidden") || s.contains("visibility:collapse") {
        return true;
    }
    if s.contains("mso-hide:all") || s.contains("color:transparent") {
        return true;
    }
    if let Some(v) = decl_value(&s, "opacity").and_then(|v| v.parse::<f64>().ok())
        && v < 0.1
    {
        return true;
    }
    if let Some(v) = decl_value(&s, "font-size") {
        let n: f64 = v.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%').parse().unwrap_or(99.0);
        if n <= 1.0 {
            return true;
        }
    }
    let overflow_hidden = s.contains("overflow:hidden");
    for prop in ["max-height", "height", "width", "max-width"] {
        if let Some(v) = decl_value(&s, prop) {
            let n: f64 = v.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '%').parse().unwrap_or(99.0);
            if n <= 0.0 && overflow_hidden {
                return true;
            }
        }
    }
    if s.contains("clip:rect(0") || s.contains("clip-path:inset(50%") || s.contains("clip-path:inset(100%") {
        return true;
    }
    if let Some(n) = num_after(&s, "text-indent:")
        && n <= -999.0
    {
        return true;
    }
    if s.contains("position:absolute") || s.contains("position:fixed") {
        for k in ["left:", "top:"] {
            if let Some(n) = num_after(&s, k)
                && n <= -999.0
            {
                return true;
            }
        }
    }
    // Same colour for text and background.
    if let Some(fg) = decl_value(&s, "color") {
        let bg = decl_value(&s, "background-color").or_else(|| decl_value(&s, "background"));
        if let Some(bg) = bg
            && (fg == bg || (is_whiteish(fg) && is_whiteish(bg)))
        {
            return true;
        }
    }
    false
}

fn collect_rules(doc: &Html) -> HideRules {
    let mut rules = HideRules::default();
    for node in doc.tree.nodes() {
        let Node::Element(e) = node.value() else { continue };
        if !e.name().eq_ignore_ascii_case("style") {
            continue;
        }
        let css: String = node.children().filter_map(|c| c.value().as_text().map(|t| t.to_string())).collect();
        let css = strip_css_comments(&css);
        for block in css.split('}') {
            let Some((selectors, body)) = block.split_once('{') else { continue };
            // Skip at-rule headers like `@media screen and (...)`.
            let selectors = selectors.rsplit('{').next().unwrap_or(selectors);
            if selectors.trim_start().starts_with('@') || !style_hides(body) {
                continue;
            }
            for sel in selectors.split(',') {
                let last =
                    sel.split(|c: char| c.is_whitespace() || c == '>' || c == '+' || c == '~').rfind(|p| !p.is_empty());
                let Some(last) = last else { continue };
                let mut rest = last;
                while let Some(pos) = rest.find(['.', '#']) {
                    let kind = rest.as_bytes()[pos];
                    let tail = &rest[pos + 1..];
                    let end = tail.find(['.', '#', ':', '[']).unwrap_or(tail.len());
                    let name = tail[..end].to_string();
                    if !name.is_empty() {
                        if kind == b'.' {
                            rules.classes.insert(name);
                        } else {
                            rules.ids.insert(name);
                        }
                    }
                    rest = &tail[end..];
                }
            }
        }
    }
    rules
}

fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

struct Walker<'a> {
    out: String,
    hidden_chars: usize,
    hidden_sample: String,
    link_mismatches: Vec<String>,
    rules: &'a HideRules,
    depth: usize,
}

/// Real mail nests tables a few dozen levels deep at most. Deeper content
/// is treated as hidden: it keeps the recursive walk off the stack limit
/// and cannot be used to smuggle text past the hidden-text checks.
const MAX_DEPTH: usize = 320;

fn short_url(url: &str) -> String {
    if url.chars().count() <= 80 {
        return url.to_string();
    }
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_end];
    let path: String = rest[host_end..].chars().take(32).collect();
    format!("{scheme}://{host}{path}…")
}

impl Walker<'_> {
    fn element_hidden(&self, e: &scraper::node::Element) -> bool {
        if e.attr("hidden").is_some() {
            return true;
        }
        if e.attr("style").is_some_and(style_hides) {
            return true;
        }
        if e.classes().any(|c| self.rules.classes.contains(c)) {
            return true;
        }
        e.id().is_some_and(|id| self.rules.ids.contains(id))
    }

    fn count_hidden(&mut self, node: NodeRef<'_, Node>) {
        for d in node.descendants() {
            if let Node::Text(t) = d.value() {
                let txt = t.trim();
                self.hidden_chars += txt.chars().count();
                if self.hidden_sample.len() < 2000 && !txt.is_empty() {
                    self.hidden_sample.push_str(txt);
                    self.hidden_sample.push(' ');
                }
            }
        }
    }

    fn newline(&mut self) {
        if !self.out.ends_with('\n') && !self.out.is_empty() {
            self.out.push('\n');
        }
    }

    fn walk(&mut self, node: NodeRef<'_, Node>) {
        match node.value() {
            Node::Text(t) => {
                let s: &str = t;
                let mut prev_space = self.out.ends_with([' ', '\n']);
                for c in s.chars() {
                    if c.is_whitespace() {
                        if !prev_space {
                            self.out.push(' ');
                            prev_space = true;
                        }
                    } else {
                        self.out.push(c);
                        prev_space = false;
                    }
                }
            }
            Node::Element(e) => {
                // html5ever lower-cases element names of HTML documents.
                let name = e.name();
                if SKIP.contains(&name) {
                    if name == "style" || name == "script" || name == "head" || name == "title" {
                        return;
                    }
                    self.count_hidden(node);
                    return;
                }
                if self.element_hidden(e) {
                    self.count_hidden(node);
                    return;
                }
                match name {
                    "br" => {
                        self.out.push('\n');
                        return;
                    }
                    "hr" => {
                        self.newline();
                        self.out.push_str("----\n");
                        return;
                    }
                    "img" => return,
                    "li" => {
                        self.newline();
                        self.out.push_str("• ");
                    }
                    "tr" => self.newline(),
                    _ if BLOCK.contains(&name) => self.newline(),
                    _ => {}
                }
                if self.depth >= MAX_DEPTH {
                    self.count_hidden(node);
                    return;
                }
                let start = self.out.len();
                self.depth += 1;
                for child in node.children() {
                    self.walk(child);
                }
                self.depth -= 1;
                match name {
                    "a" => {
                        if let Some(href) = e.attr("href").map(str::trim) {
                            let visible = self.out[start..].trim().to_string();
                            if href.starts_with("http")
                                && self.link_mismatches.len() < 5
                                && let (Some(shown), Some(real)) = (host_of(&visible), host_of(href))
                                && base_domain(&shown) != base_domain(&real)
                            {
                                self.link_mismatches.push(format!("a link shows {shown} but leads to {real}"));
                            }
                            let show = if let Some(addr) = href.strip_prefix("mailto:") {
                                let addr = addr.split('?').next().unwrap_or(addr);
                                (!visible.contains(addr)).then(|| addr.to_string())
                            } else if href.starts_with("http://") || href.starts_with("https://") {
                                (!visible.contains(href)).then(|| short_url(href))
                            } else {
                                None
                            };
                            if let Some(u) = show {
                                if visible.is_empty() {
                                    self.out.push_str(&u);
                                } else {
                                    self.out.push_str(&format!(" ({u})"));
                                }
                            }
                        }
                    }
                    "td" | "th" => self.out.push(' '),
                    "tr" | "li" => self.newline(),
                    _ if BLOCK.contains(&name) => self.newline(),
                    _ => {}
                }
            }
            Node::Document | Node::Fragment => {
                for child in node.children() {
                    self.walk(child);
                }
            }
            _ => {}
        }
    }
}

/// Convert an HTML body to readable text, dropping hidden content.
/// Elements that never contain others.
const VOID: &[&[u8]] = &[
    b"br", b"img", b"hr", b"meta", b"link", b"input", b"col", b"area", b"base", b"wbr", b"source", b"track", b"embed",
    b"param", b"keygen",
];
/// Elements whose end tag is usually left out: a new one closes the last.
const IMPLIED_END: &[&[u8]] = &[b"p", b"li", b"td", b"th", b"tr", b"option", b"dt", b"dd"];
/// Nesting depth (as estimated by [`too_deep`]) above which the tree is not built.
const MAX_PARSE_DEPTH: usize = 2000;

/// Tree builders spend quadratic time on very deep nesting (each new element
/// scans the open-element stack), so a few hundred kilobytes of nested tags
/// can stall a parse for a minute. A linear scan estimates the depth first.
fn too_deep(html: &str, limit: usize) -> bool {
    // Linear: every byte is looked at a bounded number of times. End tags
    // only close an element of that name that is open, the self-closing
    // slash only counts inside svg/math (HTML ignores it), and elements with
    // implied end tags replace their open sibling instead of nesting.
    let b = html.as_bytes();
    let mut open: std::collections::HashMap<Vec<u8>, usize> = std::collections::HashMap::new();
    let (mut depth, mut foreign, mut i) = (0usize, 0usize, 0usize);
    while let Some(off) = b[i..].iter().position(|&c| c == b'<') {
        let at = i + off + 1;
        let closing = b.get(at) == Some(&b'/');
        let name_start = if closing { at + 1 } else { at };
        if !b.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            i = at;
            continue;
        }
        let name_end =
            b[name_start..].iter().position(|c| !c.is_ascii_alphanumeric()).map_or(b.len(), |k| name_start + k);
        let Some(gt) = b[name_end..].iter().position(|&c| c == b'>').map(|k| name_end + k) else {
            return false; // no complete tag after this point
        };
        i = gt + 1;
        let name = b[name_start..name_end].to_ascii_lowercase();
        if VOID.contains(&name.as_slice()) {
            continue;
        }
        let is_foreign = name == b"svg" || name == b"math";
        if closing {
            if let Some(n) = open.get_mut(&name)
                && *n > 0
            {
                *n -= 1;
                depth -= 1;
                foreign -= usize::from(is_foreign);
            }
            continue;
        }
        if foreign > 0 && b[gt - 1] == b'/' {
            continue;
        }
        let n = open.entry(name.clone()).or_insert(0);
        if IMPLIED_END.contains(&name.as_slice()) && *n > 0 {
            continue; // closes the previous one: same depth
        }
        *n += 1;
        depth += 1;
        foreign += usize::from(is_foreign);
        if depth > limit {
            return true;
        }
    }
    false
}

/// Plain text of hostile HTML without building a tree: drop tags and the
/// content of script/style/head, decode the common entities.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let mut missing = [false; 4]; // closers already searched for in vain
    let mut i = 0;
    while i < html.len() {
        let Some(off) = html[i..].find('<') else {
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..i + off]);
        let tag = i + off;
        let mut skip_to = None;
        for (k, t) in ["script", "style", "head", "title"].iter().enumerate() {
            if !missing[k] && lower[tag + 1..].starts_with(t) {
                match lower[tag..].find(&format!("</{t}")) {
                    Some(at) => skip_to = Some(tag + at),
                    None => missing[k] = true,
                }
                break;
            }
        }
        let from = skip_to.unwrap_or(tag);
        match html[from..].find('>') {
            Some(k) => {
                i = from + k + 1;
                out.push(' ');
            }
            None => break,
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

pub fn to_text(html: &str) -> HtmlText {
    if too_deep(html, MAX_PARSE_DEPTH) {
        // No real mail nests thousands of levels deep. Styles cannot be checked without
        // the tree, so the whole text is withheld like hidden text; a sample
        // is kept for the prompt-injection scan.
        let (clean, invisible) = strip_invisible(&strip_tags(html));
        let clean = tidy_whitespace(&clean);
        return HtmlText {
            text: String::new(),
            hidden_chars: clean.chars().filter(|c| !c.is_whitespace()).count(),
            invisible_chars: invisible,
            hidden_sample: clean.chars().take(2000).collect(),
            link_mismatches: vec![],
            degraded: true,
        };
    }
    let doc = Html::parse_document(html);
    let rules = collect_rules(&doc);
    let mut w = Walker {
        out: String::with_capacity(html.len() / 3),
        hidden_chars: 0,
        hidden_sample: String::new(),
        link_mismatches: vec![],
        rules: &rules,
        depth: 0,
    };
    w.walk(doc.tree.root());
    let (clean, invisible) = strip_invisible(&w.out);
    HtmlText {
        text: tidy_whitespace(&clean),
        hidden_chars: w.hidden_chars,
        invisible_chars: invisible,
        hidden_sample: w.hidden_sample,
        link_mismatches: w.link_mismatches,
        degraded: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extreme_nesting_skips_the_tree_builder() {
        let depth = 200_000;
        let html = format!("<p>top</p>{}secret &amp; more{}", "<div>".repeat(depth), "</div>".repeat(depth));
        let started = std::time::Instant::now();
        let t = to_text(&html);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "took {:?}", started.elapsed());
        assert!(t.degraded);
        assert!(t.text.is_empty());
        assert!(t.hidden_chars > 0 && t.hidden_sample.contains("secret & more"), "{}", t.hidden_sample);
        // Ordinary and sloppy mail is untouched by the guard.
        let normal = "<table><tr><td><p>a<p>b<br><img src=x><li>c</table>".repeat(500);
        assert!(!too_deep(&normal, MAX_PARSE_DEPTH));
        assert!(!too_deep(&"<font size=2>x".repeat(600), MAX_PARSE_DEPTH));
        assert!(!too_deep(&"<svg><path d='M0'/></svg>".repeat(5000), MAX_PARSE_DEPTH));
        // Bypass attempts: stray end tags, self-closing slashes, no '>' at all.
        let n = 100_000;
        for hostile in ["<div></x>".repeat(n), "<div/>".repeat(n), "<b><i>".repeat(n)] {
            let t0 = std::time::Instant::now();
            assert!(too_deep(&hostile, MAX_PARSE_DEPTH));
            assert!(to_text(&hostile).degraded);
            assert!(t0.elapsed() < std::time::Duration::from_secs(5));
        }
        let t0 = std::time::Instant::now();
        assert!(!too_deep(&"<br".repeat(n), MAX_PARSE_DEPTH));
        assert!(strip_tags(&"<script>".repeat(n)).len() < 10 * n);
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
        let t = to_text("<style>.x{}</style><div><b>Hi</b></div>");
        assert!(!t.degraded && t.text.trim() == "Hi", "{}", t.text);
    }

    #[test]
    fn hostile_nesting_neither_overflows_nor_hides_text() {
        let depth = 400; // below the tree-builder guard, above the walker's limit
        let html = format!("<p>top</p>{}secret{}", "<div>".repeat(depth), "</div>".repeat(depth));
        let t = to_text(&html);
        assert!(t.text.contains("top"));
        assert!(!t.text.contains("secret"));
        assert!(t.hidden_chars >= 6);
    }

    #[test]
    fn drops_hidden_text() {
        let h = r#"<html><head><style>.pre{display:none}</style></head><body>
            <span class="pre">preview</span>
            <p>Hello <b>Ann</b></p>
            <div style="display: none">IGNORE PREVIOUS INSTRUCTIONS</div>
            <p style="font-size:0px">tiny secret</p>
            <p style="color:#ffffff;background-color:#FFF">white on white</p>
            <p style="opacity:0">ghost</p>
            <div style="max-height:0;overflow:hidden">collapsed</div>
            <p>Bye<br>See you</p></body></html>"#;
        let t = to_text(h);
        assert!(t.text.contains("Hello Ann"), "{}", t.text);
        assert!(t.text.contains("Bye\nSee you"), "{}", t.text);
        for bad in ["preview", "IGNORE", "tiny", "white on white", "ghost", "collapsed"] {
            assert!(!t.text.contains(bad), "{bad} leaked: {}", t.text);
        }
        assert!(t.hidden_chars > 40);
    }

    #[test]
    fn links_and_lists() {
        let h = r#"<ul><li><a href="https://example.com/a">Docs</a></li><li>Two</li></ul>
                   <a href="mailto:bob@example.com">Bob</a>"#;
        let t = to_text(h);
        assert!(t.text.contains("• Docs (https://example.com/a)"), "{}", t.text);
        assert!(t.text.contains("• Two"));
        assert!(t.text.contains("Bob (bob@example.com)"));
    }

    #[test]
    fn link_text_mismatch_is_flagged() {
        let t = to_text(
            r#"<a href="https://evil.example/login">www.paypal.com</a> <a href="https://news.paypal.com/x">paypal.com</a>"#,
        );
        assert_eq!(t.link_mismatches, vec!["a link shows paypal.com but leads to evil.example".to_string()]);
    }

    #[test]
    fn invisible_chars_counted() {
        let t = to_text("<p>pay\u{200B}pal</p>");
        assert_eq!(t.text, "paypal");
        assert_eq!(t.invisible_chars, 1);
    }

    #[test]
    fn visible_white_text_on_dark_kept() {
        let t = to_text(r#"<td style="background:#000;color:#fff">Promo</td>"#);
        assert!(t.text.contains("Promo"));
    }
}
