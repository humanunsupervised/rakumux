//! Text selected with the mouse in a terminal view: from the cell a drag
//! began at to the cell it's at now, in reading order, in cells of the
//! screen as shown (the visible window, scrolled back or not). It never
//! leaves its view, as the outer terminal's own selection would.

use vt::vt100;

/// `(row, col)` of a terminal cell.
pub(super) type Cell = (u16, u16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Selection {
    anchor: Cell,
    head: Cell,
}

impl Selection {
    /// A drag that began at `cell` and hasn't moved yet.
    pub(super) fn at(cell: Cell) -> Self {
        Self {
            anchor: cell,
            head: cell,
        }
    }

    pub(super) fn extend_to(&mut self, cell: Cell) {
        self.head = cell;
    }

    /// Nothing selected yet: the drag is still on the cell it began at.
    pub(super) fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// First and last cell, in reading order.
    fn span(&self) -> (Cell, Cell) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether `cell` is selected: from the first cell to the end of its
    /// row, every row between, and the last row up to the last cell.
    pub(super) fn contains(&self, cell: Cell) -> bool {
        let (first, last) = self.span();
        !self.is_empty() && first <= cell && cell <= last
    }

    /// The selected text of `screen`: rows a program wrapped stay one line,
    /// and trailing blanks are left out.
    pub(super) fn text(&self, screen: &vt100::Screen) -> String {
        let (first, last) = self.span();
        let (_, cols) = screen.size();
        let text = screen.contents_between(first.0, first.1, last.0, (last.1 + 1).min(cols));
        text.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    }
}
