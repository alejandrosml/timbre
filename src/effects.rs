//! What happens to colours on their way to the screen, for telling whether
//! text can be seen: CSS filters (`filter`, `backdrop-filter`), blending
//! (`mix-blend-mode`), and the copies of letters `text-shadow` draws.

use crate::css::Paint;
use std::rc::Rc;

/// Steps followed at most, out from one piece of text. Real mail uses one
/// or two; past this many, what is drawn there is taken as unseen.
const MAX_STEPS: usize = 32;

/// One step a colour goes through.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    /// A filter function: each channel (0 to 1) becomes `m·rgb + b`, kept
    /// within 0 to 1.
    Affine([[f32; 3]; 3], [f32; 3]),
    /// Blending with the colour under the element (`mix-blend-mode`).
    Blend(Blend, [f32; 3]),
}

impl Op {
    /// Blending in `mode` with this colour under the element.
    pub fn blend(mode: Blend, under: (u8, u8, u8)) -> Op {
        Op::Blend(mode, channels(under))
    }

    fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        match self {
            Op::Affine(m, b) => {
                let mut out = [0.0; 3];
                for (i, row) in m.iter().enumerate() {
                    out[i] = (row[0] * c[0] + row[1] * c[1] + row[2] * c[2] + b[i]).clamp(0.0, 1.0);
                }
                out
            }
            Op::Blend(mode, under) => mode.mix(*under, c),
        }
    }
}

fn channels((r, g, b): (u8, u8, u8)) -> [f32; 3] {
    [f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0]
}

fn rgb(c: [f32; 3]) -> Paint {
    // NaN becomes 0.
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    Paint::Rgb(byte(c[0]), byte(c[1]), byte(c[2]))
}

/// The steps colours drawn inside an element go through: its own, then
/// those of the elements around it.
#[derive(Debug)]
pub struct Effects {
    ops: Vec<Op>,
    outer: Option<Rc<Effects>>,
    /// Steps in all, out to the page.
    steps: usize,
}

impl Effects {
    /// `outer`, with an element's own steps inside it.
    pub fn inside(outer: &Option<Rc<Effects>>, ops: Vec<Op>) -> Option<Rc<Effects>> {
        if ops.is_empty() {
            return outer.clone();
        }
        let steps = ops.len().saturating_add(outer.as_ref().map_or(0, |o| o.steps));
        Some(Rc::new(Effects { ops, outer: outer.clone(), steps }))
    }

    /// Colour `p`, drawn where these effects are, as it shows on screen.
    /// Transparent stays transparent; pictures and colours not decoded stay
    /// as they are.
    pub fn apply(this: &Option<Rc<Effects>>, p: Paint) -> Paint {
        let Some(mut at) = this.as_deref() else { return p };
        if at.steps > MAX_STEPS {
            return Paint::Clear;
        }
        let Paint::Rgb(r, g, b) = p else { return p };
        let mut c = channels((r, g, b));
        loop {
            for op in &at.ops {
                c = op.apply(c);
            }
            match at.outer.as_deref() {
                Some(o) => at = o,
                None => return rgb(c),
            }
        }
    }
}

/// What a `filter` (or `backdrop-filter`) value does.
#[derive(Debug, Default)]
pub struct Filter {
    /// Its colour steps, in order.
    pub ops: Vec<Op>,
    /// How far it blurs, in CSS pixels.
    pub blur: f32,
    /// What its `url()`s point at: an element of the page by its id
    /// (`#id`), or (`None`) an SVG in another file.
    pub urls: Vec<Option<String>>,
    /// Its `drop-shadow()`s: copies of what is drawn, letters included.
    pub shadows: Vec<Shadow>,
    /// It holds more functions than are followed: the rest were not read,
    /// and what it draws is taken as unseen.
    pub cut: bool,
}

/// A step `m·rgb + b` with `m` all on its diagonal.
fn diagonal(m: f32, b: f32) -> Op {
    Op::Affine([[m, 0.0, 0.0], [0.0, m, 0.0], [0.0, 0.0, m]], [b; 3])
}

/// A step from the Filter Effects matrices: `base` plus `t` times `pull`.
fn matrix(base: [[f32; 3]; 3], pull: [[f32; 3]; 3], t: f32) -> Op {
    let mut m = base;
    for (row, p) in m.iter_mut().zip(pull) {
        for (v, d) in row.iter_mut().zip(p) {
            *v += d * t;
        }
    }
    Op::Affine(m, [0.0; 3])
}

/// Luminance weights rows, as the Filter Effects matrices start from.
const LUMA: [[f32; 3]; 3] = [[0.2126, 0.7152, 0.0722]; 3];
const LUMA_PULL: [[f32; 3]; 3] = [[0.7874, -0.7152, -0.0722], [-0.2126, 0.2848, -0.0722], [-0.2126, -0.7152, 0.9278]];
const SATURATE: [[f32; 3]; 3] = [[0.213, 0.715, 0.072]; 3];
const SATURATE_PULL: [[f32; 3]; 3] = [[0.787, -0.715, -0.072], [-0.213, 0.285, -0.072], [-0.213, -0.715, 0.928]];
const SEPIA: [[f32; 3]; 3] = [[0.393, 0.769, 0.189], [0.349, 0.686, 0.168], [0.272, 0.534, 0.131]];
const SEPIA_PULL: [[f32; 3]; 3] = [[0.607, -0.769, -0.189], [-0.349, 0.314, -0.168], [-0.272, -0.534, 0.869]];

/// The filter functions of a value: `invert()`, `brightness()` and the
/// like as colour steps, `blur()` as a radius, `drop-shadow()`s and
/// `url()`s noted. `opacity()` is read with the `opacity` property.
pub fn filter(v: &str, font: f32) -> Filter {
    let mut f = Filter::default();
    let mut rest = v;
    while let Some(open) = rest.find('(') {
        if f.ops.len() + f.urls.len() + f.shadows.len() > MAX_STEPS {
            f.cut = true;
            break;
        }
        let mut depth = 0;
        let close = rest[open..].char_indices().find_map(|(i, c)| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(open + i)
        });
        let Some(close) = close else { break };
        let name = rest[..open].trim().to_ascii_lowercase();
        let args = rest[open + 1..close].trim();
        rest = &rest[close + 1..];
        // An amount: a number or a percentage, 1 when left out.
        let amount = || if args.is_empty() { Some(1.0) } else { crate::css::fraction(args).filter(|a| *a >= 0.0) };
        let op = match name.as_str() {
            "url" => {
                let target = args.trim_matches(|c| c == '"' || c == '\'').trim();
                f.urls.push(target.strip_prefix('#').map(String::from));
                None
            }
            "drop-shadow" => {
                f.shadows.extend(shadows(args, font).into_iter().take(1));
                None
            }
            "blur" => {
                let r = if args.is_empty() { Some(0.0) } else { crate::css::length_of(args, font, font) };
                f.blur = f.blur.hypot(r.unwrap_or(0.0).max(0.0));
                None
            }
            "invert" => amount().map(|a| a.min(1.0)).map(|a| diagonal(1.0 - 2.0 * a, a)),
            "brightness" => amount().map(|a| diagonal(a, 0.0)),
            "contrast" => amount().map(|a| diagonal(a, 0.5 - 0.5 * a)),
            "grayscale" => amount().map(|a| matrix(LUMA, LUMA_PULL, 1.0 - a.min(1.0))),
            "sepia" => amount().map(|a| matrix(SEPIA, SEPIA_PULL, 1.0 - a.min(1.0))),
            "saturate" => amount().map(|a| matrix(SATURATE, SATURATE_PULL, a)),
            "hue-rotate" => {
                let deg = if args.is_empty() { Some(0.0) } else { crate::calc::angle(args) };
                deg.map(|d| {
                    let (sin, cos) = (d as f32).to_radians().sin_cos();
                    let cos_part = [[0.787, -0.715, -0.072], [-0.213, 0.285, -0.072], [-0.213, -0.715, 0.928]];
                    let sin_part = [[-0.213, -0.715, 0.928], [0.143, 0.140, -0.283], [-0.787, 0.715, 0.072]];
                    let mut m = SATURATE;
                    for i in 0..3 {
                        for j in 0..3 {
                            m[i][j] += cos * cos_part[i][j] + sin * sin_part[i][j];
                        }
                    }
                    Op::Affine(m, [0.0; 3])
                })
            }
            _ => None,
        };
        f.ops.extend(op);
    }
    f
}

/// `mix-blend-mode`s other than `normal`, as WebKit blends them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blend {
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    PlusDarker,
    PlusLighter,
}

impl Blend {
    pub fn of(v: &str) -> Option<Blend> {
        Some(match v.trim() {
            "multiply" => Blend::Multiply,
            "screen" => Blend::Screen,
            "overlay" => Blend::Overlay,
            "darken" => Blend::Darken,
            "lighten" => Blend::Lighten,
            "color-dodge" => Blend::ColorDodge,
            "color-burn" => Blend::ColorBurn,
            "hard-light" => Blend::HardLight,
            "soft-light" => Blend::SoftLight,
            "difference" => Blend::Difference,
            "exclusion" => Blend::Exclusion,
            "hue" => Blend::Hue,
            "saturation" => Blend::Saturation,
            "color" => Blend::Color,
            "luminosity" => Blend::Luminosity,
            "plus-darker" => Blend::PlusDarker,
            "plus-lighter" => Blend::PlusLighter,
            _ => return None,
        })
    }

    /// Colour `s` drawn over `b` (Compositing and Blending, level 1).
    fn mix(self, b: [f32; 3], s: [f32; 3]) -> [f32; 3] {
        let each = |f: &dyn Fn(f32, f32) -> f32| [f(b[0], s[0]), f(b[1], s[1]), f(b[2], s[2])];
        let hard = |b: f32, s: f32| {
            if s <= 0.5 {
                b * 2.0 * s
            } else {
                let t = 2.0 * s - 1.0;
                b + t - b * t
            }
        };
        match self {
            Blend::Multiply => each(&|b, s| b * s),
            Blend::Screen => each(&|b, s| b + s - b * s),
            Blend::Overlay => each(&|b, s| hard(s, b)),
            Blend::Darken => each(&f32::min),
            Blend::Lighten => each(&f32::max),
            Blend::ColorDodge => each(&|b, s| {
                if b <= 0.0 {
                    0.0
                } else if s >= 1.0 {
                    1.0
                } else {
                    (b / (1.0 - s)).min(1.0)
                }
            }),
            Blend::ColorBurn => each(&|b, s| {
                if b >= 1.0 {
                    1.0
                } else if s <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - b) / s).min(1.0)
                }
            }),
            Blend::HardLight => each(&hard),
            Blend::SoftLight => each(&|b, s| {
                if s <= 0.5 {
                    b - (1.0 - 2.0 * s) * b * (1.0 - b)
                } else {
                    let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                    b + (2.0 * s - 1.0) * (d - b)
                }
            }),
            Blend::Difference => each(&|b, s| (b - s).abs()),
            Blend::Exclusion => each(&|b, s| b + s - 2.0 * b * s),
            Blend::PlusDarker => each(&|b, s| (b + s - 1.0).max(0.0)),
            Blend::PlusLighter => each(&|b, s| (b + s).min(1.0)),
            Blend::Hue => set_lum(set_sat(s, sat(b)), lum(b)),
            Blend::Saturation => set_lum(set_sat(b, sat(s)), lum(b)),
            Blend::Color => set_lum(s, lum(b)),
            Blend::Luminosity => set_lum(b, lum(s)),
        }
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    let c = [c[0] + d, c[1] + d, c[2] + d];
    let l = lum(c);
    let (low, high) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    let mut out = c;
    if low < 0.0 && l - low > 1e-6 {
        out = out.map(|v| l + (v - l) * l / (l - low));
    }
    if high > 1.0 && high - l > 1e-6 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (high - l));
    }
    out.map(|v| v.clamp(0.0, 1.0))
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (low, high) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    if high > low { c.map(|v| (v - low) * s / (high - low)) } else { [0.0; 3] }
}

/// A copy of the letters that `text-shadow` draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadow {
    pub dx: f32,
    pub dy: f32,
    pub blur: f32,
    /// `None`: the text's own colour.
    pub colour: Option<Paint>,
}

/// The shadows of a `text-shadow` value (the first 16).
pub fn shadows(v: &str, font: f32) -> Vec<Shadow> {
    crate::css::split_top(v, b',')
        .into_iter()
        .take(16)
        .filter_map(|one| {
            let (mut lengths, mut colour) = (vec![], None);
            for t in crate::css::tokens(one.trim()) {
                if let Some(n) = crate::css::length_of(t, font, font) {
                    lengths.push(n);
                } else if t != "currentcolor" {
                    colour = crate::css::paint(t);
                }
            }
            (lengths.len() >= 2).then(|| Shadow {
                dx: lengths[0],
                dy: lengths[1],
                blur: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
                colour,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn through(v: &str, p: Paint) -> Paint {
        Effects::apply(&Effects::inside(&None, filter(v, 16.0).ops), p)
    }

    #[test]
    fn filters_change_colours_as_webkit_draws_them() {
        let (black, white) = (Paint::Rgb(0, 0, 0), Paint::Rgb(255, 255, 255));
        assert_eq!(through("invert(1)", black), white);
        assert_eq!(through("invert(100%)", white), black);
        assert_eq!(through("invert(0.5)", black), Paint::Rgb(128, 128, 128));
        assert_eq!(through("brightness(0)", white), black);
        assert_eq!(through("brightness(300%)", Paint::Rgb(100, 100, 100)), white);
        assert_eq!(through("contrast(0)", black), Paint::Rgb(128, 128, 128));
        assert_eq!(through("grayscale(1)", Paint::Rgb(255, 0, 0)), Paint::Rgb(54, 54, 54));
        assert_eq!(through("hue-rotate(0deg)", Paint::Rgb(10, 20, 30)), Paint::Rgb(10, 20, 30));
        assert_eq!(through("saturate(1)", Paint::Rgb(10, 20, 30)), Paint::Rgb(10, 20, 30));
        assert_eq!(through("invert(1) invert(1)", Paint::Rgb(10, 20, 30)), Paint::Rgb(10, 20, 30));
        // Transparent stays so; a negative amount is no filter.
        assert_eq!(through("invert(1)", Paint::Clear), Paint::Clear);
        assert_eq!(through("invert(-1)", black), black);
        let f = filter("blur(3px) blur(4px) url(#F) url('x.svg#y')", 16.0);
        assert!((f.blur - 5.0).abs() < 1e-4);
        assert_eq!(f.urls, [Some("F".to_string()), None]);
        let f = filter("drop-shadow(1px 2px 0 red) invert(1)", 16.0);
        assert_eq!((f.shadows.len(), f.ops.len()), (1, 1));
        // Steps past what real mail uses: nothing drawn there is seen.
        let deep = (0..40).fold(None, |e, _| Effects::inside(&e, filter("invert(0)", 16.0).ops));
        assert_eq!(Effects::apply(&deep, black), Paint::Clear);
    }

    #[test]
    fn blending_as_the_spec_mixes() {
        let (b, w) = ([0.0; 3], [1.0; 3]);
        assert_eq!(Blend::Screen.mix(w, b), w);
        assert_eq!(Blend::Multiply.mix(b, w), b);
        assert_eq!(Blend::Difference.mix(w, w), b);
        assert_eq!(Blend::Lighten.mix(w, b), w);
        assert_eq!(Blend::Luminosity.mix(w, b), b);
        assert_eq!(Blend::Color.mix(w, [1.0, 0.0, 0.0]), w);
        assert_eq!(Blend::Exclusion.mix(w, b), w);
        assert_eq!(Blend::of("plus-lighter"), Some(Blend::PlusLighter));
        assert_eq!(Blend::of("normal"), None);
    }

    #[test]
    fn shadows_are_read() {
        let s = shadows("0 0 0 #000, 1px 2px red, 3px 4px 5px", 16.0);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].colour, Some(Paint::Rgb(0, 0, 0)));
        assert_eq!((s[1].dx, s[1].dy, s[1].blur), (1.0, 2.0, 0.0));
        assert_eq!(s[2].colour, None);
        assert!(shadows("none", 16.0).is_empty());
    }
}
