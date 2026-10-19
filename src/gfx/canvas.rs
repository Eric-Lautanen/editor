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

    /// A straight antialiased segment from (x0,y0) to (x1,y1) with round
    /// quality: drawn as a thick line by walking the longer axis one pixel at
    /// a time and stamping a disc of the given thickness (faded by coverage
    /// across the disc), which gives spinner arcs and rules a clean look.
    pub fn stroke_segment(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, th: f32, c: Color) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.5 {
            return;
        }
        let r = th / 2.0;
        let n = len.ceil() as i32;
        let (ux, uy) = (dx / len, dy / len);
        for i in 0..=n {
            let px = x0 + ux * i as f32;
            let py = y0 + uy * i as f32;
            // Stamp one pixel-column disc: the alpha falls off radially so a
            // thick segment has antialiased edges rather than a hard border.
            let ix0 = (px - r).floor() as i32;
            let ix1 = (px + r).ceil() as i32;
            let iy0 = (py - r).floor() as i32;
            let iy1 = (py + r).ceil() as i32;
            for yy in iy0..iy1 {
                for xx in ix0..ix1 {
                    let ddx = xx as f32 + 0.5 - px;
                    let ddy = yy as f32 + 0.5 - py;
                    let d = (ddx * ddx + ddy * ddy).sqrt();
                    let cov = (r - d + 0.5).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        self.put(xx, yy, c, (cov * 255.0 + 0.5) as u32);
                    }
                }
            }
        }
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
    ///
    /// An outline glyph is tinted with `c`; a *colour* glyph (an emoji bitmap)
    /// carries its own pixels and ignores `c` entirely, because tinting it
    /// would destroy the artwork.
    pub fn draw_glyph(&mut self, g: &Glyph, x: i32, baseline: i32, c: Color) {
        let gx = x + g.left;
        let gy = baseline - g.top;
        if let Some(rgba) = &g.color {
            for j in 0..g.height {
                for i in 0..g.width {
                    let o = (j * g.width + i) * 4;
                    let a = rgba[o + 3];
                    if a != 0 {
                        let px = Color::rgba(rgba[o], rgba[o + 1], rgba[o + 2], a);
                        // cov 255: the alpha is already in `px`.
                        self.put(gx + i as i32, gy + j as i32, px, 255);
                    }
                }
            }
            // Parts the artwork asked to draw in the text colour (COLR's
            // `0xFFFF` sentinel). They are kept as a mask rather than baked
            // in, because the text colour only exists here.
            if let Some(fg) = &g.fg {
                for j in 0..g.height {
                    for i in 0..g.width {
                        let cov = fg[j * g.width + i] as u32;
                        if cov != 0 {
                            self.put(gx + i as i32, gy + j as i32, c, cov);
                        }
                    }
                }
            }
            return;
        }
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
        // Peekable, not a bare `chars()`: a variation selector after a codepoint
        // is what decides whether that codepoint is drawn as text or as emoji.
        let mut it = text.chars().peekable();
        while let Some(ch) = it.next() {
            if ch.is_control() {
                continue;
            }
            let next = it.peek().copied();
            if let Some(g) = fonts.glyph_at(spec.style, ch, next, spec.size) {
                self.draw_glyph(&g, pen.round() as i32, by, spec.color);
            }
            pen += fonts.advance_at(spec.style, ch, next, spec.size);
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
    fn colour_glyphs_keep_their_own_pixels() {
        let mut c = Canvas::new(2, 2);
        c.fill_rect(Rect::new(0.0, 0.0, 2.0, 2.0), Color::rgb(0, 0, 0));
        let g = Glyph {
            width: 2,
            height: 1,
            left: 0,
            top: 1, // baseline 1 - top 1 = row 0
            coverage: vec![255, 255],
            color: Some(vec![255, 0, 0, 255, 0, 255, 0, 255]),
            fg: None,
        };
        // The text colour must be ignored: tinting would destroy the artwork.
        c.draw_glyph(&g, 0, 1, Color::rgb(255, 255, 255));
        assert_eq!(c.pixels[0] & 0xFFFFFF, 0xFF0000, "red pixel lost");
        assert_eq!(c.pixels[1] & 0xFFFFFF, 0x00FF00, "green pixel lost");
    }

    #[test]
    fn colour_glyph_alpha_blends_against_the_backdrop() {
        let mut c = Canvas::new(1, 1);
        c.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), Color::rgb(0, 0, 0));
        let g = Glyph {
            width: 1,
            height: 1,
            left: 0,
            top: 1,
            coverage: vec![128],
            color: Some(vec![255, 255, 255, 128]),
            fg: None,
        };
        c.draw_glyph(&g, 0, 1, Color::rgb(0, 0, 0));
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
