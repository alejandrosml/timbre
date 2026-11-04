//! The little CSS that decides whether a reader can see a piece of text.
//!
//! Mail draws HTML with WebKit. Text cannot be seen when its element or an
//! ancestor is not displayed, transparent, clipped to nothing or moved off
//! screen ([`hides_subtree`]), and also when the inherited properties that
//! reach it say so: a font too small to read, `visibility:hidden`, a text
//! colour equal to the background behind it ([`Look`]). Inherited
//! properties can be undone further down — layouts made with MJML put
//! `font-size:0` on every column and give the text inside its real size —
//! so they are followed down the tree instead of judged on one element.
//!
//! Declarations are read with cssparser, as a browser reads them (comments,
//! escapes, a broken declaration dropped alone, an invalid value leaving the
//! one before it in place); which rules apply is [`crate::sheet`]'s work.

use crate::effects::{Blend, Effects, Op, Shadow};
use std::hash::{Hash, Hasher};

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState, QualifiedRuleParser,
    RuleBodyItemParser, RuleBodyParser, ToCss, Token,
};

/// A colour, as far as telling text from its background goes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Rgb(u8, u8, u8),
    /// Transparent, or nearly.
    Clear,
    /// A background image or gradient: text of any colour may show on it.
    Image,
    /// A colour named in a way not decoded here, by a hash of its name.
    Other(u64),
}

impl Paint {
    /// Would text of this colour vanish on that background?
    pub fn same_as(self, background: Paint) -> bool {
        match (self, background) {
            (Paint::Rgb(r1, g1, b1), Paint::Rgb(r2, g2, b2)) => {
                r1.abs_diff(r2) <= 10 && g1.abs_diff(g2) <= 10 && b1.abs_diff(b2) <= 10
            }
            (Paint::Other(a), Paint::Other(b)) => a == b,
            _ => false,
        }
    }
}

/// The colour of a link nobody styled.
const LINK: Paint = Paint::Rgb(0, 0, 238);

/// CSS's named colours.
const NAMED: &[(&str, (u8, u8, u8))] = &[
    ("aliceblue", (240, 248, 255)),
    ("antiquewhite", (250, 235, 215)),
    ("aqua", (0, 255, 255)),
    ("aquamarine", (127, 255, 212)),
    ("azure", (240, 255, 255)),
    ("beige", (245, 245, 220)),
    ("bisque", (255, 228, 196)),
    ("black", (0, 0, 0)),
    ("blanchedalmond", (255, 235, 205)),
    ("blue", (0, 0, 255)),
    ("blueviolet", (138, 43, 226)),
    ("brown", (165, 42, 42)),
    ("burlywood", (222, 184, 135)),
    ("cadetblue", (95, 158, 160)),
    ("chartreuse", (127, 255, 0)),
    ("chocolate", (210, 105, 30)),
    ("coral", (255, 127, 80)),
    ("cornflowerblue", (100, 149, 237)),
    ("cornsilk", (255, 248, 220)),
    ("crimson", (220, 20, 60)),
    ("cyan", (0, 255, 255)),
    ("darkblue", (0, 0, 139)),
    ("darkcyan", (0, 139, 139)),
    ("darkgoldenrod", (184, 134, 11)),
    ("darkgray", (169, 169, 169)),
    ("darkgreen", (0, 100, 0)),
    ("darkgrey", (169, 169, 169)),
    ("darkkhaki", (189, 183, 107)),
    ("darkmagenta", (139, 0, 139)),
    ("darkolivegreen", (85, 107, 47)),
    ("darkorange", (255, 140, 0)),
    ("darkorchid", (153, 50, 204)),
    ("darkred", (139, 0, 0)),
    ("darksalmon", (233, 150, 122)),
    ("darkseagreen", (143, 188, 143)),
    ("darkslateblue", (72, 61, 139)),
    ("darkslategray", (47, 79, 79)),
    ("darkslategrey", (47, 79, 79)),
    ("darkturquoise", (0, 206, 209)),
    ("darkviolet", (148, 0, 211)),
    ("deeppink", (255, 20, 147)),
    ("deepskyblue", (0, 191, 255)),
    ("dimgray", (105, 105, 105)),
    ("dimgrey", (105, 105, 105)),
    ("dodgerblue", (30, 144, 255)),
    ("firebrick", (178, 34, 34)),
    ("floralwhite", (255, 250, 240)),
    ("forestgreen", (34, 139, 34)),
    ("fuchsia", (255, 0, 255)),
    ("gainsboro", (220, 220, 220)),
    ("ghostwhite", (248, 248, 255)),
    ("gold", (255, 215, 0)),
    ("goldenrod", (218, 165, 32)),
    ("gray", (128, 128, 128)),
    ("green", (0, 128, 0)),
    ("greenyellow", (173, 255, 47)),
    ("grey", (128, 128, 128)),
    ("honeydew", (240, 255, 240)),
    ("hotpink", (255, 105, 180)),
    ("indianred", (205, 92, 92)),
    ("indigo", (75, 0, 130)),
    ("ivory", (255, 255, 240)),
    ("khaki", (240, 230, 140)),
    ("lavender", (230, 230, 250)),
    ("lavenderblush", (255, 240, 245)),
    ("lawngreen", (124, 252, 0)),
    ("lemonchiffon", (255, 250, 205)),
    ("lightblue", (173, 216, 230)),
    ("lightcoral", (240, 128, 128)),
    ("lightcyan", (224, 255, 255)),
    ("lightgoldenrodyellow", (250, 250, 210)),
    ("lightgray", (211, 211, 211)),
    ("lightgreen", (144, 238, 144)),
    ("lightgrey", (211, 211, 211)),
    ("lightpink", (255, 182, 193)),
    ("lightsalmon", (255, 160, 122)),
    ("lightseagreen", (32, 178, 170)),
    ("lightskyblue", (135, 206, 250)),
    ("lightslategray", (119, 136, 153)),
    ("lightslategrey", (119, 136, 153)),
    ("lightsteelblue", (176, 196, 222)),
    ("lightyellow", (255, 255, 224)),
    ("lime", (0, 255, 0)),
    ("limegreen", (50, 205, 50)),
    ("linen", (250, 240, 230)),
    ("magenta", (255, 0, 255)),
    ("maroon", (128, 0, 0)),
    ("mediumaquamarine", (102, 205, 170)),
    ("mediumblue", (0, 0, 205)),
    ("mediumorchid", (186, 85, 211)),
    ("mediumpurple", (147, 112, 219)),
    ("mediumseagreen", (60, 179, 113)),
    ("mediumslateblue", (123, 104, 238)),
    ("mediumspringgreen", (0, 250, 154)),
    ("mediumturquoise", (72, 209, 204)),
    ("mediumvioletred", (199, 21, 133)),
    ("midnightblue", (25, 25, 112)),
    ("mintcream", (245, 255, 250)),
    ("mistyrose", (255, 228, 225)),
    ("moccasin", (255, 228, 181)),
    ("navajowhite", (255, 222, 173)),
    ("navy", (0, 0, 128)),
    ("oldlace", (253, 245, 230)),
    ("olive", (128, 128, 0)),
    ("olivedrab", (107, 142, 35)),
    ("orange", (255, 165, 0)),
    ("orangered", (255, 69, 0)),
    ("orchid", (218, 112, 214)),
    ("palegoldenrod", (238, 232, 170)),
    ("palegreen", (152, 251, 152)),
    ("paleturquoise", (175, 238, 238)),
    ("palevioletred", (219, 112, 147)),
    ("papayawhip", (255, 239, 213)),
    ("peachpuff", (255, 218, 185)),
    ("peru", (205, 133, 63)),
    ("pink", (255, 192, 203)),
    ("plum", (221, 160, 221)),
    ("powderblue", (176, 224, 230)),
    ("purple", (128, 0, 128)),
    ("red", (255, 0, 0)),
    ("rosybrown", (188, 143, 143)),
    ("royalblue", (65, 105, 225)),
    ("saddlebrown", (139, 69, 19)),
    ("salmon", (250, 128, 114)),
    ("sandybrown", (244, 164, 96)),
    ("seagreen", (46, 139, 87)),
    ("seashell", (255, 245, 238)),
    ("sienna", (160, 82, 45)),
    ("silver", (192, 192, 192)),
    ("skyblue", (135, 206, 235)),
    ("slateblue", (106, 90, 205)),
    ("slategray", (112, 128, 144)),
    ("slategrey", (112, 128, 144)),
    ("snow", (255, 250, 250)),
    ("springgreen", (0, 255, 127)),
    ("steelblue", (70, 130, 180)),
    ("tan", (210, 180, 140)),
    ("teal", (0, 128, 128)),
    ("thistle", (216, 191, 216)),
    ("tomato", (255, 99, 71)),
    ("turquoise", (64, 224, 208)),
    ("violet", (238, 130, 238)),
    ("wheat", (245, 222, 179)),
    ("white", (255, 255, 255)),
    ("whitesmoke", (245, 245, 245)),
    ("yellow", (255, 255, 0)),
    ("yellowgreen", (154, 205, 50)),
    ("rebeccapurple", (102, 51, 153)),
];

/// System colours WebKit knows on a Mac, as it draws them in light mode
/// (CSS Color 4 names, the deprecated ones mapped as CSS Color 4 says, and
/// Apple's own); translucent ones as they look on white.
const SYSTEM: &[(&str, (u8, u8, u8))] = &[
    ("canvas", (255, 255, 255)),
    ("canvastext", (0, 0, 0)),
    ("linktext", (0, 0, 238)),
    ("visitedtext", (85, 26, 139)),
    ("activetext", (255, 0, 0)),
    ("buttonface", (239, 239, 239)),
    ("buttontext", (38, 38, 38)),
    ("buttonborder", (255, 255, 255)),
    ("field", (255, 255, 255)),
    ("fieldtext", (38, 38, 38)),
    ("highlight", (181, 213, 255)),
    ("highlighttext", (0, 0, 0)),
    ("selecteditem", (211, 211, 211)),
    ("selecteditemtext", (0, 0, 0)),
    ("mark", (255, 255, 0)),
    ("marktext", (0, 0, 0)),
    ("graytext", (191, 191, 191)),
    ("accentcolor", (0, 122, 255)),
    ("accentcolortext", (0, 0, 0)),
    ("activebuttontext", (0, 0, 0)),
    ("-webkit-link", (0, 0, 238)),
    ("-webkit-activelink", (255, 0, 0)),
    ("-webkit-control-background", (255, 255, 255)),
    ("-apple-system-header-text", (38, 38, 38)),
    ("-apple-system-text-background", (255, 255, 255)),
    ("-apple-system-control-background", (255, 255, 255)),
    ("-apple-system-placeholder-text", (191, 191, 191)),
    ("-apple-system-label", (38, 38, 38)),
    ("-apple-system-secondary-label", (128, 128, 128)),
    ("-apple-system-tertiary-label", (189, 189, 189)),
    ("-apple-system-quaternary-label", (230, 230, 230)),
    ("-apple-system-quinary-label", (242, 242, 242)),
    ("-apple-system-grid", (230, 230, 230)),
    ("-apple-system-separator", (230, 230, 230)),
    ("-apple-system-container-border", (230, 230, 230)),
    ("-apple-system-background", (255, 255, 255)),
    ("-apple-system-secondary-background", (255, 255, 255)),
    ("-apple-system-tertiary-background", (255, 255, 255)),
    ("-apple-system-grouped-background", (255, 255, 255)),
    ("-apple-system-secondary-grouped-background", (255, 255, 255)),
    ("-apple-system-tertiary-grouped-background", (255, 255, 255)),
    ("-apple-system-selected-content-background", (0, 99, 225)),
    ("-apple-system-unemphasized-selected-content-background", (220, 220, 220)),
    ("-apple-wireless-playback-target-active", (0, 122, 255)),
    ("-apple-system-blue", (0, 122, 255)),
    ("-apple-system-brown", (162, 132, 94)),
    ("-apple-system-gray", (142, 142, 147)),
    ("-apple-system-green", (40, 205, 65)),
    ("-apple-system-orange", (255, 149, 0)),
    ("-apple-system-pink", (255, 45, 85)),
    ("-apple-system-purple", (175, 82, 222)),
    ("-apple-system-red", (255, 59, 48)),
    ("-apple-system-yellow", (255, 204, 0)),
    ("-apple-system-app-highlight-background", (255, 255, 0)),
    ("-apple-system-alternate-selected-text", (255, 255, 255)),
    ("-apple-system-control-accent", (0, 122, 255)),
    ("-apple-system-even-alternating-content-background", (255, 255, 255)),
    ("-apple-system-odd-alternating-content-background", (244, 245, 245)),
    ("-apple-system-selected-text", (0, 0, 0)),
    ("-apple-system-unemphasized-selected-text", (0, 0, 0)),
    ("-apple-system-selected-text-background", (179, 215, 255)),
    ("-apple-system-unemphasized-selected-text-background", (220, 220, 220)),
    ("-apple-system-find-highlight-background", (255, 255, 0)),
    // Deprecated, as CSS Color 4 maps them.
    ("activeborder", (255, 255, 255)),
    ("activecaption", (255, 255, 255)),
    ("appworkspace", (255, 255, 255)),
    ("background", (255, 255, 255)),
    ("buttonhighlight", (239, 239, 239)),
    ("buttonshadow", (239, 239, 239)),
    ("captiontext", (0, 0, 0)),
    ("inactiveborder", (255, 255, 255)),
    ("inactivecaption", (255, 255, 255)),
    ("inactivecaptiontext", (191, 191, 191)),
    ("infobackground", (255, 255, 255)),
    ("infotext", (0, 0, 0)),
    ("menu", (255, 255, 255)),
    ("menutext", (0, 0, 0)),
    ("scrollbar", (255, 255, 255)),
    ("threeddarkshadow", (255, 255, 255)),
    ("threedface", (239, 239, 239)),
    ("threedhighlight", (255, 255, 255)),
    ("threedlightshadow", (255, 255, 255)),
    ("threedshadow", (255, 255, 255)),
    ("window", (255, 255, 255)),
    ("windowframe", (255, 255, 255)),
    ("windowtext", (0, 0, 0)),
];

fn hash_of(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn rgb_or_clear(r: f32, g: f32, b: f32, alpha: f32) -> Paint {
    if alpha < 0.1 {
        return Paint::Clear;
    }
    let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    Paint::Rgb(c(r), c(g), c(b))
}

fn hex(h: &str) -> Option<Paint> {
    if !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize, n: usize| u8::from_str_radix(&h[i..i + n], 16).ok().map(f32::from);
    match h.len() {
        3 | 4 => {
            let x = |i: usize| d(i, 1).map(|v| v * 17.0);
            let alpha = if h.len() == 4 { x(3)? / 255.0 } else { 1.0 };
            Some(rgb_or_clear(x(0)?, x(1)?, x(2)?, alpha))
        }
        6 | 8 => {
            let alpha = if h.len() == 8 { d(6, 2)? / 255.0 } else { 1.0 };
            Some(rgb_or_clear(d(0, 2)?, d(2, 2)?, d(4, 2)?, alpha))
        }
        _ => None,
    }
}

/// One component of a functional colour.
#[derive(Clone, Copy, Debug)]
enum Comp {
    Num(f32),
    Pct(f32),
    /// An angle, in degrees.
    Deg(f32),
    /// `none`: a missing component, counted as zero.
    Missing,
}

impl Comp {
    /// The component as a number where 100% is `full`.
    fn scaled(self, full: f32) -> Option<f32> {
        match self {
            Comp::Num(n) => Some(n),
            Comp::Pct(p) => Some(p * full / 100.0),
            Comp::Missing => Some(0.0),
            Comp::Deg(_) => None,
        }
    }

    /// A hue, in degrees.
    fn hue(self) -> Option<f32> {
        match self {
            Comp::Num(n) | Comp::Deg(n) => Some(n),
            Comp::Missing => Some(0.0),
            Comp::Pct(_) => None,
        }
    }

    /// An alpha value from 0 to 1.
    fn alpha(self) -> Option<f32> {
        self.scaled(1.0).map(|a| a.clamp(0.0, 1.0))
    }
}

/// A number, percentage or angle as colours write them, math functions
/// included; in relative colours (`rgb(from … r g b)`), `words` gives the
/// channels of the colour it starts from. Anything else (`nan`, a stray
/// word) makes the colour invalid.
fn component(t: &str, words: Option<crate::calc::Words<'_>>) -> Option<Comp> {
    let t = t.trim();
    if t == "none" {
        return Some(Comp::Missing);
    }
    let v = crate::calc::eval(t, crate::calc::Ctx { pct: crate::calc::Pct::Keep, font: 16.0, words })?;
    let n = v.n as f32;
    match v.kind {
        crate::calc::Kind::Number => Some(Comp::Num(n)),
        crate::calc::Kind::Percent => Some(Comp::Pct(n)),
        crate::calc::Kind::Angle => Some(Comp::Deg(n)),
        _ => None,
    }
}

/// The channels of a functional colour and its alpha: `r, g, b[, a]` or
/// `r g b [/ a]`.
fn channels(inner: &str) -> Option<(Vec<Comp>, Option<Comp>)> {
    channels_with(inner, None)
}

fn channels_with(inner: &str, words: Option<crate::calc::Words<'_>>) -> Option<(Vec<Comp>, Option<Comp>)> {
    let commas = split_top(inner, b',');
    let (parts, alpha): (Vec<&str>, Option<&str>) = if commas.len() > 1 {
        let mut parts = commas;
        let alpha = if parts.len() == 4 { parts.pop() } else { None };
        (parts, alpha)
    } else {
        let slash = split_top(inner, b'/');
        match slash[..] {
            [main] => (tokens(main), None),
            [main, alpha] => (tokens(main), Some(alpha)),
            _ => return None,
        }
    };
    if parts.len() != 3 {
        return None;
    }
    // Relative colours have no commas.
    if words.is_some() && commas_used(inner) {
        return None;
    }
    let comps = parts.iter().map(|p| component(p, words)).collect::<Option<Vec<_>>>()?;
    let alpha = match alpha {
        Some(a) => Some(component(a, words)?),
        None => None,
    };
    Some((comps, alpha))
}

fn commas_used(inner: &str) -> bool {
    split_top(inner, b',').len() > 1
}

/// A linear-light sRGB channel to its 0-255 encoded value.
fn encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    255.0 * if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// CIE XYZ (D65) to encoded sRGB, 0-255.
fn xyz_d65_to_rgb(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let r = 3.240_97 * x - 1.537_383 * y - 0.498_611 * z;
    let g = -0.969_244 * x + 1.875_968 * y + 0.041_555 * z;
    let b = 0.055_63 * x - 0.203_977 * y + 1.056_972 * z;
    (encode(r), encode(g), encode(b))
}

/// CIE XYZ (D50) to D65, by the Bradford transform.
fn d50_to_d65(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    (
        0.955_473_4 * x - 0.023_098_5 * y + 0.063_259_3 * z,
        -0.028_369_7 * x + 1.009_995_4 * y + 0.021_041_4 * z,
        0.012_314 * x - 0.020_507_7 * y + 1.330_365_9 * z,
    )
}

/// CIE Lab (D50) to encoded sRGB.
fn lab_to_rgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let f = |t: f32| if t.powi(3) > 0.008_856 { t.powi(3) } else { (116.0 * t - 16.0) / 903.3 };
    let (x, y, z) = d50_to_d65(0.964_22 * f(fx), f(fy), 0.825_21 * f(fz));
    xyz_d65_to_rgb(x, y, z)
}

/// OKLab to encoded sRGB.
fn oklab_to_rgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let r = 4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_;
    let g = -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_;
    let bl = -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_;
    (encode(r), encode(g), encode(bl))
}

/// HWB to encoded sRGB.
fn hwb_to_rgb(h: f32, w: f32, b: f32) -> (f32, f32, f32) {
    if w + b >= 1.0 {
        let gray = 255.0 * w / (w + b);
        return (gray, gray, gray);
    }
    let (r, g, bl) = hsl_to_rgb(h, 1.0, 0.5);
    let f = |c: f32| c * (1.0 - w - b) + 255.0 * w;
    (f(r), f(g), f(bl))
}

/// sRGB (0-255) to linear light, 0-1.
fn decode(c: f32) -> f32 {
    let c = c / 255.0;
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Encoded sRGB to HSL: hue in degrees, saturation and lightness 0-100.
fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (r, g, b) = (r / 255.0, g / 255.0, b / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l * 100.0);
    }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s * 100.0, l * 100.0)
}

/// Encoded sRGB to CIE Lab (D50).
fn rgb_to_lab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (r, g, b) = (decode(r), decode(g), decode(b));
    // Linear sRGB to XYZ (D65), then to D50 by Bradford.
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;
    let (x, y, z) = (
        1.047_929_8 * x + 0.022_946_8 * y - 0.050_192_2 * z,
        0.029_627_8 * x + 0.990_434_5 * y - 0.017_073_8 * z,
        -0.009_243_1 * x + 0.015_055_2 * y + 0.751_874_3 * z,
    );
    let f = |t: f32| if t > 0.008_856 { t.cbrt() } else { (903.3 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x / 0.964_22), f(y), f(z / 0.825_21));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// Encoded sRGB to OKLab.
fn rgb_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (r, g, b) = (decode(r), decode(g), decode(b));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    (
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    )
}

/// A relative colour, `rgb(from <colour> r g b / alpha)` and the like: the
/// channels of the colour it starts from, named, in formulas.
fn relative_colour(name: &str, inner: &str) -> Option<Paint> {
    let rest = inner.trim().strip_prefix("from ")?;
    let mut parts = tokens(rest).into_iter();
    let origin = parts.next()?;
    let (r, g, b, alpha) = match paint(origin)? {
        Paint::Rgb(r, g, b) => (f32::from(r), f32::from(g), f32::from(b), 1.0f32),
        Paint::Clear => (0.0, 0.0, 0.0, 0.0),
        _ => return None,
    };
    let rest = rest[rest.find(origin)? + origin.len()..].trim();
    let polar = |a: f32, b: f32| ((a * a + b * b).sqrt(), b.atan2(a).to_degrees().rem_euclid(360.0));
    // The channels of the colour, by the names this function uses.
    let (keys, values, rest): ([&str; 3], [f32; 3], &str) = match name {
        "rgb" | "rgba" => (["r", "g", "b"], [r, g, b], rest),
        "hsl" | "hsla" => {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            (["h", "s", "l"], [h, s, l], rest)
        }
        "hwb" => {
            let (h, _, _) = rgb_to_hsl(r, g, b);
            let (max, min) = (r.max(g).max(b), r.min(g).min(b));
            (["h", "w", "b"], [h, min / 2.55, 100.0 - max / 2.55], rest)
        }
        "lab" => {
            let (l, a, bb) = rgb_to_lab(r, g, b);
            (["l", "a", "b"], [l, a, bb], rest)
        }
        "lch" => {
            let (l, a, bb) = rgb_to_lab(r, g, b);
            let (c, h) = polar(a, bb);
            (["l", "c", "h"], [l, c, h], rest)
        }
        "oklab" => {
            let (l, a, bb) = rgb_to_oklab(r, g, b);
            (["l", "a", "b"], [l, a, bb], rest)
        }
        "oklch" => {
            let (l, a, bb) = rgb_to_oklab(r, g, b);
            let (c, h) = polar(a, bb);
            (["l", "c", "h"], [l, c, h], rest)
        }
        "color" => {
            let (space, rest) = rest.split_once(' ')?;
            if space != "srgb" {
                return None;
            }
            (["r", "g", "b"], [r / 255.0, g / 255.0, b / 255.0], rest)
        }
        _ => return None,
    };
    let words = |w: &str| -> Option<f64> {
        if w == "alpha" {
            return Some(f64::from(alpha));
        }
        keys.iter().position(|k| *k == w).map(|i| f64::from(values[i]))
    };
    let (c, a) = channels_with(rest, Some(&words))?;
    let (c0, c1, c2) = (comp_text(c[0]), comp_text(c[1]), comp_text(c[2]));
    let a = a.map_or(alpha.to_string(), comp_text);
    paint(&if name == "color" {
        format!("color(srgb {c0} {c1} {c2} / {a})")
    } else {
        format!("{name}({c0} {c1} {c2} / {a})")
    })
}

/// A worked-out channel, written back.
fn comp_text(c: Comp) -> String {
    match c {
        Comp::Num(n) => n.to_string(),
        Comp::Pct(p) => format!("{p}%"),
        Comp::Deg(d) => format!("{d}deg"),
        Comp::Missing => "none".to_string(),
    }
}

/// A colour function: rgb(), hsl(), hwb(), lab(), lch(), oklab(), oklch(),
/// color(), color-mix(), light-dark() and contrast-color(), relative
/// colours included. `None` when it is not valid.
fn colour_function(name: &str, inner: &str) -> Option<Paint> {
    if name == "color-mix" {
        return colour_mix(inner);
    }
    if name == "light-dark" {
        // Mail in light mode: the first.
        let [light, dark] = split_top(inner, b',')[..] else { return None };
        paint(dark.trim())?;
        return paint(light.trim());
    }
    if name == "contrast-color" {
        return match paint(inner.trim())? {
            Paint::Rgb(r, g, b) => {
                let lum = 0.2126 * decode(f32::from(r)) + 0.7152 * decode(f32::from(g)) + 0.0722 * decode(f32::from(b));
                Some(if lum > 0.179 { Paint::Rgb(0, 0, 0) } else { Paint::Rgb(255, 255, 255) })
            }
            _ => Some(Paint::Rgb(0, 0, 0)),
        };
    }
    if inner.trim_start().starts_with("from ") {
        return relative_colour(name, inner);
    }
    if name == "color" {
        let (space, rest) = inner.trim().split_once(char::is_whitespace)?;
        let (c, alpha) = channels(rest)?;
        let (x, y, z) = (c[0].scaled(1.0)?, c[1].scaled(1.0)?, c[2].scaled(1.0)?);
        let (r, g, b) = match space {
            // Wide-gamut spaces read as sRGB: near enough for white and black.
            "srgb" | "display-p3" | "a98-rgb" | "prophoto-rgb" | "rec2020" => (x * 255.0, y * 255.0, z * 255.0),
            "srgb-linear" => (encode(x), encode(y), encode(z)),
            "xyz" | "xyz-d65" => xyz_d65_to_rgb(x, y, z),
            "xyz-d50" => {
                let (x, y, z) = d50_to_d65(x, y, z);
                xyz_d65_to_rgb(x, y, z)
            }
            _ => return None,
        };
        let alpha = alpha.map_or(Some(1.0), Comp::alpha)?;
        return [r, g, b, alpha].iter().all(|v| v.is_finite()).then(|| rgb_or_clear(r, g, b, alpha));
    }
    let (c, alpha) = channels(inner)?;
    let alpha = alpha.map_or(Some(1.0), Comp::alpha)?;
    let (r, g, b) = match name {
        "rgb" | "rgba" => {
            let ch = |i: usize| match c[i] {
                Comp::Pct(p) => Some(p * 2.55),
                other => other.scaled(255.0),
            };
            (ch(0)?, ch(1)?, ch(2)?)
        }
        "hsl" | "hsla" => hsl_to_rgb(c[0].hue()?, c[1].scaled(100.0)? / 100.0, c[2].scaled(100.0)? / 100.0),
        "hwb" => hwb_to_rgb(c[0].hue()?, c[1].scaled(100.0)? / 100.0, c[2].scaled(100.0)? / 100.0),
        "lab" => lab_to_rgb(c[0].scaled(100.0)?, c[1].scaled(125.0)?, c[2].scaled(125.0)?),
        "lch" => {
            let (l, ch, h) = (c[0].scaled(100.0)?, c[1].scaled(150.0)?, c[2].hue()?.to_radians());
            lab_to_rgb(l, ch * h.cos(), ch * h.sin())
        }
        "oklab" => oklab_to_rgb(c[0].scaled(1.0)?, c[1].scaled(0.4)?, c[2].scaled(0.4)?),
        "oklch" => {
            let (l, ch, h) = (c[0].scaled(1.0)?, c[1].scaled(0.4)?, c[2].hue()?.to_radians());
            oklab_to_rgb(l, ch * h.cos(), ch * h.sin())
        }
        _ => return None,
    };
    [r, g, b, alpha].iter().all(|v| v.is_finite()).then(|| rgb_or_clear(r, g, b, alpha))
}

/// `color-mix(in srgb, white 30%, black)`, mixed in sRGB whatever the space
/// named: near enough to tell text from its background.
fn colour_mix(inner: &str) -> Option<Paint> {
    let parts = split_top(inner, b',');
    let [space, a, b] = parts[..] else { return None };
    if !space.trim().starts_with("in ") {
        return None;
    }
    let side = |s: &str| -> Option<(Paint, Option<f32>)> {
        let mut colour = None;
        let mut pct = None;
        for t in tokens(s.trim()) {
            match number_prefix(t) {
                Some((n, "%")) => pct = Some(n.clamp(0.0, 100.0)),
                _ => colour = Some(paint(t)?),
            }
        }
        Some((colour?, pct))
    };
    let ((ca, pa), (cb, pb)) = (side(a)?, side(b)?);
    let (pa, pb) = match (pa, pb) {
        (Some(x), Some(y)) => (x, y),
        (Some(x), None) => (x, 100.0 - x),
        (None, Some(y)) => (100.0 - y, y),
        (None, None) => (50.0, 50.0),
    };
    let total = pa + pb;
    if total <= 0.0 {
        return None;
    }
    let rgba = |p: Paint| match p {
        Paint::Rgb(r, g, b) => Some((f32::from(r), f32::from(g), f32::from(b), 1.0)),
        Paint::Clear => Some((0.0, 0.0, 0.0, 0.0)),
        _ => None,
    };
    let ((r1, g1, b1, a1), (r2, g2, b2, a2)) = (rgba(ca)?, rgba(cb)?);
    let (wa, wb) = (pa / total * a1, pb / total * a2);
    if wa + wb <= 0.0 {
        return Some(Paint::Clear);
    }
    let alpha = (wa + wb) * total.min(100.0) / 100.0;
    let mix = |x: f32, y: f32| (x * wa + y * wb) / (wa + wb);
    Some(rgb_or_clear(mix(r1, r2), mix(g1, g2), mix(b1, b2), alpha))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    if s <= 0.0 {
        return (l * 255.0, l * 255.0, l * 255.0);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        v * 255.0
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

/// A CSS colour value. `None` for keywords that keep the inherited colour
/// (`inherit`, `currentcolor`…), for `none` and for what is not a colour.
pub fn paint(v: &str) -> Option<Paint> {
    let v = v.trim().to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    if v == "transparent" {
        return Some(Paint::Clear);
    }
    if let Some(h) = v.strip_prefix('#') {
        return hex(h);
    }
    if let Some(open) = v.find('(') {
        let inner = v[open + 1..].strip_suffix(')')?;
        return colour_function(&v[..open], inner);
    }
    if let Some((_, (r, g, b))) = NAMED.iter().chain(SYSTEM).find(|(n, _)| *n == v) {
        return Some(Paint::Rgb(*r, *g, *b));
    }
    // In quirks mode only (see `valid`).
    if v == "-webkit-focus-ring-color" {
        return Some(Paint::Other(hash_of(&v)));
    }
    None
}

/// A colour attribute (`bgcolor`, `<font color>`): like CSS, and also
/// six hex digits without the `#`, as browsers accept there.
fn paint_attr(v: &str) -> Option<Paint> {
    let t = v.trim();
    if t.len() == 6 && t.bytes().all(|c| c.is_ascii_hexdigit()) {
        return hex(t);
    }
    paint(t)
}

/// A CSS number at the start of `v` (sign, decimals, exponent: `1e-9`), and
/// what follows it.
fn number_prefix(v: &str) -> Option<(f32, &str)> {
    let b = v.as_bytes();
    let mut i = usize::from(matches!(b.first(), Some(b'+' | b'-')));
    let start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if !b[start..i].iter().any(u8::is_ascii_digit) {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(b.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        let digits = j;
        while b.get(j).is_some_and(u8::is_ascii_digit) {
            j += 1;
        }
        if j > digits {
            i = j; // "1em" is a unit, not an exponent
        }
    }
    let n: f32 = v[..i].parse().ok()?;
    n.is_finite().then_some((n, &v[i..]))
}

/// Top-level parts of `s` between `sep`, outside parentheses.
pub(crate) fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (vec![], 0i32, 0);
    for (i, c) in s.bytes().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// A length in CSS pixels; `em` and `%` count from `base`. A number without
/// a unit counts as pixels (as old pages and quirks mode read it).
fn length_px(v: &str, base: f32) -> Option<f32> {
    length_of(v, base, base)
}

/// A length in CSS pixels, `em` counting from `font` and `%` from
/// `percent_of`. A number without a unit counts as pixels (as old pages and
/// quirks mode read it).
pub(crate) fn length_of(v: &str, font: f32, percent_of: f32) -> Option<f32> {
    let v = v.trim();
    if v.contains('(') {
        return crate::calc::length(v, f64::from(font), f64::from(percent_of)).map(|n| n as f32);
    }
    let (n, unit) = number_prefix(v)?;
    let unit = unit.trim().to_ascii_lowercase();
    match unit.as_str() {
        "" => Some(n),
        "%" => Some(n * percent_of / 100.0),
        u => crate::calc::dimension_px(f64::from(n), u, f64::from(font)).map(|n| n as f32),
    }
}

/// The pixel size a `font-size` value gives, from the parent's. A number
/// without a unit is ignored by WebKit outside quirks mode.
fn font_px(v: &str, parent: f32, quirks: bool) -> Option<f32> {
    let t = v.trim();
    if !quirks
        && !t.is_empty()
        && t.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && t.parse::<f32>().is_ok_and(|n| n != 0.0)
    {
        return None;
    }
    Some(match t {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" | "initial" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        "smaller" => parent / 1.2,
        "larger" => parent * 1.2,
        "inherit" | "unset" | "revert" => parent,
        // A formula's result is clamped at zero; a negative size written
        // out is invalid, and WebKit drops the declaration.
        other if other.contains('(') => length_px(other, parent)?.max(0.0),
        other => length_px(other, parent).filter(|n| *n >= 0.0)?,
    })
}

/// The size in a `font` shorthand such as `bold 14px/1.4 Arial` or `0/0 a`.
/// Without a font family after the size (`font:0`), WebKit drops it all.
fn font_shorthand_px(v: &str, parent: f32) -> Option<f32> {
    let tokens: Vec<&str> = v.split_whitespace().collect();
    tokens.iter().enumerate().find_map(|(i, token)| {
        let size = token.split('/').next().unwrap_or(token);
        let unit_ok =
            size == "0" || size.contains('(') || size.ends_with(|c: char| c.is_ascii_alphabetic() || c == '%');
        if !unit_ok || size.parse::<f32>().is_ok_and(|n| n != 0.0) || i + 1 >= tokens.len() {
            return None;
        }
        font_px(size, parent, false)
    })
}

/// Sizes of `<font size=1..7>` (and `+1`, `-1` from the default 3).
fn font_tag_px(v: &str) -> Option<f32> {
    const SIZES: [f32; 7] = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];
    let t = v.trim();
    let n: i64 = t.parse().ok()?;
    let level = if t.starts_with(['+', '-']) { 3i64.saturating_add(n) } else { n };
    Some(SIZES[(level.clamp(1, 7) - 1) as usize])
}

/// Font size an element starts from before its own styles, as a factor of
/// its parent's (WebKit's default style sheet).
fn default_scale(name: &str) -> f32 {
    match name {
        "h1" => 2.0,
        "h2" => 1.5,
        "h3" => 1.17,
        "h5" => 0.83,
        "h6" => 0.67,
        "small" => 1.0 / 1.2,
        "big" => 1.2,
        "sub" | "sup" => 1.0 / 1.2,
        _ => 1.0,
    }
}

/// Properties that can make text invisible.
const PROPS: &[&str] = &[
    "display",
    "visibility",
    "opacity",
    "font",
    "font-size",
    "color",
    "background",
    "background-color",
    "background-image",
    "overflow",
    "overflow-x",
    "overflow-y",
    "max-height",
    "height",
    "width",
    "max-width",
    "clip",
    "clip-path",
    "text-indent",
    "position",
    "left",
    "top",
    "right",
    "bottom",
    "margin",
    "margin-left",
    "margin-top",
    "-webkit-text-fill-color",
    "transform",
    "scale",
    "zoom",
    "filter",
    "content-visibility",
    "translate",
    "mask",
    "mask-image",
    "background-clip",
    "z-index",
    "isolation",
    "animation",
    "animation-name",
    "animation-duration",
    "animation-delay",
    "animation-iteration-count",
    "animation-direction",
    "animation-fill-mode",
    "animation-play-state",
    "min-height",
    "container",
    "container-type",
    "container-name",
    "min-width",
    "mix-blend-mode",
    "-webkit-text-security",
    "text-shadow",
    "-webkit-text-stroke",
    "-webkit-text-stroke-width",
    "-webkit-text-stroke-color",
    "contain",
    "will-change",
    "backdrop-filter",
    "color-scheme",
    "rotate",
    "perspective",
    "transform-style",
    "offset-path",
    "float",
    "padding",
    "padding-left",
    "padding-right",
    "border",
    "border-left",
    "border-right",
    "box-sizing",
    "margin-right",
    "white-space",
    "white-space-collapse",
    "hyphens",
    "word-break",
    "overflow-wrap",
    "padding-inline",
    "margin-inline",
    "border-inline",
    "border-width",
    "border-style",
    "border-left-width",
    "border-right-width",
    "border-left-style",
    "border-right-style",
];

/// Properties whose values name things (animations, containers): case
/// matters in those names.
const KEEP_CASE: &[&str] = &["animation", "animation-name", "container", "container-name"];

/// Prefixed names WebKit takes for the same property.
fn alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "-webkit-transform" => "transform",
        "-webkit-filter" => "filter",
        "-webkit-clip-path" => "clip-path",
        "-webkit-mask" => "mask",
        "-webkit-mask-image" => "mask-image",
        "-webkit-backdrop-filter" => "backdrop-filter",
        "word-wrap" => "overflow-wrap",
        "inline-size" => "width",
        "min-inline-size" => "min-width",
        "max-inline-size" => "max-width",
        "block-size" => "height",
        "min-block-size" => "min-height",
        "max-block-size" => "max-height",
        "padding-inline-start" => "padding-left",
        "padding-inline-end" => "padding-right",
        "margin-inline-start" => "margin-left",
        "margin-inline-end" => "margin-right",
        "margin-block-start" => "margin-top",
        "border-inline-start" => "border-left",
        "border-inline-end" => "border-right",
        "border-inline-start-width" => "border-left-width",
        "border-inline-end-width" => "border-right-width",
        "border-inline-start-style" => "border-left-style",
        "border-inline-end-style" => "border-right-style",
        "inset-inline-start" => "left",
        "inset-inline-end" => "right",
        "inset-block-start" => "top",
        "inset-block-end" => "bottom",
        "-webkit-hyphens" => "hyphens",
        "-webkit-background-clip" => "background-clip",
        "-webkit-animation" => "animation",
        "-webkit-animation-name" => "animation-name",
        "-webkit-animation-duration" => "animation-duration",
        "-webkit-animation-delay" => "animation-delay",
        "-webkit-animation-iteration-count" => "animation-iteration-count",
        "-webkit-animation-direction" => "animation-direction",
        "-webkit-animation-fill-mode" => "animation-fill-mode",
        "-webkit-animation-play-state" => "animation-play-state",
        _ => return None,
    })
}

/// The longhands a shorthand sets, among [`PROPS`].
fn longhands(p: &str) -> &'static [&'static str] {
    match p {
        "font" => &["font-size"],
        "background" => &["background-color", "background-image", "background-clip"],
        "overflow" => &["overflow-x", "overflow-y"],
        "margin" => &["margin-left", "margin-top", "margin-right"],
        "padding" => &["padding-left", "padding-right", "padding-inline"],
        "padding-inline" => &["padding-left", "padding-right"],
        "margin-inline" => &["margin-left", "margin-right"],
        "border" => &[
            "border-left",
            "border-right",
            "border-inline",
            "border-width",
            "border-style",
            "border-left-width",
            "border-right-width",
            "border-left-style",
            "border-right-style",
        ],
        "border-inline" | "border-width" | "border-style" => {
            &["border-left-width", "border-right-width", "border-left-style", "border-right-style"]
        }
        "border-left" => &["border-left-width", "border-left-style"],
        "border-right" => &["border-right-width", "border-right-style"],
        "white-space" => &["white-space-collapse"],
        "animation" => &[
            "animation-name",
            "animation-duration",
            "animation-delay",
            "animation-iteration-count",
            "animation-direction",
            "animation-fill-mode",
            "animation-play-state",
        ],
        "container" => &["container-name", "container-type"],
        "-webkit-text-stroke" => &["-webkit-text-stroke-width", "-webkit-text-stroke-color"],
        _ => &[],
    }
}

/// The shorthand that also sets this longhand.
fn shorthand(p: &str) -> Option<&'static str> {
    match p {
        "font-size" => Some("font"),
        "background-color" | "background-image" | "background-clip" => Some("background"),
        "overflow-x" | "overflow-y" => Some("overflow"),
        "margin-left" | "margin-top" | "margin-right" => Some("margin"),
        "padding-left" | "padding-right" => Some("padding"),
        "border-left" | "border-right" => Some("border"),
        "white-space-collapse" => Some("white-space"),
        "animation-name"
        | "animation-duration"
        | "animation-delay"
        | "animation-iteration-count"
        | "animation-direction"
        | "animation-fill-mode"
        | "animation-play-state" => Some("animation"),
        "container-name" | "container-type" => Some("container"),
        "-webkit-text-stroke-width" | "-webkit-text-stroke-color" => Some("-webkit-text-stroke"),
        _ => None,
    }
}

/// `display` values WebKit takes alone.
const DISPLAY: &[&str] = &[
    "none",
    "contents",
    "block",
    "inline",
    "flow",
    "flow-root",
    "table",
    "flex",
    "grid",
    "grid-lanes",
    "list-item",
    "inline-block",
    "inline-flex",
    "inline-grid",
    "inline-grid-lanes",
    "inline-table",
    "table-row",
    "table-cell",
    "table-row-group",
    "table-header-group",
    "table-footer-group",
    "table-column",
    "table-column-group",
    "table-caption",
    "-webkit-box",
    "-webkit-inline-box",
    "-webkit-flex",
    "-webkit-inline-flex",
];
const CLIP_BOXES: &[&str] =
    &["margin-box", "border-box", "padding-box", "content-box", "fill-box", "stroke-box", "view-box"];
/// Values worked out only once an element's other properties are known:
/// `var()`, `env()`, `if()` and the tree-counting functions.
fn substitutes(v: &str) -> bool {
    ["var(", "env(", "if(", "sibling-index(", "sibling-count("].iter().any(|f| v.contains(f))
}

/// `mix-blend-mode` values.
const BLEND_MODES: &[&str] = &[
    "normal",
    "multiply",
    "screen",
    "overlay",
    "darken",
    "lighten",
    "color-dodge",
    "color-burn",
    "hard-light",
    "soft-light",
    "difference",
    "exclusion",
    "hue",
    "saturation",
    "color",
    "luminosity",
    "plus-darker",
    "plus-lighter",
];

/// `text-shadow`: shadows of two or three lengths and a colour.
fn shadow_valid(v: &str) -> bool {
    split_top(v, b',').into_iter().all(|shadow| {
        let parts = tokens(shadow.trim());
        let lengths = parts.iter().filter(|p| length_ok(p, false, false)).count();
        let colours = parts.iter().filter(|p| paint(p).is_some() || **p == "currentcolor").count();
        (2..=3).contains(&lengths) && colours <= 1 && lengths + colours == parts.len()
    })
}

/// `display`: one keyword, or an outer and an inner one (`block flex`,
/// `inline flow-root list-item`); `none` and `contents` stand alone.
fn display_valid(v: &str) -> bool {
    // An outer and an inner keyword, in either order; WebKit takes neither
    // run-in, ruby nor list-item with others.
    let outer = |w: &str| matches!(w, "block" | "inline");
    let inner = |w: &str| matches!(w, "flow" | "flow-root" | "table" | "flex" | "grid" | "grid-lanes");
    match v.split(' ').collect::<Vec<_>>()[..] {
        [one] => DISPLAY.contains(&one),
        [a, b] => (outer(a) && inner(b)) || (inner(a) && outer(b)),
        _ => false,
    }
}

/// A length or percentage as WebKit takes it: a unit it knows, a bare
/// number only when zero (or in quirks mode), a formula giving a length.
fn length_ok(t: &str, quirks: bool, percent: bool) -> bool {
    if t.contains('(') {
        return match crate::calc::eval(
            t,
            crate::calc::Ctx {
                pct: if percent { crate::calc::Pct::Length(100.0) } else { crate::calc::Pct::None },
                ..Default::default()
            },
        ) {
            Some(v) => v.kind == crate::calc::Kind::Length,
            None => false,
        };
    }
    match number_prefix(t) {
        Some((n, "")) => n == 0.0 || quirks,
        Some((_, "%")) => percent,
        Some((_, unit)) => crate::calc::dimension_px(1.0, unit, 16.0).is_some(),
        None => false,
    }
}

/// A number (or, with `percent`, a percentage), formulas included.
fn number_ok(t: &str, percent: bool) -> bool {
    if t.contains('(') {
        let pct = if percent { crate::calc::Pct::Number } else { crate::calc::Pct::None };
        return crate::calc::number_value(t, pct).is_some();
    }
    number_prefix(t).is_some_and(|(_, u)| u.is_empty() || (percent && u == "%"))
}

/// An angle, or zero.
fn angle_ok(t: &str) -> bool {
    angle(t).is_some()
}

/// The name and arguments of a call: `rotate(1deg)` gives ("rotate", ["1deg"]).
fn call(t: &str) -> Option<(&str, Vec<&str>)> {
    let open = t.find('(')?;
    let inner = t[open + 1..].strip_suffix(')')?;
    let args =
        if inner.trim().is_empty() { vec![] } else { split_top(inner, b',').into_iter().map(str::trim).collect() };
    Some((&t[..open], args))
}

/// `transform`: functions WebKit knows, with arguments of the right kind.
fn transform_valid(v: &str) -> bool {
    let lp = |a: &str| length_ok(a, false, true);
    let len = |a: &str| length_ok(a, false, false);
    let num = |a: &str| number_ok(a, false);
    let scale = |a: &str| number_ok(a, true);
    let all = |args: &[&str], f: &dyn Fn(&str) -> bool| args.iter().all(|a| f(a));
    let list = tokens(v);
    !list.is_empty()
        && list.into_iter().all(|t| {
            let Some((name, args)) = call(t) else { return false };
            match (name, args.len()) {
                ("matrix", 6) | ("matrix3d", 16) => all(&args, &num),
                ("translate", 1 | 2) => all(&args, &lp),
                ("translatex" | "translatey", 1) => lp(args[0]),
                ("translatez", 1) => len(args[0]),
                ("translate3d", 3) => lp(args[0]) && lp(args[1]) && len(args[2]),
                ("scale", 1 | 2) | ("scalex" | "scaley" | "scalez", 1) | ("scale3d", 3) => all(&args, &scale),
                ("rotate" | "rotatex" | "rotatey" | "rotatez", 1) => angle_ok(args[0]),
                ("rotate3d", 4) => num(args[0]) && num(args[1]) && num(args[2]) && angle_ok(args[3]),
                ("skew", 1 | 2) => all(&args, &angle_ok),
                ("skewx" | "skewy", 1) => angle_ok(args[0]),
                ("perspective", 1) => args[0] == "none" || len(args[0]),
                _ => false,
            }
        })
}

/// `filter`: functions WebKit knows, with arguments of the right kind.
fn filter_valid(v: &str) -> bool {
    let list = tokens(v);
    !list.is_empty()
        && list.into_iter().all(|t| {
            if t.starts_with("url(") {
                return true;
            }
            let Some((name, args)) = call(t) else { return false };
            let one = args.first().copied();
            match (name, args.len()) {
                (_, n) if n > 1 => false,
                ("blur", _) => one.is_none_or(|a| length_ok(a, false, false)),
                ("brightness" | "contrast" | "grayscale" | "invert" | "opacity" | "saturate" | "sepia", _) => {
                    one.is_none_or(|a| number_ok(a, true))
                }
                ("hue-rotate", _) => one.is_none_or(angle_ok),
                ("drop-shadow", 1) => {
                    let parts = tokens(args[0]);
                    let lengths = parts.iter().filter(|p| length_ok(p, false, false)).count();
                    let colours = parts.iter().filter(|p| paint(p).is_some() || **p == "currentcolor").count();
                    (2..=3).contains(&lengths) && colours <= 1 && lengths + colours == parts.len()
                }
                _ => false,
            }
        })
}

/// `clip: rect(top, right, bottom, left)`, each a length or `auto`.
fn clip_valid(v: &str) -> bool {
    let Some(inner) = v.strip_prefix("rect(").and_then(|r| r.strip_suffix(')')) else { return false };
    let edges: Vec<&str> = if inner.contains(',') {
        split_top(inner, b',').into_iter().map(str::trim).collect()
    } else {
        inner.split_whitespace().collect()
    };
    edges.len() == 4 && edges.iter().all(|e| *e == "auto" || length_ok(e, false, false))
}

/// `clip-path`: a shape WebKit knows, with its arguments, and a box.
fn clip_path_valid(v: &str) -> bool {
    let lp = |a: &str| length_ok(a, false, true);
    let radius = |a: &str| matches!(a, "closest-side" | "farthest-side") || lp(a);
    let shapes = tokens(v).into_iter().filter(|t| !CLIP_BOXES.contains(t)).collect::<Vec<_>>();
    let boxes = tokens(v).len() - shapes.len();
    boxes <= 1
        && shapes.len() <= 1
        && shapes.into_iter().all(|t| {
            if t.starts_with("url(") {
                return true;
            }
            let Some(open) = t.find('(') else { return false };
            let Some(inner) = t[open + 1..].strip_suffix(')') else { return false };
            let (body, _round) = match inner.find(" round ") {
                Some(i) => (&inner[..i], Some(&inner[i + 7..])),
                None => (inner, None),
            };
            let (shape, at) = match body.find("at ") {
                Some(i) if i == 0 || body.as_bytes()[i - 1] == b' ' => (body[..i].trim(), Some(&body[i + 3..])),
                _ => (body.trim(), None),
            };
            let words: Vec<&str> = shape.split_whitespace().collect();
            let position_ok = at.is_none_or(|p| {
                p.split_whitespace().all(|w| matches!(w, "left" | "right" | "top" | "bottom" | "center") || lp(w))
            });
            match &t[..open] {
                "inset" => at.is_none() && (1..=4).contains(&words.len()) && words.iter().all(|w| lp(w)),
                "circle" => words.len() <= 1 && words.iter().all(|w| radius(w)) && position_ok,
                "ellipse" => matches!(words.len(), 0 | 2) && words.iter().all(|w| radius(w)) && position_ok,
                "polygon" => split_top(inner, b',').into_iter().enumerate().all(|(i, point)| {
                    let w: Vec<&str> = point.split_whitespace().collect();
                    (i == 0 && matches!(w[..], ["nonzero"] | ["evenodd"])) || (w.len() == 2 && w.iter().all(|x| lp(x)))
                }),
                "rect" => words.len() == 4 && words.iter().all(|w| *w == "auto" || lp(w)) && at.is_none(),
                "xywh" => words.len() == 4 && words.iter().all(|w| lp(w)) && at.is_none(),
                "path" | "shape" => !inner.trim().is_empty(),
                _ => false,
            }
        })
}

/// Words a `mask` layer may hold besides images, lengths and positions.
const MASK_WORDS: &[&str] = &[
    "none",
    "repeat",
    "repeat-x",
    "repeat-y",
    "no-repeat",
    "space",
    "round",
    "top",
    "bottom",
    "left",
    "right",
    "center",
    "cover",
    "contain",
    "auto",
    "alpha",
    "luminance",
    "match-source",
    "add",
    "subtract",
    "intersect",
    "exclude",
    "border-box",
    "padding-box",
    "content-box",
    "margin-box",
    "fill-box",
    "stroke-box",
    "view-box",
    "no-clip",
    "text",
    "border",
    "padding",
    "content",
    "clear",
    "copy",
    "source-over",
    "source-in",
    "source-out",
    "source-atop",
    "destination-over",
    "destination-in",
    "destination-out",
    "destination-atop",
    "xor",
    "plus-darker",
    "plus-lighter",
    "/",
];

/// `mask` and `mask-image`: layers of images, `none`, and (for the
/// shorthand) the words and sizes of a layer.
fn mask_valid(prop: &str, v: &str) -> bool {
    split_top(v, b',').into_iter().map(str::trim).all(|layer| {
        if prop == "mask-image" {
            return layer == "none" || is_image(layer);
        }
        !layer.is_empty()
            && tokens(layer).into_iter().all(|t| {
                split_top(t, b'/')
                    .into_iter()
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .all(|p| MASK_WORDS.contains(&p) || is_image(p) || length_ok(p, false, true))
            })
    })
}

/// Fonts of the system the `font` shorthand may name instead of a size and
/// family.
const SYSTEM_FONTS: &[&str] = &[
    "caption",
    "icon",
    "menu",
    "message-box",
    "small-caption",
    "status-bar",
    "-webkit-mini-control",
    "-webkit-small-control",
    "-webkit-control",
    "-apple-system-headline",
    "-apple-system-body",
    "-apple-system-subheadline",
    "-apple-system-footnote",
    "-apple-system-caption1",
    "-apple-system-caption2",
    "-apple-system-short-headline",
    "-apple-system-short-body",
    "-apple-system-short-subheadline",
    "-apple-system-short-footnote",
    "-apple-system-short-caption1",
    "-apple-system-tall-body",
    "-apple-system-title0",
    "-apple-system-title1",
    "-apple-system-title2",
    "-apple-system-title3",
    "-apple-system-title4",
];

/// Would WebKit take this value for this property? It drops a declaration
/// it does not understand, and the one before it stays: `display:none;
/// display:x` hides.
fn valid(prop: &str, v: &str, quirks: bool) -> bool {
    if matches!(v, "inherit" | "initial" | "unset" | "revert" | "revert-layer") || substitutes(v) {
        return true;
    }
    let length = |t: &str| length_ok(t, quirks, true);
    let number = |t: &str| number_ok(t, true);
    match prop {
        "display" => display_valid(v),
        "visibility" => matches!(v, "visible" | "hidden" | "collapse"),
        "content-visibility" => matches!(v, "visible" | "hidden" | "auto"),
        "opacity" => number(v),
        "font-size" => font_px(v, 16.0, quirks).is_some(),
        "font" => font_shorthand_px(v, 16.0).is_some() || SYSTEM_FONTS.contains(&v),
        "color" | "-webkit-text-fill-color" | "background-color" => {
            v == "currentcolor" || (paint(v).is_some() && (quirks || v != "-webkit-focus-ring-color"))
        }
        "background-image" => split_top(v, b',').into_iter().all(|layer| {
            let layer = layer.trim();
            layer == "none" || is_image(layer)
        }),
        "background" => background_tokens_valid(v, quirks),
        "overflow" | "overflow-x" | "overflow-y" => v.split(' ').all(|w| {
            matches!(
                w,
                "visible" | "hidden" | "clip" | "scroll" | "auto" | "overlay" | "-webkit-paged-x" | "-webkit-paged-y"
            )
        }),
        "height" | "width" | "max-height" | "max-width" | "min-height" | "min-width" => {
            matches!(
                v,
                "auto"
                    | "none"
                    | "min-content"
                    | "max-content"
                    | "fit-content"
                    | "stretch"
                    | "intrinsic"
                    | "min-intrinsic"
                    | "-webkit-fill-available"
                    | "-webkit-min-content"
                    | "-webkit-max-content"
                    | "-webkit-fit-content"
            ) || length(v)
        }
        "left" | "top" | "right" | "bottom" | "margin-left" | "margin-top" | "margin-right" => v == "auto" || length(v),
        "padding" | "padding-inline" => {
            let parts = tokens(v);
            (1..=4).contains(&parts.len()) && parts.iter().all(|w| length(w) && !w.starts_with('-'))
        }
        "padding-left" | "padding-right" => length(v) && !v.starts_with('-'),
        "box-sizing" => matches!(v, "content-box" | "border-box"),
        "float" => matches!(v, "left" | "right" | "none" | "inline-start" | "inline-end"),
        "white-space" => {
            matches!(v, "normal" | "pre" | "nowrap" | "pre-wrap" | "pre-line" | "break-spaces")
                || v.split(' ').all(|w| {
                    matches!(
                        w,
                        "collapse"
                            | "preserve"
                            | "preserve-breaks"
                            | "preserve-spaces"
                            | "break-spaces"
                            | "wrap"
                            | "nowrap"
                    )
                })
        }
        "white-space-collapse" => {
            matches!(v, "collapse" | "preserve" | "preserve-breaks" | "preserve-spaces" | "break-spaces")
        }
        "hyphens" => matches!(v, "none" | "manual" | "auto"),
        "word-break" => matches!(v, "normal" | "break-all" | "keep-all" | "break-word" | "auto-phrase"),
        "overflow-wrap" => matches!(v, "normal" | "break-word" | "anywhere"),
        "color-scheme" => dark_only(v).is_some(),
        "margin" => v.split(' ').count() <= 4 && v.split(' ').all(|w| w == "auto" || length(w)),
        "text-indent" => v.split(' ').all(|w| matches!(w, "hanging" | "each-line") || length(w)),
        "position" => matches!(v, "static" | "relative" | "absolute" | "fixed" | "sticky" | "-webkit-sticky"),
        "clip" => v == "auto" || clip_valid(v),
        "transform" => v == "none" || transform_valid(v),
        "filter" => v == "none" || filter_valid(v),
        "translate" => {
            let parts: Vec<&str> = v.split(' ').collect();
            v == "none" || (parts.len() <= 3 && parts.iter().enumerate().all(|(i, p)| length_ok(p, false, i < 2)))
        }
        "clip-path" => v == "none" || clip_path_valid(v),
        "mask-image" | "mask" => mask_valid(prop, v),
        "background-clip" => true,
        "z-index" => v == "auto" || number_ok(v, false),
        "scale" => v == "none" || (v.split(' ').count() <= 3 && v.split(' ').all(number)),
        "zoom" => matches!(v, "normal" | "reset") || number(v),
        "-webkit-text-security" => matches!(v, "none" | "disc" | "circle" | "square"),
        "mix-blend-mode" => BLEND_MODES.contains(&v),
        "text-shadow" => v == "none" || shadow_valid(v),
        "-webkit-text-stroke-width" => stroke_width_ok(v, quirks),
        "-webkit-text-stroke-color" => v == "currentcolor" || paint(v).is_some(),
        "-webkit-text-stroke" => {
            let parts = tokens(v);
            let widths = parts.iter().filter(|p| stroke_width_ok(p, quirks)).count();
            let colours = parts.iter().filter(|p| **p == "currentcolor" || paint(p).is_some()).count();
            !parts.is_empty() && widths <= 1 && colours <= 1 && widths + colours == parts.len()
        }
        "backdrop-filter" => v == "none" || filter_valid(v),
        "container-type" => matches!(v, "normal" | "size" | "inline-size"),
        "container-name" => container_names_valid(v),
        "container" => {
            let (names, kind) = v.split_once('/').unwrap_or((v, "normal"));
            container_names_valid(names.trim())
                && matches!(kind.trim().to_ascii_lowercase().as_str(), "normal" | "size" | "inline-size")
        }
        // background, clip-path: too varied to judge here.
        _ => true,
    }
}

/// `-webkit-text-stroke-width`: a length, or a keyword for one.
fn stroke_width_ok(v: &str, quirks: bool) -> bool {
    matches!(v, "thin" | "medium" | "thick") || length_ok(v, quirks, false)
}

/// `container-name`: `none`, or names a query can use.
fn container_names_valid(v: &str) -> bool {
    v.eq_ignore_ascii_case("none")
        || (!v.is_empty()
            && v.split(' ').all(|w| {
                !matches!(
                    w.to_ascii_lowercase().as_str(),
                    "none" | "and" | "or" | "not" | "default" | "initial" | "inherit" | "unset"
                ) && w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '-' || !c.is_ascii())
                    && w.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || !c.is_ascii())
            }))
}

pub(crate) type Decl = (&'static str, String, bool);

/// Values longer than this are no real mail's: the message's text is
/// withheld rather than checked against them element after element.
const MAX_VALUE: usize = 8192;

/// Turns the declarations of a block into [`Decl`]s; other properties are
/// read past. Custom properties (`--x: …`) come as `("--", "x:value")`.
pub(crate) struct DeclReader {
    quirks: bool,
    /// A value too long to be checked was met.
    hostile: bool,
}

impl DeclReader {
    pub(crate) fn new(quirks: bool) -> DeclReader {
        DeclReader { quirks, hostile: false }
    }

    pub(crate) fn hostile(&self) -> bool {
        self.hostile
    }

    /// One declaration, its name read already. A property WebKit does not
    /// know is an error: in a style rule, what looked like a declaration
    /// may then be a nested rule (`p:not(.x) {…}`).
    pub(crate) fn declaration<'i>(
        &mut self,
        name: &str,
        input: &mut Parser<'i>,
    ) -> Result<Option<Decl>, ParseError<()>> {
        let lower = name.to_ascii_lowercase();
        let custom = name.starts_with("--");
        let found = PROPS.iter().find(|p| **p == lower).copied().or_else(|| alias(&lower));
        let prop = match found {
            Some(prop) => prop,
            None if custom => "--",
            None => {
                while input.next().is_ok() {}
                return if property_supported(&lower) { Ok(None) } else { Err(ParseError::custom(())) };
            }
        };
        let case = if custom {
            Case::Custom
        } else if KEEP_CASE.contains(&prop) {
            Case::Keep
        } else {
            Case::Lower
        };
        let mut value = String::new();
        let mut important = false;
        loop {
            if input
                .try_parse(|i| {
                    cssparser::parse_important(i)?;
                    i.expect_exhausted()
                })
                .is_ok()
            {
                important = true;
                break;
            }
            let Ok(token) = input.next_including_whitespace() else { break };
            let token = token.clone();
            write_token_as(&token, input, &mut value, 0, case).map_err(|_| ParseError::custom(()))?;
        }
        let mut value = value.trim().to_string();
        if value.len() > MAX_VALUE {
            self.hostile = true;
            return Err(ParseError::custom(()));
        }
        if custom {
            return Ok(Some(("--", format!("{name}:{value}"), important)));
        }
        if value.is_empty() || !valid(prop, &value, self.quirks) {
            return Err(ParseError::custom(()));
        }
        if matches!(prop, "background" | "background-image") && !value.contains("var(") {
            value = compact_background(&value);
        }
        Ok(Some((prop, value, important)))
    }
}

impl<'i> DeclarationParser<'i> for DeclReader {
    type Declaration = Option<Decl>;
    type Error = ();

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &ParserState,
    ) -> Result<Option<Decl>, ParseError<()>> {
        self.declaration(&name, input)
    }
}

impl<'i> AtRuleParser<'i> for DeclReader {
    type Prelude = ();
    type AtRule = Option<Decl>;
    type Error = ();
}

impl<'i> QualifiedRuleParser<'i> for DeclReader {
    type Prelude = ();
    type QualifiedRule = Option<Decl>;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, Option<Decl>, ()> for DeclReader {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// A value as plain text: escapes decoded, comments dropped, lower-cased,
/// single spaces. A token no value may hold makes the declaration invalid.
/// How a value is written: lower-cased (keywords, units and functions are
/// case-insensitive, though names starting with `--` are not), as written
/// (names: custom properties, animations, layers, containers), or as
/// written with `{}` blocks allowed (a custom property's value).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Case {
    Lower,
    Keep,
    Custom,
}

/// Nesting of blocks in a value: WebKit's limit for formulas.
const MAX_VALUE_DEPTH: usize = 100;

fn write_token_as<'i>(
    t: &Token<'i>,
    input: &mut Parser<'i>,
    out: &mut String,
    depth: usize,
    case: Case,
) -> Result<(), ()> {
    let block = |open: &str, close: char, input: &mut Parser<'i>, out: &mut String| -> Result<(), ()> {
        if depth >= MAX_VALUE_DEPTH {
            return Err(());
        }
        out.push_str(open);
        input
            .parse_nested_block(|i| {
                while let Ok(t) = i.next_including_whitespace() {
                    let t = t.clone();
                    write_token_as(&t, i, out, depth + 1, case).map_err(|_| ParseError::custom(()))?;
                }
                Ok::<(), ParseError<()>>(())
            })
            .map_err(|_| ())?;
        while out.ends_with(' ') {
            out.pop();
        }
        out.push(close);
        Ok(())
    };
    match t {
        Token::WhiteSpace(_) => {
            if !out.is_empty() && !out.ends_with([' ', '(', '[']) {
                out.push(' ');
            }
        }
        Token::Comment(_) => {}
        Token::Comma => {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push(',');
        }
        Token::Function(name) => block(&format!("{}(", name.to_ascii_lowercase()), ')', input, out)?,
        Token::ParenthesisBlock => block("(", ')', input, out)?,
        Token::SquareBracketBlock => block("[", ']', input, out)?,
        Token::CurlyBracketBlock if case == Case::Custom => block("{", '}', input, out)?,
        Token::CurlyBracketBlock
        | Token::BadString(_)
        | Token::BadUrl(_)
        | Token::CloseParenthesis
        | Token::CloseSquareBracket
        | Token::CloseCurlyBracket => return Err(()),
        Token::Ident(name) if name.starts_with("--") => {
            other_to_css(t, out)?;
        }
        other => {
            let start = out.len();
            other_to_css(other, out)?;
            // Strings and addresses keep their case: `url(#Id)` names an id.
            if case == Case::Lower && !matches!(other, Token::QuotedString(_) | Token::UnquotedUrl(_)) {
                out[start..].make_ascii_lowercase();
            }
        }
    }
    Ok(())
}

fn other_to_css(t: &Token<'_>, out: &mut String) -> Result<(), ()> {
    t.to_css(out).map_err(|_| ())
}

/// The declarations of a rule or a style attribute that matter here, with
/// their values as text and `!important` noted.
#[derive(Debug, Default, Clone)]
pub struct Decls(Vec<Decl>, bool);

impl Decls {
    /// The declarations of a `style` attribute.
    pub fn parse(style: &str, quirks: bool) -> Decls {
        let mut input = Parser::new(style);
        Decls::read(&mut input, quirks)
    }

    /// The declarations of a block, read as a browser reads them.
    pub(crate) fn new() -> Decls {
        Decls(vec![], false)
    }

    /// One declaration more, later in the cascade.
    pub(crate) fn push(&mut self, (p, v, important): Decl) {
        set(&mut self.0, p, v, important);
    }

    pub(crate) fn mark_hostile(&mut self) {
        self.1 = true;
    }

    /// The declarations of `other` after these.
    pub(crate) fn extend(&mut self, other: &Decls) {
        for (p, v, important) in &other.0 {
            set(&mut self.0, p, v.clone(), *important);
        }
        self.1 |= other.1;
    }

    /// The declarations, in order.
    pub(crate) fn entries(&self) -> &[Decl] {
        &self.0
    }

    /// The declarations of only these properties.
    pub(crate) fn only(&self, props: &[&str]) -> Decls {
        Decls(self.0.iter().filter(|(p, _, _)| props.contains(p)).cloned().collect(), self.1)
    }

    pub fn read(input: &mut Parser<'_>, quirks: bool) -> Decls {
        let mut out = Decls(vec![], false);
        let mut reader = DeclReader { quirks, hostile: false };
        for item in RuleBodyParser::new(input, &mut reader) {
            if let Ok(Some((p, v, important))) = item {
                set(&mut out.0, p, v, important);
            }
        }
        out.1 = reader.hostile;
        out
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// A value too long to check was met: the message is not to be trusted.
    pub fn hostile(&self) -> bool {
        self.1
    }
}

/// One declaration more, later in the cascade. Per property only the one
/// that wins is kept: an `!important` one against later ones without, and
/// a shorthand resets its longhands, so `font-size:14px;font:0/0 a` is 0.
fn set(list: &mut Vec<Decl>, p: &'static str, v: String, important: bool) {
    if p == "--" {
        let name = v.split(':').next().unwrap_or("").to_string();
        let same = |(q, w, _): &Decl| *q == "--" && w.split(':').next() == Some(name.as_str());
        match list.iter_mut().find(|d| same(d)) {
            Some(slot) if slot.2 && !important => {}
            Some(slot) => *slot = (p, v, important),
            None => list.push((p, v, important)),
        }
        return;
    }
    if !important
        && let Some(short) = shorthand(p)
        && list.iter().any(|(q, _, imp)| *q == short && *imp)
    {
        return;
    }
    for long in longhands(p) {
        list.retain(|(q, _, imp)| !(q == long && (important || !*imp)));
    }
    match list.iter_mut().find(|(q, _, _)| *q == p) {
        Some(slot) if slot.2 && !important => {}
        Some(slot) => *slot = (p, v, important),
        None => list.push((p, v, important)),
    }
}

/// What applies to one element, declarations given in cascade order; and
/// the styles of the start of its text, when rules give some.
#[derive(Debug, Default, Clone)]
pub struct Computed(Vec<Decl>, Option<Box<FirstText>>);

/// The styles of the start of an element's text.
#[derive(Debug, Default, Clone)]
pub struct FirstText {
    /// `::first-line`: the element's own style, with the line's over it.
    pub line: Option<Computed>,
    /// `::first-letter`: the letter's own declarations, drawn over the
    /// style of the text it starts.
    pub letter: Option<Computed>,
}

impl Computed {
    #[cfg(test)]
    pub fn apply(&mut self, d: &Decls) {
        self.apply_only(d, None);
    }

    /// Apply the declarations of `d` (only those of that importance, with
    /// `only`).
    pub fn apply_only(&mut self, d: &Decls, only: Option<bool>) {
        for (p, v, important) in &d.0 {
            if only.is_none_or(|o| o == *important) {
                set(&mut self.0, p, v.clone(), *important);
            }
        }
    }

    pub fn get(&self, prop: &str) -> Option<&str> {
        self.0.iter().find(|(p, _, _)| *p == prop).map(|(_, v, _)| v.as_str())
    }

    /// The styles of the start of its text (`::first-line`,
    /// `::first-letter`), when rules give some.
    pub fn first_text(&self) -> Option<&FirstText> {
        self.1.as_deref()
    }

    pub fn set_first_text(&mut self, first: FirstText) {
        self.1 = (first.line.is_some() || first.letter.is_some()).then(|| Box::new(first));
    }

    pub fn clear_first_text(&mut self) {
        self.1 = None;
    }

    /// The same declarations, none of them `!important`: a pseudo-element
    /// inherits from its element, and its own declarations win over all.
    pub fn plain(&self) -> Computed {
        Computed(self.0.iter().map(|(p, v, _)| (*p, v.clone(), false)).collect(), None)
    }

    /// How much text working out this element's look will read.
    pub fn weight(&self) -> usize {
        self.0.iter().map(|(_, v, _)| v.len()).sum()
    }

    /// The custom properties the element sets: (name, value).
    pub fn custom(&self) -> Vec<(String, String)> {
        self.0
            .iter()
            .filter(|(p, _, _)| *p == "--")
            .filter_map(|(_, v, _)| v.split_once(':').map(|(n, v)| (n.to_string(), v.to_string())))
            .collect()
    }

    /// Put the custom properties' values in place of `var()` (and work out
    /// `env()`, `if()`, `sibling-index()`…): what cannot be worked out (a
    /// property never set and no fallback, a cycle) leaves the property
    /// unset, as in CSS.
    pub fn resolve(&mut self, vars: &Option<std::rc::Rc<Vars>>, quirks: bool, cx: &Subst<'_>) {
        let mut out = Vec::with_capacity(self.0.len());
        for (p, v, important) in std::mem::take(&mut self.0) {
            if p == "--" || !substitutes(&v) {
                out.push((p, v, important));
                continue;
            }
            let Some(mut value) = substitute(&v, &mut |name| Vars::get(vars, name, cx), 0, Some(vars), cx) else {
                continue;
            };
            if !KEEP_CASE.contains(&p) {
                value.make_ascii_lowercase();
            }
            if !valid(p, &value, quirks) {
                continue;
            }
            if matches!(p, "background" | "background-image") {
                value = compact_background(&value);
            }
            out.push((p, value, important));
        }
        self.0 = out;
        if let Some(first) = &mut self.1 {
            for c in [&mut first.line, &mut first.letter].into_iter().flatten() {
                c.resolve(vars, quirks, cx);
            }
        }
    }
}

/// What working out `var()`, `env()`, `if()` and the tree-counting
/// functions needs to know about where an element is, and the work it took.
pub struct Subst<'a> {
    /// Does a media query hold in the window the page is drawn in?
    pub media: &'a dyn Fn(&str) -> bool,
    /// Does an `@supports` condition hold?
    pub supports: &'a dyn Fn(&str) -> bool,
    /// The element's place among its siblings: (from 1, how many).
    pub sibling: &'a dyn Fn() -> (usize, usize),
    /// Steps taken. A value grown longer than any real mail's, or custom
    /// properties chained too deep, count as too many: the message's text
    /// is then withheld.
    pub work: std::cell::Cell<usize>,
}

impl Subst<'_> {
    fn spend(&self, n: usize) {
        self.work.set(self.work.get().saturating_add(n));
    }

    fn too_much(&self) {
        self.work.set(usize::MAX / 2);
    }
}

/// The custom properties in force at an element: its own, then those it
/// inherits.
#[derive(Debug)]
pub struct Vars {
    own: Vec<(String, String)>,
    parent: Option<std::rc::Rc<Vars>>,
}

/// Custom properties of one element followed through `var()`; deeper
/// chains than this are no real mail's.
const MAX_VAR_CHAIN: usize = 128;

impl Vars {
    /// The custom properties below an element that sets `own`, each worked
    /// out once (those it refers to first); the properties of a cycle are
    /// invalid, as in CSS.
    pub fn below(
        parent: &Option<std::rc::Rc<Vars>>,
        own: Vec<(String, String)>,
        cx: &Subst<'_>,
    ) -> Option<std::rc::Rc<Vars>> {
        if own.is_empty() {
            return parent.clone();
        }
        if own.len() > 1024 {
            cx.too_much();
            return parent.clone();
        }
        let cyclic = cycles(&own);
        let mut memo: Vec<Option<Option<String>>> = vec![None; own.len()];
        let mut resolved = Vec::with_capacity(own.len());
        for i in 0..own.len() {
            if let Some(v) = resolve_own(i, &own, &cyclic, parent, &mut memo, cx, 0) {
                resolved.push((own[i].0.clone(), v));
            }
        }
        Some(std::rc::Rc::new(Vars { own: resolved, parent: parent.clone() }))
    }

    pub fn get(this: &Option<std::rc::Rc<Vars>>, name: &str, cx: &Subst<'_>) -> Option<String> {
        let mut at = this.as_ref();
        let mut steps = 0;
        while let Some(v) = at {
            if let Some((_, value)) = v.own.iter().rev().find(|(n, _)| n == name) {
                cx.spend(steps + value.len() / 16);
                return Some(value.clone());
            }
            at = v.parent.as_ref();
            steps += 1 + v.own.len() / 8;
            if steps > 4000 {
                cx.too_much();
                break;
            }
        }
        cx.spend(steps);
        None
    }

    /// A value set or inherited, without counting the work (style queries).
    pub fn peek(this: &Option<std::rc::Rc<Vars>>, name: &str) -> Option<String> {
        let mut at = this.as_ref();
        let mut steps = 0;
        while let Some(v) = at {
            if let Some((_, value)) = v.own.iter().rev().find(|(n, _)| n == name) {
                return Some(value.clone());
            }
            at = v.parent.as_ref();
            steps += 1;
            if steps > 4000 {
                break;
            }
        }
        None
    }
}

/// The names a value refers to with `var()`.
fn references(v: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = v;
    while let Some(at) = find_function(rest, "var(") {
        let inner = &rest[at + 4..];
        let name = inner.split([',', ')']).next().unwrap_or("").trim();
        out.push(name);
        rest = inner;
    }
    out
}

/// Which of an element's own custom properties are in a cycle of `var()`
/// references (Tarjan's algorithm, without recursion).
fn cycles(own: &[(String, String)]) -> Vec<bool> {
    let n = own.len();
    let edges: Vec<Vec<usize>> = own
        .iter()
        .map(|(_, v)| references(v).into_iter().filter_map(|name| own.iter().position(|(m, _)| m == name)).collect())
        .collect();
    let (mut index, mut low, mut on_stack) = (vec![usize::MAX; n], vec![0; n], vec![false; n]);
    let (mut stack, mut next, mut cyclic) = (vec![], 0, vec![false; n]);
    for root in 0..n {
        if index[root] != usize::MAX {
            continue;
        }
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(v, e)) = work.last() {
            if e == 0 && index[v] == usize::MAX {
                index[v] = next;
                low[v] = next;
                next += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if let Some(&w) = edges[v].get(e) {
                if let Some(t) = work.last_mut() {
                    t.1 += 1;
                }
                if index[w] == usize::MAX {
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
                continue;
            }
            work.pop();
            if let Some(&(u, _)) = work.last() {
                low[u] = low[u].min(low[v]);
            }
            if low[v] == index[v] {
                let mut members = vec![];
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    members.push(w);
                    if w == v {
                        break;
                    }
                }
                if members.len() > 1 || edges[v].contains(&v) {
                    for m in members {
                        cyclic[m] = true;
                    }
                }
            }
        }
    }
    cyclic
}

/// One of an element's own custom properties, worked out.
fn resolve_own(
    i: usize,
    own: &[(String, String)],
    cyclic: &[bool],
    parent: &Option<std::rc::Rc<Vars>>,
    memo: &mut Vec<Option<Option<String>>>,
    cx: &Subst<'_>,
    depth: usize,
) -> Option<String> {
    if let Some(v) = &memo[i] {
        return v.clone();
    }
    if cyclic[i] {
        memo[i] = Some(None);
        return None;
    }
    if depth > MAX_VAR_CHAIN {
        cx.too_much();
        return None;
    }
    let raw = &own[i].1;
    let value = if substitutes(raw) {
        substitute(
            raw,
            &mut |name| match own.iter().position(|(m, _)| m == name) {
                Some(j) => resolve_own(j, own, cyclic, parent, memo, cx, depth + 1),
                None => Vars::get(parent, name, cx),
            },
            0,
            None,
            cx,
        )
    } else {
        Some(raw.clone())
    };
    memo[i] = Some(value.clone());
    value
}

/// Where the function `name(` (with its parenthesis) starts in `v`, as a
/// whole name rather than the end of another (`--var(`).
fn find_function(v: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = v[from..].find(name).map(|a| a + from) {
        let before = v[..at].chars().next_back();
        if before.is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_')) {
            return Some(at);
        }
        from = at + name.len();
    }
    None
}

/// The index of the parenthesis closing the one at `open`.
fn closing(v: &str, open: usize) -> Option<usize> {
    let mut level = 0i32;
    for (i, c) in v.bytes().enumerate().skip(open) {
        match c {
            b'(' => level += 1,
            b')' => {
                level -= 1;
                if level == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The functions worked out in place.
const SUBSTITUTED: &[&str] = &["var(", "env(", "if(", "sibling-index(", "sibling-count("];

/// `var(--x, fallback)` replaced by the value of `--x`, or by the fallback,
/// and the other functions of [`SUBSTITUTED`] by their values; `None` when
/// no value results. `vars` is the element's custom properties, for
/// `if(style(…))`.
fn substitute(
    v: &str,
    lookup: &mut dyn FnMut(&str) -> Option<String>,
    depth: usize,
    vars: Option<&Option<std::rc::Rc<Vars>>>,
    cx: &Subst<'_>,
) -> Option<String> {
    if depth > 32 {
        cx.too_much();
        return None;
    }
    let mut out = String::new();
    let mut rest = v;
    loop {
        let next =
            SUBSTITUTED.iter().filter_map(|f| find_function(rest, f).map(|at| (at, *f))).min_by_key(|(at, _)| *at);
        let Some((at, f)) = next else { break };
        out.push_str(&rest[..at]);
        let open = at + f.len() - 1;
        let close = closing(rest, open)?;
        let inner = &rest[open + 1..close];
        let (first, fallback) = match split_top(inner, b',')[..] {
            [_] => (inner.trim(), None),
            _ => {
                let comma = inner.find(',')?;
                (inner[..comma].trim(), Some(inner[comma + 1..].trim()))
            }
        };
        let value = match f {
            "var(" => match lookup(first) {
                Some(found) => found,
                None => substitute(fallback?, lookup, depth + 1, vars, cx)?,
            },
            "env(" => {
                if first.starts_with("safe-area-") || first.starts_with("fullscreen-") {
                    "0px".to_string()
                } else {
                    substitute(fallback?, lookup, depth + 1, vars, cx)?
                }
            }
            "if(" => if_value(inner, lookup, depth, vars, cx)?,
            "sibling-index(" => (cx.sibling)().0.to_string(),
            _ => (cx.sibling)().1.to_string(),
        };
        cx.spend(value.len() / 16 + 1);
        out.push_str(&value);
        if out.len() > MAX_VALUE {
            cx.too_much();
            return None;
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// `if(media(…): a; supports(…): b; style(…): c; else: d)`: the first
/// branch whose condition holds, worked out; nothing if none does.
fn if_value(
    inner: &str,
    lookup: &mut dyn FnMut(&str) -> Option<String>,
    depth: usize,
    vars: Option<&Option<std::rc::Rc<Vars>>>,
    cx: &Subst<'_>,
) -> Option<String> {
    for branch in split_top(inner, b';') {
        let branch = branch.trim();
        if branch.is_empty() {
            continue;
        }
        let colon = top_level_colon(branch)?;
        let (test, value) = (branch[..colon].trim(), branch[colon + 1..].trim());
        let holds = if test.eq_ignore_ascii_case("else") {
            true
        } else {
            let all = items(test)?;
            condition_opt(&all, &mut |item| match item {
                Item::Call("media", q) => Some((cx.media)(q)),
                Item::Call("supports", q) => Some((cx.supports)(q)),
                Item::Call("style", q) => Some(match vars {
                    Some(vars) => style_holds(q, &|name| Vars::peek(vars, name), 0)?,
                    None => false,
                }),
                Item::Group(_) => None,
                _ => None,
            })?
        };
        if holds {
            return substitute(value, lookup, depth + 1, vars, cx);
        }
    }
    Some(String::new())
}

/// The first `:` outside parentheses.
fn top_level_colon(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.bytes().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b':' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// An image value: a picture or a gradient.
fn is_image(v: &str) -> bool {
    v.ends_with(')') && (PICTURES.iter().any(|f| v.starts_with(f)) || gradient_stops(v).is_some())
}

/// Words a `background` shorthand may hold besides colours and images.
const BACKGROUND_WORDS: &[&str] = &[
    "none",
    "repeat",
    "repeat-x",
    "repeat-y",
    "no-repeat",
    "space",
    "round",
    "scroll",
    "fixed",
    "local",
    "top",
    "bottom",
    "left",
    "right",
    "center",
    "cover",
    "contain",
    "auto",
    "border-box",
    "padding-box",
    "content-box",
    "text",
    "/",
];

/// Is every part of a `background` shorthand something it may hold?
fn background_tokens_valid(v: &str, quirks: bool) -> bool {
    tokens(v).into_iter().all(|token| {
        split_top(token, b',').into_iter().map(str::trim).filter(|p| !p.is_empty()).all(|part| {
            split_top(part, b'/').into_iter().map(str::trim).filter(|p| !p.is_empty()).all(|p| {
                BACKGROUND_WORDS.contains(&p)
                    || is_image(p)
                    || paint(p).is_some()
                    || p == "currentcolor"
                    || match number_prefix(p) {
                        Some((n, "")) => n == 0.0 || quirks,
                        Some((_, "%")) => true,
                        Some(_) => length_px(p, 16.0).is_some(),
                        None => p.contains('('),
                    }
            })
        })
    })
}

/// Image functions that paint a picture whose pixels are not known here.
const PICTURES: &[&str] = &[
    "url(",
    "image(",
    "image-set(",
    "-webkit-image-set(",
    "cross-fade(",
    "-webkit-cross-fade(",
    "-webkit-gradient(",
    "-webkit-canvas(",
    "-webkit-named-image(",
    "filter(",
    "-webkit-filter(",
    "light-dark(",
];

/// The colours of a gradient's stops, or `None` if `v` is no gradient.
fn gradient_stops(v: &str) -> Option<Vec<Paint>> {
    let open = v.find('(')?;
    let name = &v[..open];
    let unprefixed = name.strip_prefix("-webkit-").unwrap_or(name);
    let known = matches!(
        unprefixed,
        "linear-gradient" | "radial-gradient" | "repeating-linear-gradient" | "repeating-radial-gradient"
    ) || (name == unprefixed && matches!(name, "conic-gradient" | "repeating-conic-gradient"));
    if !known {
        return None;
    }
    let inner = v[open + 1..].strip_suffix(')')?;
    Some(
        split_top(inner, b',').into_iter().filter_map(|stop| tokens(stop.trim()).into_iter().find_map(paint)).collect(),
    )
}

/// What a background value says about text drawn on it, in a word or two:
/// `url()` for a picture or a gradient of several colours; otherwise the
/// colour on top (a gradient of one colour paints like that colour);
/// `none` for no background. Long inline pictures and gradients thus cost
/// nothing more to check, element after element.
fn compact_background(v: &str) -> String {
    let (mut picture, mut layer, mut colour) = (false, None::<String>, None::<String>);
    for token in tokens(v) {
        for part in split_top(token, b',') {
            let part = part.trim();
            if part.is_empty() || part == "none" {
                continue;
            }
            if let Some(stops) = gradient_stops(part) {
                let solid =
                    stops.first().copied().filter(|first| stops.iter().all(|s| s.same_as(*first) || s == first));
                match solid {
                    Some(Paint::Clear) => {}
                    Some(Paint::Rgb(r, g, b)) if layer.is_none() => layer = Some(format!("#{r:02x}{g:02x}{b:02x}")),
                    Some(_) => {}
                    None => picture = true,
                }
                continue;
            }
            if PICTURES.iter().any(|f| part.starts_with(f)) {
                picture = true;
            } else if colour.is_none() && paint(part).is_some() {
                colour = Some(part.to_string());
            }
        }
    }
    if picture { "url()".to_string() } else { layer.or(colour).unwrap_or_else(|| "none".to_string()) }
}

/// A length of a few pixels at most: nothing to read, once overflow is
/// hidden. A percentage of an unknown size counts only when it is zero.
fn tiny(v: &str) -> bool {
    let v = v.trim();
    if v.contains('(') {
        return length_px(v, 16.0).is_some_and(|n| n <= 4.0);
    }
    match number_prefix(v) {
        Some((n, unit)) if unit.trim() == "%" => n <= 0.0,
        Some(_) => length_px(v, 16.0).is_some_and(|n| n <= 4.0),
        None => false,
    }
}

/// `clip: rect(top, right, bottom, left)` that leaves nothing to see
/// (`auto` is the edge of the box).
fn empty_clip(v: &str) -> bool {
    let Some(inner) = v.strip_prefix("rect(") else { return false };
    let edges: Vec<&str> =
        inner.trim_end_matches(')').split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).collect();
    if edges.len() != 4 {
        return false;
    }
    let edge = |i: usize, auto: f32| if edges[i] == "auto" { Some(auto) } else { length_px(edges[i], 16.0) };
    let (Some(top), Some(right), Some(bottom), Some(left)) = (edge(0, 0.0), edge(1, 1e6), edge(2, 1e6), edge(3, 0.0))
    else {
        return false;
    };
    bottom - top <= 1.0 || right - left <= 1.0
}

/// `clip-path` that leaves nothing to see: `inset()` meeting itself,
/// a circle or ellipse of no size.
fn empty_clip_path(v: &str) -> bool {
    if let Some(inner) = v.strip_prefix("inset(") {
        let inner = inner.split(" round").next().unwrap_or(inner).trim_end_matches(')');
        let pct: Vec<f32> = inner
            .split_whitespace()
            .map(|t| number_prefix(t).and_then(|(n, u)| (u == "%").then_some(n)))
            .collect::<Option<Vec<f32>>>()
            .unwrap_or_default();
        let (t, r, b, l) = match pct[..] {
            [a] => (a, a, a, a),
            [a, b] => (a, b, a, b),
            [a, b, c] => (a, b, c, b),
            [a, b, c, d] => (a, b, c, d),
            _ => return false,
        };
        return t + b >= 100.0 || l + r >= 100.0;
    }
    if let Some(inner) = v.strip_prefix("polygon(").and_then(|r| r.strip_suffix(')')) {
        // No area: every point on one line.
        let points: Vec<Vec<&str>> = split_top(inner, b',')
            .into_iter()
            .map(|p| p.split_whitespace().collect::<Vec<_>>())
            .filter(|p| p.len() == 2 && !p[0].contains("evenodd") && !p[0].contains("nonzero"))
            .collect();
        let num = |t: &str| number_prefix(t).map(|(n, _)| n);
        let xy: Option<Vec<(f32, f32)>> = points.iter().map(|p| Some((num(p[0])?, num(p[1])?))).collect();
        if let Some(xy) = xy.filter(|xy| xy.len() >= 3) {
            let area: f32 =
                xy.iter().zip(xy.iter().cycle().skip(1)).map(|((x1, y1), (x2, y2))| x1 * y2 - x2 * y1).sum::<f32>()
                    / 2.0;
            return area.abs() < 1e-3;
        }
        return false;
    }
    let zero = |rest: &str| number_prefix(rest.trim_start()).is_some_and(|(n, _)| n <= 0.0);
    v.strip_prefix("circle(").is_some_and(zero) || v.strip_prefix("ellipse(").is_some_and(zero)
}

/// `min-height` of more than a few pixels: it wins over `height` and
/// `max-height`, so the box is not flat.
fn opened(c: &Computed) -> bool {
    c.get("min-height").and_then(|v| length_px(v, 16.0)).is_some_and(|n| n > 4.0)
}

/// A length in CSS pixels, `em` taken as 16 pixels.
pub(crate) fn px(v: &str) -> Option<f32> {
    length_px(v, 16.0)
}

/// Elements laid out inline unless their style says otherwise: transforms,
/// sizes and `text-indent` do not apply to them.
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "big", "cite", "code", "dfn", "em", "font", "i", "kbd", "label", "mark", "q", "s",
    "samp", "small", "span", "strike", "strong", "sub", "sup", "time", "tt", "u", "var",
];

fn is_inline(name: &str, c: &Computed) -> bool {
    INLINE.contains(&name) && c.get("display").is_none_or(|d| d == "inline" || d == "contents")
}

/// Does this element hide everything inside it, whatever its descendants
/// say? (`display:none`, clipping to nothing, placement off screen.)
/// Transparency and shrinking add up down the tree: see [`Look`].
pub fn hides_subtree(name: &str, c: &Computed) -> bool {
    if c.get("display") == Some("none") || c.get("content-visibility") == Some("hidden") {
        return true;
    }
    let inline = is_inline(name, c);
    // Table cells grow to their content, whatever height they are given.
    let cell = match c.get("display") {
        Some(d) => d.starts_with("table"),
        None => matches!(name, "td" | "th" | "tr" | "table" | "tbody" | "thead" | "tfoot"),
    };
    let clipped = ["overflow", "overflow-x", "overflow-y"].iter().any(|p| matches!(c.get(p), Some("hidden" | "clip")));
    let low = !opened(c) && ["max-height", "height"].iter().any(|p| c.get(p).is_some_and(tiny));
    if clipped && !inline && !cell && (low || ["width", "max-width"].iter().any(|p| c.get(p).is_some_and(tiny))) {
        return true;
    }
    let position = c.get("position").unwrap_or("static");
    if matches!(position, "absolute" | "fixed") && c.get("clip").is_some_and(empty_clip) {
        return true;
    }
    if c.get("clip-path").is_some_and(empty_clip_path) {
        return true;
    }
    if ["mask-image", "mask"].iter().any(|p| c.get(p).is_some_and(transparent_mask)) {
        return true;
    }
    // Moved off the left or top of the page, which no one can scroll to, or
    // very far to the right or bottom.
    let px = |v: Option<&str>| v.and_then(|v| length_px(v, 16.0));
    let pct = |v: Option<&str>| v.and_then(|v| number_prefix(v.trim())).and_then(|(n, u)| (u == "%").then_some(n));
    let before =
        |v: Option<&str>| px(v).is_some_and(|n| n <= -500.0 || n >= 5000.0) || pct(v).is_some_and(|n| n <= -100.0);
    let after = |v: Option<&str>| px(v).is_some_and(|n| n >= 999.0 || n <= -5000.0);
    if !inline && before(c.get("text-indent").and_then(|v| v.split(' ').next())) {
        return true;
    }
    if position != "static"
        && (before(c.get("left")) || before(c.get("top")) || after(c.get("right")) || after(c.get("bottom")))
    {
        return true;
    }
    if let Some(m) = c.get("margin") {
        let m: Vec<&str> = m.split(' ').collect();
        let left = m.get(3).or(m.get(1)).or(m.first()).copied();
        if before(left) || (!inline && before(m.first().copied())) {
            return true;
        }
    }
    if before(c.get("margin-left")) || (!inline && before(c.get("margin-top"))) {
        return true;
    }
    if inline {
        return false; // transforms apply to boxes only
    }
    if let Some(t) = c.get("translate").filter(|t| *t != "none") {
        let parts: Vec<&str> = t.split(' ').collect();
        if before(parts.first().copied()) || before(parts.get(1).copied()) {
            return true;
        }
    }
    if let Some(t) = c.get("transform") {
        for (f, args) in calls(t) {
            let moved = match f {
                "translate" | "translate3d" => args.iter().take(2).any(|a| before(Some(a))),
                "translatex" | "translatey" => args.first().is_some_and(|a| before(Some(a))),
                "matrix" if args.len() == 6 => args[4..].iter().any(|a| before(Some(a))),
                "matrix3d" if args.len() == 16 => args[12..14].iter().any(|a| before(Some(a))),
                _ => false,
            };
            if moved {
                return true;
            }
        }
    }
    false
}

/// A mask that lets nothing through: a gradient between transparent stops;
/// or a picture (`url()`), not looked at here: WebKit draws nothing through
/// one that does not load, and what one that loads lets through is not
/// known.
fn transparent_mask(v: &str) -> bool {
    v.contains("url(")
        || tokens(v).into_iter().any(|t| {
            gradient_stops(t).is_some_and(|stops| !stops.is_empty() && stops.iter().all(|s| *s == Paint::Clear))
        })
}

/// What an element passes down that decides whether text can be seen.
#[derive(Debug, Clone)]
pub struct Look {
    /// Font size in CSS pixels.
    pub font_px: f32,
    /// Text colour, when one was set.
    pub color: Option<Paint>,
    /// The background behind the text: the nearest one set. A page starts
    /// white, as Mail shows it in light mode.
    pub background: Option<Paint>,
    pub visible: bool,
    /// `-webkit-text-fill-color`: what WebKit paints text with, in place of
    /// `color`, when set.
    pub fill: Option<Paint>,
    /// How much `zoom` and `transform: scale()` shrink what is drawn.
    pub scale: f32,
    /// Opacity, multiplied down the tree.
    pub opacity: f32,
    /// The colour of links, from `<body link=…>`.
    pub link: Paint,
    /// A background seen only through the text (`background-clip: text`).
    pub clip_bg: Option<Paint>,
    /// An element above, inside the same stacking context, paints a
    /// background: what has a negative `z-index` is drawn under it.
    pub bg_in_context: bool,
    /// Drawn under an element's background: hidden, whatever comes below.
    pub buried: bool,
    /// Custom properties in force (`--x`), for `var()`.
    pub vars: Option<std::rc::Rc<Vars>>,
    /// The width of the box, in CSS pixels, as a range: it is not laid out
    /// here.
    pub width: (f32, f32),
    /// Its children are flex or grid items, as wide as their container
    /// lets them be: anywhere up to its width.
    pub flexible: bool,
    /// What colours drawn here go through to the screen: filters, and
    /// blending with what is under them.
    pub effects: Option<std::rc::Rc<Effects>>,
    /// The colour under this point inside its stacking context, before
    /// those (what `mix-blend-mode` blends with); `None`: nothing painted
    /// there yet.
    pub backdrop: Option<Paint>,
    /// How far filters blur what is drawn here, in CSS pixels.
    pub blur: f32,
    /// `-webkit-text-security`: letters drawn as discs.
    pub masked: bool,
    /// `text-shadow`: copies of the letters.
    pub shadows: Option<std::rc::Rc<[Shadow]>>,
    /// `-webkit-text-stroke`: an outline of the letters, its width and its
    /// colour (`None`: the text's).
    pub stroke: (f32, Option<Paint>),
    /// The look of a block's first line (`::first-line`), while text here
    /// may be on it.
    pub line: Option<Box<Look>>,
    /// A dark colour scheme only (`color-scheme: dark`): WebKit draws text
    /// nobody coloured white, on a dark page.
    pub dark: bool,
    /// The colour under this point since the nearest backdrop root (an
    /// element with a filter, transparency, a mask, blending), before
    /// effects: what a backdrop filter here filters. `None`: nothing.
    pub under: Option<Paint>,
    /// `<html>` paints its own background: `<body>`'s is then an ordinary
    /// one, not the window's.
    pub root_bg: bool,
    /// Copies of the letters that `drop-shadow()` filters around draw.
    pub drops: Option<std::rc::Rc<[Shadow]>>,
    /// White space keeps its line breaks (`white-space: pre`…).
    pub pre: bool,
    /// Words may break to fit a line: `word-break: break-all`,
    /// `overflow-wrap: anywhere`, `hyphens: auto`.
    pub break_all: bool,
    pub wrap_anywhere: bool,
    pub hyphens: bool,
}

impl Look {
    /// The look at the top of a page drawn in this window.
    pub fn window(w: &Window) -> Look {
        Look { width: (w.width, w.width), ..Look::default() }
    }
}

impl Default for Look {
    fn default() -> Self {
        Look {
            font_px: 16.0,
            color: None,
            background: Some(Paint::Rgb(255, 255, 255)),
            visible: true,
            fill: None,
            scale: 1.0,
            opacity: 1.0,
            link: LINK,
            clip_bg: None,
            bg_in_context: false,
            buried: false,
            vars: None,
            width: (WINDOWS[0].width, WINDOWS[0].width),
            flexible: false,
            effects: None,
            backdrop: Some(Paint::Rgb(255, 255, 255)),
            blur: 0.0,
            masked: false,
            shadows: None,
            stroke: (0.0, None),
            line: None,
            dark: false,
            under: Some(Paint::Rgb(255, 255, 255)),
            root_bg: false,
            drops: None,
            pre: false,
            break_all: false,
            wrap_anywhere: false,
            hyphens: false,
        }
    }
}

/// Keeps a size finite and in range, so no product of them is ever NaN.
fn bounded(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1e6) }
}

/// A number or a percentage, as a fraction for the latter; `calc()` and
/// the like included.
pub(crate) fn fraction(t: &str) -> Option<f32> {
    let t = t.trim();
    if t.contains('(') {
        return crate::calc::number_value(t, crate::calc::Pct::Number).map(|n| n as f32);
    }
    number_prefix(t).and_then(|(n, unit)| match unit.trim() {
        "%" => Some(n / 100.0),
        "" => Some(n),
        _ => None,
    })
}

/// How much a `transform`, `scale` or `zoom` value shrinks element `name`:
/// the smaller of its horizontal and vertical factors (1 when not worked
/// out).
fn shrink(name: &str, style: &Computed) -> f32 {
    let mut f = 1.0f32;
    if let Some(z) = style.get("zoom").and_then(fraction).filter(|z| *z > 0.0) {
        f *= bounded(z); // zoom: 0 counts as 1, as in WebKit
    }
    if is_inline(name, style) {
        return f; // transforms apply to boxes, not to runs of text
    }
    if let Some(v) = style.get("scale").filter(|v| *v != "none") {
        let n: Vec<f32> = v.split_whitespace().filter_map(fraction).collect();
        if let Some(&x) = n.first() {
            f *= bounded(x.abs().min(n.get(1).copied().unwrap_or(x).abs()));
        }
    }
    if let Some(v) = style.get("transform") {
        for (name, args) in calls(v) {
            let n: Vec<f32> = args.iter().filter_map(|a| fraction(a)).collect();
            let edge_on = |deg: Option<f32>| deg.is_some_and(|d| d.to_radians().cos().abs() < 0.05);
            let factor = match (name, &n[..]) {
                ("scale" | "scale3d", [x]) => x.abs(),
                ("scale" | "scale3d", [x, y, ..]) => x.abs().min(y.abs()),
                ("scalex" | "scaley", [x, ..]) => x.abs(),
                ("matrix", [a, b, c, d, ..]) => (a * a + b * b).sqrt().min((c * c + d * d).sqrt()),
                ("matrix3d", m) if m.len() == 16 => {
                    (m[0] * m[0] + m[1] * m[1]).sqrt().min((m[4] * m[4] + m[5] * m[5]).sqrt())
                }
                // Turned a quarter around the horizontal or vertical axis:
                // seen edge-on, nothing is left of it.
                ("rotatex" | "rotatey", _) if edge_on(args.first().and_then(|a| angle(a))) => 0.0,
                ("rotate3d", _) if args.len() == 4 => {
                    let axis: Vec<f32> = args[..3].iter().filter_map(|a| fraction(a)).collect();
                    let around_z = axis.len() == 3 && axis[0].abs() < 1e-6 && axis[1].abs() < 1e-6;
                    if !around_z && edge_on(angle(args[3])) { 0.0 } else { 1.0 }
                }
                _ => 1.0,
            };
            f = bounded(f * bounded(factor));
        }
    }
    bounded(f)
}

/// The function calls of a value, as (lower-cased name, arguments), nested
/// parentheses kept whole: `translate(-50%, calc(-1px * 2)) scale(0)`.
fn calls(v: &str) -> Vec<(&str, Vec<&str>)> {
    tokens(v)
        .into_iter()
        .filter_map(|t| {
            let open = t.find('(')?;
            let inner = t[open + 1..].strip_suffix(')')?;
            Some((t[..open].trim(), split_top(inner, b',').into_iter().map(str::trim).collect()))
        })
        .collect()
}

/// An angle, in degrees.
fn angle(t: &str) -> Option<f32> {
    crate::calc::angle(t.trim()).map(|d| d as f32)
}

/// How much an element's `opacity` and `filter: opacity()` let through.
fn own_opacity(style: &Computed) -> f32 {
    let mut o = style.get("opacity").and_then(fraction).map_or(1.0, |v| v.clamp(0.0, 1.0));
    if let Some(f) = style.get("filter") {
        for part in f.split("opacity(").skip(1) {
            if let Some(v) = fraction(part.split(')').next().unwrap_or("")) {
                o *= v.clamp(0.0, 1.0);
            }
        }
    }
    o
}

/// Tokens of a value, keeping `rgb(…)` and `url(…)` whole.
pub(crate) fn tokens(v: &str) -> Vec<&str> {
    let mut out = vec![];
    let (mut depth, mut start) = (0i32, None);
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

/// The colour text has when nothing sets one: CanvasText, in light mode;
/// in a dark colour scheme, white, on WebKit's dark page.
const BLACK: Paint = Paint::Rgb(0, 0, 0);
const WHITE: Paint = Paint::Rgb(255, 255, 255);
pub const DARK_PAGE: Paint = Paint::Rgb(30, 30, 30);

impl Look {
    /// Is text that inherits this look invisible?
    pub fn hides_text(&self) -> bool {
        let size = self.font_px * self.scale;
        // NaN, should it ever come, compares as nothing: it hides.
        let at_least = |v: f32, min: f32| v.partial_cmp(&min).is_some_and(|o| o.is_ge());
        if !self.visible
            || self.buried
            || self.masked
            || !at_least(size, 4.0)
            || !at_least(self.opacity, 0.1)
            // Blurred past reading.
            || self.blur.partial_cmp(&(0.2 * size)) != Some(std::cmp::Ordering::Less)
        {
            return true;
        }
        let seen = |p: Paint| Effects::apply(&self.effects, p);
        let shows = |p: Paint| p != Paint::Clear && !self.background.is_some_and(|bg| p.same_as(bg));
        let colour = self.color.unwrap_or(if self.dark { WHITE } else { BLACK });
        let letters = match self.fill.unwrap_or(colour) {
            // Transparent letters show a background clipped to them, if any.
            Paint::Clear => self.clip_bg.unwrap_or(Paint::Clear),
            p => seen(p),
        };
        if shows(letters) {
            return false;
        }
        // A thin outline, or a sharp copy close by, draws the letters too: a
        // thick one only a blot, one far off perhaps nothing in the window.
        if self.stroke.0 > 0.0 && self.stroke.0 <= 0.2 * size && shows(seen(self.stroke.1.unwrap_or(colour))) {
            return false;
        }
        let sharp = |s: &Shadow| s.blur < 0.2 * size && s.dx.abs() <= size && s.dy.abs() <= size;
        let copied = |list: &Option<std::rc::Rc<[Shadow]>>| {
            list.as_deref().is_some_and(|l| l.iter().any(|s| sharp(s) && shows(seen(s.colour.unwrap_or(colour)))))
        };
        // A drop shadow copies what is drawn: transparent letters cast none.
        let drawn = self.fill.unwrap_or(colour) != Paint::Clear;
        !(copied(&self.shadows) || (drawn && copied(&self.drops)))
    }

    /// The look inside element `name` with these attributes and styles.
    /// `quirks`: a page without a modern doctype, where tables start again
    /// from the default font size. `on_first_line`: the element starts on
    /// the first line of a block `::first-line` styles, whose look inside
    /// it is worked out too (see [`Look::line`]).
    pub fn child<'e>(
        &self,
        name: &str,
        attr: &dyn Fn(&str) -> Option<&'e str>,
        style: &Computed,
        quirks: bool,
        on_first_line: bool,
    ) -> Look {
        let mut look = self.inner(name, attr, style, quirks);
        let parent_line = self.line.as_deref().filter(|_| on_first_line);
        let own = style.first_text().and_then(|f| f.line.as_ref());
        look.line = match (own, parent_line) {
            (Some(own), Some(line)) => Some(line.inner(name, attr, own, quirks)),
            (Some(own), None) => Some(self.inner(name, attr, own, quirks)),
            (None, Some(line)) => Some(line.inner(name, attr, style, quirks)),
            (None, None) => None,
        }
        .map(Box::new);
        look
    }

    /// The look of a block's first letter: `::first-letter`'s declarations
    /// (`decls`) over the text it starts.
    pub fn letter(&self, decls: &Computed, quirks: bool) -> Look {
        self.inner("span", &|_| None, decls, quirks)
    }

    fn inner<'e>(&self, name: &str, attr: &dyn Fn(&str) -> Option<&'e str>, style: &Computed, quirks: bool) -> Look {
        let mut look = self.clone();
        look.line = None;
        let parent = self.font_px;
        let mut px = if quirks && name == "table" { 16.0 } else { parent * default_scale(name) };
        if name == "font"
            && let Some(n) = attr("size").and_then(font_tag_px)
        {
            px = n;
        }
        if let Some(n) = style.get("font").and_then(|v| font_shorthand_px(v, parent)) {
            px = n;
        }
        if let Some(v) = style.get("font-size") {
            match font_px(v, parent, quirks) {
                Some(n) => px = n,
                // var() and the like: a size not worked out here is not
                // taken for an invisible one.
                None if v.contains('(') && px < 2.0 => px = 16.0,
                None => {}
            }
        }
        look.font_px = bounded(px);
        let font = look.font_px;
        if name == "html" {
            crate::calc::set_root_font(font.into());
        }

        // The width of the content box. A block is as wide as its parent's
        // content less its own margins, borders and padding; other boxes as
        // wide as their content, up to that, and flex and grid items anywhere
        // up to it; unless a width is given. Percentages count from both
        // ends of the parent's range.
        let display = style.get("display");
        let fits_content = is_inline(name, style)
            || display.is_some_and(|d| d.starts_with("inline") || d.starts_with("table"))
            || (display.is_none()
                && matches!(name, "table" | "td" | "th" | "img" | "button" | "input" | "select" | "textarea"))
            || style.get("position").is_some_and(|p| matches!(p, "absolute" | "fixed"))
            || style.get("float").is_some_and(|f| f != "none");
        let room = self.width;
        let range = |v: &str| -> Option<(f32, f32)> {
            let (a, b) = (length_of(v, font, room.0)?, length_of(v, font, room.1)?);
            Some((a.min(b).max(0.0), a.max(b).max(0.0)))
        };
        let by_content = |v: &str| {
            matches!(
                v,
                "min-content"
                    | "max-content"
                    | "fit-content"
                    | "-webkit-min-content"
                    | "-webkit-max-content"
                    | "-webkit-fit-content"
                    | "intrinsic"
                    | "min-intrinsic"
            )
        };
        // A range less another, never below nothing.
        let less = |(lo, hi): (f32, f32), (a, b): (f32, f32)| ((lo - b).max(0.0), (hi - a).max(0.0));
        let edge = edges(style, font, room);
        // The body has the browser's own 8 pixels on each side.
        let margins = margins_across(style, font, room, if name == "body" { 8.0 } else { 0.0 });
        let border_box = style.get("box-sizing") == Some("border-box");
        let available = less(less(room, margins), edge);
        let mut width = if fits_content || self.flexible { (0.0, available.1) } else { available };
        let given = style
            .get("width")
            .or_else(|| matches!(name, "table" | "td" | "th" | "img" | "col" | "hr").then(|| attr("width")).flatten());
        match given {
            Some(v) if by_content(v) => width = (0.0, available.1),
            Some("stretch" | "-webkit-fill-available") => width = available,
            Some(v) => match number_prefix(v.trim()) {
                // An attribute's pixels.
                Some((n, "")) => width = (n, n),
                _ => {
                    if let Some(r) = range(v) {
                        width = if border_box { less(r, edge) } else { r };
                    }
                }
            },
            None => {}
        }
        // Contained in size, a box as wide as its content is as wide as
        // nothing: what it holds does not count.
        if (fits_content || given.is_some_and(by_content)) && given.is_none_or(by_content) && size_contained(style) {
            width = (0.0, 0.0);
        }
        let limit = |v: &str| range(v).map(|r| if border_box { less(r, edge) } else { r });
        if let Some(m) = style.get("max-width").and_then(limit) {
            width = (width.0.min(m.0), width.1.min(m.1));
        }
        if let Some(m) = style.get("min-width").and_then(limit) {
            width = (width.0.max(m.0), width.1.max(m.1));
        }
        look.width = (bounded(width.0), bounded(width.1.max(width.0)));
        look.flexible = display.is_some_and(|d| d.contains("flex") || d.contains("grid") || d.contains("-webkit-box"));

        // Where lines may break: kept line breaks, words that break (all
        // inherited).
        look.pre = white_space(name, style, self.pre);
        match style.get("hyphens") {
            Some("auto") => look.hyphens = true,
            Some("manual" | "none" | "initial") => look.hyphens = false,
            _ => {}
        }
        match style.get("word-break") {
            Some("break-all" | "break-word") => look.break_all = true,
            Some("normal" | "keep-all" | "auto-phrase" | "initial") => look.break_all = false,
            _ => {}
        }
        match style.get("overflow-wrap") {
            Some("anywhere" | "break-word") => look.wrap_anywhere = true,
            Some("normal" | "initial") => look.wrap_anywhere = false,
            _ => {}
        }

        // Mail shows mail in light mode: only a page made for dark alone is
        // drawn dark.
        let scheme = match style.get("color-scheme") {
            Some("initial") => Some(false),
            Some(v) => dark_only(v),
            None => None,
        };
        if let Some(dark) = scheme {
            look.dark = dark;
            // The root's own scheme gives the window its colour, over a meta's.
            if name == "html" {
                let page = if dark { DARK_PAGE } else { WHITE };
                (look.background, look.backdrop, look.under) = (Some(page), Some(page), Some(page));
            }
        }
        if name == "body" {
            if let Some(p) = attr("text").and_then(paint_attr) {
                look.color = Some(p);
            }
            if let Some(p) = attr("link").and_then(paint_attr) {
                look.link = p;
            }
        }
        if name == "a" && attr("href").is_some() {
            look.color = Some(look.link);
        }
        if name == "font"
            && let Some(p) = attr("color").and_then(paint_attr)
        {
            look.color = Some(p);
        }
        // The browser's own colours for marked text, form controls, dialogs.
        if let Some((fg, _)) = ua_colours(name) {
            look.color = Some(fg);
        }
        match style.get("color") {
            // Inherited: `revert` goes back to the browser's own style, which
            // gives links their colour and inherits the rest.
            Some("revert" | "revert-layer") if name == "a" && attr("href").is_some() => look.color = Some(look.link),
            Some("inherit" | "currentcolor" | "unset" | "revert" | "revert-layer") => look.color = self.color,
            Some("initial") => look.color = None,
            // A variable left unresolved: as if unset, inherited.
            Some(v) if v.contains("var(") => look.color = self.color,
            Some(v) => {
                if let Some(p) = paint(v) {
                    look.color = Some(p);
                }
            }
            None => {}
        }

        match style.get("-webkit-text-fill-color") {
            Some("currentcolor" | "initial") => look.fill = None,
            Some(v) if v.contains("var(") => {}
            Some("inherit" | "unset" | "revert" | "revert-layer") | None => {}
            Some(v) => {
                if let Some(p) = paint(v) {
                    look.fill = Some(p);
                }
            }
        }
        look.scale = bounded(look.scale * shrink(name, style));
        look.opacity = (look.opacity * own_opacity(style)).clamp(0.0, 1.0);

        // Letters drawn as discs, outlined, shadowed (all inherited).
        match style.get("-webkit-text-security") {
            Some("disc" | "circle" | "square") => look.masked = true,
            Some("none" | "initial") => look.masked = false,
            _ => {}
        }
        match style.get("text-shadow") {
            Some("none" | "initial") => look.shadows = None,
            Some("inherit" | "unset" | "revert" | "revert-layer") | None => {}
            Some(v) => look.shadows = Some(crate::effects::shadows(v, font).into()),
        }
        look.stroke = stroke_of(style, self.stroke, font);

        // Filters and blending: what colours drawn inside go through.
        let mut ops = vec![];
        if let Some(v) = style.get("filter").filter(|v| *v != "none") {
            let f = crate::effects::filter(v, font);
            look.blur = look.blur.hypot(f.blur);
            if f.cut {
                // More steps than any real mail's: taken as unseen.
                look.opacity = 0.0;
            }
            if !f.shadows.is_empty() {
                // Copies of the letters, unless a background is drawn under
                // them (see below): coloured as the text where unsaid.
                let colour = look.color.unwrap_or(if look.dark { WHITE } else { BLACK });
                let mut drops: Vec<Shadow> = look.drops.as_deref().map_or(vec![], <[Shadow]>::to_vec);
                drops.extend(f.shadows.iter().map(|s| Shadow { colour: Some(s.colour.unwrap_or(colour)), ..*s }));
                drops.truncate(16);
                look.drops = Some(drops.into());
            }
            ops = f.ops;
        }
        let blend = style.get("mix-blend-mode").and_then(Blend::of);
        // With what is under it in the stacking context around it; over
        // nothing there, blending changes nothing.
        if let (Some(mode), Some(Paint::Rgb(r, g, b))) = (blend, self.backdrop) {
            ops.push(Op::blend(mode, (r, g, b)));
        }
        look.effects = Effects::inside(&self.effects, ops);

        // A box of no height paints no background behind what spills out of it.
        // The page's own background (html's, else body's) covers the window
        // whatever their height.
        let flat = !is_inline(name, style)
            && style.get("display").is_none_or(|d| !d.starts_with("table"))
            && !matches!(name, "td" | "th" | "tr" | "table" | "tbody" | "thead" | "tfoot" | "html" | "body")
            && !opened(style)
            && style.get("height").or_else(|| style.get("max-height")).is_some_and(tiny);
        let clips_to_text = match style.get("background-clip") {
            Some(v) => v.split(',').any(|p| p.trim() == "text"),
            // The shorthand sets the clip too.
            None => style.get("background").is_some_and(|v| tokens(v).contains(&"text")),
        };
        let positioned = style.get("position").is_some_and(|p| p != "static");
        let z = style.get("z-index").and_then(|v| number_prefix(v)).map(|(n, _)| n);
        if positioned && z.is_some_and(|z| z < 0.0) && self.bg_in_context {
            look.buried = true;
        }
        let set = |p: &str| style.get(p).is_some_and(|v| v != "none");
        let will_change =
            |props: &[&str]| style.get("will-change").is_some_and(|v| v.split(',').any(|w| props.contains(&w.trim())));
        // Filters, transparency, masks and blending start a backdrop root:
        // a backdrop filter inside sees only what is drawn inside it.
        const ROOTS: &[&str] = &["filter", "backdrop-filter", "mask", "mask-image", "clip-path"];
        let backdrop_root = own_opacity(style) < 1.0
            || ROOTS.iter().any(|p| set(p))
            || blend.is_some()
            || will_change(&["opacity", "filter", "backdrop-filter", "mask", "clip-path", "mix-blend-mode"]);
        // A new stacking context: what it holds is drawn above its
        // background, and blends with what it paints only.
        let stacking = backdrop_root
            || ((positioned || self.flexible) && z.is_some())
            || matches!(style.get("position"), Some("fixed" | "sticky" | "-webkit-sticky"))
            || ["transform", "translate", "scale", "rotate", "perspective", "offset-path"].iter().any(|p| set(p))
            || style.get("transform-style") == Some("preserve-3d")
            || style.get("isolation") == Some("isolate")
            || style
                .get("contain")
                .is_some_and(|v| v.split(' ').any(|w| matches!(w, "layout" | "paint" | "strict" | "content")))
            || will_change(&["transform", "translate", "scale", "rotate", "isolation", "perspective"]);
        if stacking {
            look.backdrop = None;
        }
        // What is behind the element, seen through its backdrop filter.
        if let Some(v) = style.get("backdrop-filter").filter(|v| *v != "none")
            && let Some(under) = self.under
        {
            let filtered = Effects::apply(&Effects::inside(&None, crate::effects::filter(v, font).ops), under);
            look.background = Some(Effects::apply(&self.effects, filtered));
            look.backdrop = Some(filtered);
        }
        if backdrop_root {
            look.under = None;
        }
        // The browser's own background for marked text and form controls.
        let browsers = || ["background", "background-color", "background-image"].iter().all(|p| style.get(p).is_none());
        let own = own_background(name, attr, style).or_else(|| ua_colours(name).filter(|_| browsers()).map(|c| c.1));
        // The page's own background (html's, else body's) is the window's:
        // under everything, and through none of their effects.
        let canvas = name == "html" || (name == "body" && !self.root_bg);
        if let Some(bg) = own {
            let seen = if canvas { bg } else { Effects::apply(&look.effects, bg) };
            if clips_to_text {
                look.clip_bg = Some(seen);
            } else if !flat {
                look.background = Some(seen);
                look.backdrop = Some(bg);
                look.under = Some(bg);
                // It covers a background clipped to the letters around it,
                // and the shadows of what is around it fall on it, not on
                // the letters.
                look.clip_bg = None;
                look.drops = None;
                look.bg_in_context |= !canvas;
                look.root_bg |= name == "html";
            }
        }
        if stacking {
            look.bg_in_context = false;
        }

        match style.get("visibility") {
            Some("hidden" | "collapse") => look.visible = false,
            Some("visible") => look.visible = true,
            _ => {}
        }
        look
    }
}

/// The colours WebKit's own style sheet gives an element, text and
/// background, whatever its parent's: marked text, form controls, dialogs.
fn ua_colours(name: &str) -> Option<(Paint, Paint)> {
    match name {
        "mark" => Some((BLACK, Paint::Rgb(255, 255, 0))),
        "button" => Some((BLACK, Paint::Rgb(239, 239, 239))),
        "input" | "select" | "textarea" | "dialog" => Some((BLACK, WHITE)),
        _ => None,
    }
}

/// A length across a box, as a range: percentages count from both ends of
/// `room`. `None`: not a length.
fn across(v: &str, font: f32, room: (f32, f32)) -> Option<(f32, f32)> {
    let (a, b) = (length_of(v, font, room.0)?, length_of(v, font, room.1)?);
    Some((a.min(b), a.max(b)))
}

/// The left and right values of a four-sided shorthand (`padding: 1px 2px
/// 3px 4px`).
fn left_right(v: &str) -> (&str, &str) {
    let t = tokens(v);
    let first = t.first().copied().unwrap_or("");
    let right = t.get(1).copied().unwrap_or(first);
    (t.get(3).copied().unwrap_or(right), right)
}

/// The start and end values of a two-sided shorthand (`padding-inline: 1px
/// 2px`), left and right in mail's left-to-right writing.
fn start_end(v: &str) -> (&str, &str) {
    let t = tokens(v);
    let first = t.first().copied().unwrap_or("");
    (first, t.get(1).copied().unwrap_or(first))
}

/// Padding and borders across a box (left and right together), in CSS
/// pixels, at least and at most.
fn edges(style: &Computed, font: f32, room: (f32, f32)) -> (f32, f32) {
    let len = |v: &str| across(v, font, room).map_or((0.0, 0.0), |(a, b)| (a.max(0.0), b.max(0.0)));
    let (mut left, mut right) = ((0.0, 0.0), (0.0, 0.0));
    if let Some(v) = style.get("padding") {
        let (l, r) = left_right(v);
        (left, right) = (len(l), len(r));
    }
    if let Some(v) = style.get("padding-inline") {
        let (l, r) = start_end(v);
        (left, right) = (len(l), len(r));
    }
    if let Some(v) = style.get("padding-left") {
        left = len(v);
    }
    if let Some(v) = style.get("padding-right") {
        right = len(v);
    }
    // A border is as wide as said only when drawn (a style is given):
    // per side, its width and whether it is drawn.
    let width = |w: &str| match w {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        w => number_prefix(w).and_then(|_| length_of(w, font, font)).map(|n| n.max(0.0)),
    };
    let drawn =
        |w: &str| matches!(w, "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset");
    let shorthand = |v: &str| {
        let words = tokens(v);
        (words.iter().find_map(|w| width(w)).unwrap_or(3.0), words.iter().any(|w| drawn(w)))
    };
    let (mut bl, mut br) = ((3.0, false), (3.0, false));
    for p in ["border", "border-inline"] {
        if let Some(v) = style.get(p) {
            bl = shorthand(v);
            br = bl;
        }
    }
    for (p, side) in [("border-left", &mut bl), ("border-right", &mut br)] {
        if let Some(v) = style.get(p) {
            *side = shorthand(v);
        }
    }
    if let Some(v) = style.get("border-width") {
        let (l, r) = left_right(v);
        bl.0 = width(l).unwrap_or(bl.0);
        br.0 = width(r).unwrap_or(br.0);
    }
    if let Some(v) = style.get("border-style") {
        let (l, r) = left_right(v);
        (bl.1, br.1) = (drawn(l), drawn(r));
    }
    for (p, side) in [("border-left-width", &mut bl), ("border-right-width", &mut br)] {
        if let Some(w) = style.get(p).and_then(width) {
            side.0 = w;
        }
    }
    for (p, side) in [("border-left-style", &mut bl), ("border-right-style", &mut br)] {
        if let Some(v) = style.get(p) {
            side.1 = drawn(v);
        }
    }
    let borders = if bl.1 { bl.0 } else { 0.0 } + if br.1 { br.0 } else { 0.0 };
    (left.0 + right.0 + borders, left.1 + right.1 + borders)
}

/// A box's left and right margins together, in CSS pixels, at least and at
/// most (`auto` as none); `default` on each side unless set.
fn margins_across(style: &Computed, font: f32, room: (f32, f32), default: f32) -> (f32, f32) {
    let len = |v: &str| if v == "auto" { (0.0, 0.0) } else { across(v, font, room).unwrap_or((0.0, 0.0)) };
    let (mut left, mut right) = ((default, default), (default, default));
    if let Some(v) = style.get("margin") {
        let (l, r) = left_right(v);
        (left, right) = (len(l), len(r));
    }
    if let Some(v) = style.get("margin-inline") {
        let (l, r) = start_end(v);
        (left, right) = (len(l), len(r));
    }
    if let Some(v) = style.get("margin-left") {
        left = len(v);
    }
    if let Some(v) = style.get("margin-right") {
        right = len(v);
    }
    (left.0 + right.0, left.1 + right.1)
}

/// Does white space keep its line breaks inside an element: from
/// `white-space` and `white-space-collapse`, and the elements that keep
/// them; else as around it.
fn white_space(name: &str, style: &Computed, mut pre: bool) -> bool {
    if matches!(name, "pre" | "listing" | "xmp" | "plaintext" | "textarea") {
        pre = true;
    }
    match style.get("white-space") {
        Some("normal" | "nowrap" | "initial") => pre = false,
        Some("pre" | "pre-wrap" | "pre-line" | "break-spaces") => pre = true,
        Some(v) if v.contains(' ') => pre = v.contains("preserve") || v.contains("break-spaces"),
        _ => {}
    }
    match style.get("white-space-collapse") {
        Some("preserve" | "preserve-breaks" | "break-spaces") => pre = true,
        Some("collapse" | "preserve-spaces" | "initial") => pre = false,
        _ => {}
    }
    pre
}

/// A `color-scheme` value (or `<meta name="color-scheme">`'s content): is
/// the page made for a dark scheme alone? `None`: not a value WebKit takes.
pub fn dark_only(v: &str) -> Option<bool> {
    let words: Vec<String> = v.split_whitespace().map(str::to_ascii_lowercase).collect();
    if words == ["normal"] {
        return Some(false);
    }
    let only = words.iter().filter(|w| *w == "only").count();
    let ident = |w: &str| {
        !matches!(w, "normal" | "inherit" | "initial" | "unset" | "default" | "revert" | "revert-layer")
            && w.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '-')
            && w.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    };
    let valid = words.iter().any(|w| w != "only")
        && only <= 1
        && (only == 0 || words[0] == "only" || words[words.len() - 1] == "only")
        && words.iter().all(|w| ident(w));
    valid.then(|| words.iter().any(|w| w == "dark") && !words.iter().any(|w| w == "light"))
}

/// `-webkit-text-stroke`: the width (CSS pixels) and colour (`None`: the
/// text's) of an outline around letters, inherited from `parent`.
fn stroke_of(style: &Computed, parent: (f32, Option<Paint>), font: f32) -> (f32, Option<Paint>) {
    let width = |t: &str| match t {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        t => length_of(t, font, font).map(|w| w.max(0.0)),
    };
    let mut s = parent;
    match style.get("-webkit-text-stroke") {
        Some("inherit" | "unset" | "revert" | "revert-layer") | None => {}
        Some(v) => {
            s = (0.0, None);
            for t in tokens(v) {
                if let Some(w) = width(t) {
                    s.0 = w;
                } else if t != "currentcolor" {
                    s.1 = paint(t);
                }
            }
        }
    }
    match style.get("-webkit-text-stroke-width") {
        Some("initial") => s.0 = 0.0,
        Some(v) => s.0 = width(v).unwrap_or(s.0),
        None => {}
    }
    match style.get("-webkit-text-stroke-color") {
        Some("currentcolor" | "initial") => s.1 = None,
        Some(v) => s.1 = paint(v).or(s.1),
        None => {}
    }
    s
}

/// Is the element contained in size, across at least (`container-type`,
/// `contain`)?
fn size_contained(style: &Computed) -> bool {
    let kind = |t: &str| matches!(t.trim(), "size" | "inline-size");
    let typed = match style.get("container-type") {
        Some(t) => kind(t),
        None => style.get("container").and_then(|v| v.split_once('/')).is_some_and(|(_, t)| kind(t)),
    };
    typed || style.get("contain").is_some_and(|v| v.split(' ').any(|w| matches!(w, "size" | "inline-size" | "strict")))
}

/// Can size container queries ask about this element's box? Not when
/// containment does not apply to it: an inline box that is not a block
/// inside (a `<span>`), a table and its rows and cells, no box at all.
pub fn sized_box(name: &str, style: &Computed) -> bool {
    match style.get("display") {
        Some(d) => {
            !(matches!(d, "inline" | "inline flow" | "flow inline" | "contents" | "none")
                || (d.contains("table") && d != "table-caption")
                || d.starts_with("ruby"))
        }
        None => {
            !(INLINE.contains(&name)
                || matches!(name, "table" | "tr" | "td" | "th" | "tbody" | "thead" | "tfoot" | "col" | "colgroup"))
        }
    }
}

/// Is the element a block container, the text of which `::first-line` and
/// `::first-letter` style?
pub fn block_container(name: &str, style: &Computed) -> bool {
    match style.get("display") {
        Some(d) => {
            matches!(d, "block" | "inline-block" | "list-item" | "table-cell" | "table-caption" | "flow-root")
                || (d.contains(' ') && (d.contains("flow-root") || (d.contains("block") && d.contains("flow"))))
        }
        None => {
            !(INLINE.contains(&name)
                || matches!(
                    name,
                    "table"
                        | "tr"
                        | "tbody"
                        | "thead"
                        | "tfoot"
                        | "col"
                        | "colgroup"
                        | "img"
                        | "br"
                        | "hr"
                        | "input"
                        | "select"
                        | "textarea"
                        | "button"
                ))
        }
    }
}

/// Elements on which WebKit honours the old `bgcolor` and `background`
/// attributes.
const BACKGROUND_ATTRIBUTES: &[&str] = &["body", "table", "thead", "tbody", "tfoot", "tr", "td", "th", "marquee"];

/// The background an element paints itself, if any (values compacted by
/// [`compact_background`]). Styles override the old attributes; a picture
/// covers the colour under it.
fn own_background<'e>(name: &str, attr: &dyn Fn(&str) -> Option<&'e str>, style: &Computed) -> Option<Paint> {
    let attributes = BACKGROUND_ATTRIBUTES.contains(&name);
    // A shorthand written after a longhand has reset it (see `set`).
    let image = style.get("background-image").or_else(|| style.get("background"));
    let picture = match image {
        Some(v) => v.contains("url(") || v.contains("var("),
        None => attributes && attr("background").is_some_and(|s| !s.trim().is_empty()),
    };
    if picture {
        return Some(Paint::Image);
    }
    // A gradient of one colour, in background-image, paints over the colour.
    if let Some(p) = style.get("background-image").and_then(paint).filter(|p| *p != Paint::Clear) {
        return Some(p);
    }
    let colour = match (style.get("background-color"), style.get("background")) {
        (Some(v), _) => paint(v),
        // The shorthand without a colour sets it transparent.
        (None, Some(v)) => tokens(v).into_iter().find_map(paint).or(Some(Paint::Clear)),
        (None, None) if attributes => attr("bgcolor").and_then(paint_attr),
        (None, None) => None,
    };
    colour.filter(|p| *p != Paint::Clear)
}

/// Where Mail draws a message, as media queries see it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    /// The message view, in CSS pixels.
    pub width: f32,
    pub height: f32,
    /// The screen.
    pub screen_width: f32,
    pub screen_height: f32,
    /// Device pixels per CSS pixel.
    pub resolution: f32,
}

/// The two windows a message is read in when what it shows depends on the
/// window (see [`crate::html::to_text`]): what is hidden in either is
/// hidden. A message view a little taller than wide on a laptop's Retina
/// screen, and a wide one on a large external screen.
pub const WINDOWS: [Window; 2] = [
    Window { width: 900.0, height: 940.0, screen_width: 1728.0, screen_height: 1117.0, resolution: 2.0 },
    Window { width: 1600.0, height: 900.0, screen_width: 2560.0, screen_height: 1440.0, resolution: 1.0 },
];

/// A ratio, `16/9` or `1.5`.
fn ratio(v: &str) -> Option<f32> {
    let (a, b) = match v.split_once('/') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (v.trim(), "1"),
    };
    let num = |t: &str| number_prefix(t).filter(|(n, u)| u.is_empty() && *n >= 0.0).map(|(n, _)| n);
    let (a, b) = (num(a)?, num(b)?);
    Some(if b == 0.0 { f32::INFINITY } else { a / b })
}

fn plain_length(v: &str) -> Option<f32> {
    length_px(v, 16.0)
}

fn integer(v: &str) -> Option<f32> {
    number_prefix(v).filter(|(_, u)| u.is_empty()).map(|(n, _)| n)
}

/// How a media feature's value is read, in its unit.
type ReadValue = fn(&str) -> Option<f32>;

/// The value of a media feature that has a range, in the window, and how
/// a value of it is written.
fn range_feature(name: &str, w: &Window) -> Option<(f32, ReadValue)> {
    Some(match name {
        "width" => (w.width, plain_length),
        "height" => (w.height, plain_length),
        "device-width" => (w.screen_width, plain_length),
        "device-height" => (w.screen_height, plain_length),
        "aspect-ratio" => (w.width / w.height, ratio),
        "device-aspect-ratio" => (w.screen_width / w.screen_height, ratio),
        "resolution" => (w.resolution, resolution),
        "-webkit-device-pixel-ratio" => (w.resolution, integer_or_decimal),
        "color" => (8.0, integer),
        "color-index" | "monochrome" => (0.0, integer),
        _ => return None,
    })
}

fn integer_or_decimal(v: &str) -> Option<f32> {
    number_prefix(v).filter(|(_, u)| u.is_empty()).map(|(n, _)| n)
}

/// `-webkit-min-device-pixel-ratio` is a bound of
/// `-webkit-device-pixel-ratio`.
fn webkit_feature(base: &str) -> &str {
    match base {
        "device-pixel-ratio" => "-webkit-device-pixel-ratio",
        other => other,
    }
}

/// Mail's window on a Mac as media queries see it: light, scripts off,
/// mouse and trackpad, the sizes of `w`. `None` for a feature WebKit does
/// not know: the query then fails, as in WebKit.
fn feature_holds(name: &str, value: &str, w: &Window) -> Option<bool> {
    let boolean = value.is_empty();
    let (bound, base) = match (name.strip_prefix("min-"), name.strip_prefix("max-")) {
        (Some(b), _) => (Some(true), b),
        (_, Some(b)) => (Some(false), b),
        _ => match (name.strip_prefix("-webkit-min-"), name.strip_prefix("-webkit-max-")) {
            (Some(b), _) => (Some(true), webkit_feature(b)),
            (_, Some(b)) => (Some(false), webkit_feature(b)),
            _ => (None, name),
        },
    };
    if let Some((actual, read)) = range_feature(base, w) {
        return match (bound, boolean) {
            (None, true) => Some(actual != 0.0),
            (Some(_), true) => None,
            (bound, false) => {
                let wanted = read(value)?;
                Some(match bound {
                    Some(true) => actual >= wanted - 0.001,
                    Some(false) => actual <= wanted + 0.001,
                    None => (actual - wanted).abs() < 0.001,
                })
            }
        };
    }
    if bound.is_some() {
        return None;
    }
    Some(match name {
        "orientation" => match value {
            "portrait" => w.height >= w.width,
            "landscape" => w.width > w.height,
            "" => true,
            _ => return None,
        },
        "prefers-color-scheme" => value == "light",
        "prefers-reduced-motion" | "prefers-reduced-transparency" | "prefers-contrast" | "prefers-reduced-data" => {
            value == "no-preference"
        }
        "inverted-colors" | "forced-colors" => value == "none",
        "scripting" => value == "none",
        "update" => boolean || value == "fast",
        "overflow-block" | "overflow-inline" => boolean || value == "scroll",
        "display-mode" => boolean || value == "browser",
        "color-gamut" => boolean || matches!(value, "srgb" | "p3"),
        "dynamic-range" | "video-dynamic-range" => value == "standard",
        "hover" | "any-hover" => boolean || value == "hover",
        "pointer" | "any-pointer" => boolean || value == "fine",
        "grid" => value == "0",
        "-webkit-transform-3d" | "-webkit-transform-2d" | "-webkit-transition" | "-webkit-animation" => {
            boolean || number_prefix(value).is_some_and(|(n, _)| n == 1.0)
        }
        _ => return None,
    })
}

/// A resolution in device pixels per CSS pixel: `2`, `2dppx`, `2x`, `192dpi`.
fn resolution(value: &str) -> Option<f32> {
    let (n, unit) = number_prefix(value.trim())?;
    match unit.trim() {
        "" | "x" | "dppx" => Some(n),
        "dpi" => Some(n / 96.0),
        "dpcm" => Some(n * 2.54 / 96.0),
        _ => None,
    }
}

/// The values and operators of range syntax: `400px<width<=700px` gives
/// (["400px", "width", "700px"], ["<", "<="]). `None` without operators.
fn range_parts(f: &str) -> Option<(Vec<String>, Vec<&'static str>)> {
    let f: String = f.chars().filter(|c| !c.is_whitespace()).collect();
    let ops = ["<=", ">=", "<", ">", "="];
    let mut parts = vec![];
    let mut seps = vec![];
    let mut rest = f.as_str();
    'outer: loop {
        for (i, _) in rest.char_indices() {
            if let Some(op) = ops.iter().find(|op| rest[i..].starts_with(**op)) {
                parts.push(rest[..i].to_string());
                seps.push(*op);
                rest = &rest[i + op.len()..];
                continue 'outer;
            }
        }
        parts.push(rest.to_string());
        break;
    }
    (!seps.is_empty() && parts.len() == seps.len() + 1 && seps.len() <= 2).then_some((parts, seps))
}

/// Range syntax: `width < 600px`, `600px <= width`, `400px < width < 700px`,
/// `aspect-ratio > 1/1000`, `resolution >= 2dppx`.
fn range_holds(f: &str, window: &Window) -> Option<bool> {
    let (parts, seps) = range_parts(f)?;
    let feature = parts.iter().position(|p| range_feature(p, window).is_some())?;
    let name = parts[feature].as_str();
    for (k, op) in seps.iter().enumerate() {
        let (value, flipped) = if k < feature { (parts[k].as_str(), true) } else { (parts[k + 1].as_str(), false) };
        // "width < X" asks for a maximum; "X < width" for a minimum.
        let max = matches!((*op, flipped), ("<" | "<=", false) | (">" | ">=", true));
        let min = matches!((*op, flipped), (">" | ">=", false) | ("<" | "<=", true));
        let (actual, read) = range_feature(name, window)?;
        let wanted = read(value)?;
        let strict = matches!(*op, "<" | ">");
        let holds = if max {
            if strict { actual < wanted } else { actual <= wanted + 0.001 }
        } else if min {
            if strict { actual > wanted } else { actual >= wanted - 0.001 }
        } else {
            (actual - wanted).abs() < 0.001
        };
        if !holds {
            return Some(false);
        }
    }
    Some(true)
}

/// A condition's text as CSS reads it: comments dropped, escapes decoded,
/// lower-cased, single spaces. `None` if a token makes it invalid.
pub(crate) fn normalize(text: &str) -> Option<String> {
    normalize_as(text, Case::Lower)
}

/// As [`normalize`], names keeping their case (`@layer A`, `@container
/// card`, `style(--theme: Dark)`).
pub(crate) fn normalize_keep(text: &str) -> Option<String> {
    normalize_as(text, Case::Keep)
}

fn normalize_as(text: &str, case: Case) -> Option<String> {
    let mut input = Parser::new(text);
    let mut out = String::new();
    while let Ok(t) = input.next_including_whitespace() {
        let t = t.clone();
        write_token_as(&t, &mut input, &mut out, 0, case).ok()?;
    }
    Some(out.trim().to_string())
}

/// A piece of a normalized condition.
#[derive(Debug, PartialEq)]
pub(crate) enum Item<'a> {
    Word(&'a str),
    /// `(…)`, its inside.
    Group(&'a str),
    /// `name(…)`.
    Call(&'a str, &'a str),
    Comma,
}

/// The pieces of a normalized condition: words, groups in parentheses
/// (strings inside them kept whole), calls and commas.
pub(crate) fn items(s: &str) -> Option<Vec<Item<'_>>> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    // The end of the group whose '(' is at `open`, strings skipped.
    let close = |open: usize| -> Option<usize> {
        let (mut depth, mut quote, mut j) = (0i32, 0u8, open);
        while j < b.len() {
            match (quote, b[j]) {
                (0, b'"' | b'\'') => quote = b[j],
                (q, b'\\') if q != 0 => j += 1,
                (q, c) if q != 0 && c == q => quote = 0,
                (0, b'(') => depth += 1,
                (0, b')') => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
            j += 1;
        }
        None
    };
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b',' => {
                out.push(Item::Comma);
                i += 1;
            }
            b'(' => {
                let end = close(i)?;
                out.push(Item::Group(s[i + 1..end].trim()));
                i = end + 1;
            }
            _ => {
                let start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'(' | b',' | b')') {
                    i += 1;
                }
                if b.get(i) == Some(&b'(') && i > start {
                    let end = close(i)?;
                    out.push(Item::Call(&s[start..i], s[i + 1..end].trim()));
                    i = end + 1;
                } else if i > start {
                    out.push(Item::Word(&s[start..i]));
                } else {
                    return None; // a stray ')'
                }
            }
        }
    }
    Some(out)
}

/// `not A`, `A and B and …`, `A or B or …` (not mixed), each `A` judged by
/// `one`: the grammar shared by media queries and `@supports`.
pub(crate) fn condition(items: &[Item<'_>], one: &dyn Fn(&Item<'_>) -> bool) -> bool {
    match items {
        [Item::Word("not"), x] => !one(x),
        [first, rest @ ..] => {
            let mut op = None;
            let mut all = vec![one(first)];
            for pair in rest.chunks(2) {
                let [Item::Word(w), x] = pair else { return false };
                if op.is_some_and(|o| o != *w) || !matches!(*w, "and" | "or") {
                    return false;
                }
                op = Some(*w);
                all.push(one(x));
            }
            if op == Some("or") { all.iter().any(|v| *v) } else { all.iter().all(|v| *v) }
        }
        [] => false,
    }
}

/// One `( … )` of a media query: a nested condition, or a feature.
fn media_in_parens(item: &Item<'_>, depth: usize, window: &Window) -> bool {
    let Item::Group(inner) = item else { return false };
    if depth > 16 {
        return false;
    }
    if inner.starts_with('(') || inner.starts_with("not ") || inner.starts_with("not(") {
        return items(inner).is_some_and(|it| condition(&it, &|x| media_in_parens(x, depth + 1, window)));
    }
    if let Some(h) = range_holds(inner, window) {
        return h;
    }
    let (name, value) = inner.split_once(':').unwrap_or((inner, ""));
    feature_holds(name.trim(), value.trim(), window) == Some(true)
}

/// Does a media query list hold for Mail on a Mac, in `window` (see
/// [`feature_holds`])?
pub fn media_applies(prelude: &str, window: &Window) -> bool {
    let Some(text) = normalize(prelude) else { return false };
    let Some(all) = items(&text) else { return false };
    if all.is_empty() {
        return true;
    }
    all.split(|i| *i == Item::Comma).any(|query| {
        let (negated, query) = match query {
            [Item::Word("not"), Item::Word(t), rest @ ..] if !t.is_empty() => (true, (Some(*t), rest)),
            [Item::Word("only"), Item::Word(t), rest @ ..] => (false, (Some(*t), rest)),
            [Item::Word(t), rest @ ..] if *t != "not" => (false, (Some(*t), rest)),
            rest => (false, (None, rest)),
        };
        let (media_type, rest) = query;
        let holds = match media_type {
            Some(t) => {
                let type_ok = matches!(t, "all" | "screen");
                match rest {
                    [] => type_ok,
                    [Item::Word("and"), cond @ ..] => {
                        // After a media type, conditions joined by "and" only.
                        type_ok
                            && !cond.contains(&Item::Word("or"))
                            && condition(cond, &|x| media_in_parens(x, 0, window))
                    }
                    _ => false,
                }
            }
            None => !rest.is_empty() && condition(rest, &|x| media_in_parens(x, 0, window)),
        };
        holds != negated
    })
}

/// `not A`, `A and B…`, `A or B…`, as [`condition`], but `None` when a part
/// is not understood: WebKit then drops the rule, `not` or not. Every part
/// is judged, so that one not understood is always found.
fn condition_opt(items: &[Item<'_>], one: &mut dyn FnMut(&Item<'_>) -> Option<bool>) -> Option<bool> {
    match items {
        [Item::Word(not), x] if not.eq_ignore_ascii_case("not") => one(x).map(|v| !v),
        [first, rest @ ..] => {
            let mut op: Option<String> = None;
            let mut all = vec![one(first)?];
            for pair in rest.chunks(2) {
                let [Item::Word(w), x] = pair else { return None };
                let w = w.to_ascii_lowercase();
                if op.as_ref().is_some_and(|o| *o != w) || !matches!(w.as_str(), "and" | "or") {
                    return None;
                }
                op = Some(w);
                all.push(one(x)?);
            }
            Some(if op.as_deref() == Some("or") { all.iter().any(|v| *v) } else { all.iter().all(|v| *v) })
        }
        [] => None,
    }
}

/// A test in a container query: a size feature in parentheses
/// (`min-width: 400px`, `width > 30em`, `orientation: portrait`) or the
/// inside of `style()`.
pub(crate) enum Probe<'a> {
    Size(&'a str),
    Style(&'a str),
}

/// Does a container query (normalized, see [`normalize`]) hold, its tests
/// judged by `probe`? `None`: not a query WebKit reads.
pub(crate) fn container_holds(text: &str, probe: &mut dyn FnMut(Probe<'_>) -> Option<bool>) -> Option<bool> {
    container_part(text, probe, 0)
}

fn container_part(text: &str, probe: &mut dyn FnMut(Probe<'_>) -> Option<bool>, depth: usize) -> Option<bool> {
    if depth > 16 {
        return None;
    }
    let all = items(text)?;
    condition_opt(&all, &mut |item| match item {
        Item::Group(inner) if starts_condition(inner) => container_part(inner, &mut *probe, depth + 1),
        Item::Group(inner) => probe(Probe::Size(inner)),
        Item::Call("style", inner) => probe(Probe::Style(inner)),
        _ => None,
    })
}

/// A nested condition rather than a feature: `(…)` or `not …`.
fn starts_condition(inner: &str) -> bool {
    let lower = inner.get(..4).map(str::to_ascii_lowercase).unwrap_or_default();
    inner.starts_with('(') || lower == "not " || lower == "not("
}

/// Does a size feature hold for a container whose width and height are in
/// these ranges (in CSS pixels)? When it holds for some sizes in them and
/// not for others, as `assume` says: the page is then read both ways, and
/// what either way hides is hidden (see [`crate::html::to_text`]). `needs`
/// notes the sizes asked about, across and down; `values`, the widths
/// compared with. `None`: not a feature, or a value, WebKit reads.
pub(crate) fn size_feature_holds(
    f: &str,
    width: (f32, f32),
    height: (f32, f32),
    needs: &mut (bool, bool),
    assume: bool,
    font: f32,
    mut values: Option<&mut Vec<(f32, bool)>>,
) -> Option<bool> {
    let f = &f.to_ascii_lowercase();
    // Which size a feature asks about: 0 across, 1 down.
    let axis = |name: &str| match name {
        "width" | "inline-size" => Some(0),
        "height" | "block-size" => Some(1),
        _ => None,
    };
    let mut range = |k: usize| {
        if k == 0 {
            needs.0 = true;
            width
        } else {
            needs.1 = true;
            height
        }
    };
    // How `size op v` holds across a range of sizes: everywhere, nowhere,
    // or (`None`) for some sizes only.
    let compare = |(lo, hi): (f32, f32), op: &str, v: f32| -> Option<bool> {
        if op == "=" {
            return if lo == hi {
                Some(lo == v)
            } else if v < lo || v > hi {
                Some(false)
            } else {
                None
            };
        }
        let at = |w: f32| match op {
            "<" => w < v,
            "<=" => w <= v,
            ">" => w > v,
            _ => w >= v,
        };
        (at(lo) == at(hi)).then_some(at(lo))
    };
    if let Some((parts, seps)) = range_parts(f) {
        let feature = parts.iter().position(|p| axis(p).is_some() || p == "aspect-ratio")?;
        if parts[feature] == "aspect-ratio" {
            range(0);
            range(1);
            return Some(assume);
        }
        let across = axis(&parts[feature])? == 0;
        let sizes = range(if across { 0 } else { 1 });
        let mut all = Some(true);
        for (k, op) in seps.iter().enumerate() {
            let (value, flipped) = if k < feature { (&parts[k], true) } else { (&parts[k + 1], false) };
            let v = length_px(value, font)?;
            if across && let Some(values) = values.as_deref_mut() {
                values.push((v, length_px(value, 2.0 * font) != Some(v)));
            }
            // "X < width" is "width > X".
            let op = match (*op, flipped) {
                ("<", true) => ">",
                ("<=", true) => ">=",
                (">", true) => "<",
                (">=", true) => "<=",
                (op, _) => op,
            };
            all = match (all, compare(sizes, op, v)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
        }
        return Some(all.unwrap_or(assume));
    }
    let (name, value) = match f.split_once(':') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (f.trim(), None),
    };
    let (bound, base) = match (name.strip_prefix("min-"), name.strip_prefix("max-")) {
        (Some(b), _) => ("min", b),
        (_, Some(b)) => ("max", b),
        _ => ("", name),
    };
    if let Some(k) = axis(base) {
        let sizes = range(k);
        return match (bound, value) {
            ("", None) => Some(compare(sizes, ">", 0.0).unwrap_or(assume)),
            (_, None) => None,
            (bound, Some(v)) => {
                let px = length_px(v, font)?;
                if k == 0
                    && let Some(values) = values
                {
                    values.push((px, length_px(v, 2.0 * font) != Some(px)));
                }
                let v = px;
                let op = match bound {
                    "min" => ">=",
                    "max" => "<=",
                    _ => "=",
                };
                Some(compare(sizes, op, v).unwrap_or(assume))
            }
        };
    }
    match (base, bound, value) {
        ("aspect-ratio", _, v) => {
            let ratio =
                v.is_none_or(|v| v.split('/').all(|n| number_prefix(n.trim()).is_some_and(|(_, u)| u.is_empty())));
            range(0);
            range(1);
            ratio.then_some(assume)
        }
        ("orientation", "", v) => {
            let (w, h) = (range(0), range(1));
            // Portrait: at least as tall as wide.
            let portrait = if h.0 >= w.1 {
                Some(true)
            } else if h.1 < w.0 {
                Some(false)
            } else {
                None
            };
            match v {
                None => Some(true),
                Some("portrait") => Some(portrait.unwrap_or(assume)),
                Some("landscape") => Some(portrait.map(|p| !p).unwrap_or(assume)),
                Some(_) => None,
            }
        }
        _ => None,
    }
}

/// A `style()` test: `--x: value` (has that value), `--x` (has one), or
/// `not`, `and`, `or` of such tests in parentheses; `get` gives a custom
/// property's value where the query looks. WebKit tests custom properties
/// only.
pub(crate) fn style_holds(f: &str, get: &dyn Fn(&str) -> Option<String>, depth: usize) -> Option<bool> {
    let f = f.trim();
    if depth > 16 {
        return None;
    }
    if f.starts_with("--") {
        return Some(match f.split_once(':') {
            Some((name, value)) => get(name.trim()).is_some_and(|v| v.trim() == value.trim()),
            None => get(f).is_some(),
        });
    }
    let all = items(f)?;
    condition_opt(&all, &mut |item| match item {
        Item::Group(inner) => style_holds(inner, get, depth + 1),
        _ => None,
    })
}

/// Does WebKit on a Mac know this property? (Not `-moz-` or `-ms-` ones,
/// nor iOS's `-webkit-overflow-scrolling`.)
pub fn property_supported(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    n.starts_with("--") || crate::css_properties::KNOWN.binary_search(&n.as_str()).is_ok()
}

/// Would WebKit take this declaration, for `@supports (prop: value)`? The
/// properties checked here have their values checked too.
pub fn declaration_supported(name: &str, value: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    let v = value.trim().to_ascii_lowercase();
    if !property_supported(&n) || v.is_empty() {
        return false;
    }
    match PROPS.iter().find(|p| **p == n).copied().or_else(|| alias(&n)) {
        Some(prop) => valid(prop, &v, false),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_are_read_as_a_browser_reads_them() {
        let get = |style: &str, prop: &str| {
            let mut c = Computed::default();
            c.apply(&Decls::parse(style, false));
            c.get(prop).map(str::to_string)
        };
        assert_eq!(get("display:none;display:bogus", "display").as_deref(), Some("none"));
        assert_eq!(get("opacity:0;opacity:x", "opacity").as_deref(), Some("0"));
        assert_eq!(get("color:#fff;color:notacolour", "color").as_deref(), Some("#fff"));
        assert_eq!(get("display:none/**/", "display").as_deref(), Some("none"));
        assert_eq!(get("display:\\6e one", "display").as_deref(), Some("none"));
        assert_eq!(get("DISPLAY : NONE ! important", "display").as_deref(), Some("none"));
        assert_eq!(get("font-size:14px;font:0/0 a", "font-size"), None, "the shorthand reset it");
        assert_eq!(get("font:0/0 a;font-size:14px", "font-size").as_deref(), Some("14px"));
        assert_eq!(get("font:0", "font"), None, "no family: invalid");
        assert_eq!(get("font-size:14", "font-size"), None, "no unit outside quirks mode");
        assert_eq!(get("height:1;overflow:hidden", "height"), None);
        assert_eq!(get("height:0;overflow:hidden", "height").as_deref(), Some("0"));
        assert_eq!(get("width:calc(100% - 20px)", "width").as_deref(), Some("calc(100% - 20px)"));
        assert_eq!(get("color:rgb( 255 , 255 , 255 )", "color").as_deref(), Some("rgb(255, 255, 255)"));
        assert_eq!(get("background:url(\"x.png\") no-repeat;color:red", "color").as_deref(), Some("red"));
        assert_eq!(get("x:\"unterminated\ndisplay:none", "display"), None, "a bad string spoils its declaration");
        assert_eq!(get("display:block !important;display:none", "display").as_deref(), Some("block"));
        let mut c = Computed::default();
        c.apply(&Decls::parse("font:0/0 a !important", false));
        c.apply(&Decls::parse("font-size:14px", false));
        assert_eq!(c.get("font-size"), None, "an important shorthand wins over a later longhand");
    }

    #[test]
    fn supported_properties() {
        assert!(property_supported("display"));
        assert!(property_supported("border-top-left-radius"));
        assert!(property_supported("container-type"));
        assert!(!property_supported("text-bogus"));
        assert!(declaration_supported("display", "grid"));
        assert!(!declaration_supported("display", "bogus"));
        assert!(property_supported("-webkit-text-fill-color"));
        assert!(!property_supported("bogus"));
        assert!(!property_supported("-moz-appearance"));
        assert!(!property_supported("-webkit-overflow-scrolling"));
    }

    #[test]
    fn colours_by_name() {
        assert!(paint("aliceblue").unwrap().same_as(paint("ghostwhite").unwrap()));
        assert_eq!(paint("rebeccapurple"), Some(Paint::Rgb(102, 51, 153)));
        assert_eq!(paint("center"), None, "not a colour");
        assert_eq!(paint("canvas"), Some(Paint::Rgb(255, 255, 255)));
    }

    #[test]
    fn colours() {
        assert_eq!(paint("#FFF"), Some(Paint::Rgb(255, 255, 255)));
        assert_eq!(paint("rgb(255, 255, 255)"), Some(Paint::Rgb(255, 255, 255)));
        assert_eq!(paint("rgba(0,0,0,0)"), Some(Paint::Clear));
        assert_eq!(paint("hsl(0, 0%, 100%)"), Some(Paint::Rgb(255, 255, 255)));
        assert_eq!(paint("#ffffff00"), Some(Paint::Clear));
        assert_eq!(paint("White"), Some(Paint::Rgb(255, 255, 255)));
        assert_eq!(paint("inherit"), None);
        assert_eq!(paint_attr("FFFFFF"), Some(Paint::Rgb(255, 255, 255)));
        assert!(Paint::Rgb(254, 254, 254).same_as(Paint::Rgb(255, 255, 255)));
        assert!(!Paint::Rgb(255, 255, 255).same_as(Paint::Image));
    }

    #[test]
    fn font_sizes() {
        assert_eq!(font_px("0px", 16.0, false), Some(0.0));
        assert_eq!(font_px("14px", 0.0, false), Some(14.0));
        assert_eq!(font_px("1em", 0.0, false), Some(0.0));
        assert_eq!(font_px("150%", 10.0, false), Some(15.0));
        assert_eq!(font_px("14", 0.0, false), None, "ignored outside quirks mode");
        assert_eq!(font_px("14", 0.0, true), Some(14.0));
        assert_eq!(font_tag_px("+2147483647"), Some(48.0));
        assert_eq!(font_px("-5px", 16.0, false), None, "invalid: dropped");
        assert_eq!(font_px("1e-9px", 16.0, false), Some(1e-9));
        assert_eq!(font_px("calc(0px)", 16.0, false), Some(0.0));
        assert_eq!(font_px("calc(16px * 0)", 16.0, false), Some(0.0));
        assert_eq!(font_px("calc(-5px)", 16.0, false), Some(0.0), "a formula is clamped, not dropped");
        assert_eq!(font_px("min(0px, 2em)", 16.0, false), Some(0.0));
        assert_eq!(font_px("clamp(0px, 0px, 20px)", 16.0, false), Some(0.0));
        assert_eq!(font_px("calc(1em + 2px)", 16.0, false), Some(18.0));
        // 1vw: a hundredth of the window being read (900 pixels unless set).
        assert_eq!(font_px("max(12px, calc(1vw + 0.5em))", 16.0, false), Some(17.0));
        assert_eq!(font_px("var(--size)", 16.0, false), None);
        assert_eq!(font_px("calc(1px * 2px)", 16.0, false), None, "invalid");
        assert!(font_px("calc(4px / 0)", 16.0, false).is_some_and(|n| n > 1e30), "infinity, as CSS Values 4 has it");
        assert_eq!(font_px("calc(x)", 16.0, false), None, "invalid");
        assert_eq!(font_px("calc(0)", 16.0, false), None, "a number is no length");
        assert_eq!(font_px("calc(round(down, 15px, 10px))", 16.0, false), Some(10.0));
        assert_eq!(font_px("calc(sin(0) * 1px)", 16.0, false), Some(0.0));
        assert_eq!(font_px(&format!("{}1px{}", "calc(".repeat(50), ")".repeat(50)), 16.0, false), Some(1.0));
        let deep = format!("{}1px{}", "calc(".repeat(200_000), ")".repeat(200_000));
        assert_eq!(font_px(&deep, 16.0, false), None, "no stack overflow");
        let deep = format!("{}1px{}", "min(".repeat(200_000), ")".repeat(200_000));
        assert_eq!(font_px(&deep, 16.0, false), None);
        let deep = format!("calc({}1px{})", "(".repeat(200_000), ")".repeat(200_000));
        assert_eq!(font_px(&deep, 16.0, false), None);
        assert_eq!(number_prefix("1em"), Some((1.0, "em")));
        assert_eq!(number_prefix(".5px"), Some((0.5, "px")));
        assert_eq!(number_prefix("1e999px"), None);
        assert_eq!(number_prefix("-"), None);
        assert_eq!(font_shorthand_px("0/0 a", 16.0), Some(0.0));
        assert_eq!(font_shorthand_px("bold 14px/1.4 Arial, sans-serif", 16.0), Some(14.0));
        assert_eq!(font_shorthand_px("700 normal Arial", 16.0), None);
        assert_eq!(font_tag_px("1"), Some(10.0));
        assert_eq!(font_tag_px("+1"), Some(18.0));
    }

    #[test]
    fn media_queries_as_on_a_mac() {
        for window in &WINDOWS {
            let media_applies = |q: &str| media_applies(q, window);
            assert!(media_applies(" only screen and (min-width:480px)"));
            assert!(!media_applies(" only screen and (max-width:480px)"));
            assert!(!media_applies("screen and (max-width: 600px)"));
            assert!(!media_applies(" (prefers-color-scheme: dark)"));
            assert!(!media_applies(" print"));
            assert!(media_applies(" print, screen"));
            assert!(media_applies(" not print"));
            assert!(!media_applies(" (width <= 480px)"));
            assert!(!media_applies(" screen yahoo"), "Yahoo's hack");
            assert!(media_applies(" screen and (-webkit-min-device-pixel-ratio:0)"));
            assert!(media_applies(" (min-width:480px) and (max-width:2000px)"));
            assert!(media_applies(" all"));
            assert!(!media_applies(" (max-width: bogus)"), "a value not understood");
        }
        // A rule for windows wider than the narrower one.
        assert!(!media_applies(" (min-width: 1000px)", &WINDOWS[0]));
        assert!(media_applies(" (min-width: 1000px)", &WINDOWS[1]));
        // Features with a range, in range syntax too.
        let [narrow, wide] = &WINDOWS;
        assert!(media_applies("(aspect-ratio > 1/1000)", narrow));
        assert!(
            !media_applies("(min-aspect-ratio: 100/1)", narrow) && !media_applies("(min-aspect-ratio: 100/1)", wide)
        );
        assert!(media_applies("(resolution >= 1dppx)", narrow) && media_applies("(resolution >= 1dppx)", wide));
        assert!(media_applies("(color > 0)", narrow));
        assert!(!media_applies("(monochrome)", narrow));
        assert!(media_applies("(orientation: portrait)", narrow) && !media_applies("(orientation: portrait)", wide));
        assert!(!media_applies("(min-height: 1199px)", narrow) && !media_applies("(min-height: 1199px)", wide));
        assert!(!media_applies("(min-device-width: 2560px)", narrow));
        assert!(media_applies("(-webkit-min-device-pixel-ratio: 2)", narrow));
        assert!(!media_applies("(-webkit-min-device-pixel-ratio: 2)", wide));
        assert!(media_applies("(400px < width < 1000px)", narrow) && !media_applies("(400px < width < 1000px)", wide));
    }
}
