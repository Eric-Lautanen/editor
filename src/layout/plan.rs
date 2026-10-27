//! A document addressed by scroll position.
//!
//! Every reader kind lays its document out as a vertical stack of *units*: a
//! line for text, code, logs and configs, a block for Markdown, a row for CSV.
//! A whole-document display list keeps a draw op (and a `String`) for every one
//! of those, which is why an 8 MB code file used to hold 1.6 million ops.
//!
//! A plan keeps only where each unit starts — one `f32` — plus the document
//! height. That is enough to find the units a viewport covers by binary search
//! and lay out just those, so a big document costs the same per frame as a
//! small one and nothing whole-document is retained. Each kind still owns its
//! own painter; what is shared here is the index and the exact rule for which
//! units a band covers, so a new kind gets banded painting by supplying a
//! plan, not by inventing a cache.

/// Start y (document space) of each unit, in order, plus the total height.
#[derive(Default, Clone)]
pub struct Rows {
    tops: Vec<f32>,
    height: f32,
    /// Space below the last unit. Stored so [`Rows::splice`] can price the
    /// final unit from the document height alone.
    tail: f32,
}

impl Rows {
    pub fn new(tops: Vec<f32>, height: f32, tail: f32) -> Self {
        Self { tops, height, tail }
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    pub fn len(&self) -> usize {
        self.tops.len()
    }

    /// Replace `removed` units at `start` with units of the given heights,
    /// shifting every unit below by the height difference.
    ///
    /// This is the whole-document work a plan patch avoids: the caller
    /// measures only the units an edit inserted, and the index is re-based in
    /// one pass over the tops. Returns `false` when the range does not fit the
    /// plan, so a caller replays edits without checking the plan's shape can
    /// still fall back to building it again.
    pub fn splice(&mut self, start: usize, removed: usize, heights: &[f32]) -> bool {
        if start > self.tops.len() || start + removed > self.tops.len() {
            return false;
        }
        let base = if start < self.tops.len() {
            self.tops[start]
        } else {
            self.height - self.tail
        };
        let end_top = if start + removed < self.tops.len() {
            self.tops[start + removed]
        } else {
            self.height - self.tail
        };
        let mut fresh = Vec::with_capacity(heights.len());
        let mut y = base;
        for &h in heights {
            fresh.push(y);
            y += h;
        }
        let dy = y - end_top;
        self.tops.splice(start..start + removed, fresh);
        if dy != 0.0 {
            for t in &mut self.tops[start + heights.len()..] {
                *t += dy;
            }
            self.height += dy;
        }
        true
    }

    pub fn top(&self, i: usize) -> f32 {
        self.tops.get(i).copied().unwrap_or(self.height)
    }

    /// How tall unit `i` is: the distance to the next unit's top, or — for the
    /// last one — from its top down to the bottom of the last unit, which is
    /// the document height less the space held below it.
    pub fn unit_h(&self, i: usize) -> f32 {
        let top = self.top(i);
        match self.tops.get(i + 1) {
            Some(next) => next - top,
            None => (self.height - self.tail - top).max(0.0),
        }
    }

    /// The index of the unit containing `y`: the last unit that starts at or
    /// above `y`, clamped to the document. A band that begins inside a wrapped
    /// unit still starts at that unit.
    pub fn at(&self, y: f32) -> usize {
        match self.tops.partition_point(|&t| t <= y) {
            0 => 0,
            n => n - 1,
        }
    }

    /// The units that can paint anything in `[top, bottom)`: from the unit
    /// containing `top` up to the last unit that starts at or above `bottom`.
    pub fn band(&self, top: f32, bottom: f32) -> std::ops::Range<usize> {
        if self.tops.is_empty() {
            return 0..0;
        }
        let start = self.at(top);
        let end = self.tops.partition_point(|&t| t <= bottom).max(start + 1);
        start..end.min(self.tops.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Rows {
        // Four units of 10 px, then a 40 px one.
        Rows::new(vec![0.0, 10.0, 20.0, 30.0], 70.0, 0.0)
    }

    #[test]
    fn a_splice_prices_only_its_units_and_shifts_the_rest() {
        let mut r = rows();
        // Unit 1 grows from 10 to 25: everything below moves down 15.
        assert!(r.splice(1, 1, &[25.0]));
        assert_eq!(r.height(), 85.0);
        assert_eq!(r.top(0), 0.0);
        assert_eq!(r.top(1), 10.0);
        assert_eq!(r.top(2), 35.0);
        assert_eq!(r.top(3), 45.0);
        assert_eq!(r.band(30.0, 50.0), 1..4);
    }

    #[test]
    fn a_splice_can_add_and_remove_units() {
        let mut r = rows();
        // Replace unit 2 (10 px) with three units of 5 px.
        assert!(r.splice(2, 1, &[5.0, 5.0, 5.0]));
        assert_eq!(r.len(), 6);
        assert_eq!(r.top(2), 20.0);
        assert_eq!(r.top(3), 25.0);
        assert_eq!(r.top(4), 30.0);
        assert_eq!(r.top(5), 35.0);
        assert_eq!(r.height(), 75.0);
        // And drop them again: the shape returns to the original.
        assert!(r.splice(2, 3, &[10.0]));
        assert_eq!(r.len(), 4);
        assert_eq!(r.height(), 70.0);
        // The final unit is priced from the height and the tail.
        assert!(r.splice(3, 1, &[20.0]));
        assert_eq!(r.height(), 50.0);
        // Appending at the end of the index works too.
        assert!(r.splice(4, 0, &[7.0]));
        assert_eq!(r.len(), 5);
        assert_eq!(r.height(), 57.0);
        assert_eq!(r.top(4), 50.0);
    }

    #[test]
    fn a_splice_outside_the_index_is_refused() {
        let mut r = rows();
        assert!(!r.splice(5, 0, &[1.0]));
        assert!(!r.splice(2, 3, &[1.0]));
        assert_eq!(r.height(), 70.0);
    }

    /// Building an index with splices then replacing a middle run must land on
    /// exactly the tops a fresh index of the same units has.
    #[test]
    fn splices_match_a_fresh_index_of_the_same_units() {
        let hs = [4.0f32, 6.0, 8.0, 10.0, 12.0];
        let index = |hs: &[f32]| {
            let mut tops = Vec::new();
            let mut y = 0.0;
            for &h in hs {
                tops.push(y);
                y += h;
            }
            Rows::new(tops, y, 0.0)
        };
        let mut patched = Rows::default();
        for &h in &hs {
            let n = patched.len();
            assert!(patched.splice(n, 0, &[h]));
        }
        let fresh = index(&hs);
        for i in 0..fresh.len() {
            assert_eq!(patched.top(i), fresh.top(i), "top {i}");
        }
        assert_eq!(patched.height(), fresh.height());

        // Replace units 2..4 with two 7 px units; compare to a rebuild.
        let mut after = hs.to_vec();
        after.splice(2..4, [7.0, 7.0]);
        let mut rebuilt = index(&after);
        assert!(patched.splice(2, 2, &[7.0, 7.0]));
        for i in 0..rebuilt.len() {
            assert_eq!(patched.top(i), rebuilt.top(i), "patched top {i}");
        }
        assert_eq!(patched.height(), rebuilt.height());
        let _ = rebuilt.splice(0, 0, &[]);
    }

    #[test]
    fn a_band_starts_at_the_unit_that_contains_its_top() {
        let r = rows();
        assert_eq!(r.at(0.0), 0);
        assert_eq!(r.at(9.9), 0);
        // A top inside a wrapped unit starts at that unit, not the next.
        assert_eq!(r.at(25.0), 2);
        // Past the end it is the last unit, so a scrolled-to-bottom pane
        // still paints its final row.
        assert_eq!(r.at(500.0), 3);
        assert_eq!(r.band(25.0, 45.0), 2..4);
        assert_eq!(r.band(0.0, 9.0), 0..1);
        assert_eq!(r.band(69.0, 69.0), 3..4);
    }

    #[test]
    fn every_unit_that_reaches_the_band_is_included_and_nothing_above_it() {
        let r = rows();
        let (top, bottom) = (15.0, 35.0);
        let band = r.band(top, bottom);
        // Unit 1 straddles the top edge; unit 2 covers the bottom edge; unit
        // 3 starts exactly at it (a row on the boundary paints its pixels).
        assert_eq!(band, 1..4);
        // Nothing wholly above the band.
        for i in 0..band.start {
            assert!(r.top(i + 1) <= top, "unit {i} ends below the band");
        }
        // Everything the band covers is in it.
        for i in band.clone() {
            assert!(r.top(i) <= bottom || i == band.start);
        }
    }

    #[test]
    fn an_empty_document_has_an_empty_band() {
        let r = Rows::default();
        assert_eq!(r.band(0.0, 900.0), 0..0);
        assert_eq!(r.at(10.0), 0);
    }
}
