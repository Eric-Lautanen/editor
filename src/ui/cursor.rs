/// Mouse cursor shapes the app asks the platform to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    /// Standard arrow.
    Default,
    /// I-beam: over editable text.
    Text,
    /// Pointing hand: over clickable things (links, toggles, tabs, buttons).
    Hand,
}
