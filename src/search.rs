//! Search: full-text over Timbre's index joined with Mail's live metadata,
//! or metadata-only while the index is being built.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, types::Value};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::index::Progress;
use crate::store::{Meta, Store};
use crate::util::{clip, iso_local, now_unix, parse_when};

#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Words to find in subject, people, body and attachment names. Accent-insensitive.
    /// Supports "exact phrase", prefix*, a OR b, -exclude, and operators
    /// from: to: subject: in: is:unread is:flagged has:attachment after:YYYY-MM-DD before:YYYY-MM-DD.
    pub query: Option<String>,
    /// Sender name or address contains this.
    pub from: Option<String>,
    /// A To/Cc recipient name or address contains this.
    pub to: Option<String>,
    /// Subject contains this.
    pub subject: Option<String>,
    /// Mailbox name or path ("Archive/2024"), or a role: inbox, sent, drafts, archive, junk, trash.
    pub mailbox: Option<String>,
    /// Account name (as in list_mailboxes), e.g. "iCloud".
    pub account: Option<String>,
    /// On or after this date (YYYY-MM-DD or ISO 8601).
    pub after: Option<String>,
    /// Before this date (YYYY-MM-DD or ISO 8601).
    pub before: Option<String>,
    pub unread: Option<bool>,
    pub flagged: Option<bool>,
    pub has_attachments: Option<bool>,
    /// Junk and Trash are skipped unless a mailbox is named or this is true.
    pub include_junk_and_trash: Option<bool>,
    /// "relevance" (default with a query) or "newest" / "oldest".
    pub sort: Option<String>,
    /// Results per page, 1-100 (default 20).
    pub limit: Option<u32>,
    /// Skip this many results (pagination).
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Hit {
    pub id: i64,
    pub date: String,
    pub from: String,
    pub subject: String,
    pub mailbox: String,
    pub account: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub flagged: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub attachments: i64,
    /// Matching excerpt, matches marked «like this».
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Sender authentication problem, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResult {
    pub total: u64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub total_is_lower_bound: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u32>,
    /// "fulltext" or "metadata" (subjects and senders only).
    pub engine: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    pub took_ms: u64,
    pub results: Vec<Hit>,
}

const COUNT_CAP: u64 = 1_000;
/// Broad queries are ranked over this many of their most recent matches.
const RANK_WINDOW: usize = 10_000;

/// Gmail-style operators pulled out of a free-text query.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    pub text: Vec<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub mailbox: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub unread: Option<bool>,
    pub flagged: Option<bool>,
    pub has_attachments: Option<bool>,
}

/// Split a query into terms, keeping "quoted phrases" whole.
fn split_terms(q: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut in_quote = false;
    for c in q.chars() {
        match c {
            '"' | '“' | '”' | '«' | '»' => {
                if in_quote {
                    cur.push('"');
                    out.push(std::mem::take(&mut cur));
                    in_quote = false;
                } else {
                    if !cur.trim().is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    cur.push('"');
                    in_quote = true;
                }
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        if in_quote {
            cur.push('"');
        }
        out.push(cur);
    }
    out
}

pub fn parse_query(q: &str) -> Parsed {
    let mut p = Parsed::default();
    for term in split_terms(q) {
        let lower = term.to_lowercase();
        let value = |prefix: &str| term[prefix.len()..].trim_matches('"').to_string();
        match lower.as_str() {
            "is:unread" => p.unread = Some(true),
            "is:read" => p.unread = Some(false),
            "is:flagged" | "is:starred" => p.flagged = Some(true),
            "has:attachment" | "has:attachments" => p.has_attachments = Some(true),
            _ if lower.starts_with("from:") && term.len() > 5 => p.from = Some(value("from:")),
            _ if lower.starts_with("to:") && term.len() > 3 => p.to = Some(value("to:")),
            _ if lower.starts_with("subject:") && term.len() > 8 => p.subject = Some(value("subject:")),
            _ if lower.starts_with("in:") && term.len() > 3 => p.mailbox = Some(value("in:")),
            _ if lower.starts_with("after:") && term.len() > 6 => p.after = Some(value("after:")),
            _ if lower.starts_with("before:") && term.len() > 7 => p.before = Some(value("before:")),
            _ => p.text.push(term),
        }
    }
    p
}

fn quote_fts(term: &str) -> Option<String> {
    let t = crate::norm::fold(term.trim_matches('"'));
    let cleaned: String = t.chars().filter(|c| !c.is_control()).collect();
    if !cleaned.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }
    Some(format!("\"{}\"", cleaned.replace('"', "\"\"")))
}

/// Build an FTS5 MATCH expression from free-text terms. Every term is
/// quoted, so user input can never be read as FTS5 syntax.
pub fn build_fts(terms: &[String]) -> Option<String> {
    let mut groups: Vec<Vec<String>> = vec![];
    let mut negatives: Vec<String> = vec![];
    let mut join_or = false;
    for raw in terms {
        if raw == "OR" || raw == "|" {
            join_or = true;
            continue;
        }
        let (neg, body) = match raw.strip_prefix('-') {
            Some(rest) if !rest.is_empty() => (true, rest),
            _ => (false, raw.as_str()),
        };
        let phrase = body.starts_with('"');
        // There is no prefix index (it doubles the index size): short
        // prefixes would scan a large part of the term list.
        let prefix = !phrase && body.ends_with('*') && body.trim_end_matches('*').chars().count() >= 3;
        let core = body.trim_end_matches('*');
        let Some(mut q) = quote_fts(core) else { continue };
        if prefix {
            q.push('*');
        }
        if neg {
            negatives.push(q);
            join_or = false;
            continue;
        }
        match (join_or, groups.last_mut()) {
            (true, Some(g)) => g.push(q),
            _ => groups.push(vec![q]),
        }
        join_or = false;
    }
    if groups.is_empty() {
        return None;
    }
    let pos = groups
        .iter()
        .map(|g| if g.len() == 1 { g[0].clone() } else { format!("({})", g.join(" OR ")) })
        .collect::<Vec<_>>()
        .join(" AND ");
    Some(if negatives.is_empty() { pos } else { format!("({pos}) NOT ({})", negatives.join(" OR ")) })
}

fn like(term: &str) -> String {
    let esc = term.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    format!("%{esc}%")
}

fn auth_warning(auth: Option<&str>) -> Option<String> {
    let a = auth?;
    let dmarc_fail = a.contains("dmarc=fail");
    let spf_fail = a.contains("spf=fail") || a.contains("spf=softfail");
    let dkim_pass = a.contains("dkim=pass");
    (dmarc_fail || (spf_fail && !dkim_pass)).then(|| format!("sender failed authentication ({a})"))
}

struct Filters {
    sql: Vec<String>,
    args: Vec<Value>,
}

impl Filters {
    fn push(&mut self, clause: String, args: impl IntoIterator<Item = Value>) {
        self.sql.push(clause);
        self.args.extend(args);
    }
}

pub fn search(
    conn: &Connection,
    store: &Store,
    cfg: &Config,
    progress: &Progress,
    req: &SearchParams,
) -> Result<SearchResult, String> {
    let started = std::time::Instant::now();
    let parsed = parse_query(req.query.as_deref().unwrap_or(""));
    let pick = |a: &Option<String>, b: Option<String>| a.clone().filter(|s| !s.trim().is_empty()).or(b);
    let from = pick(&req.from, parsed.from.clone());
    let to = pick(&req.to, parsed.to.clone());
    let subject = pick(&req.subject, parsed.subject.clone());
    let mailbox = pick(&req.mailbox, parsed.mailbox.clone());
    let after = pick(&req.after, parsed.after.clone());
    let before = pick(&req.before, parsed.before.clone());
    let unread = req.unread.or(parsed.unread);
    let flagged = req.flagged.or(parsed.flagged);
    let has_attachments = req.has_attachments.or(parsed.has_attachments);
    let limit = req.limit.unwrap_or(20).clamp(1, 100);
    let offset = req.offset.unwrap_or(0).min(50_000);
    let fts = build_fts(&parsed.text);
    let s = &store.schema;

    let mut f = Filters { sql: vec![s.not_deleted.clone()], args: vec![] };
    if let Some(v) = &from {
        f.push(
            "(lower(COALESCE(a.address,'')) LIKE ? ESCAPE '\\' OR lower(COALESCE(a.comment,'')) LIKE ? ESCAPE '\\')"
                .into(),
            [Value::Text(like(v)), Value::Text(like(v))],
        );
    }
    if let Some(v) = &to
        && let Some(col) = &s.rec_msg_col
    {
        let typ = s.rec_type_col.as_deref().map(|t| format!("AND r.{t} IN (0, 1) ")).unwrap_or_default();
        f.push(
            format!(
                "EXISTS (SELECT 1 FROM mail.recipients r LEFT JOIN mail.addresses ra ON ra.ROWID = r.address \
                     WHERE r.{col} = m.ROWID {typ}AND (lower(COALESCE(ra.address,'')) LIKE ? ESCAPE '\\' \
                     OR lower(COALESCE(ra.comment,'')) LIKE ? ESCAPE '\\'))"
            ),
            [Value::Text(like(v)), Value::Text(like(v))],
        );
    }
    if let Some(v) = &subject {
        f.push(
            format!("lower({} || COALESCE(s.subject,'')) LIKE ? ESCAPE '\\'", s.subject_prefix),
            [Value::Text(like(v))],
        );
    }
    let mut named_mailbox = false;
    if let Some(v) = &mailbox {
        let ids = store.resolve_mailboxes(v, req.account.as_deref());
        if ids.is_empty() {
            return Err(format!(
                "No mailbox matches '{v}'. Call list_mailboxes to see names, or use a role: inbox, sent, drafts, archive, junk, trash."
            ));
        }
        named_mailbox = true;
        f.sql.push(format!("m.mailbox IN ({})", ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",")));
    } else if let Some(acct) = req.account.as_deref().filter(|a| !a.trim().is_empty()) {
        let a = acct.trim().to_lowercase();
        let ids: Vec<String> = store
            .mailboxes()
            .values()
            .filter(|m| m.account.to_lowercase().contains(&a))
            .map(|m| m.id.to_string())
            .collect();
        if ids.is_empty() {
            return Err(format!("No account matches '{acct}'. Accounts: {}", store.account_names().join(", ")));
        }
        f.sql.push(format!("m.mailbox IN ({})", ids.join(",")));
    }
    if !named_mailbox && !req.include_junk_and_trash.unwrap_or(false) {
        let skip = store.mailboxes_with_roles(&cfg.excluded_roles);
        if !skip.is_empty() {
            f.sql.push(format!("m.mailbox NOT IN ({})", skip.iter().map(i64::to_string).collect::<Vec<_>>().join(",")));
        }
    }
    for (val, op, name) in [(&after, ">=", "after"), (&before, "<", "before")] {
        if let Some(v) = val {
            let ts = parse_when(v).ok_or_else(|| format!("Could not read the '{name}' date '{v}'. Use YYYY-MM-DD."))?;
            f.push(format!("m.date_received {op} ?"), [Value::Integer(store.to_db(ts))]);
        }
    }
    if let Some(u) = unread {
        f.sql.push(if u { s.unread.clone() } else { format!("NOT {}", s.unread) });
    }
    if let Some(fl) = flagged {
        f.sql.push(if fl { s.flagged.clone() } else { format!("NOT {}", s.flagged) });
    }
    if let Some(h) = has_attachments {
        f.sql.push(format!("{} {} 0", s.attach_count, if h { ">" } else { "=" }));
    }

    let use_fts = fts.is_some() && progress.ready();
    let mut notice = None;
    let sort = req.sort.as_deref().unwrap_or(if use_fts { "relevance" } else { "newest" }).to_lowercase();
    let now_db = store.to_db(now_unix());

    let (from_sql, mut select_args, order) = if use_fts {
        let order = match sort.as_str() {
            "newest" => "m.date_received DESC".to_string(),
            "oldest" => "m.date_received ASC".to_string(),
            // bm25 is negative (lower is better); recent messages get up to 1.5x.
            _ => "bm25(fts, 4.0, 2.5, 1.0, 2.0) * (1.0 + 0.75 / (1.0 + MAX(0, ? - m.date_received) / 3888000.0)) ASC"
                .to_string(),
        };
        let args = vec![Value::Text(fts.clone().unwrap_or_default())];
        ("fts JOIN mail.messages m ON m.ROWID = fts.rowid".to_string(), args, order)
    } else {
        if fts.is_some() || !parsed.text.is_empty() {
            // No index yet: fall back to subject and sender matching.
            let words: Vec<String> = parsed
                .text
                .iter()
                .map(|t| t.trim_matches(['"', '*', '-']).to_string())
                .filter(|t| !t.is_empty())
                .collect();
            for w in &words {
                f.push(
                    format!(
                        "(lower({} || COALESCE(s.subject,'')) LIKE ? ESCAPE '\\' OR lower(COALESCE(a.comment,'') || ' ' || COALESCE(a.address,'')) LIKE ? ESCAPE '\\')",
                        s.subject_prefix
                    ),
                    [Value::Text(like(w)), Value::Text(like(w))],
                );
            }
            let v = progress.view();
            notice = Some(match v.percent {
                Some(p) => {
                    format!("Full-text index is being built ({p}%). Searching subjects and senders only for now.")
                }
                None => format!("Full-text index is {}. Searching subjects and senders only.", v.state),
            });
        }
        let order = if sort == "oldest" { "m.date_received ASC" } else { "m.date_received DESC" }.to_string();
        ("mail.messages m".to_string(), vec![], order)
    };

    let where_sql = f.sql.join(" AND ");
    let fts_expr = fts.clone().unwrap_or_default();
    let fts_clause = if use_fts { "fts MATCH ? AND " } else { "" };
    // Subjects and senders are joined only when a filter needs them.
    let joins = if !use_fts || from.is_some() || subject.is_some() { Store::META_JOINS } else { "" };
    let err = |e: rusqlite::Error| format!("search failed: {e}");

    // Total (capped: an exact count of a broad query costs more than the page).
    let count_sql = format!(
        "SELECT count(*) FROM (SELECT 1 FROM {from_sql} {joins} WHERE {fts_clause}{where_sql} LIMIT {cap})",
        cap = COUNT_CAP + 1
    );
    let mut count_args = select_args.clone();
    count_args.extend(f.args.iter().cloned());
    let total: u64 = conn
        .query_row(&count_sql, rusqlite::params_from_iter(count_args.iter()), |r| r.get::<_, i64>(0))
        .map_err(err)? as u64;

    // Very broad queries are ranked over their most recent matches only:
    // bm25 costs a little for every match, and recency weighs in anyway.
    let mut window = String::new();
    if use_fts && order.contains("bm25") && (offset + limit) as usize <= RANK_WINDOW / 2 {
        let floor: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM fts WHERE fts MATCH ?1 ORDER BY rowid DESC LIMIT 1 OFFSET ?2",
                rusqlite::params![fts_expr, RANK_WINDOW as i64 - 1],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        if let Some(floor) = floor {
            window = format!("fts.rowid >= {floor} AND ");
        }
    }
    select_args.extend(f.args.iter().cloned());
    if use_fts && order.contains('?') {
        select_args.push(Value::Integer(now_db));
    }
    select_args.push(Value::Integer(limit as i64));
    select_args.push(Value::Integer(offset as i64));
    // Rank ids only. Metadata and snippets are read for the returned page
    // alone: computing snippets for every match made searches 2.5x slower.
    let rank = |window: &str| -> Result<Vec<i64>, String> {
        let sql = format!(
            "SELECT m.ROWID FROM {from_sql} {joins} WHERE {fts_clause}{window}{where_sql} ORDER BY {order} LIMIT ? OFFSET ?"
        );
        let mut stmt = conn.prepare(&sql).map_err(err)?;
        let ids = stmt.query_map(rusqlite::params_from_iter(select_args.iter()), |r| r.get(0)).map_err(err)?;
        ids.collect::<rusqlite::Result<Vec<i64>>>().map_err(err)
    };
    let mut ids = rank(&window)?;
    if !window.is_empty() && (ids.len() as u32) < limit && total > offset as u64 + ids.len() as u64 {
        // Filters removed most of the window: rank every match instead.
        ids = rank("")?;
    }

    let mut metas: HashMap<i64, Meta> = store
        .get_metas(conn, &ids)
        .map_err(|e| format!("search failed: {e}"))?
        .into_iter()
        .map(|m| (m.id, m))
        .collect();
    let mut snippet_stmt = conn
        .prepare_cached("SELECT snippet(fts, -1, '«', '»', '…', 14) FROM fts WHERE fts MATCH ?1 AND rowid = ?2")
        .map_err(err)?;
    let mut auth_stmt = conn.prepare_cached("SELECT auth FROM docs WHERE id = ?1").map_err(err)?;
    let mailboxes = store.mailboxes();
    let mut results = vec![];
    for id in ids {
        let Some(m) = metas.remove(&id) else { continue };
        let snippet: Option<String> = if use_fts {
            snippet_stmt.query_row(rusqlite::params![fts_expr, id], |r| r.get(0)).optional().map_err(err)?.flatten()
        } else {
            None
        };
        let auth: Option<String> = auth_stmt.query_row([id], |r| r.get(0)).optional().map_err(err)?.flatten();
        let mb = mailboxes.get(&m.mailbox);
        results.push(Hit {
            id: m.id,
            date: iso_local(m.date).unwrap_or_default(),
            from: m.from,
            subject: m.subject,
            mailbox: mb.map(|x| x.display_path()).unwrap_or_default(),
            account: mb.map(|x| x.account.clone()).unwrap_or_default(),
            unread: m.unread,
            flagged: m.flagged,
            attachments: m.attachments,
            snippet: snippet.filter(|s| !s.trim().is_empty()).map(|s| clip(s.trim(), 280).0),
            warning: auth_warning(auth.as_deref()),
        });
    }
    let shown = offset as u64 + results.len() as u64;
    Ok(SearchResult {
        total: total.min(COUNT_CAP),
        total_is_lower_bound: total > COUNT_CAP,
        next_offset: (shown < total).then_some(offset + results.len() as u32),
        engine: if use_fts { "fulltext" } else { "metadata" }.to_string(),
        notice,
        took_ms: started.elapsed().as_millis() as u64,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_terms_are_folded_and_short_prefixes_ignored() {
        let q = |t: &[&str]| build_fts(&t.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(q(&["cœur"]).as_deref(), Some("\"coeur\""));
        assert_eq!(q(&["fac*"]).as_deref(), Some("\"fac\"*"));
        assert_eq!(q(&["fa*"]).as_deref(), Some("\"fa\""));
    }

    #[test]
    fn operators_are_extracted() {
        let p = parse_query("facture from:orange is:unread after:2026-01-01 \"mise en demeure\" in:inbox");
        assert_eq!(p.from.as_deref(), Some("orange"));
        assert_eq!(p.unread, Some(true));
        assert_eq!(p.after.as_deref(), Some("2026-01-01"));
        assert_eq!(p.mailbox.as_deref(), Some("inbox"));
        assert_eq!(p.text, vec!["facture".to_string(), "\"mise en demeure\"".to_string()]);
    }

    #[test]
    fn fts_expression_is_safe() {
        let t = |q: &str| build_fts(&parse_query(q).text);
        assert_eq!(t("facture"), Some("\"facture\"".into()));
        assert_eq!(t("fact*"), Some("\"fact\"*".into()));
        assert_eq!(t("a OR b c"), Some("(\"a\" OR \"b\") AND \"c\"".into()));
        assert_eq!(t("rapport -brouillon"), Some("(\"rapport\") NOT (\"brouillon\")".into()));
        assert_eq!(t("\"mise en demeure\""), Some("\"mise en demeure\"".into()));
        // FTS5 syntax characters are neutralized inside quotes.
        assert_eq!(t("NEAR(a b) col:x"), Some("\"NEAR(a\" AND \"b)\" AND \"col:x\"".into()));
        assert_eq!(t("-only"), None);
        assert_eq!(t("\"\"\" ***"), None);
    }
}
