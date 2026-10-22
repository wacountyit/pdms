// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 PDMS contributors

//! Accent color math: WCAG contrast and the derived theme shades.

/// Page background (`--ground`); links must reach 4.5:1 on it.
pub const GROUND: Rgb = Rgb(0xF3, 0xF5, 0xF8);
pub const WHITE: Rgb = Rgb(0xFF, 0xFF, 0xFF);
pub const MIN_TEXT_CONTRAST: f64 = 4.5;
/// WCAG 2.1 non-text contrast (marks such as underlines and chart bars).
pub const MIN_MARK_CONTRAST: f64 = 3.0;

/// The dark-mode page colors these derivations are checked against (the
/// `--ground`, `--surface` and `--ink` dark tokens in `app.css`, R2 spec §7).
pub const DARK_GROUND: Rgb = Rgb(0x0F, 0x17, 0x20);
pub const DARK_SURFACE: Rgb = Rgb(0x17, 0x21, 0x2B);
pub const DARK_INK: Rgb = Rgb(0xE6, 0xEB, 0xF0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AccentError {
    Invalid,
    TooLight {
        ratio: f64,
    },
    /// No readable dark-mode shades can be derived from it.
    NoDarkShades,
}

impl AccentError {
    pub fn message(&self, input: &str) -> String {
        match self {
            Self::Invalid => "Enter the accent as a hex color such as #1F3A5F.".into(),
            Self::TooLight { ratio } => format!(
                "White text on {input} is hard to read ({ratio:.1}:1; at least 4.5:1 is needed). Choose a darker shade."
            ),
            Self::NoDarkShades => format!(
                "{input} can't be made readable in dark mode. Choose a more saturated or lighter color."
            ),
        }
    }
}

/// Parses `#RRGGBB` (case-insensitive, surrounding whitespace ignored).
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim();
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn to_linear(c: u8) -> f64 {
    let c = f64::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn from_linear(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

fn luminance(c: Rgb) -> f64 {
    0.2126 * to_linear(c.0) + 0.7152 * to_linear(c.1) + 0.0722 * to_linear(c.2)
}

/// WCAG 2.1 contrast ratio, 1.0 ..= 21.0.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn to_oklab(c: Rgb) -> (f64, f64, f64) {
    let (r, g, b) = (to_linear(c.0), to_linear(c.1), to_linear(c.2));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    (
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    )
}

fn from_oklab(lab: (f64, f64, f64)) -> Rgb {
    let (big_l, a, b) = lab;
    let l = (big_l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m = (big_l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s = (big_l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    Rgb(
        from_linear(4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s),
        from_linear(-1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s),
        from_linear(-0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s),
    )
}

/// Scales OKLCH lightness by `factor` (hue and chroma kept).
pub fn darken(c: Rgb, factor: f64) -> Rgb {
    let (l, a, b) = to_oklab(c);
    from_oklab((l * factor, a, b))
}

/// `weight` of `c` over white, mixed in sRGB.
/// Moves OKLab lightness `step` of the way towards white.
pub fn lighten(c: Rgb, step: f64) -> Rgb {
    let (l, a, b) = to_oklab(c);
    from_oklab((l + (1.0 - l) * step, a, b))
}

/// `c` at `weight` over the background `bg`.
pub fn mix_over(c: Rgb, bg: Rgb, weight: f64) -> Rgb {
    let mix = |x: u8, y: u8| (f64::from(x) * weight + f64::from(y) * (1.0 - weight)).round() as u8;
    Rgb(mix(c.0, bg.0), mix(c.1, bg.1), mix(c.2, bg.2))
}

pub fn tint(c: Rgb, weight: f64) -> Rgb {
    let mix = |x: u8| (f64::from(x) * weight + 255.0 * (1.0 - weight)).round() as u8;
    Rgb(mix(c.0), mix(c.1), mix(c.2))
}

/// The accent must carry white button text at 4.5:1.
pub fn validate_accent(input: &str) -> Result<Rgb, AccentError> {
    let c = parse_hex(input).ok_or(AccentError::Invalid)?;
    let ratio = contrast(c, WHITE);
    if ratio < MIN_TEXT_CONTRAST {
        return Err(AccentError::TooLight { ratio });
    }
    // The accent must hold in both modes (R2 spec §7).
    derive_dark(c).ok_or(AccentError::NoDarkShades)?;
    Ok(c)
}

/// The accent's dark-mode shades (R2 spec §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarkShades {
    /// Links: at least 4.5:1 on the dark ground and the dark surface.
    pub link: String,
    /// Marks (tab underlines, current-item bars, chart bars): at least 3:1 on
    /// the dark surface.
    pub mark: String,
    /// Highlight backgrounds, with `--ink` text at least 4.5:1.
    pub tint: String,
}

/// Lightens the accent until it is readable on the dark page; `None` when that
/// cannot be done.
pub fn derive_dark(accent: Rgb) -> Option<DarkShades> {
    let lighten_until = |ok: &dyn Fn(Rgb) -> bool| {
        let mut c = accent;
        for _ in 0..40 {
            if ok(c) {
                return Some(c);
            }
            c = lighten(c, 0.08);
        }
        ok(c).then_some(c)
    };
    let link = lighten_until(&|c| {
        contrast(c, DARK_GROUND) >= MIN_TEXT_CONTRAST
            && contrast(c, DARK_SURFACE) >= MIN_TEXT_CONTRAST
    })?;
    let mark = lighten_until(&|c| contrast(c, DARK_SURFACE) >= MIN_MARK_CONTRAST)?;
    let tint = mix_over(accent, DARK_SURFACE, 0.18);
    if contrast(DARK_INK, tint) < MIN_TEXT_CONTRAST {
        return None;
    }
    Some(DarkShades {
        link: link.hex(),
        mark: mark.hex(),
        tint: tint.hex(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColors {
    pub accent: String,
    pub hover: String,
    pub link: String,
    pub tint: String,
    /// Dark-mode shades (R2 spec §7).
    pub dark: DarkShades,
}

/// Hover is 12% darker; link is darkened in 4% steps (at least one) until it
/// reaches 4.5:1 on the page background; tint is 12% accent over white.
pub fn derive_theme(accent: Rgb) -> ThemeColors {
    let mut link = darken(accent, 0.96);
    for _ in 0..40 {
        if contrast(link, GROUND) >= MIN_TEXT_CONTRAST {
            break;
        }
        link = darken(link, 0.96);
    }
    ThemeColors {
        accent: accent.hex(),
        hover: darken(accent, 0.88).hex(),
        link: link.hex(),
        tint: tint(accent, 0.12).hex(),
        // A saved accent always passed `validate_accent`; the fallback only
        // covers a value that predates the dark check.
        dark: derive_dark(accent).unwrap_or_else(|| DarkShades {
            link: "#E6EBF0".into(),
            mark: "#E6EBF0".into(),
            tint: DARK_SURFACE.hex(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_round_trip() {
        assert_eq!(parse_hex(" #b45309 "), Some(Rgb(0xB4, 0x53, 0x09)));
        assert_eq!(Rgb(0xB4, 0x53, 0x09).hex(), "#B45309");
        for bad in ["B45309", "#B4530", "#B45309FF", "#GGGGGG", "", "red"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn contrast_matches_wcag_reference_values() {
        assert!((contrast(Rgb(0, 0, 0), WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Rgb(0xB4, 0x53, 0x09), WHITE) - 5.02).abs() < 0.01);
        assert!((contrast(Rgb(0x15, 0x20, 0x2B), GROUND) - 15.10).abs() < 0.02);
    }

    #[test]
    fn accent_too_light_is_refused() {
        match validate_accent("#F5D76E") {
            Err(AccentError::TooLight { ratio }) => assert!(ratio < 4.5),
            other => panic!("expected TooLight, got {other:?}"),
        }
        assert_eq!(validate_accent("nope"), Err(AccentError::Invalid));
        assert!(validate_accent("#B45309").is_ok());
        assert!(validate_accent("#1F3A5F").is_ok());
        assert!(
            AccentError::TooLight { ratio: 2.1 }
                .message("#F5D76E")
                .contains("2.1:1")
        );
    }

    #[test]
    fn derived_link_is_readable_for_every_valid_accent() {
        for hex in [
            "#B45309", "#1F3A5F", "#0B6B5F", "#7A1F2B", "#3446C4", "#6D28D9", "#0F766E", "#767676",
        ] {
            let accent = validate_accent(hex).unwrap();
            let t = derive_theme(accent);
            let link = parse_hex(&t.link).unwrap();
            assert!(contrast(link, GROUND) >= 4.5, "{hex} -> {}", t.link);
            assert_ne!(t.link, t.accent, "link must be at least one step darker");
            let hover = parse_hex(&t.hover).unwrap();
            assert!(contrast(hover, WHITE) >= contrast(accent, WHITE));
        }
    }

    #[test]
    fn dark_shades_hold_for_every_valid_accent() {
        for hex in [
            "#B45309", "#1F3A5F", "#0B6B5F", "#7A1F2B", "#3446C4", "#6D28D9", "#0F766E", "#767676",
            "#000000", "#15202B",
        ] {
            let accent = validate_accent(hex).unwrap();
            let d = derive_dark(accent).unwrap();
            let link = parse_hex(&d.link).unwrap();
            assert!(contrast(link, DARK_GROUND) >= 4.5, "{hex} link {}", d.link);
            assert!(contrast(link, DARK_SURFACE) >= 4.5, "{hex} link {}", d.link);
            let mark = parse_hex(&d.mark).unwrap();
            assert!(contrast(mark, DARK_SURFACE) >= 3.0, "{hex} mark {}", d.mark);
            let tint = parse_hex(&d.tint).unwrap();
            assert!(contrast(DARK_INK, tint) >= 4.5, "{hex} tint {}", d.tint);
        }
    }

    #[test]
    fn darken_keeps_hue_roughly() {
        let d = darken(Rgb(0xB4, 0x53, 0x09), 0.88);
        assert!(d.0 > d.1 && d.1 > d.2, "still an orange: {d:?}");
    }
}
