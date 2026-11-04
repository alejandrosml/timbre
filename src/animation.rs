//! CSS animations, as far as they show or hide text while a message is
//! read.
//!
//! A message is taken as read ten seconds after it opens. An animation
//! waiting for its delay, finished, or paused shows one state at that time:
//! its keyframes are interpolated there. One still running (or repeating
//! for ever) shows all its states in turn, and a reader sees the text if
//! any of them shows it: it is judged by the most visible of its states.

use crate::css::{Computed, Decl, Decls};

/// When the message is read, in seconds after it opens.
const AT: f64 = 10.0;

/// The keyframes of an `@keyframes` rule: offsets from 0 to 1, in order.
#[derive(Default, Clone, Debug)]
pub struct Keyframes {
    frames: Vec<(f32, Decls)>,
}

impl Keyframes {
    /// Add the declarations of a keyframe at these offsets.
    pub fn add(&mut self, offsets: &[f32], decls: &Decls) {
        for &at in offsets {
            match self.frames.iter_mut().find(|(o, _)| *o == at) {
                Some((_, d)) => d.extend(decls),
                None => self.frames.push((at, decls.clone())),
            }
        }
        self.frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    }

    pub fn hostile(&self) -> bool {
        self.frames.iter().any(|(_, d)| d.hostile())
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }
}

/// How the time of an animation maps to its progress.
#[derive(Clone, Copy, Debug)]
enum Easing {
    /// Smooth, from (0,0) to (1,1): ease, cubic-bezier(), linear().
    Smooth(f64, f64, f64, f64),
    /// steps(n, position): `start` jumps at the start of each step.
    Steps(u32, StepAt),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum StepAt {
    Start,
    End,
    None,
    Both,
}

impl Easing {
    fn of(t: &str) -> Option<Easing> {
        Some(match t {
            "linear" => Easing::Smooth(0.0, 0.0, 1.0, 1.0),
            "ease" => Easing::Smooth(0.25, 0.1, 0.25, 1.0),
            "ease-in" => Easing::Smooth(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Easing::Smooth(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Easing::Smooth(0.42, 0.0, 0.58, 1.0),
            "step-start" => Easing::Steps(1, StepAt::Start),
            "step-end" => Easing::Steps(1, StepAt::End),
            _ => {
                let open = t.find('(')?;
                let inner = t[open + 1..].strip_suffix(')')?;
                let args: Vec<&str> = inner.split(',').map(str::trim).collect();
                match &t[..open] {
                    "cubic-bezier" => {
                        let n: Vec<f64> = args.iter().filter_map(|a| a.parse().ok()).collect();
                        match n[..] {
                            [x1, y1, x2, y2]
                                if args.len() == 4 && (0.0..=1.0).contains(&x1) && (0.0..=1.0).contains(&x2) =>
                            {
                                Easing::Smooth(x1, y1, x2, y2)
                            }
                            _ => return None,
                        }
                    }
                    "steps" => {
                        let n: u32 = args.first()?.parse().ok()?;
                        let at = match args.get(1).copied() {
                            None | Some("end" | "jump-end") => StepAt::End,
                            Some("start" | "jump-start") => StepAt::Start,
                            Some("jump-none") => StepAt::None,
                            Some("jump-both") => StepAt::Both,
                            _ => return None,
                        };
                        if n == 0 || (n == 1 && at == StepAt::None) {
                            return None;
                        }
                        Easing::Steps(n.min(1000), at)
                    }
                    // A piecewise-linear curve: near enough to linear here.
                    "linear" => Easing::Smooth(0.0, 0.0, 1.0, 1.0),
                    _ => return None,
                }
            }
        })
    }

    /// The output progress for input progress `p` (0 to 1).
    fn apply(self, p: f64) -> f64 {
        match self {
            Easing::Smooth(x1, y1, x2, y2) => {
                // Solve x(s) = p for the curve's parameter, then give y(s).
                let bez =
                    |a: f64, b: f64, s: f64| 3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3);
                let (mut lo, mut hi) = (0.0, 1.0);
                for _ in 0..40 {
                    let mid = (lo + hi) / 2.0;
                    if bez(x1, x2, mid) < p { lo = mid } else { hi = mid }
                }
                bez(y1, y2, (lo + hi) / 2.0)
            }
            Easing::Steps(n, at) => {
                let n = f64::from(n);
                let jumps = match at {
                    StepAt::Start | StepAt::End => n,
                    StepAt::None => n - 1.0,
                    StepAt::Both => n + 1.0,
                };
                let mut step = (p * n).floor();
                if matches!(at, StepAt::Start | StepAt::Both) {
                    step += 1.0;
                }
                if p >= 1.0 && at != StepAt::Start && at != StepAt::Both {
                    step = n;
                }
                (step / jumps).clamp(0.0, 1.0)
            }
        }
    }

    /// The outputs it gives over a whole iteration, for judging a running
    /// animation: the steps' values, or none for a smooth curve (whose
    /// extremes are at the keyframes).
    fn outputs(self) -> Option<Vec<f64>> {
        match self {
            Easing::Smooth(..) => None,
            Easing::Steps(n, _) => {
                Some((0..n.min(100) * 2 + 1).map(|i| self.apply(f64::from(i) / f64::from(n.min(100) * 2))).collect())
            }
        }
    }
}

/// One animation of an element.
#[derive(Clone, Debug)]
pub struct Animation {
    pub name: String,
    duration: f64,
    delay: f64,
    /// `None`: infinite.
    count: Option<f64>,
    direction: String,
    fill: String,
    paused: bool,
    easing: Easing,
}

impl Default for Animation {
    fn default() -> Self {
        Animation {
            name: "none".into(),
            duration: 0.0,
            delay: 0.0,
            count: Some(1.0),
            direction: "normal".into(),
            fill: "none".into(),
            paused: false,
            easing: Easing::Smooth(0.25, 0.1, 0.25, 1.0),
        }
    }
}

/// A time in seconds: `2s`, `150ms`, `1e3ms`, formulas.
fn seconds(t: &str) -> Option<f64> {
    crate::calc::time(t)
}

/// A value's comma-separated parts, outside parentheses.
fn split(v: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (vec![], 0i32, 0);
    for (i, c) in v.bytes().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(v[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(v[start..].trim());
    out
}

/// Words of a value, keeping `steps(…)` and the like whole.
fn words(v: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (vec![], 0i32, None);
    for (i, c) in v.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth <= 0 => {
                if let Some(s) = start.take() {
                    out.push(&v[s..i]);
                }
                continue;
            }
            _ => {}
        }
        start.get_or_insert(i);
    }
    if let Some(s) = start {
        out.push(&v[s..]);
    }
    out
}

/// One animation of the `animation` shorthand. Each word goes to the first
/// longhand it fits that is not set yet; the name takes what is left, so
/// `animation: forwards 1s forwards` animates keyframes named "forwards".
fn shorthand(one: &str) -> Animation {
    let mut a = Animation::default();
    let (mut times, mut named, mut count, mut direction, mut fill, mut state, mut easing) =
        (0, false, false, false, false, false, false);
    for w in words(one) {
        let l = w.to_ascii_lowercase();
        if let Some(s) = seconds(&l) {
            match times {
                0 => a.duration = s,
                _ => a.delay = s,
            }
            times += 1;
        } else if !easing && Easing::of(&l).is_some() {
            a.easing = Easing::of(&l).unwrap_or(a.easing);
            easing = true;
        } else if !count && (l == "infinite" || crate::calc::number_value(&l, crate::calc::Pct::None).is_some()) {
            a.count = if l == "infinite" {
                None
            } else {
                crate::calc::number_value(&l, crate::calc::Pct::None).map(|n| n.max(0.0))
            };
            count = true;
        } else if !direction && matches!(l.as_str(), "normal" | "reverse" | "alternate" | "alternate-reverse") {
            a.direction = l;
            direction = true;
        } else if !fill && matches!(l.as_str(), "none" | "forwards" | "backwards" | "both") {
            a.fill = l;
            fill = true;
        } else if !state && matches!(l.as_str(), "running" | "paused") {
            a.paused = l == "paused";
            state = true;
        } else if !named {
            a.name = w.trim_matches(|c| c == '"' || c == '\'').to_string();
            named = true;
        }
    }
    a
}

/// The animations an element runs, from `animation` and its longhands.
pub fn animations(c: &Computed) -> Vec<Animation> {
    let mut list: Vec<Animation> =
        c.get("animation").map(|v| split(v).into_iter().map(shorthand).collect()).unwrap_or_default();
    // Longhands, each a list matched to the animations by position.
    let long = |p: &str| c.get(p).map(split);
    if let Some(names) = long("animation-name") {
        list.resize_with(names.len().max(list.len()), Animation::default);
        list.truncate(names.len());
        for (a, n) in list.iter_mut().zip(names) {
            a.name = n.trim_matches(|c| c == '"' || c == '\'').to_string();
        }
    }
    let each = |p: &str, list: &mut Vec<Animation>, f: &dyn Fn(&mut Animation, &str)| {
        if let Some(values) = long(p).filter(|v| !v.is_empty()) {
            for (i, a) in list.iter_mut().enumerate() {
                f(a, &values[i % values.len()].to_ascii_lowercase());
            }
        }
    };
    each("animation-duration", &mut list, &|a, v| a.duration = seconds(v).unwrap_or(a.duration));
    each("animation-delay", &mut list, &|a, v| a.delay = seconds(v).unwrap_or(a.delay));
    each("animation-iteration-count", &mut list, &|a, v| {
        a.count = if v == "infinite" {
            None
        } else {
            crate::calc::number_value(v, crate::calc::Pct::None).map(|n| n.max(0.0)).or(a.count)
        }
    });
    each("animation-direction", &mut list, &|a, v| a.direction = v.to_string());
    each("animation-fill-mode", &mut list, &|a, v| a.fill = v.to_string());
    each("animation-play-state", &mut list, &|a, v| a.paused = v == "paused");
    each("animation-timing-function", &mut list, &|a, v| a.easing = Easing::of(v).unwrap_or(a.easing));
    list.retain(|a| !a.name.eq_ignore_ascii_case("none") && !a.name.is_empty());
    list
}

/// What an animation shows while the message is read.
enum Shows {
    Nothing,
    /// One state: progress through the keyframes, 0 to 1.
    State(f64),
    /// All its states in turn.
    Running,
}

impl Animation {
    /// Directed progress for iteration `i` at progress `p` within it.
    fn directed(&self, i: f64, p: f64) -> f64 {
        let odd = (i as u64) % 2 == 1;
        let reverse = match self.direction.as_str() {
            "reverse" => true,
            "alternate" => odd,
            "alternate-reverse" => !odd,
            _ => false,
        };
        let p = if reverse { 1.0 - p } else { p };
        self.easing.apply(p)
    }

    fn shows(&self) -> Shows {
        let backwards = matches!(self.fill.as_str(), "backwards" | "both");
        let forwards = matches!(self.fill.as_str(), "forwards" | "both");
        // Paused from the start: held at time 0.
        let local = if self.paused { 0.0 } else { AT };
        let t = local - self.delay;
        if t < 0.0 {
            return if backwards { Shows::State(self.directed(0.0, 0.0)) } else { Shows::Nothing };
        }
        let active = match self.count {
            Some(c) => self.duration * c,
            None => f64::INFINITY,
        };
        if t >= active {
            if !forwards {
                return Shows::Nothing;
            }
            let count = self.count.unwrap_or(0.0);
            if count == 0.0 || self.duration == 0.0 && count == 0.0 {
                return Shows::State(self.directed(0.0, 0.0));
            }
            let (i, p) = if count.fract() == 0.0 { (count - 1.0, 1.0) } else { (count.floor(), count.fract()) };
            return Shows::State(self.directed(i, p));
        }
        if self.paused || self.duration <= 0.0 {
            let (i, p) = if self.duration > 0.0 {
                ((t / self.duration).floor(), (t / self.duration).fract())
            } else {
                (0.0, 0.0)
            };
            return Shows::State(self.directed(i, p));
        }
        Shows::Running
    }
}

/// A numeric value of a property that fades: opacity, or a font size in
/// pixels.
fn amount(prop: &str, v: &str) -> Option<f64> {
    match prop {
        "opacity" => crate::calc::number_value(v, crate::calc::Pct::Number).map(|n| n.clamp(0.0, 1.0)),
        "font-size" => crate::calc::length(v, 16.0, 16.0).filter(|_| !v.contains("em") && !v.contains('%')),
        _ => None,
    }
}

/// The value of `prop` at progress `p`: between the keyframes around it,
/// the frames without it standing for the element's own value (`base`).
fn value_at(frames: &[(f32, Vec<Decl>)], prop: &str, p: f64, base: Option<&str>) -> Option<String> {
    // Without a value of its own, an element is opaque and visible.
    let base = base.or(match prop {
        "opacity" => Some("1"),
        "visibility" => Some("visible"),
        _ => None,
    });
    let at = |d: &[Decl]| d.iter().find(|(q, _, _)| *q == prop).map(|(_, v, _)| v.clone());
    let mut before: (f64, Option<String>) = (0.0, base.map(str::to_string));
    let mut after: (f64, Option<String>) = (1.0, base.map(str::to_string));
    for (o, d) in frames {
        let o = f64::from(*o);
        let Some(v) = at(d) else { continue };
        if o <= p {
            before = (o, Some(v));
        } else {
            after = (o, Some(v));
            break;
        }
    }
    match (&before.1, &after.1) {
        (Some(a), Some(b)) if a == b => Some(a.clone()),
        (Some(a), Some(b)) => {
            let f = if after.0 > before.0 { (p - before.0) / (after.0 - before.0) } else { 0.0 };
            match (amount(prop, a), amount(prop, b)) {
                (Some(x), Some(y)) => {
                    let n = x + (y - x) * f;
                    Some(if prop == "font-size" { format!("{n}px") } else { n.to_string() })
                }
                _ => Some(if f < 0.5 { a.clone() } else { b.clone() }),
            }
        }
        (Some(a), None) => Some(a.clone()),
        (None, Some(b)) => Some(b.clone()),
        (None, None) => None,
    }
}

/// Of the values a property takes in turn, the one that shows text most:
/// the highest opacity or size, `visible` if any is; for the rest, the
/// value if all agree, else nothing (the element's own).
fn most_visible(prop: &str, values: &[Option<String>]) -> Option<String> {
    let all: Vec<&str> = values.iter().map(|v| v.as_deref().unwrap_or("")).collect();
    match prop {
        "opacity" | "font-size" => {
            let n: Option<Vec<f64>> = values.iter().map(|v| v.as_deref().and_then(|v| amount(prop, v))).collect();
            let max = n?.into_iter().fold(f64::NEG_INFINITY, f64::max);
            Some(if prop == "font-size" { format!("{max}px") } else { max.to_string() })
        }
        "visibility" => {
            Some(if all.iter().any(|v| !matches!(*v, "hidden" | "collapse")) { "visible" } else { "hidden" }.into())
        }
        _ => all.windows(2).all(|w| w[0] == w[1]).then(|| values[0].clone()).flatten(),
    }
}

/// Properties keyframes cannot set.
fn animatable(prop: &str) -> bool {
    prop != "--" && !prop.starts_with("animation")
}

/// What the animations of an element set while the message is read, the
/// later ones over the earlier; `keyframes` finds the keyframes of a name,
/// `base` is what the element has without them.
pub fn effect(base: &Computed, keyframes: &dyn Fn(&str) -> Option<Keyframes>) -> Decls {
    let mut out = Decls::default();
    for a in animations(base) {
        let Some(k) = keyframes(&a.name) else { continue };
        // !important declarations in keyframes are ignored.
        let frames: Vec<(f32, Vec<Decl>)> = k
            .frames
            .iter()
            .map(|(o, d)| (*o, d.entries().iter().filter(|(p, _, i)| !*i && animatable(p)).cloned().collect()))
            .collect();
        let mut props: Vec<&'static str> = frames.iter().flat_map(|(_, d)| d.iter().map(|(p, _, _)| *p)).collect();
        props.sort_unstable();
        props.dedup();
        match a.shows() {
            Shows::Nothing => {}
            Shows::State(p) => {
                for prop in props {
                    if let Some(v) = value_at(&frames, prop, p, base.get(prop)) {
                        out.push((prop, v, false));
                    }
                }
            }
            Shows::Running => {
                // The states it passes through: each keyframe and the ends,
                // or the steps of a stepped animation.
                let points: Vec<f64> = match a.easing.outputs() {
                    Some(steps) => steps,
                    None => {
                        let mut p: Vec<f64> = frames.iter().map(|(o, _)| f64::from(*o)).collect();
                        p.extend([0.0, 1.0]);
                        p
                    }
                };
                for prop in props {
                    let values: Vec<Option<String>> =
                        points.iter().map(|p| value_at(&frames, prop, *p, base.get(prop))).collect();
                    if let Some(v) = most_visible(prop, &values) {
                        out.push((prop, v, false));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_and_steps() {
        let linear = Easing::of("linear").unwrap();
        assert!((linear.apply(0.3) - 0.3).abs() < 1e-3);
        let ease = Easing::of("ease").unwrap();
        assert!(ease.apply(0.5) > 0.7, "ease is ahead at half time");
        assert_eq!(Easing::of("step-start").unwrap().apply(0.01), 1.0);
        assert_eq!(Easing::of("step-end").unwrap().apply(0.99), 0.0);
        assert_eq!(Easing::of("steps(4, end)").unwrap().apply(0.5), 0.5);
        assert!(Easing::of("steps(0)").is_none());
        assert!(Easing::of("cubic-bezier(2, 0, 1, 1)").is_none());
    }

    #[test]
    fn shorthand_words_go_where_they_fit() {
        let a = shorthand("forwards 1ms forwards");
        assert_eq!((a.name.as_str(), a.fill.as_str()), ("forwards", "forwards"));
        let a = shorthand("k 1ms 1e30s backwards");
        assert!((a.delay / 1e30 - 1.0).abs() < 1e-6, "{}", a.delay);
        let a = shorthand("K 2s infinite alternate");
        assert_eq!((a.name.as_str(), a.count, a.direction.as_str()), ("K", None, "alternate"));
    }
}
