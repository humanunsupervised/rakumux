//! Pure geometry for a bounded page of terminal cards. Cards crop the
//! terminal; their dimensions are never sent to a session.

use ratatui::layout::Rect;

/// Maximum number of visible cards (and therefore preview streams).
pub const MAX_CARDS: usize = 6;

/// A page selected by the current card, including the new-session card.
pub struct GridPage {
    /// Columns in this page, for directional navigation.
    pub columns: usize,
    /// Maximum entries per page at this screen size.
    pub capacity: usize,
    /// First filtered session index on this page.
    pub start: usize,
    /// Total pages, including the new-session card.
    pub pages: usize,
    /// Filtered indices and rectangles of visible cards.
    pub cards: Vec<(usize, Rect)>,
}

impl GridPage {
    /// Reserve one header row, then tile at most six cards, even on very
    /// large terminals. Tiny screens still have one reachable card.
    pub fn new(area: Rect, count: usize, selected: usize) -> Self {
        let body = Rect {
            y: area.y + area.height.min(1),
            height: area.height.saturating_sub(1),
            ..area
        };
        let max_columns = usize::from((body.width / 48).clamp(1, 3));
        let max_rows = usize::from((body.height / 10).clamp(1, 2));
        let capacity = (max_columns * max_rows).min(MAX_CARDS);
        let selected = selected.min(count.saturating_sub(1));
        let start = selected / capacity * capacity;
        let entries = count.saturating_sub(start).min(capacity).max(1);
        let rows = entries.div_ceil(max_columns).min(max_rows);
        let columns = entries.div_ceil(rows).min(max_columns);
        let mut cards = Vec::new();
        if !body.is_empty() {
            for index in start..(start + capacity).min(count) {
                let local = index - start;
                let col = (local % columns) as u16;
                let row = (local / columns) as u16;
                let x = body.x + (u32::from(body.width) * u32::from(col) / columns as u32) as u16;
                let y = body.y + (u32::from(body.height) * u32::from(row) / rows as u32) as u16;
                let right =
                    body.x + (u32::from(body.width) * u32::from(col + 1) / columns as u32) as u16;
                let bottom =
                    body.y + (u32::from(body.height) * u32::from(row + 1) / rows as u32) as u16;
                cards.push((index, Rect::new(x, y, right - x, bottom - y)));
            }
        }
        Self {
            columns,
            capacity,
            start,
            pages: count.div_ceil(capacity).max(1),
            cards,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_pages_are_bounded_tiled_and_keep_selection_visible() {
        for (width, height) in [
            (1, 1),
            (5, 3),
            (40, 19),
            (45, 39),
            (80, 23),
            (160, 47),
            (300, 100),
        ] {
            let area = Rect::new(0, 0, width, height);
            for count in [1, 2, 4, 7, 50] {
                for selected in 0..count {
                    let page = GridPage::new(area, count, selected);
                    assert!(page.capacity <= MAX_CARDS);
                    assert!(page.cards.len() <= MAX_CARDS);
                    if height > 1 {
                        assert!(page.cards.iter().any(|(index, _)| *index == selected));
                    }
                    for (i, (_, card)) in page.cards.iter().enumerate() {
                        assert!(!card.is_empty());
                        assert_eq!(card.intersection(area), *card);
                        assert!(card.y > area.y);
                        for (_, other) in &page.cards[i + 1..] {
                            assert!(card.intersection(*other).is_empty());
                        }
                    }
                }
            }
        }
        assert!(GridPage::new(Rect::default(), 10, 3).cards.is_empty());
        let one = GridPage::new(Rect::new(0, 0, 160, 47), 1, 0);
        assert_eq!(one.cards[0].1, Rect::new(0, 1, 160, 46));
    }
}
