//! Keys whose escape sequence arrives in pieces.
//!
//! An arrow key is three bytes (`ESC [ A`). Over a slow link (a phone on a
//! mobile network, Termius over SSH) they can reach Orchestra in two reads.
//! crossterm then reports a lone `ESC` as the Esc key and the rest as `[`
//! and `A`, so in Orchestra mode an arrow would act as Esc ("back to the
//! session"). Like tmux's `escape-time`, a lone Esc is held for
//! [`ESCAPE_TIME`]: when the rest of a key's sequence follows, that key is
//! reported instead; otherwise the Esc (and whatever followed) is, in order.

use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// How long a lone Esc waits for the rest of a sequence while keys go to a
/// session's program. A phone on a mobile network can leave more than
/// 100 ms between the pieces; a real Esc reaches the program this much
/// later.
pub const ESCAPE_TIME: Duration = Duration::from_millis(300);

/// The same in Orchestra mode, where Esc only goes back to a session or
/// cancels: an arrow's pieces may come later still on a slow link (a
/// retransmitted packet), and an arrow taken for Esc throws the user into a
/// session.
pub const ESCAPE_TIME_ORCHESTRA: Duration = Duration::from_millis(800);

/// Holds a lone Esc and the pieces after it until they make a key or time
/// runs out.
#[derive(Debug, Default)]
pub struct EscapeJoiner {
    /// The characters after the Esc so far (`[`, `O`, digits, `;`).
    pending: Option<String>,
    deadline: Option<Instant>,
    /// The wait the held Esc started with.
    wait: Duration,
}

impl EscapeJoiner {
    /// When the held Esc is reported as one, if nothing completes it.
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Take in a terminal event; returns the events to handle now, in order.
    /// `wait` is how long a lone Esc waits for the rest of a key
    /// ([`ESCAPE_TIME`], [`ESCAPE_TIME_ORCHESTRA`]).
    pub fn feed(&mut self, event: Event, now: Instant, wait: Duration) -> Vec<Event> {
        let Some(pending) = self.pending.as_mut() else {
            if is_lone_esc(&event) {
                self.pending = Some(String::new());
                self.wait = wait;
                self.deadline = Some(now + wait);
                return Vec::new();
            }
            return vec![event];
        };
        if let Some(c) = plain_char(&event) {
            let continues = match pending.as_str() {
                "" => c == '[' || c == 'O',
                p if p.starts_with('[') => c.is_ascii_digit() || c == ';',
                _ => false,
            };
            if continues {
                pending.push(c);
                self.deadline = Some(now + self.wait);
                return Vec::new();
            }
            if let Some(event) = complete(pending, c) {
                self.pending = None;
                self.deadline = None;
                return vec![event];
            }
        }
        // Not a key's sequence: the Esc and its pieces were keys of their
        // own; then this event (which may be another lone Esc).
        let mut out = self.flush();
        out.extend(self.feed(event, now, self.wait));
        out
    }

    /// The held Esc and its pieces as they came, once `now` is past the
    /// deadline.
    pub fn expire(&mut self, now: Instant) -> Vec<Event> {
        match self.deadline {
            Some(deadline) if now >= deadline => self.flush(),
            _ => Vec::new(),
        }
    }

    fn flush(&mut self) -> Vec<Event> {
        self.deadline = None;
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        let mut out = vec![Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))];
        out.extend(pending.chars().map(|c| {
            let modifiers = if c.is_ascii_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            Event::Key(KeyEvent::new(KeyCode::Char(c), modifiers))
        }));
        out
    }
}

fn is_lone_esc(event: &Event) -> bool {
    matches!(event, Event::Key(key)
        if key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && key.kind == KeyEventKind::Press)
}

/// The character a key press typed, with Shift (crossterm reports `A` as
/// Shift+A) and nothing else held.
fn plain_char(event: &Event) -> Option<char> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers,
            kind: KeyEventKind::Press,
            ..
        }) if (*modifiers - KeyModifiers::SHIFT).is_empty() => Some(*c),
        _ => None,
    }
}

/// The key (or focus report) `ESC <pending> <last>` encodes, if any.
fn complete(pending: &str, last: char) -> Option<Event> {
    match (pending, last) {
        // Focus reports (the window coming to the front or going away).
        ("[", 'I') => return Some(Event::FocusGained),
        ("[", 'O') => return Some(Event::FocusLost),
        _ => {}
    }
    complete_key(pending, last).map(Event::Key)
}

/// The key `ESC <pending> <last>` encodes, if any.
fn complete_key(pending: &str, last: char) -> Option<KeyEvent> {
    if pending == "O" {
        let code = match last {
            'A' => KeyCode::Up,
            'B' => KeyCode::Down,
            'C' => KeyCode::Right,
            'D' => KeyCode::Left,
            'H' => KeyCode::Home,
            'F' => KeyCode::End,
            'P' => KeyCode::F(1),
            'Q' => KeyCode::F(2),
            'R' => KeyCode::F(3),
            'S' => KeyCode::F(4),
            _ => return None,
        };
        return Some(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let params = pending.strip_prefix('[')?;
    let mut fields = params.split(';');
    let first = fields.next().unwrap_or("");
    let modifiers = match fields.next() {
        Some(m) => modifiers(m.parse().ok()?)?,
        None => KeyModifiers::NONE,
    };
    if fields.next().is_some() {
        return None;
    }
    let code = match (first, last) {
        ("" | "1", 'A') => KeyCode::Up,
        ("" | "1", 'B') => KeyCode::Down,
        ("" | "1", 'C') => KeyCode::Right,
        ("" | "1", 'D') => KeyCode::Left,
        ("" | "1", 'H') => KeyCode::Home,
        ("" | "1", 'F') => KeyCode::End,
        ("1" | "7", '~') => KeyCode::Home,
        ("4" | "8", '~') => KeyCode::End,
        ("2", '~') => KeyCode::Insert,
        ("3", '~') => KeyCode::Delete,
        ("5", '~') => KeyCode::PageUp,
        ("6", '~') => KeyCode::PageDown,
        ("", 'Z') => return Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

/// xterm's modifier parameter: 1 + Shift(1) + Alt(2) + Ctrl(4).
fn modifiers(param: u8) -> Option<KeyModifiers> {
    let bits = param.checked_sub(1)?;
    if bits > 7 {
        return None;
    }
    let mut modifiers = KeyModifiers::NONE;
    if bits & 1 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if bits & 2 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if bits & 4 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    Some(modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// What crossterm reports for the bytes after a lone ESC.
    fn typed(text: &str) -> Vec<Event> {
        text.chars()
            .map(|c| {
                let modifiers = if c.is_ascii_uppercase() {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::NONE
                };
                Event::Key(KeyEvent::new(KeyCode::Char(c), modifiers))
            })
            .collect()
    }

    fn run(joiner: &mut EscapeJoiner, events: Vec<Event>, now: Instant) -> Vec<Event> {
        run_at(joiner, events, now, ESCAPE_TIME)
    }

    fn run_at(
        joiner: &mut EscapeJoiner,
        events: Vec<Event>,
        now: Instant,
        wait: Duration,
    ) -> Vec<Event> {
        events
            .into_iter()
            .flat_map(|event| joiner.feed(event, now, wait))
            .collect()
    }

    #[test]
    fn an_arrow_in_pieces_is_an_arrow() {
        let now = Instant::now();
        for (rest, code) in [
            ("[A", KeyCode::Up),
            ("[B", KeyCode::Down),
            ("[C", KeyCode::Right),
            ("[D", KeyCode::Left),
            ("OA", KeyCode::Up),
            ("OB", KeyCode::Down),
            ("[5~", KeyCode::PageUp),
            ("[6~", KeyCode::PageDown),
            ("[H", KeyCode::Home),
            ("[4~", KeyCode::End),
            ("OP", KeyCode::F(1)),
        ] {
            let mut joiner = EscapeJoiner::default();
            assert_eq!(
                joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME),
                vec![],
                "{rest}"
            );
            assert_eq!(joiner.deadline(), Some(now + ESCAPE_TIME));
            assert_eq!(
                run(&mut joiner, typed(rest), now),
                vec![key(code)],
                "{rest}"
            );
            assert_eq!(joiner.deadline(), None);
        }
        // With modifiers: Shift+Up, Ctrl+Right, Alt+Down.
        let mut joiner = EscapeJoiner::default();
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        assert_eq!(
            run(&mut joiner, typed("[1;2A"), now),
            vec![Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT))]
        );
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        assert_eq!(
            run(&mut joiner, typed("[1;5C"), now),
            vec![Event::Key(KeyEvent::new(
                KeyCode::Right,
                KeyModifiers::CONTROL
            ))]
        );
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        assert_eq!(
            run(&mut joiner, typed("[1;3B"), now),
            vec![Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT))]
        );
    }

    #[test]
    fn a_focus_report_in_pieces_is_a_focus_report() {
        let now = Instant::now();
        let mut joiner = EscapeJoiner::default();
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        assert_eq!(run(&mut joiner, typed("[I"), now), vec![Event::FocusGained]);
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        assert_eq!(run(&mut joiner, typed("[O"), now), vec![Event::FocusLost]);
    }

    #[test]
    fn orchestra_mode_waits_longer_for_the_rest() {
        let now = Instant::now();
        let mut joiner = EscapeJoiner::default();
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME_ORCHESTRA);
        assert_eq!(joiner.deadline(), Some(now + ESCAPE_TIME_ORCHESTRA));
        // 400 ms later, the rest of the arrow still makes an arrow.
        let later = now + Duration::from_millis(400);
        assert_eq!(joiner.expire(later), vec![]);
        assert_eq!(
            run_at(&mut joiner, typed("[A"), later, ESCAPE_TIME_ORCHESTRA),
            vec![key(KeyCode::Up)]
        );
    }

    #[test]
    fn a_real_esc_comes_through_after_the_escape_time() {
        let now = Instant::now();
        let mut joiner = EscapeJoiner::default();
        assert_eq!(joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME), vec![]);
        assert_eq!(joiner.expire(now + ESCAPE_TIME / 2), vec![]);
        assert_eq!(joiner.expire(now + ESCAPE_TIME), vec![key(KeyCode::Esc)]);
        assert_eq!(joiner.deadline(), None);
        assert_eq!(joiner.expire(now + ESCAPE_TIME * 2), vec![]);
    }

    #[test]
    fn whatever_follows_an_esc_that_starts_no_key_keeps_its_order() {
        let now = Instant::now();
        let mut joiner = EscapeJoiner::default();
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        // `j` right after Esc: Esc, then j.
        assert_eq!(
            joiner.feed(key(KeyCode::Char('j')), now, ESCAPE_TIME),
            vec![key(KeyCode::Esc), key(KeyCode::Char('j'))]
        );
        // Esc `[` then `x`: all three, in order.
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        let mut out = run(&mut joiner, typed("["), now);
        out.extend(joiner.feed(key(KeyCode::Char('x')), now, ESCAPE_TIME));
        assert_eq!(
            out,
            vec![
                key(KeyCode::Esc),
                key(KeyCode::Char('[')),
                key(KeyCode::Char('x'))
            ]
        );
        // Esc Esc: the first comes through, the second waits.
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        assert_eq!(
            joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME),
            vec![key(KeyCode::Esc)]
        );
        assert_eq!(joiner.expire(now + ESCAPE_TIME), vec![key(KeyCode::Esc)]);
        // A held key or other input also ends it.
        joiner.feed(key(KeyCode::Esc), now, ESCAPE_TIME);
        assert_eq!(
            joiner.feed(Event::FocusGained, now, ESCAPE_TIME),
            vec![key(KeyCode::Esc), Event::FocusGained]
        );
        // Whole keys pass straight through.
        assert_eq!(
            joiner.feed(key(KeyCode::Up), now, ESCAPE_TIME),
            vec![key(KeyCode::Up)]
        );
    }
}
