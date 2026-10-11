//! The UI font: IBM Plex Sans Regular in place of egui's default
//! proportional font (Ubuntu Light, which read too thin) — the font Zed draws
//! its UI in, drawn unhinted so it looks as it does there.
//!
//! The font file and its licence sit unmodified in
//! `assets/fonts/IBMPlexSans/`: the OFL reserves the name "Plex", so a
//! subset or otherwise changed file couldn't ship under it. Plex's digits are
//! all one width by default, which keeps changing numbers still; egui shapes
//! without OpenType features, so a font whose figures are tabular only under
//! `tnum` wouldn't do that. See `030-ui-design.md` § Theme & Contrast.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily, FontTweak};

/// IBM Plex Sans Regular, the UI's proportional font.
const REGULAR: &[u8] = include_bytes!("../../../assets/fonts/IBMPlexSans/IBMPlexSans-Regular.ttf");

/// `font_data` key of [`REGULAR`].
const REGULAR_KEY: &str = "IBMPlexSans-Regular";

/// egui's font definitions with Plex Regular first in the proportional
/// family, drawn unhinted. egui hints by default, snapping outlines to the
/// pixel grid: sharper, but thinner and more mechanical than macOS's own
/// unhinted text (Core Text, which Zed and native apps draw with) — side by
/// side with Zed, hinted Plex read as a different font. egui's own fonts stay
/// behind Plex as fallbacks (emoji, symbols Plex lacks); monospace is
/// untouched. Installed once, from the eframe creation closure.
pub(crate) fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let tweak = FontTweak {
        hinting: Some(false),
        ..FontTweak::default()
    };
    fonts.font_data.insert(
        REGULAR_KEY.to_owned(),
        Arc::new(FontData::from_static(REGULAR).tweak(tweak)),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, REGULAR_KEY.to_owned());
    fonts
}

#[cfg(test)]
mod tests {
    use egui::FontId;
    use egui::epaint::{Fonts, TextOptions};

    use super::*;

    /// The header readouts are left-aligned in fixed slots, so their digits
    /// must all be one width or the numbers jitter as they count. Plex's are;
    /// a font swap that loses this needs tabular figures laid out in code.
    #[test]
    fn digits_are_tabular() {
        let mut fonts = Fonts::new(TextOptions::default(), font_definitions());
        let mut fonts = fonts.with_pixels_per_point(1.0);
        let font = FontId::proportional(24.0);
        let zero = fonts.glyph_width(&font, '0');
        for digit in '1'..='9' {
            assert_eq!(fonts.glyph_width(&font, digit), zero, "{digit}");
        }
    }

    #[test]
    fn plex_leads_the_proportional_family_unhinted() {
        let fonts = font_definitions();
        assert_eq!(fonts.families[&FontFamily::Proportional][0], REGULAR_KEY);
        assert_eq!(fonts.font_data[REGULAR_KEY].tweak.hinting, Some(false));
    }
}
