use super::{
    color::Color,
    font::{FontStyle, FontSystem, Glyph},
    geom::Rect,
};

/// Software framebuffer, 0xAARRGGBB pixels (alpha ignored on present), row-major, top-down.
pub struct Canvas {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
    /// Clip rectangle in pixels: x0, y0, x1, y1 (exclusive). All drawing respects it.
    clip: (i32, i32, i32, i32),
}

#[inline]
fn mix(dst: u32, src: Color, a: u32) -> u32 {
    // a in 0..=255 already includes src.a
    let inv = 255 - a;
    let ch = |d: u32, s: u8| (s as u32 * a + d * inv + 127) / 255;
    let r = ch((dst >> 16) & 0xFF, src.r);
    let g = ch((dst >> 8) & 0xFF, src.g);
    let b = ch(dst & 0xFF, src.b);
    0xFF00_0000 | r << 16 | g << 8 | b
}

/// How to draw a run of text: pixel size, face, and ink color.
#[derive(Clone, Copy, Debug)]
pub struct TextSpec {
    pub size: f32,
    pub style: FontStyle,
    pub color: Color,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::begin(crate::mem::Component::Canvas);
        let pixels = vec![0; width * height];
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::end();
        Self {
            width,
            height,
            pixels,
            clip: (0, 0, width as i32, height as i32),
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::begin(crate::mem::Component::Canvas);
        self.pixels.resize(width * height, 0);
        #[cfg(any(test, feature = "mem-census"))]
        crate::mem::end();
        self.clip = (0, 0, width as i32, height as i32);
    }

    /// Restrict drawing to `r` (intersected with the canvas). `None` removes the clip.
    pub fn set_clip(&mut self, r: Option<Rect>) {
        let full = (0, 0, self.width as i32, self.height as i32);
        self.clip = match r {
            None => full,
            Some(r) => (
                (r.x.floor() as i32).max(full.0),
                (r.y.floor() as i32).max(full.1),
                (r.right().ceil() as i32).min(full.2),
                (r.bottom().ceil() as i32).min(full.3),
            ),
        };
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: Color, cov: u32) {
        let (x0, y0, x1, y1) = self.clip;
        if x < x0 || y < y0 || x >= x1 || y >= y1 || cov == 0 {
            return;
        }
        let a = (cov * c.a as u32 + 127) / 255;
        let i = y as usize * self.width + x as usize;
        self.pixels[i] = if a >= 255 {
            c.pack() | 0xFF00_0000
        } else {
            mix(self.pixels[i], c, a)
        };
    }

    /// Fill a rectangle (pixel-snapped: rounds edges). Alpha-blends if `c.a < 255`.
    pub fn fill_rect(&mut self, r: Rect, c: Color) {
        let (cx0, cy0, cx1, cy1) = self.clip;
        let x0 = (r.x.round() as i32).max(cx0);
        let y0 = (r.y.round() as i32).max(cy0);
        let x1 = (r.right().round() as i32).min(cx1);
        let y1 = (r.bottom().round() as i32).min(cy1);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        for y in y0..y1 {
            let row = y as usize * self.width;
            if c.a == 255 {
                self.pixels[row + x0 as usize..row + x1 as usize].fill(c.pack() | 0xFF00_0000);
            } else {
                for x in x0..x1 {
                    self.put(x, y, c, 255);
                }
            }
        }
    }

    /// Anti-aliased rounded rectangle.
    pub fn fill_rounded_rect(&mut self, r: Rect, radius: f32, c: Color) {
        let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
        let (cx0, cy0, cx1, cy1) = self.clip;
        let x0 = (r.x.floor() as i32).max(cx0);
        let y0 = (r.y.floor() as i32).max(cy0);
        let x1 = (r.right().ceil() as i32).min(cx1);
        let y1 = (r.bottom().ceil() as i32).min(cy1);
        let (hw, hh) = (r.w / 2.0, r.h / 2.0);
        let (mx, my) = (r.x + hw, r.y + hh);
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = ((x as f32 + 0.5 - mx).abs() - (hw - rad)).max(0.0);
                let dy = ((y as f32 + 0.5 - my).abs() - (hh - rad)).max(0.0);
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (rad - d + 0.5).clamp(0.0, 1.0);
                self.put(x, y, c, (cov * 255.0 + 0.5) as u32);
            }
        }
    }

    /// Blit a glyph with its pen origin at integer (`x`, `baseline`).
    pub fn draw_glyph(&mut self, g: &Glyph, x: i32, baseline: i32, c: Color) {
        let gx = x + g.left;
        let gy = baseline - g.top;
        for j in 0..g.height {
            for i in 0..g.width {
                let cov = g.coverage[j * g.width + i] as u32;
                if cov != 0 {
                    self.put(gx + i as i32, gy + j as i32, c, cov);
                }
            }
        }
    }

    /// Draw a single line of text. `baseline` is the y of the text baseline. Returns the end x.
    pub fn draw_text(
        &mut self,
        fonts: &FontSystem,
        text: &str,
        x: f32,
        baseline: f32,
        spec: TextSpec,
    ) -> f32 {
        let mut pen = x;
        let by = baseline.round() as i32;
        for ch in text.chars() {
            if ch.is_control() {
                continue;
            }
            if let Some(g) = fonts.glyph(spec.style, ch, spec.size) {
                self.draw_glyph(&g, pen.round() as i32, by, spec.color);
            }
            pen += fonts.advance(spec.style, ch, spec.size);
        }
        pen
    }

    /// Binary PPM (P6) for headless snapshots / golden tests.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        out.reserve(self.pixels.len() * 3);
        for p in &self.pixels {
            out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_clips() {
        let mut c = Canvas::new(4, 4);
        c.fill_rect(Rect::new(2.0, 2.0, 100.0, 100.0), Color::rgb(1, 2, 3));
        assert_eq!(
            c.pixels[3 * 4 + 3] & 0xFFFFFF,
            Color::rgb(1, 2, 3).pack() & 0xFFFFFF
        );
        assert_eq!(c.pixels[0], 0);
    }

    #[test]
    fn clip_rect_limits_drawing() {
        let mut c = Canvas::new(10, 10);
        c.set_clip(Some(Rect::new(0.0, 0.0, 5.0, 5.0)));
        c.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::rgb(255, 255, 255));
        assert_ne!(c.pixels[0], 0);
        assert_eq!(c.pixels[9 * 10 + 9], 0);
    }

    #[test]
    fn alpha_blend_midpoint() {
        let mut c = Canvas::new(1, 1);
        c.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), Color::rgb(0, 0, 0));
        c.fill_rect(
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Color::rgba(255, 255, 255, 128),
        );
        let v = (c.pixels[0] & 0xFF) as i32;
        assert!((v - 128).abs() <= 1, "got {v}");
    }

    #[test]
    fn text_draws_pixels() {
        let f = FontSystem::load_embedded();
        let mut c = Canvas::new(120, 30);
        c.fill_rect(Rect::new(0.0, 0.0, 120.0, 30.0), Color::rgb(255, 255, 255));
        let spec = TextSpec {
            size: 16.0,
            style: FontStyle::Regular,
            color: Color::rgb(0, 0, 0),
        };
        c.draw_text(&f, "Hi", 4.0, 20.0, spec);
        assert!(
            c.pixels.iter().any(|&p| (p & 0xFF) < 128),
            "no dark pixels drawn"
        );
    }
}
