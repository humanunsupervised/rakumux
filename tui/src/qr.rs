//! QR codes drawn with text: each character is `▀` with the upper module as
//! its foreground and the lower one as its background, so a row of text
//! holds two rows of modules. Dark modules are colour 16 and light ones 231
//! of the 256-colour palette (black and white in the colour cube, which
//! terminal themes leave alone), with a light quiet zone around the code:
//! scanners want dark on light, whatever the terminal's own colours.

use qrcodegen::{QrCode, QrCodeEcc};

/// Light modules around the code (the spec asks for 4; 2 scan fine on a
/// screen and keep the code small).
const QUIET: i32 = 2;
/// 256-colour palette index of dark modules.
pub const DARK: u8 = 16;
/// 256-colour palette index of light modules.
pub const LIGHT: u8 = 231;

/// A QR code ready to draw.
pub struct Qr {
    code: QrCode,
}

impl Qr {
    /// The code for `text` (error correction Low, raised when it fits in
    /// the same size); `None` when it is too long for a QR code.
    pub fn encode(text: &str) -> Option<Qr> {
        QrCode::encode_text(text, QrCodeEcc::Low)
            .ok()
            .map(|code| Qr { code })
    }

    fn modules(&self) -> i32 {
        self.code.size() + 2 * QUIET
    }

    /// Columns it takes (modules plus the quiet zone).
    pub fn width(&self) -> u16 {
        self.modules() as u16
    }

    /// Rows of text it takes.
    pub fn height(&self) -> u16 {
        ((self.modules() + 1) / 2) as u16
    }

    /// Whether the module at `(x, y)` of the drawing (quiet zone included)
    /// is dark.
    fn dark(&self, x: i32, y: i32) -> bool {
        self.code.get_module(x - QUIET, y - QUIET)
    }

    /// Each text row's cells as `(upper dark, lower dark)`.
    pub fn cells(&self) -> Vec<Vec<(bool, bool)>> {
        let n = self.modules();
        (0..self.height() as i32)
            .map(|row| {
                (0..n)
                    .map(|x| (self.dark(x, 2 * row), self.dark(x, 2 * row + 1)))
                    .collect()
            })
            .collect()
    }

    /// The code as text with ANSI 256-colour escapes, one line per row.
    pub fn ansi(&self) -> String {
        let colour = |dark: bool| if dark { DARK } else { LIGHT };
        let mut out = String::new();
        for row in self.cells() {
            let mut last = None;
            for (upper, lower) in row {
                let pair = (colour(upper), colour(lower));
                if last != Some(pair) {
                    out.push_str(&format!("\x1b[38;5;{};48;5;{}m", pair.0, pair.1));
                    last = Some(pair);
                }
                out.push('▀');
            }
            out.push_str("\x1b[0m\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINK: &str = "orchestra://connect?url=http%3A%2F%2Fbuild-machine.tail1234ab.ts.net\
                        %3A7337&token=abcdefghijklmnopqrstuvwx27";

    #[test]
    fn a_pairing_link_fits_a_terminal() {
        let qr = Qr::encode(LINK).unwrap();
        // Version 6 (41 modules) plus the quiet zone.
        assert_eq!(qr.width(), 45);
        assert_eq!(qr.height(), 23);
        let cells = qr.cells();
        assert_eq!(cells.len(), 23);
        assert!(cells.iter().all(|row| row.len() == 45));
    }

    #[test]
    fn the_quiet_zone_is_light_and_the_finders_are_dark() {
        let qr = Qr::encode(LINK).unwrap();
        let cells = qr.cells();
        // The first text row holds module rows 0 and 1: quiet zone.
        assert!(cells[0].iter().all(|&(u, l)| !u && !l));
        // Row 1 holds module rows 2 and 3: the top of the finder patterns
        // (dark from column 2), then their inner light ring on row 3.
        assert_eq!(cells[1][0], (false, false));
        assert_eq!(cells[1][1], (false, false));
        assert_eq!(cells[1][2], (true, true));
        assert_eq!(cells[1][3], (true, false));
        let last = cells.last().unwrap();
        // Module rows 44 and 45 (past the end): light.
        assert!(last.iter().all(|&(_, l)| !l));
    }

    #[test]
    fn ansi_draws_every_cell_and_resets_each_line() {
        let qr = Qr::encode("orchestra").unwrap();
        let text = qr.ansi();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), qr.height() as usize);
        for line in lines {
            assert_eq!(line.matches('▀').count(), qr.width() as usize);
            assert!(line.ends_with("\x1b[0m"));
            assert!(line.starts_with("\x1b[38;5;231;48;5;231m"), "{line:?}");
        }
    }

    #[test]
    fn too_much_text_has_no_code() {
        assert!(Qr::encode(&"x".repeat(4000)).is_none());
    }
}
