pub mod display_list;
pub mod json_tree;
pub mod markdown;
pub mod text;
pub mod theme;

use crate::gfx::font::FontSystem;
use theme::Theme;

/// Everything layout needs besides the document itself. All outputs are in *physical pixels*.
pub struct Ctx<'a> {
    pub fonts: &'a FontSystem,
    pub theme: &'a Theme,
    /// OS DPI scale (1.0 = 96 dpi).
    pub scale: f32,
    /// User zoom (Ctrl +/-).
    pub zoom: f32,
}

impl Ctx<'_> {
    /// Logical px -> physical px.
    pub fn px(&self, logical: f32) -> f32 {
        logical * self.scale * self.zoom
    }
    pub fn body_size(&self) -> f32 {
        self.px(self.theme.base_size)
    }
}
