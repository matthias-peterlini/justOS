//! Color strings accepted by color attributes.
//!
//! ```text
//! "#rgb"  "#rgba"  "#rrggbb"  "#rrggbbaa"     literal sRGB colors
//! "primary"  "surface-container-high"  ...    theme colors (see ColorToken)
//! "primary/40"  "#000000/50"                  any of the above at 40 % / 50 % opacity
//! "transparent"
//! ```
//!
//! Theme colors are resolved by the renderer against the palette generated
//! from the window's `accent` and `theme`, so a whole app re-colors itself
//! when either changes.

use crate::prelude::*;
use crate::suggest::best_match;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    /// Straight (non-premultiplied) sRGB with alpha.
    Rgba([u8; 4]),
    /// A palette role, with an opacity from 0 to 100 %.
    Token { token: ColorToken, opacity: u8 },
}

macro_rules! color_tokens {
    ($( $(#[$doc:meta])* $name:ident = $text:literal, )*) => {
        /// Color roles of the generated palette (Material 3 naming).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ColorToken { $( $(#[$doc])* $name, )* }

        impl ColorToken {
            pub const ALL: &'static [ColorToken] = &[$( ColorToken::$name, )*];

            pub fn name(self) -> &'static str {
                match self { $( ColorToken::$name => $text, )* }
            }

            pub fn from_name(name: &str) -> Option<ColorToken> {
                match name { $( $text => Some(ColorToken::$name), )* _ => None }
            }
        }
    };
}

color_tokens! {
    /// Accent for the most prominent elements (filled buttons, switches, sliders).
    Primary = "primary",
    OnPrimary = "on-primary",
    PrimaryContainer = "primary-container",
    OnPrimaryContainer = "on-primary-container",
    /// Less prominent accent (tonal buttons, chips).
    Secondary = "secondary",
    OnSecondary = "on-secondary",
    SecondaryContainer = "secondary-container",
    OnSecondaryContainer = "on-secondary-container",
    /// Contrasting accent, hue rotated from the primary.
    Tertiary = "tertiary",
    OnTertiary = "on-tertiary",
    TertiaryContainer = "tertiary-container",
    OnTertiaryContainer = "on-tertiary-container",
    Error = "error",
    OnError = "on-error",
    ErrorContainer = "error-container",
    OnErrorContainer = "on-error-container",
    /// Window background.
    Surface = "surface",
    OnSurface = "on-surface",
    /// Secondary text.
    OnSurfaceVariant = "on-surface-variant",
    SurfaceDim = "surface-dim",
    SurfaceBright = "surface-bright",
    SurfaceContainerLowest = "surface-container-lowest",
    SurfaceContainerLow = "surface-container-low",
    SurfaceContainer = "surface-container",
    SurfaceContainerHigh = "surface-container-high",
    SurfaceContainerHighest = "surface-container-highest",
    Outline = "outline",
    OutlineVariant = "outline-variant",
    InverseSurface = "inverse-surface",
    InverseOnSurface = "inverse-on-surface",
    InversePrimary = "inverse-primary",
    Shadow = "shadow",
    Scrim = "scrim",
}

impl Color {
    pub const TRANSPARENT: Color = Color::Rgba([0, 0, 0, 0]);

    pub fn token(token: ColorToken) -> Color {
        Color::Token { token, opacity: 100 }
    }

    pub fn parse(text: &str) -> Result<Color, String> {
        let text = text.trim();
        let (base, opacity) = match text.split_once('/') {
            Some((base, pct)) => {
                let pct = pct.trim().strip_suffix('%').unwrap_or(pct.trim());
                match pct.parse::<u8>() {
                    Ok(p) if p <= 100 => (base.trim(), p),
                    _ => return Err(format!("invalid opacity in color {text:?}: expected a percentage from 0 to 100")),
                }
            }
            None => (text, 100),
        };
        let color = if let Some(hex) = base.strip_prefix('#') {
            Color::Rgba(parse_hex(hex).ok_or_else(|| format!("invalid color {text:?}: expected #rgb, #rgba, #rrggbb or #rrggbbaa"))?)
        } else if base == "transparent" {
            Color::TRANSPARENT
        } else if let Some(token) = ColorToken::from_name(base) {
            Color::token(token)
        } else {
            let mut msg = format!("unknown color {text:?}: expected a hex color such as \"#6750a4\" or a theme color such as \"primary\"");
            if let Some(similar) = best_match(base, ColorToken::ALL.iter().map(|t| t.name())) {
                msg.push_str(&format!(" (did you mean {similar:?}?)"));
            }
            return Err(msg);
        };
        Ok(color.with_opacity(opacity))
    }

    /// Scale the alpha by `percent` (0–100).
    pub fn with_opacity(self, percent: u8) -> Color {
        let percent = percent.min(100);
        match self {
            Color::Rgba([r, g, b, a]) => Color::Rgba([r, g, b, ((a as u16 * percent as u16 + 50) / 100) as u8]),
            Color::Token { token, opacity } => Color::Token { token, opacity: ((opacity as u16 * percent as u16 + 50) / 100) as u8 },
        }
    }
}

fn parse_hex(hex: &str) -> Option<[u8; 4]> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let nibble = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok().map(|v| v * 17);
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(match hex.len() {
        3 => [nibble(0)?, nibble(1)?, nibble(2)?, 255],
        4 => [nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?],
        6 => [byte(0)?, byte(2)?, byte(4)?, 255],
        8 => [byte(0)?, byte(2)?, byte(4)?, byte(6)?],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_forms() {
        assert_eq!(Color::parse("#fff"), Ok(Color::Rgba([255, 255, 255, 255])));
        assert_eq!(Color::parse("#0f08"), Ok(Color::Rgba([0, 255, 0, 136])));
        assert_eq!(Color::parse("#6750A4"), Ok(Color::Rgba([0x67, 0x50, 0xa4, 255])));
        assert_eq!(Color::parse("#6750a480"), Ok(Color::Rgba([0x67, 0x50, 0xa4, 0x80])));
        assert_eq!(Color::parse(" primary "), Ok(Color::token(ColorToken::Primary)));
        assert_eq!(Color::parse("on-surface/60"), Ok(Color::Token { token: ColorToken::OnSurface, opacity: 60 }));
        assert_eq!(Color::parse("#000/50%"), Ok(Color::Rgba([0, 0, 0, 128])));
        assert_eq!(Color::parse("transparent"), Ok(Color::TRANSPARENT));
    }

    #[test]
    fn rejects_garbage_with_hints() {
        for bad in ["#12", "#12345", "#ggg", "#+1f", "red", "primary/101", "primary/x", ""] {
            assert!(Color::parse(bad).is_err(), "{bad:?} accepted");
        }
        assert!(Color::parse("surface-containr").unwrap_err().contains("\"surface-container\""));
        for t in ColorToken::ALL {
            assert_eq!(ColorToken::from_name(t.name()), Some(*t));
        }
    }
}
