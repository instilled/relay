//! The colours a row can carry: a palette name or `#rrggbb`, as swatches to
//! sit behind a name the way `claude agents` draws them. Stated outright in
//! truecolor — the terminal's own red, blue and the rest are whatever its
//! theme made them, and the swatches should look the same everywhere.

use ratatui::style::Color;

/// The colours `^g` cycles through, in order.
pub const PALETTE: [&str; 8] = ["red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan"];

/// A colour a harness picked, by Claude's names for them: soft mid-tones.
/// A name not among them is none.
pub fn picked(name: &str) -> Option<Color> {
    let (r, g, b) = match name {
        "red" => (0xD9, 0x60, 0x5B),
        "blue" => (0x5B, 0x8F, 0xD9),
        "green" => (0x6B, 0xB0, 0x6E),
        "yellow" => (0xD9, 0xB9, 0x5B),
        "cyan" => (0x5B, 0xB8, 0xC4),
        "purple" => (0xA2, 0x7F, 0xD1),
        "orange" => (0xD9, 0x91, 0x5B),
        "pink" => (0xD5, 0x7F, 0xAE),
        _ => return None,
    };
    Some(Color::Rgb(r, g, b))
}

/// One of your own colours: a palette name, or `#rrggbb`.
pub fn parse(s: &str) -> Option<Color> {
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?));
    }
    picked(s)
}

/// Whether near-black text reads better than near-white on `c`: by its
/// relative luminance (sRGB, as WCAG has it).
pub fn dark_text_on(c: Color) -> bool {
    let Color::Rgb(r, g, b) = c else {
        return true;
    };
    let lin = |v: u8| {
        let v = f64::from(v) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let l = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    // Equal contrast against black and white sits at about 0.179.
    l > 0.179
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_hex_parse_and_nothing_else() {
        assert_eq!(parse("pink"), picked("pink"));
        assert_eq!(parse("#ff8800"), Some(Color::Rgb(0xFF, 0x88, 0x00)));
        assert_eq!(parse("#FF8800"), Some(Color::Rgb(0xFF, 0x88, 0x00)));
        for bad in ["", "#", "#fff", "#ff88001", "#gg0000", "ff8800", "chartreuse", "#ééé"] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_text_flips_with_the_luminance() {
        assert!(dark_text_on(Color::Rgb(0xFF, 0xFF, 0x00)));
        assert!(!dark_text_on(Color::Rgb(0x10, 0x20, 0x60)));
        assert!(!dark_text_on(Color::Rgb(0, 0, 0)));
        assert!(dark_text_on(Color::Rgb(255, 255, 255)));
        for c in PALETTE {
            assert!(dark_text_on(picked(c).unwrap()), "{c}");
        }
    }
}
