//! Text width as the renderer draws it. Every view measures with this trait
//! instead of `unicode_width::UnicodeWidthStr`, whose string width joins an
//! Arabic lam and the alef after it into one cell where the renderer draws
//! two; see [`haarness_opentui::display_width`].

pub trait DisplayWidth {
    /// Terminal cells this text takes once drawn.
    fn width(&self) -> usize;
}

impl DisplayWidth for str {
    fn width(&self) -> usize {
        haarness_opentui::display_width(self)
    }
}
