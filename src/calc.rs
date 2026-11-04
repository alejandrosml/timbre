//! CSS math functions — `calc()`, `min()`, `clamp()`, `round()`, `mod()`,
//! `sin()`, `pow()`, `hypot()`, `abs()`… — worked out with their types
//! checked, as WebKit does: a formula WebKit rejects (`calc(x)`, a length
//! times a length, `opacity: calc(1px)`) is rejected here too, so the
//! declaration before it stays in force as it does in Mail.
//!
//! Values come lower-cased and with comments removed (see
//! [`crate::css`]); they are read again with cssparser's tokenizer.

use cssparser::{ParseError, Parser, Token};

/// What a value measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Number,
    /// CSS pixels.
    Length,
    /// Degrees.
    Angle,
    /// Seconds.
    Time,
    /// Hertz.
    Frequency,
    /// Dots per CSS pixel.
    Resolution,
    /// A percentage left as such (see [`Pct::Keep`]).
    Percent,
}

/// A worked-out value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Val {
    pub n: f64,
    pub kind: Kind,
}

/// What a percentage stands for where the value is used.
#[derive(Clone, Copy, Debug)]
pub enum Pct {
    /// A share of this many pixels.
    Length(f64),
    /// A number: 50% is 0.5.
    Number,
    /// Left as a percentage.
    Keep,
    /// Not allowed.
    None,
}

/// Values for words a formula may hold (`r`, `g`, `b` in relative colours).
pub type Words<'a> = &'a dyn Fn(&str) -> Option<f64>;

/// How a value is read.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub pct: Pct,
    /// The font size `em` counts from.
    pub font: f64,
    /// Words standing for numbers, as in relative colours (`r`, `alpha`).
    pub words: Option<Words<'a>>,
}

impl Default for Ctx<'_> {
    fn default() -> Self {
        Ctx { pct: Pct::Length(16.0), font: 16.0, words: None }
    }
}

/// Nesting followed, as WebKit's limit.
const MAX_DEPTH: usize = 100;
/// Arguments of one function, and terms of one sum.
const MAX_TERMS: usize = 256;

type R<T> = Result<T, ParseError<()>>;

fn fail<T>() -> R<T> {
    Err(ParseError::custom(()))
}

/// The math functions WebKit knows.
pub const FUNCTIONS: &[&str] = &[
    "calc",
    "-webkit-calc",
    "min",
    "max",
    "clamp",
    "round",
    "mod",
    "rem",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "pow",
    "sqrt",
    "hypot",
    "log",
    "exp",
    "abs",
    "sign",
    "random",
];

/// Is `name` a math function?
pub fn is_function(name: &str) -> bool {
    FUNCTIONS.contains(&name)
}

/// The value of a math function (`calc(…)`, `min(…)`…), a number, a
/// dimension or a percentage; `None` if WebKit would reject it. NaN counts
/// as 0 and infinities as the largest values, as CSS has them.
pub fn eval(text: &str, ctx: Ctx<'_>) -> Option<Val> {
    let mut input = Parser::new(text);
    let v = input
        .parse_entirely(|p| {
            p.skip_whitespace();
            let v = top(p, ctx)?;
            p.skip_whitespace();
            Ok::<_, ParseError<()>>(v)
        })
        .ok()?;
    let n = if v.n.is_nan() { 0.0 } else { v.n.clamp(-3.4e38, 3.4e38) };
    Some(Val { n, kind: v.kind })
}

/// A whole value: one math function or one plain number, dimension or
/// percentage.
fn top<'i>(p: &mut Parser<'i>, ctx: Ctx<'_>) -> R<Val> {
    match p.next()?.clone() {
        Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            if !is_function(&name) {
                return fail();
            }
            p.parse_nested_block(|p| function(&name, p, ctx, 1))
        }
        // A channel of a relative colour (`r`, `alpha`), alone.
        Token::Ident(word) if ctx.words.is_some() => match ctx.words.and_then(|f| f(&word.to_ascii_lowercase())) {
            Some(n) => Ok(Val { n, kind: Kind::Number }),
            None => fail(),
        },
        t => leaf(&t, ctx),
    }
}

/// A number, dimension or percentage token.
fn leaf(t: &Token<'_>, ctx: Ctx<'_>) -> R<Val> {
    match t {
        Token::Number { value, .. } => Ok(Val { n: f64::from(*value), kind: Kind::Number }),
        Token::Percentage { unit_value, .. } => percent(f64::from(*unit_value) * 100.0, ctx),
        Token::Dimension { value, unit, .. } => dimension(f64::from(*value), &unit.to_ascii_lowercase(), ctx),
        _ => fail(),
    }
}

fn percent(n: f64, ctx: Ctx<'_>) -> R<Val> {
    match ctx.pct {
        Pct::Length(base) => Ok(Val { n: n * base / 100.0, kind: Kind::Length }),
        Pct::Number => Ok(Val { n: n / 100.0, kind: Kind::Number }),
        Pct::Keep => Ok(Val { n, kind: Kind::Percent }),
        Pct::None => fail(),
    }
}

thread_local! {
    /// The window lengths are worked out for (width, height), on this
    /// thread: see [`set_viewport`].
    static VIEWPORT: std::cell::Cell<(f64, f64)> = const { std::cell::Cell::new((900.0, 940.0)) };
    /// A window unit was worked out since [`viewport_used`] was last asked.
    static VIEWPORT_USED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The root element's font size, for `rem`.
    static ROOT_FONT: std::cell::Cell<f64> = const { std::cell::Cell::new(16.0) };
    /// The size of the nearest size container, for `cqw` and the like.
    static CONTAINER: std::cell::Cell<Option<(f64, f64)>> = const { std::cell::Cell::new(None) };
}

/// Work out window units (`vw`, `vh`…) for a window this large, on this
/// thread, until set again.
pub fn set_viewport(width: f64, height: f64) {
    VIEWPORT.with(|v| v.set((width, height)));
}

/// Was a window unit worked out since last asked? (Asking resets it.)
pub fn viewport_used() -> bool {
    VIEWPORT_USED.with(|v| v.replace(false))
}

/// Work out `rem` from this font size, on this thread, until set again.
pub fn set_root_font(px: f64) {
    ROOT_FONT.with(|v| v.set(px));
}

/// Work out container units (`cqw`…) for a container this large (width,
/// height), or the window's with none; returns the size set before.
pub fn set_container(size: Option<(f64, f64)>) -> Option<(f64, f64)> {
    CONTAINER.with(|v| v.replace(size))
}

/// A number with a unit, in CSS pixels (`None`: not a length).
pub fn dimension_px(n: f64, unit: &str, font: f64) -> Option<f64> {
    let (w, h) = VIEWPORT.with(std::cell::Cell::get);
    let root = ROOT_FONT.with(std::cell::Cell::get);
    let (cw, ch) = CONTAINER.with(std::cell::Cell::get).unwrap_or((w, h));
    if unit.starts_with('v') || unit.starts_with("sv") || unit.starts_with("lv") || unit.starts_with("dv") {
        VIEWPORT_USED.with(|v| v.set(true));
    }
    Some(match unit {
        "px" => n,
        "pt" => n * 4.0 / 3.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "q" => n * 96.0 / 101.6,
        "em" => n * font,
        "rem" => n * root,
        "ex" | "ch" | "cap" => n * font / 2.0,
        "rex" | "rch" | "rcap" => n * root / 2.0,
        "ic" => n * font,
        "ric" => n * root,
        "lh" => n * font * 1.2,
        "rlh" => n * root * 1.2,
        // Window units, and container units (the window's with no container).
        "vw" | "vi" | "svw" | "lvw" | "dvw" | "svi" | "lvi" | "dvi" => n * w / 100.0,
        "vh" | "vb" | "svh" | "lvh" | "dvh" | "svb" | "lvb" | "dvb" => n * h / 100.0,
        "vmin" | "svmin" | "lvmin" | "dvmin" => n * w.min(h) / 100.0,
        "vmax" | "svmax" | "lvmax" | "dvmax" => n * w.max(h) / 100.0,
        "cqw" | "cqi" => n * cw / 100.0,
        "cqh" | "cqb" => n * ch / 100.0,
        "cqmin" => n * cw.min(ch) / 100.0,
        "cqmax" => n * cw.max(ch) / 100.0,
        _ => return None,
    })
}

fn dimension(n: f64, unit: &str, ctx: Ctx<'_>) -> R<Val> {
    if let Some(px) = dimension_px(n, unit, ctx.font) {
        return Ok(Val { n: px, kind: Kind::Length });
    }
    let (n, kind) = match unit {
        "deg" => (n, Kind::Angle),
        "rad" => (n.to_degrees(), Kind::Angle),
        "grad" => (n * 0.9, Kind::Angle),
        "turn" => (n * 360.0, Kind::Angle),
        "s" => (n, Kind::Time),
        "ms" => (n / 1000.0, Kind::Time),
        "hz" => (n, Kind::Frequency),
        "khz" => (n * 1000.0, Kind::Frequency),
        "dppx" | "x" => (n, Kind::Resolution),
        "dpi" => (n / 96.0, Kind::Resolution),
        "dpcm" => (n * 2.54 / 96.0, Kind::Resolution),
        _ => return fail(),
    };
    Ok(Val { n, kind })
}

/// `a + b`, `a - b`: values of one kind.
fn add(a: Val, b: Val, sign: f64) -> R<Val> {
    if a.kind != b.kind {
        return fail();
    }
    Ok(Val { n: a.n + sign * b.n, kind: a.kind })
}

/// A sum of products, up to the end of the input or a comma. `+` and `-`
/// need whitespace on both sides.
fn sum<'i>(p: &mut Parser<'i>, ctx: Ctx<'_>, depth: usize) -> R<Val> {
    let mut total = product(p, ctx, depth)?;
    for _ in 0..MAX_TERMS {
        let state = p.state();
        if !matches!(p.next_including_whitespace(), Ok(Token::WhiteSpace(_))) {
            p.reset(&state);
            return Ok(total);
        }
        let sign = match p.next_including_whitespace().cloned() {
            Ok(Token::Delim('+')) => 1.0,
            Ok(Token::Delim('-')) => -1.0,
            _ => {
                p.reset(&state);
                return Ok(total);
            }
        };
        if !matches!(p.next_including_whitespace(), Ok(Token::WhiteSpace(_))) {
            return fail();
        }
        let next = product(p, ctx, depth)?;
        total = add(total, next, sign)?;
    }
    fail()
}

/// `* b` and `/ b` after a value already read.
fn more_products<'i>(p: &mut Parser<'i>, mut left: Val, ctx: Ctx<'_>, depth: usize) -> R<Val> {
    for _ in 0..MAX_TERMS {
        let state = p.state();
        let op = match p.next().cloned() {
            Ok(Token::Delim('*')) => '*',
            Ok(Token::Delim('/')) => '/',
            _ => {
                p.reset(&state);
                return Ok(left);
            }
        };
        let right = value(p, ctx, depth)?;
        left = if op == '*' {
            match (left.kind, right.kind) {
                (Kind::Number, k) | (k, Kind::Number) => Val { n: left.n * right.n, kind: k },
                _ => return fail(),
            }
        } else {
            if right.kind != Kind::Number {
                return fail();
            }
            Val { n: left.n / right.n, kind: left.kind }
        };
    }
    fail()
}

fn product<'i>(p: &mut Parser<'i>, ctx: Ctx<'_>, depth: usize) -> R<Val> {
    let first = value(p, ctx, depth)?;
    more_products(p, first, ctx, depth)
}

/// A number, dimension, percentage, constant, `( … )` or function.
fn value<'i>(p: &mut Parser<'i>, ctx: Ctx<'_>, depth: usize) -> R<Val> {
    if depth > MAX_DEPTH {
        return fail();
    }
    match p.next()?.clone() {
        Token::ParenthesisBlock => p.parse_nested_block(|p| inner_sum(p, ctx, depth + 1)),
        Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            if !is_function(&name) {
                return fail();
            }
            p.parse_nested_block(|p| function(&name, p, ctx, depth + 1))
        }
        Token::Ident(word) => {
            let word = word.to_ascii_lowercase();
            let n = match word.as_str() {
                "e" => std::f64::consts::E,
                "pi" => std::f64::consts::PI,
                "infinity" => f64::INFINITY,
                "-infinity" => f64::NEG_INFINITY,
                "nan" => f64::NAN,
                w => match ctx.words.and_then(|f| f(w)) {
                    Some(n) => n,
                    None => return fail(),
                },
            };
            Ok(Val { n, kind: Kind::Number })
        }
        t => leaf(&t, ctx),
    }
}

/// A whole sum inside parentheses or a function.
fn inner_sum<'i>(p: &mut Parser<'i>, ctx: Ctx<'_>, depth: usize) -> R<Val> {
    p.skip_whitespace();
    let v = sum(p, ctx, depth)?;
    p.skip_whitespace();
    p.expect_exhausted()?;
    Ok(v)
}

/// The comma-separated sums of a function's arguments.
fn args<'i>(p: &mut Parser<'i>, ctx: Ctx<'_>, depth: usize) -> R<Vec<Val>> {
    let mut out = vec![];
    loop {
        p.skip_whitespace();
        out.push(sum(p, ctx, depth)?);
        p.skip_whitespace();
        match p.next() {
            Ok(Token::Comma) => {}
            Err(_) => return Ok(out),
            Ok(_) => return fail(),
        }
        if out.len() > MAX_TERMS {
            return fail();
        }
    }
}

/// All of one kind.
fn same(v: &[Val]) -> R<Kind> {
    match v.first() {
        Some(first) if v.iter().all(|x| x.kind == first.kind) => Ok(first.kind),
        _ => fail(),
    }
}

fn number(v: Val) -> R<f64> {
    if v.kind == Kind::Number { Ok(v.n) } else { fail() }
}

fn function<'i>(name: &str, p: &mut Parser<'i>, ctx: Ctx<'_>, depth: usize) -> R<Val> {
    if depth > MAX_DEPTH {
        return fail();
    }
    if name == "calc" || name == "-webkit-calc" {
        return inner_sum(p, ctx, depth);
    }
    if name == "round" {
        // round([nearest | up | down | to-zero,]? A [, B]?)
        p.skip_whitespace();
        let strategy = p
            .try_parse(|p| {
                let s = p.expect_ident()?.to_ascii_lowercase();
                p.skip_whitespace();
                p.expect_comma()?;
                match s.as_str() {
                    "nearest" | "up" | "down" | "to-zero" => Ok(s),
                    _ => fail(),
                }
            })
            .unwrap_or_else(|_| "nearest".to_string());
        let a = args(p, ctx, depth)?;
        let (x, step) = match a[..] {
            [x] if x.kind == Kind::Number => (x, Val { n: 1.0, kind: Kind::Number }),
            [x, b] if x.kind == b.kind => (x, b),
            _ => return fail(),
        };
        let q = x.n / step.n;
        let r = match strategy.as_str() {
            "up" => q.ceil(),
            "down" => q.floor(),
            "to-zero" => q.trunc(),
            _ => (q + 0.5).floor(),
        };
        return Ok(Val { n: r * step.n, kind: x.kind });
    }
    if name == "random" {
        // random([caching options,]? min, max [, by step]?): somewhere
        // between; taken as the middle.
        p.skip_whitespace();
        let _ = p.try_parse(|p| {
            let t = p.next()?.clone();
            match t {
                Token::Ident(ref w) if w.starts_with("--") || w.eq_ignore_ascii_case("per-element") => {}
                Token::Ident(ref w) if w.eq_ignore_ascii_case("fixed") => {
                    p.expect_number()?;
                }
                _ => return fail(),
            }
            p.skip_whitespace();
            p.expect_comma()?;
            Ok(())
        });
        p.skip_whitespace();
        let lo = sum(p, ctx, depth)?;
        p.skip_whitespace();
        p.expect_comma()?;
        p.skip_whitespace();
        let hi = sum(p, ctx, depth)?;
        p.skip_whitespace();
        if p.try_parse(|p| p.expect_comma()).is_ok() {
            p.skip_whitespace();
            p.expect_ident_matching("by")?;
            p.skip_whitespace();
            let step = sum(p, ctx, depth)?;
            if step.kind != lo.kind {
                return fail();
            }
        }
        p.skip_whitespace();
        p.expect_exhausted()?;
        if lo.kind != hi.kind {
            return fail();
        }
        return Ok(Val { n: (lo.n + hi.n) / 2.0, kind: lo.kind });
    }
    let a = args(p, ctx, depth)?;
    let v = |n: f64, kind: Kind| Ok(Val { n, kind });
    match (name, &a[..]) {
        ("min" | "max", [..]) => {
            let k = same(&a)?;
            let pick = if name == "min" { f64::min } else { f64::max };
            v(a.iter().map(|x| x.n).reduce(pick).unwrap_or(0.0), k)
        }
        ("clamp", [lo, x, hi]) => {
            let k = same(&a)?;
            v(x.n.min(hi.n).max(lo.n), k)
        }
        ("mod" | "rem", [x, y]) => {
            let k = same(&a)?;
            let r = x.n % y.n;
            let r = if name == "mod" && r != 0.0 && (r < 0.0) != (y.n < 0.0) { r + y.n } else { r };
            v(r, k)
        }
        ("sin" | "cos" | "tan", [x]) => {
            let rad = match x.kind {
                Kind::Angle => x.n.to_radians(),
                Kind::Number => x.n,
                _ => return fail(),
            };
            v(
                match name {
                    "sin" => rad.sin(),
                    "cos" => rad.cos(),
                    _ => rad.tan(),
                },
                Kind::Number,
            )
        }
        ("asin" | "acos" | "atan", [x]) => {
            let n = number(*x)?;
            v(
                match name {
                    "asin" => n.asin(),
                    "acos" => n.acos(),
                    _ => n.atan(),
                }
                .to_degrees(),
                Kind::Angle,
            )
        }
        ("atan2", [y, x]) => {
            same(&a)?;
            v(y.n.atan2(x.n).to_degrees(), Kind::Angle)
        }
        ("pow", [x, y]) => v(number(*x)?.powf(number(*y)?), Kind::Number),
        ("sqrt", [x]) => v(number(*x)?.sqrt(), Kind::Number),
        ("hypot", [..]) => {
            let k = same(&a)?;
            v(a.iter().map(|x| x.n * x.n).sum::<f64>().sqrt(), k)
        }
        ("log", [x]) => v(number(*x)?.ln(), Kind::Number),
        ("log", [x, b]) => v(number(*x)?.ln() / number(*b)?.ln(), Kind::Number),
        ("exp", [x]) => v(number(*x)?.exp(), Kind::Number),
        ("abs", [x]) => v(x.n.abs(), x.kind),
        ("sign", [x]) => v(
            if x.n > 0.0 {
                1.0
            } else if x.n < 0.0 {
                -1.0
            } else {
                x.n
            },
            Kind::Number,
        ),
        _ => fail(),
    }
}

/// A length in pixels: a math function giving a length, `em` from `font`
/// and percentages from `pct_base`.
pub fn length(text: &str, font: f64, pct_base: f64) -> Option<f64> {
    let v = eval(text, Ctx { pct: Pct::Length(pct_base), font, words: None })?;
    (v.kind == Kind::Length).then_some(v.n)
}

/// A number (percentages as fractions when `pct` is [`Pct::Number`]).
pub fn number_value(text: &str, pct: Pct) -> Option<f64> {
    let v = eval(text, Ctx { pct, font: 16.0, words: None })?;
    (v.kind == Kind::Number).then_some(v.n)
}

/// An angle, in degrees.
pub fn angle(text: &str) -> Option<f64> {
    let v = eval(text, Ctx { pct: Pct::None, font: 16.0, words: None })?;
    match v.kind {
        Kind::Angle => Some(v.n),
        Kind::Number if v.n == 0.0 && !text.contains('(') => Some(0.0),
        _ => None,
    }
}

/// A time, in seconds.
pub fn time(text: &str) -> Option<f64> {
    let v = eval(text, Ctx { pct: Pct::None, font: 16.0, words: None })?;
    (v.kind == Kind::Time).then_some(v.n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(t: &str) -> Option<f64> {
        length(t, 16.0, 100.0).map(|v| (v * 1e4).round() / 1e4)
    }

    #[test]
    fn formulas_as_webkit_reads_them() {
        assert_eq!(len("calc(1px + 2px)"), Some(3.0));
        assert_eq!(len("calc(1px+2px)"), None, "+ needs spaces");
        assert_eq!(len("calc(2 * 3px)"), Some(6.0));
        assert_eq!(len("calc(2px*3px)"), None, "a length times a length");
        assert_eq!(len("calc(6px / 2)"), Some(3.0));
        assert_eq!(len("calc(6px / 2px)"), None);
        assert_eq!(len("calc(x)"), None);
        assert_eq!(len("calc(0)"), None, "a number where a length goes");
        assert_eq!(len("calc(50%)"), Some(50.0));
        assert_eq!(len("calc(1em + 10%)"), Some(26.0));
        assert_eq!(len("min(10px, 1em)"), Some(10.0));
        assert_eq!(len("clamp(0px, 50px, 20px)"), Some(20.0));
        assert_eq!(len("max(1px, 2px, 3px)"), Some(3.0));
        assert_eq!(len("round(up, 11px, 5px)"), Some(15.0));
        assert_eq!(len("mod(-7px, 5px)"), Some(3.0));
        assert_eq!(len("rem(-7px, 5px)"), Some(-2.0));
        assert_eq!(len("hypot(3px, 4px)"), Some(5.0));
        assert_eq!(len("abs(-4px)"), Some(4.0));
        assert_eq!(len("calc(1px * sign(-3px))"), Some(-1.0));
        assert_eq!(len("calc(pow(2, 3) * 1px)"), Some(8.0));
        assert_eq!(len("calc(sqrt(16) * 1px)"), Some(4.0));
        assert_eq!(len("calc(sin(90deg) * 10px)"), Some(10.0));
        assert_eq!(len("calc(cos(0) * 10px)"), Some(10.0));
        assert_eq!(len("calc((((((((((((((((((0px))))))))))))))))))"), Some(0.0));
        assert!(len("calc(1px / 0)").is_some_and(|v| v > 1e30), "infinity");
        assert_eq!(len("calc(0px * nan)"), Some(0.0), "NaN counts as 0");
        assert_eq!(number_value("calc(0)", Pct::Number), Some(0.0));
        assert_eq!(number_value("calc(50%)", Pct::Number), Some(0.5));
        assert_eq!(number_value("calc(1px)", Pct::Number), None);
        assert_eq!(number_value("calc(e - e)", Pct::Number), Some(0.0));
        assert!(number_value("random(0, 0.05)", Pct::Number).is_some_and(|v| (v - 0.025).abs() < 1e-6));
        assert_eq!(angle("calc(atan2(1, 1))").map(|d| d.round()), Some(45.0));
        assert_eq!(angle("calc(asin(1))").map(|d| d.round()), Some(90.0));
        assert_eq!(time("calc(1s + 500ms)"), Some(1.5));
        // Nesting past WebKit's limit.
        let deep = format!("calc({}0px{})", "(".repeat(120), ")".repeat(120));
        assert_eq!(len(&deep), None);
    }
}
