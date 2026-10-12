#[derive(Default, Debug, Clone, Copy)]
pub struct ScrollState {
    pub offset: f32,
    pub content_height: f32,
    pub viewport_height: f32,
}

impl ScrollState {
    pub fn max(&self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }
    pub fn scroll_by(&mut self, dy: f32) {
        self.offset = (self.offset + dy).clamp(0.0, self.max());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clamps() {
        let mut s = ScrollState {
            offset: 0.0,
            content_height: 1000.0,
            viewport_height: 400.0,
        };
        s.scroll_by(5000.0);
        assert_eq!(s.offset, 600.0);
        s.scroll_by(-5000.0);
        assert_eq!(s.offset, 0.0);
    }
}
