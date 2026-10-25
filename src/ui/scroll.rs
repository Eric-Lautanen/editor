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
pub(crate) const GLIDE_SETTLED: f32 = 0.5;

/// One frame of the shared glide easing: exponential ease-out toward
/// `target`, capped in speed so a huge delta reads as motion rather than a
/// teleport. Snaps onto the target inside [`GLIDE_SETTLED`]. Both scroll
/// axes ride this so wheel input feels identical horizontally and
/// vertically.
pub(crate) fn glide_step(offset: f32, target: f32, dt_ms: f32) -> f32 {
    let d = target - offset;
    if d.abs() <= GLIDE_SETTLED {
        return target;
    }
    // Exponential ease-out, capped in speed: `1 - e^-x` never reaches 1, so
    // the view always approaches the target from behind and can never
    // overshoot it.
    let want = d * (1.0 - (-dt_ms / GLIDE_TAU_MS).exp());
    let max = GLIDE_MAX_PX_PER_MS * dt_ms.max(0.0);
    offset + want.clamp(-max, max)
}

/// Fraction of a coast's speed kept per millisecond. The velocity decays
/// geometrically from wherever the finger let go — the rolodex feel, cranked
/// as the user asked: the page keeps moving the way it *was* moving and
/// bleeds off smoothly instead of arriving somewhere and stopping. The whole
/// ride is the geometric sum v/(1 − decay) ≈ 667·v px, so a 3 px/ms release
/// carries ~2000 px over a ~2.6 s bleed-off; even a soft 0.3 px/ms push
/// coasts ~160 px over a full second.
const COAST_DECAY_PER_MS: f32 = 0.9985;
/// A coast slower than this (px/ms) is finished: below one pixel per frame at
/// 60 Hz the remaining motion is invisible.
const COAST_MIN_PX_PER_MS: f32 = 0.06;
/// Fastest a coast may start at, in px/ms (8 px/ms = 8000 px/s). Higher
/// release velocities are clamped here — but 8 is *deliberately* generous, so
/// a fast flick really can throw the document a couple of thousand px: the
/// cap exists to stop a violent fling ripping past hundreds of screens, not
/// to slow the fast scroll a power user is asking for. (A release this hard
/// carries ~1600 px of travel at the current decay.)
#[cfg(any(target_os = "windows", test))]
const COAST_MAX_PX_PER_MS: f32 = 8.0;

#[derive(Default, Debug, Clone, Copy)]
pub struct ScrollState {
    /// The user asked the OS to reduce motion (see
    /// [`crate::platform::prefers_reduced_motion`]). The wheel glide — a ride
    /// the view takes to a destination the input already chose — is switched
    /// off; the input still moves the view exactly as far, just at once. A
    /// touch flick's coast stays: it is the tail of a direct manipulation, not
    /// decoration, and platforms that offer this setting (iOS, Android, macOS)
    /// keep scroll deceleration too. Switching it off made every flick stop
    /// dead, which reads as a broken app rather than a calmer one.
    pub reduce_motion: bool,
    /// What the panes paint at.
    pub offset: f32,
    /// Where a glide is heading; equal to `offset` when nothing is animating.
    pub target: f32,
    /// While coasting, the residual speed in px/ms. A glide has a fixed place
    /// to arrive at; a coast has none — the velocity *itself* is the state,
    /// decaying to zero, and where the view ends up is whatever the decay
    /// integrates to. Published like `offset`/`target`: a nonzero value is a
    /// ride in flight, and that is all "coasting" means.
    pub coast: f32,
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
    /// lag. Any glide *and any coast* in flight ends here: the finger owns the
    /// view, and a coast that kept going hands-off would ignore it.
    pub fn scroll_by(&mut self, dy: f32) {
        self.coast = 0.0;
        self.offset = (self.offset + dy).clamp(0.0, self.max());
        self.target = self.offset;
    }

    /// The finger let go at `v` px/ms: keep the motion it had, decaying.
    /// Near-zero velocities start nothing (a release after the finger held
    /// still simply stops, like any other direct manipulation). Any glide in
    /// flight is abandoned — the finger's momentum replaces it.
    #[cfg(any(target_os = "windows", test))]
    pub fn coast_from(&mut self, v: f32) {
        self.target = self.offset;
        // Deliberately not gated on `reduce_motion`: the coast is the finger's
        // own momentum carried past the lift, and a flick that stops dead the
        // instant it is released is the one thing touch scrolling cannot do.
        // What reduced motion switches off is the *wheel* glide (see
        // `scroll_smooth`), a ride the input did not ask to watch.
        self.coast = v.clamp(-COAST_MAX_PX_PER_MS, COAST_MAX_PX_PER_MS);
        if self.coast.abs() <= COAST_MIN_PX_PER_MS {
            self.coast = 0.0;
        }
    }

    /// Move *and glide there*: for coarse, discrete steps — a wheel notch, a
    /// PageUp/PageDown. The distance accumulates, so spinning the wheel builds
    /// one continuous glide instead of a glide per notch.
    ///
    /// A coast in flight is **replaced**, not extended: its stale `target` is
    /// nowhere the ride is any more, so gliding there after the notch would
    /// yank the view backwards. Re-anchor the target to where the coast has
    /// actually got to and let the wheel have the view from here.
    pub fn scroll_smooth(&mut self, dy: f32) {
        if self.coast != 0.0 {
            self.coast = 0.0;
            self.target = self.offset;
        }
        self.target = (self.target + dy).clamp(0.0, self.max());
        // A resize (or a shorter document) can leave the offset outside the
        // range the target is clamped to; keep the view inside it too.
        self.offset = self.offset.clamp(0.0, self.max());
        if self.reduce_motion || (self.target - self.offset).abs() <= GLIDE_SETTLED {
            // A step too small to see still ends up applied, not dropped — and
            // with motion reduced the step simply *is* the destination.
            self.offset = self.target;
        }
    }

    /// Is there distance left for a glide to cover? Exact, not
    /// `GLIDE_SETTLED`-tolerant: [`step`](Self::step) is what snaps the last
    /// fraction onto the target, and the caller has to call it once more after
    /// this returns false for that to happen. Stopping with a sub-pixel gap
    /// would leave the view a fraction off the position every later scroll is
    /// measured from, and never quite on the end of the document. A coast in
    /// flight counts too: something is moving either way.
    pub fn animating(&self) -> bool {
        self.coast != 0.0 || self.offset != self.target
    }

    /// Advance the animation by `dt_ms` of real time. Returns whether the view
    /// moved, i.e. whether a repaint is worth it.
    pub fn step(&mut self, dt_ms: f32) -> bool {
        if self.coast != 0.0 {
            // `powf` once covers the whole interval; splitting `dt` into
            // simulated sub-steps would be indistinguishable from this at
            // animation frame sizes and four times the work.
            let decay = COAST_DECAY_PER_MS.powf(dt_ms);
            let was = self.offset;
            let want = self.offset + self.coast * dt_ms;
            self.offset = want.clamp(0.0, self.max());
            self.coast *= decay;
            // The end of the document is a stop, not a wall: a coast wanting
            // to travel past an edge is only held there, so once the clamp
            // has left the view exactly where it was there is nothing left to
            // animate — and its remaining velocity (up to a couple of seconds
            // of it) must not keep the frame clock, and the whole pane,
            // repainting a still picture. `offset == target` is the same stop
            // for a release that landed on the edge.
            if self.coast.abs() <= COAST_MIN_PX_PER_MS
                || self.offset == was
                || self.offset == self.target
            {
                self.coast = 0.0;
                self.target = self.offset;
            }
            return self.offset != was;
        }
        let was = self.offset;
        self.offset = glide_step(self.offset, self.target, dt_ms);
        self.offset != was
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
            coast: 0.0,
            content_height: 10_000.0,
            viewport_height: 800.0,
            ..ScrollState::default()
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

    #[test]
    fn a_flick_coasts_and_bleeds_off() {
        // The report: touch scrolling should "coast a little after scroll".
        // The release velocity is the state: the view keeps moving the way the
        // finger was moving, decaying geometrically — never reversing, never
        // overshooting a destination it does not have.
        let mut s = scroller(0.0);
        s.coast_from(3.0); // a solid 3000 px/s flick
        assert!(s.coast != 0.0 && s.animating());
        let mut prev = 0.0;
        let mut last_move = 0.0;
        let mut ms = 0.0;
        while s.animating() {
            s.step(FAST_FRAME_MS);
            ms += FAST_FRAME_MS;
            assert!(s.offset >= prev, "a coast never reverses: {ms} ms in");
            prev = s.offset;
            last_move = ms;
        }
        assert_eq!(s.offset, prev, "the last frame moved too");
        // Seconds of ride for a hard flick: the long tail IS the feel
        // (decay 0.9985/ms keeps ~22% of the speed after 1 s).
        assert!(
            last_move >= 2400.0,
            "a hard flick should coast for seconds, coasted {last_move} ms"
        );
        // The whole coast is the geometric sum: v₀/(1 − decay per ms) =
        // 3 / 0.0015 = 2000 px of total travel, one long rolodex bleed-off.
        assert!(
            prev > 1900.0 && prev < 2100.0,
            "counter-integral of a decaying 3 px/ms velocity, landed at {prev}"
        );
    }

    #[test]
    fn a_soft_push_coasts_briefly() {
        // The decay is slow by design, so "brief" still means a comfortable
        // second and ~160 px for a soft nudge — never a dead stop.
        let mut s = scroller(0.0);
        s.coast_from(0.3);
        assert!(s.coast != 0.0);
        let (_, ms) = run(&mut s, FAST_FRAME_MS);
        assert!(
            (900.0..1300.0).contains(&ms),
            "a soft push coasts about a second: {ms} ms"
        );
        assert!(
            (100.0..220.0).contains(&s.offset),
            "and travels its little sum: {}",
            s.offset
        );
    }

    #[test]
    fn reduced_motion_steps_instead_of_gliding() {
        // The whole glide is switched off, but the distance is not: a wheel
        // notch is a scroll, not an animation, and the view must land exactly
        // where the same notch lands with motion on.
        let mut s = scroller(0.0);
        s.reduce_motion = true;
        s.scroll_smooth(480.0);
        assert_eq!(s.offset, 480.0, "the notch is applied at once");
        assert_eq!(s.target, 480.0);
        assert!(!s.animating(), "nothing left to animate");
        assert!(!s.step(FAST_FRAME_MS));
        // Two notches add up like they always did.
        s.scroll_smooth(480.0);
        assert_eq!(s.offset, 960.0);
        // And it still clamps at the ends of the document.
        s.scroll_smooth(100_000.0);
        assert_eq!(s.offset, s.max());
    }

    #[test]
    fn reduced_motion_keeps_the_flick_but_steps_the_wheel() {
        // The report: with "Animation effects" off, a fast flick stopped dead
        // under the finger. The coast is the end of a direct manipulation, so
        // it survives; only the wheel's animated ride is dropped.
        let mut s = scroller(0.0);
        s.reduce_motion = true;
        s.coast_from(3.0);
        assert!(s.coast != 0.0, "a flick must still keep travelling");
        assert!(s.animating());
        let (seen, ms) = run(&mut s, FAST_FRAME_MS);
        let moved = seen.last().copied().unwrap_or(0.0);
        assert!(moved > 300.0, "the flick coasted only {moved} px");
        assert!(ms > 200.0, "and stopped after {ms} ms");
        // The wheel, meanwhile, is applied at once: a notch is a scroll, not
        // an animation.
        s.scroll_smooth(-240.0);
        assert!(s.coast == 0.0, "the wheel takes the view from here");
        assert_eq!(s.target, s.offset);
    }

    #[test]
    fn a_release_that_held_still_starts_nothing() {
        // The finger arrived and stopped for a beat before lifting: no
        // velocity, no coast — the ride just ends where the hand is.
        let mut s = scroller(5.0);
        s.coast_from(0.04);
        assert!(s.coast == 0.0);
        assert!(!s.animating());
        assert!(!s.step(FAST_FRAME_MS));
    }

    #[test]
    fn a_violent_fling_is_clamped() {
        let mut s = scroller(0.0);
        s.coast_from(900.0);
        assert_eq!(s.coast, COAST_MAX_PX_PER_MS);
    }

    #[test]
    fn the_coast_stops_at_the_edges() {
        // A coast toward the top of the document stops there, cleanly —
        // hitting the end is a stop, not a wall that launches it backwards.
        let mut s = scroller(300.0);
        s.coast_from(-3.0);
        while s.animating() {
            s.step(FAST_FRAME_MS);
            assert!(s.offset >= 0.0);
        }
        assert_eq!(s.offset, 0.0);
        // And the `target` follows, so `animating` is a reliable stop signal.
        assert_eq!(s.target, 0.0);
    }

    #[test]
    fn a_coast_into_an_edge_stops_instead_of_riding_it_out() {
        // The clamp is a stop. The ride used to keep its velocity after the
        // view had been held at the end of the document, asking for frames —
        // and repainting a still pane — for the rest of the decay.
        let mut s = scroller(0.0);
        s.offset = s.max() - 100.0;
        s.target = s.offset;
        s.coast_from(3.0);
        let mut frames = 0;
        while s.animating() {
            s.step(FAST_FRAME_MS);
            frames += 1;
            assert!(frames < 10, "the ride sat on the edge for {frames} frames");
        }
        assert_eq!(s.offset, s.max());
        assert!(!s.step(FAST_FRAME_MS), "nothing left to paint");
    }

    #[test]
    fn a_finger_takes_over_from_a_coast() {
        // Touch down mid-coast: the ride is over, the finger owns the view,
        // exactly as it takes over from a wheel glide.
        let mut s = scroller(0.0);
        s.coast_from(3.0);
        s.step(FAST_FRAME_MS);
        let coasting_at = s.offset;
        assert!(coasting_at > 0.0);
        s.scroll_by(5.0);
        assert_eq!(s.offset, coasting_at + 5.0);
        assert!(s.coast == 0.0);
        assert!(!s.animating());
    }

    #[test]
    fn a_wheel_notch_replaces_a_coast() {
        // The coast and the glide are separate states — and a wheel notch
        // arriving mid-coast must *replace* the ride, not glide to the old
        // target afterwards, which would yank the view back to where the
        // coast was heading before the wheel spoke. So `scroll_smooth`
        // re-anchors the target to where the coast has actually got to; that
        // is also what makes `gliding()` tell the truth for callers that
        // re-target.
        let mut s = scroller(0.0);
        s.coast_from(2.0);
        assert!(s.coast != 0.0, "a coast is not a glide");
        // Spin the coast for a moment so the offset and the stale target part.
        s.step(FAST_FRAME_MS);
        assert!(s.coast != 0.0, "still riding");
        s.scroll_smooth(480.0);
        assert!(s.coast == 0.0 && s.animating(), "the wheel wins the view");
        assert_eq!(s.target, s.offset + 480.0, "aimed from where we now are");
        // And the whole of the rest of the motion is that glide: the coast
        // velocity is gone, so repeated steps close on the target and never
        // run past it.
        let (seen, _) = run(&mut s, FAST_FRAME_MS);
        for w in seen.windows(2) {
            assert!(w[1] <= s.target + 0.001);
        }
        assert_eq!(s.offset, s.target);
    }

    #[test]
    fn a_slow_frame_rate_stretches_no_coast() {
        // Time-based like the glide: 8 frames of 20 ms = 5 frames of 32 ms.
        let mut slow = scroller(0.0);
        let mut fast = scroller(0.0);
        slow.coast_from(-1.0);
        fast.coast_from(-1.0);
        slow.step(20.0);
        slow.step(20.0);
        slow.step(20.0);
        slow.step(20.0);
        slow.step(20.0);
        slow.step(20.0);
        slow.step(20.0);
        slow.step(20.0);
        for _ in 0..5 {
            fast.step(32.0);
        }
        assert!(
            (slow.offset - fast.offset).abs() < 1.0,
            "the frame rate must not change the feel: {} vs {}",
            slow.offset,
            fast.offset
        );
    }
}
