//! A large synthetic mail store for benchmarks (debug builds only).
//!
//! The mix imitates a real mailbox: HTML newsletters with nested tables,
//! style sheets, hidden preheaders and tracking links; transactional HTML in
//! quoted-printable; plain replies with quoted history; base64 attachments;
//! some ISO-8859-1. Content is generated from a fixed seed, so runs compare.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::config::Config;
use crate::fixture::{ACCOUNT, T0};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
    /// Text with a realistic vocabulary: two thirds common words, one third
    /// drawn from 60,000 rarer words with a Zipf-like (log-uniform) rank.
    fn words(&mut self, n: usize) -> String {
        let lang = [FR, EN, ES][self.below(3)];
        let mut s = String::new();
        for i in 0..n {
            if i > 0 {
                s.push(' ');
            }
            if self.below(3) == 0 {
                let u = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
                let rank = ((u * (RARE as f64).ln()).exp() as usize).min(RARE) - 1;
                s.push_str(&rare_word(rank));
            } else {
                s.push_str(self.pick(lang));
            }
        }
        s
    }
    fn words_n(&mut self, base: usize, extra: usize) -> String {
        let n = base + self.below(extra);
        self.words(n)
    }
    fn token_n(&mut self, base: usize, extra: usize) -> String {
        let n = base + self.below(extra);
        self.token(n)
    }
    fn token(&mut self, len: usize) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        (0..len).map(|_| A[self.below(A.len())] as char).collect()
    }
}

const FR: &[&str] = &[
    "bonjour",
    "merci",
    "réunion",
    "facture",
    "projet",
    "équipe",
    "demain",
    "semaine",
    "rendez-vous",
    "contrat",
    "œuvre",
    "cœur",
    "délai",
    "à",
    "de",
    "la",
    "le",
    "et",
    "pour",
    "avec",
    "nous",
    "vous",
    "dossier",
    "rapport",
    "budget",
    "client",
    "commande",
    "livraison",
    "paiement",
    "été",
    "année",
    "prochaine",
    "réponse",
    "question",
    "document",
    "pièce",
    "jointe",
    "signé",
    "très",
    "bien",
    "cordialement",
    "voici",
    "suite",
    "notre",
    "échange",
];
const EN: &[&str] = &[
    "hello", "thanks", "meeting", "invoice", "project", "team", "tomorrow", "week", "schedule", "contract", "the",
    "and", "for", "with", "we", "you", "report", "budget", "customer", "order", "delivery", "payment", "update",
    "review", "deadline", "please", "find", "attached", "regards", "next", "quarter", "release", "feedback",
];
const ES: &[&str] = &[
    "hola",
    "gracias",
    "reunión",
    "factura",
    "proyecto",
    "equipo",
    "mañana",
    "semana",
    "contrato",
    "el",
    "la",
    "y",
    "para",
    "con",
    "nosotros",
    "usted",
    "informe",
    "presupuesto",
    "cliente",
    "pedido",
    "entrega",
    "pago",
    "año",
    "próximo",
    "respuesta",
    "documento",
    "adjunto",
    "saludos",
    "atentamente",
    "comité",
    "revisión",
];
const NAMES: &[&str] = &[
    "Marie Dupont",
    "Jean Martin",
    "Ana García",
    "John Smith",
    "Lucía Fernández",
    "Paul Durand",
    "Emma Wilson",
    "Carlos Ruiz",
    "Sophie Bernard",
    "Liam Brown",
    "Chloé Petit",
    "Diego López",
];
const DOMAINS: &[&str] = &[
    "example.com",
    "exemple.fr",
    "ejemplo.es",
    "news.example.org",
    "shop.example.net",
    "mail.example.io",
    "orange.fr",
    "free.fr",
    "gmail.com",
    "icloud.com",
];

const RARE: usize = 60_000;
const SYLLABLES: &[&str] = &[
    "ba", "ce", "di", "fo", "gu", "la", "me", "ni", "po", "ru", "sa", "te", "vi", "zo", "tra", "pre", "con", "mar",
    "ber", "lin", "sto", "qui", "ran", "dé", "ré", "ción", "ment", "ing", "er", "ol", "ix", "an",
];

/// The rank-th rare word: a deterministic syllable sequence (mixed-radix digits).
fn rare_word(rank: usize) -> String {
    let mut w = String::new();
    let mut r = rank + SYLLABLES.len();
    while r > 0 {
        w.push_str(SYLLABLES[r % SYLLABLES.len()]);
        r /= SYLLABLES.len();
    }
    w
}

fn qp(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 11 / 10);
    let mut col = 0;
    for &b in text.as_bytes() {
        let piece = if b == b'\n' {
            out.push_str("\r\n");
            col = 0;
            continue;
        } else if (33..=126).contains(&b) && b != b'=' || b == b' ' {
            (b as char).to_string()
        } else {
            format!("={b:02X}")
        };
        if col + piece.len() > 75 {
            out.push_str("=\r\n");
            col = 0;
        }
        col += piece.len();
        out.push_str(&piece);
    }
    out
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + data.len() / 57 * 2 + 4);
    for (i, c) in data.chunks(3).enumerate() {
        if i > 0 && i % 19 == 0 {
            out.push_str("\r\n");
        }
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' }).collect()
}

fn newsletter(r: &mut Rng) -> String {
    let mut h = String::with_capacity(60_000);
    h.push_str("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>");
    for i in 0..40 {
        let _ =
            write!(h, ".c{i}{{font-family:Arial,sans-serif;color:#{:06x};padding:{}px}} ", r.next() & 0xFFFFFF, i % 9);
    }
    h.push_str(".pre{display:none}</style></head><body>");
    let _ = write!(h, "<span class=\"pre\" style=\"display:none;max-height:0\">{}</span>", r.words(25));
    let depth = 4 + r.below(8);
    for _ in 0..depth {
        h.push_str("<table width=\"100%\" cellpadding=\"0\" cellspacing=\"0\"><tr><td align=\"center\">");
    }
    for _ in 0..(15 + r.below(40)) {
        let url = format!(
            "https://click.{}/ls/click?upn={}&utm_source=newsletter&utm_campaign={}",
            r.pick(DOMAINS),
            r.token_n(120, 200),
            r.token(12)
        );
        let _ = write!(
            h,
            "<tr><td class=\"c{}\"><h2>{}</h2><p>{}</p><a href=\"{url}\">{}</a><img src=\"https://img.{}/{}.png\" width=\"600\"></td></tr>",
            r.below(40),
            r.words(5),
            r.words_n(30, 60),
            r.words(3),
            r.pick(DOMAINS),
            r.token(40)
        );
    }
    for _ in 0..depth {
        h.push_str("</td></tr></table>");
    }
    let _ = write!(
        h,
        "<p style=\"font-size:11px\">{} <a href=\"https://{}/unsubscribe?u={}\">unsubscribe</a></p></body></html>",
        r.words(20),
        r.pick(DOMAINS),
        r.token(64)
    );
    h
}

fn receipt(r: &mut Rng) -> String {
    let mut h = String::from("<html><body><table>");
    for _ in 0..(5 + r.below(20)) {
        let _ = write!(h, "<tr><td>{}</td><td>{},{:02} €</td></tr>", r.words(4), r.below(500), r.below(100));
    }
    let _ = write!(h, "</table><p>{}</p></body></html>", r.words(40));
    h
}

fn reply(r: &mut Rng) -> String {
    let mut t = String::new();
    for _ in 0..(2 + r.below(10)) {
        t.push_str(&r.words_n(8, 15));
        t.push('\n');
    }
    let _ = writeln!(
        t,
        "\nLe 3 oct. 2026 à 10:12, {} <{}@{}> a écrit :\n",
        r.pick(NAMES),
        r.token(8).to_lowercase(),
        r.pick(DOMAINS)
    );
    for _ in 0..(5 + r.below(40)) {
        let _ = writeln!(t, "> {}", r.words(10));
    }
    t
}

struct Synth {
    mailbox: usize,
    from: i64,
    to: i64,
    subject: String,
    date: i64,
    read: bool,
    attach: bool,
    raw: Vec<u8>,
}

fn synth(r: &mut Rng, id: i64, people: &[(String, String)]) -> Synth {
    let sender = r.below(people.len());
    let (from_name, from_addr) = &people[sender];
    let to = 1 + r.below(people.len()) as i64;
    let subject = r.words_n(3, 6);
    let date = T0 - (r.below(3650 * 86_400)) as i64;
    let head = |ct: &str| {
        format!(
            "From: {from_name} <{from_addr}>\r\nTo: moi@icloud.com\r\nSubject: {subject}\r\nDate: Mon, 5 Oct 2026 10:00:00 +0200\r\nMessage-ID: <{id}.synth@example.com>\r\nAuthentication-Results: mx.icloud.com; spf=pass smtp.mailfrom={from_addr}; dkim=pass; dmarc=pass\r\nMIME-Version: 1.0\r\n{ct}"
        )
    };
    let kind = r.below(100);
    let mut attach = false;
    let raw: Vec<u8> = if kind < 35 {
        if r.below(10) == 0 {
            let mut v = head("Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: 8bit\r\n\r\n")
                .into_bytes();
            v.extend(latin1(&reply(r)));
            v
        } else {
            format!(
                "{}{}",
                head("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n"),
                reply(r)
            )
            .into_bytes()
        }
    } else if kind < 75 {
        let html = newsletter(r);
        if r.below(10) < 7 {
            format!(
                "{}--alt\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{}\r\n--alt\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{}\r\n--alt--\r\n",
                head("Content-Type: multipart/alternative; boundary=alt\r\n\r\n"),
                qp(&r.words(200)),
                qp(&html)
            )
            .into_bytes()
        } else {
            format!(
                "{}{}",
                head("Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n"),
                qp(&html)
            )
            .into_bytes()
        }
    } else if kind < 90 {
        format!(
            "{}{}",
            head("Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n"),
            qp(&receipt(r))
        )
        .into_bytes()
    } else {
        attach = true;
        let blob: Vec<u8> = (0..(30_000 + r.below(270_000))).map(|_| r.next() as u8).collect();
        format!(
            "{}--mix\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n--mix\r\nContent-Type: application/pdf; name=\"{}.pdf\"\r\nContent-Disposition: attachment; filename=\"{}.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{}\r\n--mix--\r\n",
            head("Content-Type: multipart/mixed; boundary=mix\r\n\r\n"),
            r.words(40),
            r.words(2).replace(' ', "_"),
            r.words(2).replace(' ', "_"),
            b64(&blob)
        )
        .into_bytes()
    };
    let mailbox = match r.below(100) {
        0..50 => 0,
        50..85 => 1,
        85..95 => 4,
        _ => 2,
    };
    Synth { mailbox, from: sender as i64, to, subject, date, read: r.below(10) > 1, attach, raw }
}

/// Build the fixture store, then add `count` synthetic messages to it.
pub fn build(dir: &Path, count: usize) -> Result<Config> {
    let cfg = crate::fixture::build(dir)?;
    let v10 = cfg.mail_root.join("V10");
    let mut conn = Connection::open(v10.join("MailData/Envelope Index"))?;
    let tx = conn.transaction()?;
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut people = vec![];
    let mut first_addr = 0;
    for i in 0..400 {
        let name = NAMES[i % NAMES.len()].to_string();
        let addr =
            format!("{}{}@{}", name.split(' ').next().unwrap_or("x").to_lowercase(), i, DOMAINS[i % DOMAINS.len()]);
        tx.execute("INSERT INTO addresses(address, comment) VALUES(?1, ?2)", params![addr, name])?;
        if i == 0 {
            first_addr = tx.last_insert_rowid();
        }
        people.push((name, addr));
    }
    let mailboxes: Vec<i64> = {
        let mut st = tx.prepare("SELECT ROWID FROM mailboxes ORDER BY ROWID")?;
        st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    let names = ["INBOX", "Archive", "Junk", "Deleted Messages", "Sent Messages"];
    let mut subjects: HashMap<String, i64> = HashMap::new();
    let mut bytes = 0usize;
    for n in 0..count {
        let id = 10_000 + n as i64;
        let m = synth(&mut r, id, &people);
        let subj = match subjects.get(&m.subject) {
            Some(s) => *s,
            None => {
                tx.execute("INSERT INTO subjects(subject) VALUES(?1)", [&m.subject])?;
                let s = tx.last_insert_rowid();
                subjects.insert(m.subject.clone(), s);
                s
            }
        };
        let flags = if m.attach { 1i64 << 10 } else { 0 } | m.read as i64;
        tx.execute(
            "INSERT INTO messages(ROWID, sender, subject_prefix, subject, date_sent, date_received, mailbox, flags, read, flagged, deleted, size, conversation_id)
             VALUES(?1, ?2, '', ?3, ?4, ?4, ?5, ?6, ?7, 0, 0, ?8, ?1)",
            params![id, first_addr + m.from, subj, m.date, mailboxes[m.mailbox], flags, m.read as i64, m.raw.len() as i64],
        )?;
        tx.execute(
            "INSERT INTO recipients(message, address, type, position) VALUES(?1, ?2, 0, 0)",
            params![id, first_addr + m.to - 1],
        )?;
        let data = v10
            .join(ACCOUNT)
            .join(format!("{}.mbox", names[m.mailbox]))
            .join("0F0E0D0C-2222-4B3A-8C9D-1E2F3A4B5C6D/Data")
            .join(crate::emlx::data_subdirs(id))
            .join("Messages");
        std::fs::create_dir_all(&data)?;
        let mut file = format!("{}\n", m.raw.len()).into_bytes();
        file.extend_from_slice(&m.raw);
        file.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>flags</key><integer>0</integer></dict></plist>\n");
        bytes += file.len();
        std::fs::write(data.join(format!("{id}.emlx")), file)?;
    }
    tx.commit()?;
    eprintln!("{count} synthetic messages, {} MB of .emlx", bytes >> 20);
    Ok(cfg)
}
