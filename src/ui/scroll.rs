//! Vertical scroll position for a pane: where the view *is* (`offset`, the
//! value every paint uses) and, while a glide is running, where it is heading
//! (`target`).
//!
//! The two ways in are deliberately different. A finger, a scrollbar thumb or
//! anything else the pointer drives moves the view *immediately* — animating
//! input that is already continuous only adds latency. A wheel notch or a
//! PageDown is the opposite: it is one discrete jump of a big distance, and
//! jumping it in one frame is exactly what the eye reads as choppy, so those
//! glide to the new position over ~130 ms.

/// Time constant of the exponential approach to the target, in ms: the motion
/// is ~90% done in ~2.3τ and the last pixels taper off over a further τ.
///
/// Measured in *time*, not frames, so the same glide feels the same whether it
/// is stepped by the X11 loop's ~120 Hz clock, Win32's ~50 Hz `WM_TIMER`, or
/// the 40 ms heartbeat of a host that has no animation clock at all.
const GLIDE_TAU_MS: f32 = 55.0;
/// Fastest a glide may travel, px per ms (6 px/ms = 6000 px/s). A huge
/// accumulated delta — a fling, a held PageDown — still reads as motion
/// instead of teleporting across the document in one frame.
const GLIDE_MAX_PX_PER_MS: f32 = 6.0;
/// Travel below which a glide is over: sub-pixel motion is invisible, and
/// chasing it would keep the frame clock running forever.
const GLIDE_SETTLED: f32 = 0.5;

#[derive(Default, Debug, Clone, Copy)]
pub struct ScrollState {
    /// What the panes paint at.
    pub offset: f32,
    /// Where a glide is heading; equal to `offset` when nothing is animating.
    pub target: f32,
    pub content_height: f32,
    pub viewport_height: f32,
}

impl ScrollState {
    pub fn max(&self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }

    /// Move *immediately*: direct manipulation — a finger on the glass, a
    /// scrollbar thumb, a caret being followed, drag auto-scroll — has to track
    /// the input exactly. Putting a glide in the way of a finger would read as
    /// lag. Any glide in flight is abandoned at the current position.
    pub fn scroll_by(&mut self, dy: f32) {
        self.offset = (self.offset + dy).clamp(0.0, self.max());
        self.target = self.offset;
    }

    /// Move *and glide there*: for coarse, discrete steps — a wheel notch, a
    /// PageUp/PageDown. The distance accumulates, so spinning the wheel builds
    /// one continuous glide instead of a glide per notch.
    pub fn scroll_smooth(&mut self, dy: f32) {
        self.target = (self.target + dy).clamp(0.0, self.max());
        // A resize (or a shorter document) can leave the offset outside the
        // range the target is clamped to; keep the view inside it too.
        self.offset = self.offset.clamp(0.0, self.max());
        if (self.target - self.offset).abs() <= GLIDE_SETTLED {
            // A step too small to see still ends up applied, not dropped.
            self.offset = self.target;
        }
    }

    /// Is there distance left for a glide to cover? Exact, not
    /// `GLIDE_SETTLED`-tolerant: [`step`](Self::step) is what snaps the last
    /// fraction onto the target, and the caller has to call it once more after
    /// this returns false for that to happen. Stopping with a sub-pixel gap
    /// would leave the view a fraction off the position every later scroll is
    /// measured from, and never quite on the end of the document.
    pub fn animating(&self) -> bool {
        self.offset != self.target
    }

    /// Advance the glide by `dt_ms` of real time. Returns whether the view
    /// moved, i.e. whether a repaint is worth it.
    pub fn step(&mut self, dt_ms: f32) -> bool {
        let d = self.target - self.offset;
        if d.abs() <= GLIDE_SETTLED {
            let was = self.offset;
            self.offset = self.target;
            return self.offset != was;
        }
        // Exponential ease-out, capped in speed: `1 - e^-x` never reaches 1, so
        // the view always approaches the target from behind and can never
        // overshoot it.
        let want = d * (1.0 - (-dt_ms / GLIDE_TAU_MS).exp());
        let max = GLIDE_MAX_PX_PER_MS * dt_ms.max(0.0);
        self.offset += want.clamp(-max, max);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame from the slowest clock the app runs on: the 40 ms heartbeat.
    const SLOW_FRAME_MS: f32 = 40.0;
    /// A frame from the park: two display refreshes at 60 Hz.
    const FAST_FRAME_MS: f32 = 16.0;

    fn scroller(offset: f32) -> ScrollState {
        ScrollState {
            offset,
            target: offset,
            content_height: 10_000.0,
            viewport_height: 800.0,
        }
    }

    /// Run a glide to completion in `frame_ms` steps. Returns the offset after
    /// each frame and how long the whole thing took.
    fn run(s: &mut ScrollState, frame_ms: f32) -> (Vec<f32>, f32) {
        let mut seen = Vec::new();
        let mut ms = 0.0;
        while s.animating() {
            s.step(frame_ms);
            ms += frame_ms;
            seen.push(s.offset);
            assert!(seen.len() < 2_000, "a glide must settle");
        }
        (seen, ms)
    }

    /// Milliseconds for a glide of `d` to cover 90% of it — the part the eye
    /// reads as the scroll, with the rest left as an easing tail.
    fn ms_until_90(d: f32, frame_ms: f32) -> f32 {
        let mut s = scroller(0.0);
        s.scroll_smooth(d);
        let mut ms = 0.0;
        while s.offset < d * 0.9 {
            s.step(frame_ms);
            ms += frame_ms;
        }
        ms
    }

    #[test]
    fn clamps() {
        let mut s = scroller(0.0);
        s.scroll_by(50_000.0);
        assert_eq!(s.offset, s.max());
        assert_eq!(s.target, s.max());
        s.scroll_by(-50_000.0);
        assert_eq!(s.offset, 0.0);
        assert_eq!(s.target, 0.0);
    }

    #[test]
    fn a_coarse_step_glides_instead_of_jumping() {
        // The whole point: a wheel notch is one discrete jump today, which is
        // what reads as choppy. It must arrive over several frames, in one
        // direction, without overshooting.
        let mut s = scroller(0.0);
        s.scroll_smooth(480.0);
        assert_eq!(s.offset, 0.0, "nothing moves on the frame of the notch");
        assert_eq!(s.target, 480.0);
        assert!(s.animating());
        let (seen, ms) = run(&mut s, FAST_FRAME_MS);
        assert!(seen.len() >= 6, "a notch needs several frames: {seen:?}");
        for w in seen.windows(2) {
            assert!(w[1] > w[0], "glide must not go backwards: {seen:?}");
            assert!(w[1] <= 480.0, "glide must not overshoot: {seen:?}");
        }
        assert_eq!(s.offset, 480.0, "and it lands exactly on the target");
        assert!(
            ms_until_90(480.0, FAST_FRAME_MS) <= 200.0,
            "the visible part of a scroll should be over quickly, took {} ms",
            ms_until_90(480.0, FAST_FRAME_MS)
        );
        assert!(ms <= 600.0, "a glide must settle: {ms} ms");
    }

    #[test]
    fn the_glide_looks_the_same_at_any_frame_rate() {
        // Same distance, same elapsed time: the shape of the motion is a
        // property of time, not of how often the host can paint. A host on the
        // 40 ms heartbeat and one on a 120 Hz frame clock must not give the
        // user two different feels.
        //
        // Under the speed cap (see `GLIDE_MAX_PX_PER_MS`) a *huge* delta is
        // deliberately exempt: what a fling must not do is teleport, and a
        // bigger step is allowed to travel proportionally further. 120 px is a
        // normal few-notch scroll, well inside the cap either way.
        let mut slow = scroller(0.0);
        let mut fast = scroller(0.0);
        slow.scroll_smooth(120.0);
        fast.scroll_smooth(120.0);
        for _ in 0..4 {
            slow.step(SLOW_FRAME_MS);
            for _ in 0..5 {
                fast.step(SLOW_FRAME_MS / 5.0);
            }
        }
        // Equal to within f32 rounding of the two different step sizes — the
        // point is that there is no *visible* difference between them.
        assert!(
            (slow.offset - fast.offset).abs() < 0.01,
            "the frame rate must not change the feel: {} vs {}",
            slow.offset,
            fast.offset
        );
        assert!(
            slow.offset > 100.0,
            "160 ms in, most of the distance should be covered: {}",
            slow.offset
        );
    }

    #[test]
    fn spinning_the_wheel_builds_one_glide() {
        // Three notches in quick succession must not fight each other: the
        // distance adds up and the view keeps accelerating toward it.
        let mut s = scroller(0.0);
        for _ in 0..3 {
            s.scroll_smooth(480.0);
            s.step(FAST_FRAME_MS);
        }
        assert_eq!(s.target, 1440.0);
        assert!(s.offset > 240.0, "and it is already moving: {}", s.offset);
        let (_, ms) = run(&mut s, FAST_FRAME_MS);
        assert_eq!(s.offset, 1440.0);
        assert!(ms <= 900.0, "a three-notch fling must settle: {ms} ms");
    }

    #[test]
    fn a_fine_delta_is_applied_at_once() {
        // Touchpad and touch input arrive in small, high-frequency deltas:
        // those are already smooth, and gliding them would only add latency.
        let mut s = scroller(100.0);
        s.scroll_by(3.0);
        assert_eq!(s.offset, 103.0);
        s.scroll_by(-3.0);
        assert_eq!(s.offset, 100.0);
        assert!(!s.animating());
    }

    #[test]
    fn direct_manipulation_takes_over_from_a_glide() {
        let mut s = scroller(0.0);
        s.scroll_smooth(480.0);
        s.step(FAST_FRAME_MS);
        let gliding_at = s.offset;
        assert!(gliding_at > 0.0 && gliding_at < 480.0);
        // A finger touches down mid-glide: the view jumps to the finger and the
        // glide is abandoned rather than fighting it.
        s.scroll_by(5.0);
        assert_eq!(s.offset, gliding_at + 5.0);
        assert_eq!(s.target, s.offset);
        assert!(!s.animating());
        assert!(!s.step(FAST_FRAME_MS));
    }

    #[test]
    fn a_huge_delta_still_moves_at_a_visible_speed() {
        let mut s = scroller(0.0);
        s.scroll_smooth(9_000.0);
        let mut prev = 0.0;
        while s.animating() {
            s.step(FAST_FRAME_MS);
            let moved = s.offset - prev;
            assert!(
                moved <= GLIDE_MAX_PX_PER_MS * FAST_FRAME_MS + 0.001,
                "frame moved {moved}px"
            );
            prev = s.offset;
        }
        assert_eq!(s.offset, 9_000.0);
    }

    #[test]
    fn a_glide_stops_at_the_end_of_the_document() {
        let mut s = scroller(0.0);
        s.scroll_smooth(100_000.0);
        run(&mut s, FAST_FRAME_MS);
        assert_eq!(s.offset, s.max());
    }
}
