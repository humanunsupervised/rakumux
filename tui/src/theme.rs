use ratatui::style::{Color, Style};
use std::env;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSupport {
    Truecolor,
    Ansi256,
    Ansi16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: Color,
    pub surface: Color,
    pub border: Color,
    pub text_primary: Color,
    /// Text of the selected list item (white).
    pub text_bright: Color,
    pub text_muted: Color,
    pub text_subtle: Color,
    pub neutral_gray: Color,
    pub accent_primary: Color,
    pub accent_green: Color,
    pub accent_orange: Color,
    pub accent_pink: Color,
    pub accent_purple: Color,
    pub accent_gold: Color,
    /// Background of side panels such as the session list.
    pub panel: Color,
    /// Background of the selected list item.
    pub selection: Color,
    /// Border of unfocused panes.
    pub border_dim: Color,
}

impl Palette {
    /// Text on the app background: what every area starts from, so text
    /// and cells without colours of their own never show the outer
    /// terminal's defaults.
    pub fn base(&self) -> Style {
        Style::default().fg(self.text_primary).bg(self.background)
    }

    /// `color` faded toward the background by `amount` (0 keeps it, 1 is the
    /// background) and partly greyed: how things look that are shown but
    /// don't take input, like the session pane in Rakumux mode. `None`
    /// when this palette can't fade (16 colours; use `Modifier::DIM`) or
    /// `color` is the terminal's default.
    pub fn faded(&self, color: Color, amount: f32) -> Option<Color> {
        let target = rgb(self.background)?;
        let (r, g, b) = rgb(color)?;
        // A 16-colour palette (named background) has no colours in between.
        if !matches!(self.background, Color::Rgb(..) | Color::Indexed(..)) {
            return None;
        }
        let keep = 1.0 - amount.clamp(0.0, 1.0);
        // The colour's difference from the background, greyed part of the
        // way (Rec. 601 luma) and shrunk: the background itself stays put.
        let d = |c: u8, t: u8| c as f32 - t as f32;
        let (dr, dg, db) = (d(r, target.0), d(g, target.1), d(b, target.2));
        let luma = 0.299 * dr + 0.587 * dg + 0.114 * db;
        let mix = |delta: f32, t: u8| {
            let grey = delta + (luma - delta) * FADE_GREY;
            (t as f32 + grey * keep).round().clamp(0.0, 255.0) as u8
        };
        let faded = (mix(dr, target.0), mix(dg, target.1), mix(db, target.2));
        Some(match self.background {
            Color::Rgb(..) => Color::Rgb(faded.0, faded.1, faded.2),
            _ => Color::Indexed(nearest_indexed(faded)),
        })
    }
}

/// How much [`Palette::faded`] greys a colour before fading it.
const FADE_GREY: f32 = 0.35;

/// Tokyo Night's terminal colours, standing in for the 16 ANSI colours,
/// which the terminal defines.
const ANSI: [(u8, u8, u8); 16] = [
    (0x15, 0x16, 0x1e),
    (0xf7, 0x76, 0x8e),
    (0x9e, 0xce, 0x6a),
    (0xe0, 0xaf, 0x68),
    (0x7a, 0xa2, 0xf7),
    (0xbb, 0x9a, 0xf7),
    (0x7d, 0xcf, 0xff),
    (0xa9, 0xb1, 0xd6),
    (0x41, 0x48, 0x68),
    (0xf7, 0x76, 0x8e),
    (0x9e, 0xce, 0x6a),
    (0xe0, 0xaf, 0x68),
    (0x7a, 0xa2, 0xf7),
    (0xbb, 0x9a, 0xf7),
    (0x7d, 0xcf, 0xff),
    (0xc0, 0xca, 0xf5),
];

/// The six levels of each channel in the 256-colour cube.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// What `color` looks like as RGB: exact for 24-bit colours and the
/// 256-colour cube and greys, Tokyo Night's for the 16 ANSI colours.
/// `None` for the terminal's default colour.
pub fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    let ansi = |i: usize| Some(ANSI[i]);
    match color {
        Color::Reset => None,
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(i @ 0..=15) => ansi(i as usize),
        Color::Indexed(i @ 16..=231) => {
            let i = i - 16;
            Some((
                CUBE[(i / 36) as usize],
                CUBE[(i / 6 % 6) as usize],
                CUBE[(i % 6) as usize],
            ))
        }
        Color::Indexed(i) => {
            let v = 8 + 10 * (i - 232);
            Some((v, v, v))
        }
        Color::Black => ansi(0),
        Color::Red => ansi(1),
        Color::Green => ansi(2),
        Color::Yellow => ansi(3),
        Color::Blue => ansi(4),
        Color::Magenta => ansi(5),
        Color::Cyan => ansi(6),
        Color::Gray => ansi(7),
        Color::DarkGray => ansi(8),
        Color::LightRed => ansi(9),
        Color::LightGreen => ansi(10),
        Color::LightYellow => ansi(11),
        Color::LightBlue => ansi(12),
        Color::LightMagenta => ansi(13),
        Color::LightCyan => ansi(14),
        Color::White => ansi(15),
    }
}

/// The 256-colour cube or grey nearest to `(r, g, b)`.
fn nearest_indexed((r, g, b): (u8, u8, u8)) -> u8 {
    let level = |c: u8| {
        CUBE.iter()
            .enumerate()
            .min_by_key(|(_, v)| (**v as i32 - c as i32).abs())
            .map_or(0, |(i, _)| i as u8)
    };
    let (cr, cg, cb) = (level(r), level(g), level(b));
    let cube = 16 + 36 * cr + 6 * cg + cb;
    let average = (r as u32 + g as u32 + b as u32) / 3;
    let grey_step = ((average.saturating_sub(8) + 5) / 10).min(23) as u8;
    let grey = 232 + grey_step;
    let distance = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    let cube_rgb = (CUBE[cr as usize], CUBE[cg as usize], CUBE[cb as usize]);
    let v = 8 + 10 * grey_step;
    if distance((v, v, v)) < distance(cube_rgb) {
        grey
    } else {
        cube
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub palette: Palette,
}

static THEME: OnceLock<Theme> = OnceLock::new();

pub fn current() -> &'static Theme {
    THEME.get_or_init(|| Theme::from_env(&|name| env::var(name).ok()))
}

pub fn palette() -> &'static Palette {
    &current().palette
}

/// How many colours the terminal shows, from the environment (`var` reads
/// one variable). 24-bit colour is assumed unless the terminal clearly
/// can't show it: `COLORTERM` is often missing although the terminal has
/// it (under tmux, over SSH, terminals that don't set it), so its absence
/// says little. `GWR_FORCE_COLOR` overrides.
fn detect_color_support(var: &dyn Fn(&str) -> Option<String>) -> ColorSupport {
    let get = |name: &str| var(name).unwrap_or_default().to_ascii_lowercase();
    match get("GWR_FORCE_COLOR").as_str() {
        "truecolor" | "24bit" | "high" => return ColorSupport::Truecolor,
        "256" | "ansi256" | "ansi-256" => return ColorSupport::Ansi256,
        "16" | "ansi" | "low" => return ColorSupport::Ansi16,
        _ => {}
    }

    let colorterm = get("COLORTERM");
    let term = get("TERM");
    if colorterm.contains("truecolor")
        || colorterm.contains("24bit")
        || term.contains("truecolor")
        || term.contains("24bit")
        || term.contains("direct")
    {
        return ColorSupport::Truecolor;
    }

    // The Linux console, `dumb`, VT-style and 8/16-colour terminals.
    let vt = term
        .strip_prefix("vt")
        .is_some_and(|model| model.starts_with(|c: char| c.is_ascii_digit()));
    if vt
        || matches!(term.as_str(), "dumb" | "linux" | "ansi" | "cons25")
        || term.ends_with("-8color")
        || term.ends_with("-16color")
    {
        return ColorSupport::Ansi16;
    }

    // GNU screen (before 5) and macOS Terminal (before 26) garble 24-bit
    // colours. tmux converts them for terminals that lack them.
    let in_screen = var("STY").is_some() && var("TMUX").is_none();
    if in_screen || get("TERM_PROGRAM") == "apple_terminal" {
        return ColorSupport::Ansi256;
    }

    ColorSupport::Truecolor
}

impl Theme {
    /// Tokyo Night where the terminal can show it, an approximation in its
    /// indexed colours otherwise. `GWR_THEME` overrides: `tokyo` always,
    /// `basic` for the indexed palette.
    fn from_env(var: &dyn Fn(&str) -> Option<String>) -> Theme {
        let support = detect_color_support(var);

        if let Some(force) = var("GWR_THEME") {
            match force.to_ascii_lowercase().as_str() {
                "tokyo" | "tokyo-night" | "tokyonight" | "high" | "truecolor" => {
                    return Theme::tokyo_night()
                }
                "basic" | "ansi" | "fallback" | "low" => return Theme::basic(support),
                "auto" => {} // fall through to detection
                other => {
                    if var("RAKUMUX_DEBUG").is_some() {
                        eprintln!(
                            "DEBUG: Unknown GWR_THEME value '{}', falling back to auto",
                            other
                        );
                    }
                }
            }
        }

        match support {
            ColorSupport::Truecolor => Theme::tokyo_night(),
            ColorSupport::Ansi256 | ColorSupport::Ansi16 => Theme::basic(support),
        }
    }

    /// Tokyo Night (night). The whole app, the session list included, sits
    /// on its blue `bg`.
    fn tokyo_night() -> Theme {
        let background = Color::Rgb(26, 27, 38);
        Theme {
            palette: Palette {
                // bg; the storm style's bg for bars; blue
                background,
                surface: Color::Rgb(36, 40, 59),
                border: Color::Rgb(122, 162, 247),
                // fg
                text_primary: Color::Rgb(192, 202, 245),
                text_bright: Color::Rgb(255, 255, 255),
                // comment, dark3, dark5
                text_muted: Color::Rgb(86, 95, 137),
                text_subtle: Color::Rgb(84, 92, 126),
                neutral_gray: Color::Rgb(115, 122, 162),
                accent_primary: Color::Rgb(122, 162, 247),
                accent_green: Color::Rgb(158, 206, 106),
                accent_orange: Color::Rgb(255, 158, 100),
                accent_pink: Color::Rgb(247, 118, 142),
                accent_purple: Color::Rgb(187, 154, 247),
                // yellow
                accent_gold: Color::Rgb(224, 175, 104),
                // bg, bg_visual, fg_gutter
                panel: background,
                selection: Color::Rgb(40, 52, 87),
                border_dim: Color::Rgb(59, 66, 97),
            },
        }
    }

    /// Indexed colours: the nearest 256-colour matches to Tokyo Night (its
    /// background becomes the cube's dark blue), or the 16 ANSI colours.
    /// `Truecolor` gets the 256-colour palette too (`GWR_THEME=basic`).
    fn basic(support: ColorSupport) -> Theme {
        let palette = match support {
            ColorSupport::Truecolor | ColorSupport::Ansi256 => Palette {
                background: Color::Indexed(17),      // #00005f
                surface: Color::Indexed(18),         // #000087
                border: Color::Indexed(111),         // #87afff ≈ blue
                text_primary: Color::Indexed(189),   // #d7d7ff ≈ fg
                text_bright: Color::Indexed(231),    // #ffffff
                text_muted: Color::Indexed(60),      // #5f5f87 ≈ comment
                text_subtle: Color::Indexed(60),     // ≈ dark3
                neutral_gray: Color::Indexed(103),   // #8787af ≈ dark5
                accent_primary: Color::Indexed(111), // ≈ blue
                accent_green: Color::Indexed(149),   // #afd75f ≈ green
                accent_orange: Color::Indexed(215),  // #ffaf5f ≈ orange
                accent_pink: Color::Indexed(204),    // #ff5f87 ≈ red
                accent_purple: Color::Indexed(141),  // #af87ff ≈ purple
                accent_gold: Color::Indexed(179),    // #d7af5f ≈ yellow
                panel: Color::Indexed(17),
                selection: Color::Indexed(18),
                border_dim: Color::Indexed(238), // #444444 ≈ fg_gutter
            },
            ColorSupport::Ansi16 => Palette {
                background: Color::Black,
                surface: Color::DarkGray,
                border: Color::Blue,
                text_primary: Color::White,
                text_bright: Color::White,
                text_muted: Color::DarkGray,
                text_subtle: Color::Gray,
                neutral_gray: Color::Gray,
                accent_primary: Color::LightBlue,
                accent_green: Color::LightGreen,
                accent_orange: Color::Yellow,
                accent_pink: Color::LightRed,
                accent_purple: Color::Magenta,
                accent_gold: Color::Yellow,
                panel: Color::Black,
                selection: Color::DarkGray,
                border_dim: Color::DarkGray,
            },
        };

        Theme { palette }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A `var` reading only `pairs`.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn support(pairs: &[(&str, &str)]) -> ColorSupport {
        detect_color_support(&env(pairs))
    }

    fn theme(pairs: &[(&str, &str)]) -> Palette {
        Theme::from_env(&env(pairs)).palette
    }

    /// RGB of an xterm 256-colour index from the colour cube.
    fn cube(index: u8) -> (u8, u8, u8) {
        let level = |n: u8| if n == 0 { 0 } else { 55 + 40 * n };
        let n = index - 16;
        (level(n / 36), level(n / 6 % 6), level(n % 6))
    }

    #[test]
    fn truecolor_is_assumed_without_colorterm() {
        use ColorSupport::*;
        assert_eq!(support(&[("TERM", "xterm-256color")]), Truecolor);
        assert_eq!(support(&[("TERM", "xterm")]), Truecolor);
        assert_eq!(support(&[]), Truecolor);
        // tmux converts 24-bit colours when its terminal lacks them.
        assert_eq!(
            support(&[("TERM", "tmux-256color"), ("TMUX", "/tmp/tmux-1/default")]),
            Truecolor
        );
        assert_eq!(support(&[("TERM", "vte-256color")]), Truecolor);
        assert_eq!(support(&[("COLORTERM", "truecolor")]), Truecolor);
        assert_eq!(support(&[("TERM", "xterm-direct")]), Truecolor);
    }

    #[test]
    fn limited_terminals_get_indexed_colours() {
        use ColorSupport::*;
        for term in ["linux", "dumb", "vt100", "vt220", "ansi", "xterm-16color"] {
            assert_eq!(support(&[("TERM", term)]), Ansi16, "{term}");
        }
        let apple = [
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "Apple_Terminal"),
        ];
        assert_eq!(support(&apple), Ansi256);
        let screen = [("TERM", "screen.xterm-256color"), ("STY", "1.pts-0.host")];
        assert_eq!(support(&screen), Ansi256);
        // ...unless they say they have 24-bit colour.
        let apple = [
            ("TERM_PROGRAM", "Apple_Terminal"),
            ("COLORTERM", "truecolor"),
        ];
        assert_eq!(support(&apple), Truecolor);
    }

    #[test]
    fn force_color_overrides_detection() {
        use ColorSupport::*;
        let forced = |value| support(&[("GWR_FORCE_COLOR", value), ("COLORTERM", "truecolor")]);
        assert_eq!(forced("256"), Ansi256);
        assert_eq!(forced("16"), Ansi16);
        let linux = [("GWR_FORCE_COLOR", "truecolor"), ("TERM", "linux")];
        assert_eq!(support(&linux), Truecolor);
    }

    #[test]
    fn tokyo_night_is_the_default() {
        let p = theme(&[("TERM", "xterm-256color")]);
        assert_eq!(p, Theme::tokyo_night().palette);
        assert_eq!(p.background, Color::Rgb(0x1a, 0x1b, 0x26));
        assert_eq!(p.text_primary, Color::Rgb(0xc0, 0xca, 0xf5));
        assert_eq!(
            p.base(),
            Style::default().fg(p.text_primary).bg(p.background)
        );
        // Forced either way.
        assert_eq!(
            theme(&[("TERM", "linux"), ("GWR_THEME", "tokyo")]),
            Theme::tokyo_night().palette
        );
        assert_eq!(
            theme(&[("GWR_THEME", "basic")]),
            Theme::basic(ColorSupport::Ansi256).palette
        );
    }

    #[test]
    fn the_list_sits_on_the_app_background_in_every_palette() {
        for force in ["truecolor", "256", "16"] {
            let p = theme(&[("GWR_FORCE_COLOR", force)]);
            assert_eq!(p.panel, p.background, "{force}");
            // The selected row, the dimmed footer group and the pin mark
            // stand out from it.
            for color in [p.selection, p.border_dim, p.text_muted, p.accent_gold] {
                assert_ne!(color, p.background, "{force}");
            }
            assert_ne!(p.border_dim, p.accent_primary, "{force}");
        }
    }

    #[test]
    fn the_256_colour_palette_is_blue_not_grey() {
        let p = Theme::basic(ColorSupport::Ansi256).palette;
        for color in [p.background, p.surface, p.selection] {
            let Color::Indexed(index @ 16..=231) = color else {
                panic!("{color:?} isn't from the colour cube");
            };
            let (r, g, b) = cube(index);
            assert!(b > r && b > g, "{color:?} = {:?}", (r, g, b));
        }
        let Color::Indexed(text) = p.text_primary else {
            panic!("{:?}", p.text_primary);
        };
        let (r, g, b) = cube(text);
        assert!(b >= r && b >= g, "text {:?}", (r, g, b));
    }

    #[test]
    fn faded_colours_move_toward_the_background() {
        let tokyo = Theme::tokyo_night().palette;
        assert_eq!(tokyo.faded(tokyo.background, 0.5), Some(tokyo.background));
        assert_eq!(tokyo.faded(tokyo.text_primary, 1.0), Some(tokyo.background));
        let Some(Color::Rgb(r, g, b)) = tokyo.faded(tokyo.text_primary, 0.5) else {
            panic!("24-bit in, 24-bit out");
        };
        // Between the text and the background, and greyer than the text.
        assert!((26..192).contains(&r) && (27..202).contains(&g) && (38..245).contains(&b));
        assert!(b.abs_diff(r) < 245 - 192);
        assert_eq!(tokyo.faded(Color::Reset, 0.5), None);

        let indexed = theme(&[("GWR_FORCE_COLOR", "256")]);
        assert!(matches!(
            indexed.faded(Color::Indexed(189), 0.5),
            Some(Color::Indexed(16..=255))
        ));
        let ansi = theme(&[("GWR_FORCE_COLOR", "16")]);
        assert_eq!(
            ansi.faded(Color::White, 0.5),
            None,
            "16 colours dim instead"
        );
    }

    #[test]
    fn indexed_colours_have_their_xterm_values() {
        assert_eq!(rgb(Color::Indexed(16)), Some((0, 0, 0)));
        assert_eq!(rgb(Color::Indexed(231)), Some((255, 255, 255)));
        assert_eq!(rgb(Color::Indexed(196)), Some((255, 0, 0)));
        assert_eq!(rgb(Color::Indexed(232)), Some((8, 8, 8)));
        assert_eq!(rgb(Color::Indexed(255)), Some((238, 238, 238)));
        assert_eq!(nearest_indexed((255, 255, 255)), 231);
        assert_eq!(nearest_indexed((255, 0, 0)), 196);
        assert_eq!(nearest_indexed((128, 128, 128)), 244);
    }
}
