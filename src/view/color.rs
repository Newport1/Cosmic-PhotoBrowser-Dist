//! UI color mapping for xmp color labels / reject state (kept out of the pure xmp core).

use crate::xmp::ColorLabel;
use cosmic::iced::Color;

/// Hex color for a ColorLabel swatch/dot (define once).
pub(crate) fn label_color(l: ColorLabel) -> Color {
    match l {
        ColorLabel::Red => Color::from_rgb(
            0xe0 as f32 / 255.0,
            0x50 as f32 / 255.0,
            0x50 as f32 / 255.0,
        ),
        ColorLabel::Yellow => Color::from_rgb(
            0xd8 as f32 / 255.0,
            0xb0 as f32 / 255.0,
            0x20 as f32 / 255.0,
        ),
        ColorLabel::Green => Color::from_rgb(
            0x3f as f32 / 255.0,
            0xaf as f32 / 255.0,
            0x50 as f32 / 255.0,
        ),
        ColorLabel::Blue => Color::from_rgb(
            0x4f as f32 / 255.0,
            0x7f as f32 / 255.0,
            0xe0 as f32 / 255.0,
        ),
        ColorLabel::Purple => Color::from_rgb(
            0x9a as f32 / 255.0,
            0x55 as f32 / 255.0,
            0xd0 as f32 / 255.0,
        ),
    }
}

/// Reject indicator accent (thin border / active toggle).
pub(crate) fn reject_color() -> Color {
    Color::from_rgb(
        0xd0 as f32 / 255.0,
        0x40 as f32 / 255.0,
        0x40 as f32 / 255.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_color_returns_distinct_non_black_colors() {
        let cols: Vec<_> = ColorLabel::all().iter().map(|&l| label_color(l)).collect();
        // All different
        for i in 0..cols.len() {
            for j in (i + 1)..cols.len() {
                assert_ne!(cols[i], cols[j]);
            }
        }
        // Not black/zero
        for c in &cols {
            assert!(c.r > 0.1 || c.g > 0.1 || c.b > 0.1);
        }
    }
}
