//! Writing the clipboard of the terminal this window runs in: OSC 52, which
//! reaches it over SSH too when the terminal allows it. A multiplexer
//! between (tmux, GNU screen on the machine Rakumux runs on) keeps OSC 52
//! from its programs to itself unless it's passed through, so there each
//! write is wrapped in that multiplexer's passthrough as well.

use std::ffi::OsString;

/// The multiplexer the TUI runs in, read from its environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mux {
    /// Straight to the terminal.
    #[default]
    None,
    /// tmux (`TMUX`): takes OSC 52 itself with `set-clipboard on`, passes
    /// it to the terminal wrapped in `DCS tmux;` with `allow-passthrough on`.
    Tmux,
    /// GNU screen (`STY`): passes a DCS string's content to the terminal.
    Screen,
}

impl Mux {
    /// tmux when `TMUX` is set, else screen when `STY` is (`var` reads one).
    pub fn from_env(var: impl Fn(&str) -> Option<OsString>) -> Mux {
        let set = |name| var(name).is_some_and(|value| !value.is_empty());
        if set("TMUX") {
            Mux::Tmux
        } else if set("STY") {
            Mux::Screen
        } else {
            Mux::None
        }
    }
}

/// GNU screen ends a DCS string past a few hundred bytes: the write goes in
/// pieces of this many base64 characters, each in a DCS of its own.
const SCREEN_PIECE: usize = 76;

/// The bytes that put `data` (base64) on the clipboard `selection` (`c`,
/// `p`, … as OSC 52 names them) of the terminal, through `mux`.
pub fn osc52(selection: &str, data: &str, mux: Mux) -> Vec<u8> {
    let plain = format!("\x1b]52;{selection};{data}\x07");
    match mux {
        Mux::None => plain.into_bytes(),
        // Raw for `set-clipboard on`, wrapped for `allow-passthrough on`:
        // tmux drops what it isn't set up to take.
        Mux::Tmux => format!(
            "{plain}\x1bPtmux;{}\x1b\\",
            plain.replace('\x1b', "\x1b\x1b")
        )
        .into_bytes(),
        Mux::Screen => {
            let pieces: Vec<&str> = data
                .as_bytes()
                .chunks(SCREEN_PIECE)
                .map(|piece| std::str::from_utf8(piece).unwrap_or_default())
                .collect();
            format!(
                "\x1bP\x1b]52;{selection};{}\x07\x1b\\",
                pieces.join("\x1b\\\x1bP")
            )
            .into_bytes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_osc52_ends_with_bel() {
        assert_eq!(osc52("c", "aGk=", Mux::None), b"\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn tmux_gets_it_raw_and_through_its_passthrough_with_escapes_doubled() {
        assert_eq!(
            osc52("c", "aGk=", Mux::Tmux),
            b"\x1b]52;c;aGk=\x07\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
        );
    }

    #[test]
    fn screen_gets_it_in_pieces_each_in_a_dcs() {
        let data = "A".repeat(SCREEN_PIECE + 4);
        let out = String::from_utf8(osc52("c", &data, Mux::Screen)).unwrap();
        assert_eq!(
            out,
            format!(
                "\x1bP\x1b]52;c;{}\x1b\\\x1bP{}\x07\x1b\\",
                "A".repeat(SCREEN_PIECE),
                "AAAA"
            )
        );
    }

    #[test]
    fn the_multiplexer_comes_from_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(*v))
            }
        };
        assert_eq!(Mux::from_env(env(&[])), Mux::None);
        assert_eq!(
            Mux::from_env(env(&[("TMUX", "/tmp/tmux-1000/default,1,0")])),
            Mux::Tmux
        );
        assert_eq!(
            Mux::from_env(env(&[("STY", "123.pts-0.host")])),
            Mux::Screen
        );
        assert_eq!(Mux::from_env(env(&[("TMUX", "")])), Mux::None);
    }
}
