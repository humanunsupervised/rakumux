//! Kimi Code's app label: its brand blue.
pub(super) fn color() -> ratatui::style::Color {
    crate::theme::palette().label_azure
}

#[cfg(test)]
mod tests {
    #[test]
    fn kimi_is_labelled_in_its_brand_blue() {
        assert_eq!(
            super::super::color("kimi"),
            Some(crate::theme::palette().label_azure)
        );
    }
}
