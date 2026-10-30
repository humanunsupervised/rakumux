//! Kimi Code's app label: a quiet blue, as its own brand blue.
pub(super) fn color() -> ratatui::style::Color {
    crate::theme::palette().label_blue
}

#[cfg(test)]
mod tests {
    #[test]
    fn kimi_is_labelled_in_a_quiet_blue() {
        assert_eq!(
            super::super::color("kimi"),
            Some(crate::theme::palette().label_blue)
        );
    }
}
