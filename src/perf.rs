//! Cost gate: synthetic large documents, real budgets.
//!
//! The regressions this exists for are not subtle — a re-parse of the whole
//! document on every frame, a layout that goes quadratic in the number of
//! lines, a preview that keeps the file text twice — and none of them show up
//! as a wrong pixel. They show up as a window that gets slower the bigger the
//! file, which is exactly what a UI test cannot see and a developer on a small
//! sample never feels.
//!
//! So: generate one document per supported family at a size a user would
//! actually hit (8 MB — an eighth of the 64 MB refusal limit), open it, paint
//! it, scroll it, and compare five numbers against a budget:
//!
//! * **load** — parse + build the first layout, i.e. how long the window is
//!   blank after a double-click.
//! * **first frame** — the first paint, canvas allocation included.
//! * **frame** — mean paint while scrolling, i.e. what smooth scrolling costs.
//! * **churn** — bytes allocated per frame while scrolling. This is the one
//!   that catches "re-lay-out the whole document every frame": a frame that
//!   allocates megabytes is doing per-frame work proportional to the file, no
//!   matter how fast the machine is.
//! * **live** — heap held by the open tab once it has painted, over the
//!   process's own baseline, i.e. the user's RAM for that one file.
//!
//! These are `#[ignore]`d because they measure wall-clock: `cargo test` in
//! debug builds is 10–30x slower, and a debug number says nothing about what a
//! user feels. CI runs them in release (`cargo test --release -- --ignored
//! perf --nocapture`), on one OS, with budgets set at roughly 3x the numbers
//! measured on a runner — loose enough not to flake on a noisy neighbour, tight
//! enough that an order-of-magnitude regression (the only kind worth failing a
//! build over) cannot slip through.
//!
//! To see the current numbers without failing anything, run:
//! `cargo test --release -- --ignored perf --nocapture`.

use crate::app::App;
use crate::platform::{Handler, NullHost};
use std::time::Instant;

/// How big each synthetic document is.
///
/// Eight megabytes: past an eighth of the open limit. Every reader kind is
/// planned and painted as a band now, so a big document costs the plan (and,
/// for Markdown, one measure pass) rather than a whole-document display list,
/// and the gate can afford the size where that matters. `pane_ops` is the
/// other half of the same rule: the number of ops the pane holds after the
/// first frame must stay a band, not a document.
const TARGET_BYTES: usize = 8 * 1024 * 1024;

/// Frames painted while scrolling. Enough for a mean that a single GC-ish
/// hiccup cannot move, few enough to keep the gate to a few seconds.
const FRAMES: usize = 24;

/// The memory census is process-global, so two kinds measured at once would
/// bill each other's documents (and the live numbers would swing by hundreds
/// of megabytes with the scheduler). The gate runs them one at a time.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One budget, in the units the gate reports.
#[derive(Debug, Clone, Copy)]
struct Budget {
    load_ms: f64,
    first_frame_ms: f64,
    frame_ms: f64,
    churn_mb: f64,
    live_mb: f64,
    /// Ops the pane may hold after the first frame. A band is a screenful;
    /// an op per line is the regression this catches.
    pane_ops: usize,
}

#[derive(Debug, Clone, Copy)]
struct Cost {
    load_ms: f64,
    first_frame_ms: f64,
    /// G2: open → streamed load → band → real pane installed, with the
    /// shipped switches (async worker when the pane qualifies). First frame
    /// says "something on screen"; this says "readable".
    readable_ms: f64,
    frame_ms: f64,
    churn_mb: f64,
    live_mb: f64,
    ops: usize,
    /// Paint after one keystroke in Split, for a planned pane — `typing_ms`
    /// is the keystroke near the top, `typing_end_ms` the same one on the
    /// last line (G4: `same_lines` and the patch slot move grow with the
    /// caret's position, and the top position hides them). `None` for a pane
    /// with no plan (CSV): its next layout is its parser's.
    typing_ms: Option<f64>,
    typing_end_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Json,
    Jsonl,
    Markdown,
    Log,
    Csv,
    Code,
}

impl Kind {
    fn all() -> [Kind; 6] {
        [
            Kind::Json,
            Kind::Jsonl,
            Kind::Markdown,
            Kind::Log,
            Kind::Csv,
            Kind::Code,
        ]
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Json => "json",
            Kind::Jsonl => "jsonl",
            Kind::Markdown => "markdown",
            Kind::Log => "log",
            Kind::Csv => "csv",
            Kind::Code => "code",
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Kind::Json => "json",
            Kind::Jsonl => "jsonl",
            Kind::Markdown => "md",
            Kind::Log => "log",
            Kind::Csv => "csv",
            Kind::Code => "rs",
        }
    }

    fn budget(self) -> Budget {
        match self {
            // Calibrated from three consecutive runs on a developer box:
            // roughly 3x the slowest of them. CI runners are slower and
            // noisier than a laptop, and a gate that flakes gets deleted, so
            // the margins are wide on purpose — an order of magnitude is the
            // regression worth failing a build over, and these still catch it.
            //
            // `frame` and `churn` are the same for every kind because their
            // measured range is narrow (0.4-3 ms, 0-0.2 MB/frame). Load and
            // first frame differ by what each kind makes the app do up front:
            // a parse, a plan, or (Markdown) a full measure pass, because a
            // block's height is whatever its spans wrap to.
            //
            // `live` is a gross-leak check, not a target. JSON's is the
            // largest because the parsed tree is the document it paints from;
            // the others are text plus a plan.
            //
            // `pane_ops` is the structural guarantee: whatever the size, what
            // the pane holds is the viewport. CSV is the exception and is
            // bounded by its parser's row cap instead, so it cannot grow with
            // the file either.
            Kind::Json => Budget {
                load_ms: 800.0,
                first_frame_ms: 1200.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
                pane_ops: 2_000,
            },
            Kind::Jsonl => Budget {
                load_ms: 800.0,
                first_frame_ms: 1600.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
                pane_ops: 2_000,
            },
            Kind::Markdown => Budget {
                load_ms: 800.0,
                first_frame_ms: 5000.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
                pane_ops: 2_000,
            },
            Kind::Log => Budget {
                load_ms: 800.0,
                first_frame_ms: 300.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 120.0,
                pane_ops: 2_000,
            },
            Kind::Csv => Budget {
                load_ms: 800.0,
                first_frame_ms: 600.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 120.0,
                pane_ops: 40_000,
            },
            Kind::Code => Budget {
                load_ms: 3000.0,
                first_frame_ms: 300.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 120.0,
                pane_ops: 2_000,
            },
        }
    }
}

/// Deterministic filler: a failing gate must fail at the same document every
/// time, or the first debugging step is "did the input even match?".
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, words: &[&'a str]) -> &'a str {
        words[(self.next() as usize) % words.len()]
    }
}

const WORDS: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa",
];

/// Build a document of `kind` at least `target` bytes long. Valid input for
/// its parser: the gate measures the app, not the error path.
fn synthetic(kind: Kind, target: usize) -> String {
    let mut rng = Rng(0x5eed);
    let mut s = String::with_capacity(target + 64 * 1024);
    match kind {
        Kind::Json => {
            s.push_str("[\n");
            let mut i = 0usize;
            while s.len() < target {
                s.push_str(&format!(
                    "  {{\n    \"id\": {i},\n    \"name\": \"item-{i}-{w}\",\n    \
                     \"active\": {active},\n    \"score\": {whole}.{frac:02},\n    \
                     \"tags\": [\"{a}\", \"{b}\", \"{c}\"],\n    \
                     \"nested\": {{\"x\": {x}, \"y\": {y}, \"note\": \"{note}\"}}\n  }},\n",
                    w = rng.pick(WORDS),
                    active = i.is_multiple_of(3),
                    whole = rng.next() % 1000,
                    frac = rng.next() % 100,
                    a = rng.pick(WORDS),
                    b = rng.pick(WORDS),
                    c = rng.pick(WORDS),
                    x = rng.next() as i64 - 500,
                    y = rng.next() % 10_000,
                    note = rng.pick(WORDS),
                ));
                i += 1;
            }
            // Drop the trailing comma, close the array.
            s.truncate(s.trim_end_matches(",\n").len());
            s.push_str("\n]\n");
        }
        Kind::Jsonl => {
            let mut i = 0usize;
            while s.len() < target {
                s.push_str(&format!(
                    "{{\"ts\":\"2026-10-17T12:{:02}:{:02}Z\",\"level\":\"{}\",\"id\":{i},\
                     \"msg\":\"{} {}\",\"ok\":{}}}\n",
                    i % 60,
                    (i * 7) % 60,
                    rng.pick(&["info", "warn", "error", "debug"]),
                    rng.pick(WORDS),
                    rng.pick(WORDS),
                    i.is_multiple_of(2),
                ));
                i += 1;
            }
        }
        Kind::Markdown => {
            let mut i = 0usize;
            while s.len() < target {
                match i % 5 {
                    0 => s.push_str(&format!("## Section {i}\n\n")),
                    1 => {
                        s.push_str(&format!("### {}", rng.pick(WORDS)));
                        s.push_str("\n\n");
                    }
                    2 => {
                        // A paragraph of inline marks, the layout's expensive
                        // case: emphasis, code spans, links, all re-measured
                        // per word when a line breaks.
                        for _ in 0..6 {
                            s.push_str(&format!(
                                "Some **{}** text with `{}` and a [link](https://example.com/{i}) \
                                 that wraps around the column. ",
                                rng.pick(WORDS),
                                rng.pick(WORDS),
                            ));
                        }
                        s.push_str("\n\n");
                    }
                    3 => {
                        s.push_str("- ");
                        for _ in 0..5 {
                            s.push_str(&format!("{} ", rng.pick(WORDS)));
                        }
                        s.push_str("\n- nested\n  - deeper\n\n");
                    }
                    _ => {
                        s.push_str("```rust\nfn ");
                        s.push_str(rng.pick(WORDS));
                        s.push_str("() -> u32 { 42 }\n```\n\n");
                    }
                }
                i += 1;
            }
        }
        Kind::Log => {
            let mut i = 0usize;
            while s.len() < target {
                s.push_str(&format!(
                    "2026-10-17T12:{:02}:{:02}.{:03}Z {} {:<8} [worker-{}] {} {} id={}\n",
                    i % 60,
                    (i * 3) % 60,
                    i % 1000,
                    rng.pick(&["INFO", "WARN", "ERROR", "DEBUG", "TRACE"]),
                    rng.pick(&["api", "db", "cache", "auth"]),
                    i % 8,
                    rng.pick(WORDS),
                    rng.pick(WORDS),
                    i,
                ));
                i += 1;
            }
        }
        Kind::Csv => {
            s.push_str("id,name,status,score,city,note,qty,when\n");
            let mut i = 0usize;
            while s.len() < target {
                s.push_str(&format!(
                    "{i},{}-{},{} ,{}.{},{},\"{}, {}\",{},\"2026-10-17 12:{:02}\"\n",
                    rng.pick(WORDS),
                    i,
                    rng.pick(&["active", "paused", "done"]),
                    rng.next() % 1000,
                    rng.next() % 100,
                    rng.pick(&["Berlin", "Lisbon", "Osaka", "Quito"]),
                    rng.pick(WORDS),
                    rng.pick(WORDS),
                    rng.next() % 500,
                    i % 60,
                ));
                i += 1;
            }
        }
        Kind::Code => {
            let mut i = 0usize;
            while s.len() < target {
                s.push_str(&format!(
                    "/// {} the {}.\n\
                     pub fn {}_{} (input: &str, n: usize) -> Option<String> {{\n    \
                     let mut out = String::with_capacity(n);\n    \
                     for (i, ch) in input.chars().enumerate() {{\n        \
                     // {} {}\n        \
                     if i % {} == 0 {{ out.push(ch); }}\n    \
                     }}\n    \
                     Some(format!(\"{{}}: {{}}\", {}, out))\n}}\n\n",
                    rng.pick(&["Return", "Build", "Parse", "Check"]),
                    rng.pick(WORDS),
                    rng.pick(WORDS),
                    i,
                    rng.pick(WORDS),
                    rng.pick(WORDS),
                    2 + (rng.next() as usize) % 9,
                    i,
                ));
                i += 1;
            }
        }
    }
    s
}

/// Open `kind`'s synthetic document, then measure it.
fn measure(kind: Kind) -> Cost {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let text = synthetic(kind, TARGET_BYTES);
    assert!(
        text.len() >= TARGET_BYTES,
        "{} generator produced only {} bytes",
        kind.name(),
        text.len()
    );
    let path =
        crate::testing::temp_dir().join(format!("editor_perf_{}.{}", kind.name(), kind.ext()));
    std::fs::write(&path, &text).unwrap();
    // The generator's own copy is 2 MB; drop it before the memory baseline so
    // the harness never bills itself to the document.
    drop(text);

    // G1 · A fair load measurement. The fixture was written milliseconds ago,
    // and the file system has never served it, so the first read below would
    // pay first-touch (571 ms vs 4.8 ms warm on the baseline machine) — a
    // cost no one waiting on the Open dialog pays, because that file already
    // exists and has been touched many times. One untimed read warms the
    // page cache and lets the first-touch finish. No sleeps: the warm read
    // *is* the settle, and it works the same on a noisy CI runner.
    let warm = std::fs::read(&path).unwrap();
    drop(warm);

    // 1440x900 at 1x: a common laptop window, and the width the wrap logic
    // was tuned against. Defined before the G2 run below, which paints at
    // the same size so the two panes compare.
    let (w, h) = (1440usize, 900usize);

    // G2 · The shipped path's "readable" number. Shipped, `async_layout` is
    // on (it is everything but `!headless` that ever calls this), so a
    // document whose whole-document layout costs seconds is laid out on a
    // worker while the band stays up. What the user calls readable is the
    // whole trip: open → streamed load → first band → real pane installed.
    // Run that same open with the shipped switches, pump the pump, and time
    // it. The end state is also compared against what the *inline* build
    // (the run below) installs for the same bytes: same band ops and same
    // content height, or the two paths are not interchangeable and the
    // equality the patch tests assert has a hole across the worker boundary.
    let readable = {
        let mut a = App::new_streaming(Some(path.clone()), false).expect("open");
        let mut ahost = NullHost::default();
        let mut a_canvas = crate::gfx::canvas::Canvas::new(w, h);
        // The same geometry the inline run paints at: `render_headless`
        // reports a resize before its paint, and here the app is fed the
        // event directly — a pane built at the default size would compare
        // against nothing.
        a.on_event(
            crate::ui::event::Event::Resized {
                width: w,
                height: h,
                scale: 1.0,
            },
            &mut ahost,
        );
        let t = Instant::now();
        Handler::paint(&mut a, &mut a_canvas); // first frame: the band ships
        let mut guard = 0;
        // The streamer and the worker both have to be done, and the seal's
        // relayout has to have *run*: `streamer` clearing and `layout_dirty`
        // setting happen inside the same tick, so pump one more paint.
        while a.tabs[0].streamer.is_some() || a.preview_job_pending() {
            a.on_event(crate::ui::event::Event::Tick, &mut ahost);
            Handler::paint(&mut a, &mut a_canvas);
            guard += 1;
            assert!(
                guard < 4800,
                "{}: the shipped path never became readable",
                kind.name()
            );
        }
        a.on_event(crate::ui::event::Event::Tick, &mut ahost);
        Handler::paint(&mut a, &mut a_canvas); // the pane the user reads
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let pane = (
            a.preview.list.ops.len(),
            a.preview.list.height,
            pane_fingerprint(&a.preview.list),
            a.scroll.content_height,
            format!(
                "streamer={} pending={} lines={} scroll={:.1} hmax={:.1} plan_rows={}",
                a.tabs[0].streamer.is_some(),
                a.preview_job_pending(),
                a.tabs[0].buffer.line_count(),
                a.h_scroll,
                a.h_max,
                a.tabs[0].plan.slots.len(),
            ),
        );
        drop(a);
        (ms, pane)
    };

    // Live bytes are relative to the process's own baseline, taken before the
    // document exists: the harness runs every kind in one process, and a
    // global cache (fonts, glyphs) filled by the previous kind must not be
    // billed to this one.
    #[cfg(any(test, feature = "mem-census"))]
    let live_before = crate::mem::snapshot().global_live;

    // Load: the constructor parses and lays out; nothing is painted yet.
    let t = Instant::now();
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    let load_ms = t.elapsed().as_secs_f64() * 1000.0;

    // The document has to be really there: a gate that measures an empty
    // window would pass for the wrong reason.
    let held = app.buffer.text().len();
    assert!(
        held > TARGET_BYTES / 2,
        "{}: buffer holds {held} bytes",
        kind.name()
    );

    let mut host = NullHost::default();

    let t = Instant::now();
    let canvas = app.render_headless(w, h, 1.0);
    let first_frame_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert!(
        !canvas.pixels.is_empty(),
        "{}: the first frame painted nothing",
        kind.name()
    );

    // Steady state. The surface is reused, as the platform reuses its own,
    // and no resize is sent: `render_headless` reports a resize every call,
    // which marks the layout dirty and would bill a full relayout to every
    // "scroll frame" — the number this gate is least interested in.
    let mut canvas = crate::gfx::canvas::Canvas::new(w, h);
    for i in 0..3 {
        app.set_scroll(i as f32 * 120.0);
        app.on_event(crate::ui::event::Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
    }

    #[cfg(any(test, feature = "mem-census"))]
    let total_before = crate::mem::snapshot().global_total;
    let t = Instant::now();
    for i in 0..FRAMES {
        app.set_scroll((i * 37) as f32);
        app.on_event(crate::ui::event::Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
    }
    let frame_ms = t.elapsed().as_secs_f64() * 1000.0 / FRAMES as f64;
    #[cfg(any(test, feature = "mem-census"))]
    let churn_mb = {
        let total_after = crate::mem::snapshot().global_total;
        total_after.saturating_sub(total_before) as f64 / 1024.0 / 1024.0 / FRAMES as f64
    };
    #[cfg(not(any(test, feature = "mem-census")))]
    let churn_mb = f64::NAN;

    #[cfg(any(test, feature = "mem-census"))]
    let live_mb = {
        let now = crate::mem::snapshot().global_live;
        now.saturating_sub(live_before) as f64 / 1024.0 / 1024.0
    };
    #[cfg(not(any(test, feature = "mem-census")))]
    let live_mb = f64::NAN;

    let ops = app.preview.list.ops.len();

    // G2's equality arm: the inline run's pane, rewound to the same state
    // the async run settled at (scroll 0, installed — the scroll frames
    // above moved the band, so the pane is re-painted at 0 first).
    app.set_scroll(0.0);
    app.on_event(crate::ui::event::Event::Tick, &mut host);
    Handler::paint(&mut app, &mut canvas);
    let installed = (
        app.preview.list.ops.len(),
        app.preview.list.height,
        pane_fingerprint(&app.preview.list),
        app.scroll.content_height,
        format!(
            "streamer={} pending={} lines={} scroll={:.1} hmax={:.1} plan_rows={}",
            app.tabs[0].streamer.is_some(),
            app.preview_job_pending(),
            app.tabs[0].buffer.line_count(),
            app.h_scroll,
            app.h_max,
            app.tabs[0].plan.slots.len(),
        ),
    );
    if readable.1 != installed {
        let a_path = crate::testing::temp_dir()
            .join(format!("editor_fingerprint_{}_async.txt", kind.name()));
        let i_path = crate::testing::temp_dir()
            .join(format!("editor_fingerprint_{}_inline.txt", kind.name()));
        let _ = std::fs::write(&a_path, &readable.1 .2);
        let _ = std::fs::write(&i_path, &installed.2);
        panic!(
            "{}: the worker-installed pane is not the pane the inline build produces\n  async: {}\n  inline: {}\n(dumps: {} and {})",
            kind.name(),
            readable.1.4,
            installed.4,
            a_path.display(),
            i_path.display(),
        );
    }

    // A big JSONL's records and index land on the worker, so they have to be
    // in place before a keystroke can be judged. A line pane is planned
    // inline, and for it this loop does nothing.
    let mut guard = 0;
    while app.preview_job_pending() {
        app.on_event(crate::ui::event::Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(
            guard < 600,
            "{}: the pane's layout never landed",
            kind.name()
        );
    }

    // Typing. The number is reported, but the assertion is structural: a
    // planned pane must *patch* its plan for the keystroke, never re-measure
    // the document. A rebuild would be correct and still pass every pixel
    // test, which is exactly why the gate checks the path taken. A virtual
    // JSONL counts: its records and their index are a plan like any other's,
    // and so does a virtual JSON tree, whose node ranges let the keystroke be
    // localized to one value.
    let planned = app.tabs[0].pane_plan.is_some()
        || !app.tabs[0].plan.slots.is_empty()
        || app.tabs[0].json_doc.is_some();
    let typing_ms = if planned {
        app.set_mode(crate::app::Mode::Split);
        app.on_event(
            crate::ui::event::Event::Resized {
                width: w,
                height: h,
                scale: 1.0,
            },
            &mut host,
        );
        Handler::paint(&mut app, &mut canvas);
        // The warmup relayout above dispatched Split's whole-document replan
        // to the worker (async waits are measured elsewhere). The keystroke
        // is only honest to judge against a plan that is already in: the
        // first keystroke never patched markdown within T2's block splice
        // when a pending job let the paint bill the worker's plan instead.
        let mut guard = 0;
        while app.preview_job_pending() {
            app.on_event(crate::ui::event::Event::Tick, &mut host);
            Handler::paint(&mut app, &mut canvas);
            guard += 1;
            assert!(
                guard < 600,
                "{}: the split replan never landed",
                kind.name()
            );
        }
        // A JSON tree is patched at the node the caret is in, so the caret
        // goes inside a string: at byte zero the keystroke would leave the
        // document unparseable, and a re-parse would be the honest answer to
        // that (and would fail the assertion below for the wrong reason).
        if kind == Kind::Json {
            let line = (0..app.tabs[0].buffer.line_count())
                .find(|&i| app.tabs[0].buffer.line(i).contains("\"name\""))
                .expect("the fixture has a name");
            let row = app.tabs[0].buffer.line(line);
            let col = row.find("item-").expect("the name is a string") + 5;
            app.tabs[0].editor.caret = crate::core::buffer::Pos { line, col };
        }
        let before = app.plan_patches();
        app.on_event(crate::ui::event::Event::Text("x".into()), &mut host);
        let t = Instant::now();
        Handler::paint(&mut app, &mut canvas);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(
            app.plan_patches() > before,
            "{}: a keystroke rebuilt the plan instead of patching it",
            kind.name()
        );
        assert!(
            !app.preview_job_pending(),
            "{}: a keystroke sent the document to the worker instead of patching it",
            kind.name()
        );
        // And a keystroke that leaves the text unparseable is still a keystroke
        // the pane owes an answer to: one error row where the text is, not a
        // re-parse of the document. A dangling comma in a small nested array is
        // the shape of it — the node it breaks is a few rows, the tree around
        // it keeps every measurement it had.
        if kind == Kind::Json {
            let line = (0..app.tabs[0].buffer.line_count())
                .find(|&i| app.tabs[0].buffer.line(i).contains("\"tags\": ["))
                .expect("the fixture has a nested array");
            let col = app.tabs[0].buffer.line(line).find('[').unwrap() + 1;
            app.tabs[0].editor.caret = crate::core::buffer::Pos { line, col };
            let before = app.plan_patches();
            app.on_event(crate::ui::event::Event::Text(",".into()), &mut host);
            Handler::paint(&mut app, &mut canvas);
            assert!(
                app.plan_patches() > before,
                "{}: an unparseable keystroke re-planned the document",
                kind.name()
            );
            assert!(
                !app.preview_job_pending(),
                "{}: an unparseable keystroke went to the worker",
                kind.name()
            );
        }

        // G4 · Type at both ends: the same keystroke again, with the caret
        // moved to the LAST line. Costs that grow with the caret position —
        // `same_lines`' from-zero line walk in `sync_dirty`, the JSON patch's
        // slot move to the caret's node — are invisible at the top position
        // this function used to measure alone. JSON mirrors the top rule:
        // the caret goes inside a string (the last record's `"name"`), or a
        // dangling duplicate would anchor the number to a re-parse instead.
        let last = app.tabs[0].buffer.line_count() - 1;
        if kind == Kind::Json {
            // Inside a string on the last record — the same rule the top
            // position follows, applied from the other end.
            let line = (0..=last)
                .rev()
                .find(|&i| app.tabs[0].buffer.line(i).contains("\"name\""))
                .expect("the fixture has a name");
            let col = app.tabs[0]
                .buffer
                .line(line)
                .find("item-")
                .expect("the name is a string")
                + 5;
            app.tabs[0].editor.caret = crate::core::buffer::Pos { line, col };
        } else {
            app.tabs[0].editor.caret = crate::core::buffer::Pos { line: last, col: 0 };
        }
        let before = app.plan_patches();
        app.on_event(crate::ui::event::Event::Text("x".into()), &mut host);
        let t = Instant::now();
        Handler::paint(&mut app, &mut canvas);
        let end_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(
            app.plan_patches() > before,
            "{}: the end keystroke rebuilt the plan instead of patching it",
            kind.name()
        );
        assert!(
            !app.preview_job_pending(),
            "{}: the end keystroke sent the document to the worker",
            kind.name()
        );
        Some((ms, end_ms))
    } else {
        None
    };

    let _ = std::fs::remove_file(&path);
    Cost {
        load_ms,
        first_frame_ms,
        readable_ms: readable.0,
        frame_ms,
        churn_mb,
        live_mb,
        ops,
        typing_ms: typing_ms.map(|(top, _)| top),
        typing_end_ms: typing_ms.map(|(_, end)| end),
    }
}

/// A canonical digest of one display list, for the G2 equality check: byte
/// strings differ across the worker boundary, but a Debug rendering of each
/// op (text, position, style, color) is deterministic at a fixed font,
/// scale and zoom — and a band is a screenful, so this stays cheap.
fn pane_fingerprint(list: &crate::layout::display_list::DisplayList) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(64 * 1024);
    for op in &list.ops {
        let _ = write!(s, "{op:?}|",);
    }
    s
}

/// `PERF_REPORT_ONLY=1` prints the table without failing — for calibration,
/// or when investigating a number and pass/fail is in the way. CI sets
/// nothing, so the budgets are enforced there.
fn check(kind: Kind) {
    let cost = measure(kind);
    let b = kind.budget();
    let over = [
        ("load", cost.load_ms, b.load_ms, "ms"),
        ("first frame", cost.first_frame_ms, b.first_frame_ms, "ms"),
        ("frame", cost.frame_ms, b.frame_ms, "ms"),
        ("churn", cost.churn_mb, b.churn_mb, "MB/frame"),
        ("live", cost.live_mb, b.live_mb, "MB"),
        ("pane ops", cost.ops as f64, b.pane_ops as f64, "ops"),
    ];
    let typing = match cost.typing_ms {
        Some(ms) => format!("{ms:6.1} ms"),
        None => "     — ".to_string(),
    };
    let type_end = match cost.typing_end_ms {
        Some(ms) => format!("{ms:6.1} ms"),
        None => "     — ".to_string(),
    };
    println!(
        "{:<9} load {:8.1} ms | first {:7.1} ms | read {:7.1} ms | frame {:6.2} ms | churn {:5.2} MB | live {:6.1} MB | type {typing} | end {type_end} | pane {:6} ops",
        kind.name(),
        cost.load_ms,
        cost.first_frame_ms,
        cost.readable_ms,
        cost.frame_ms,
        cost.churn_mb,
        cost.live_mb,
        cost.ops,
    );
    if std::env::var_os("PERF_REPORT_ONLY").is_some() {
        return;
    }
    let exceeded: Vec<String> = over
        .iter()
        .filter(|(_, got, budget, _)| got > budget)
        .map(|(what, got, budget, unit)| {
            format!(
                "{what} {got:.1} {unit} > {budget:.1} {unit} ({:.1}x budget)",
                got / budget
            )
        })
        .collect();
    assert!(
        exceeded.is_empty(),
        "{} is over its cost budget:\n  {}\n\
         (a number an order of magnitude over is a per-frame/per-line regression, \
         not laptop noise: `cargo test --release -- --ignored perf --nocapture` \
         prints the neighbours for comparison)",
        kind.name(),
        exceeded.join("\n  ")
    );
}

#[test]
#[ignore = "measures wall-clock; run in release for a meaningful number"]
fn perf_json() {
    check(Kind::Json);
}

#[test]
#[ignore = "measures wall-clock; run in release for a meaningful number"]
fn perf_jsonl() {
    check(Kind::Jsonl);
}

#[test]
#[ignore = "measures wall-clock; run in release for a meaningful number"]
fn perf_markdown() {
    check(Kind::Markdown);
}

#[test]
#[ignore = "measures wall-clock; run in release for a meaningful number"]
fn perf_log() {
    check(Kind::Log);
}

#[test]
#[ignore = "measures wall-clock; run in release for a meaningful number"]
fn perf_csv() {
    check(Kind::Csv);
}

#[test]
#[ignore = "measures wall-clock; run in release for a meaningful number"]
fn perf_code() {
    check(Kind::Code);
}

/// The generators themselves: valid input, at the size the gate claims, and
/// the same bytes on every run. Cheap, so it runs in the normal suite — a
/// broken generator would otherwise only show up as a mysterious budget
/// failure in the release job.
#[test]
#[ignore = "diagnostic breakdown; run in release with --nocapture"]
fn pipeline_breakdown() {
    use crate::layout::{self, theme::Theme, Ctx};
    use crate::ui::preview_view::PreviewView;

    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let fonts = crate::gfx::font::FontSystem::load_embedded();
    let theme = Theme::light();
    let ctx = Ctx {
        fonts: &fonts,
        theme: &theme,
        scale: 1.0,
        zoom: 1.0,
    };

    let json = synthetic(Kind::Json, TARGET_BYTES);
    let t = Instant::now();
    let value = crate::parse::json::parser::parse(&json).expect("json");
    println!(
        "json parse        {:8.1} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
    let t = Instant::now();
    let list = layout::json_tree::layout_value(&value, &Default::default(), 1440.0, &ctx);
    println!(
        "json tree layout  {:8.1} ms ({} ops)",
        t.elapsed().as_secs_f64() * 1000.0,
        list.ops.len()
    );
    let t = Instant::now();
    let rows = PreviewView { list }.rows(&fonts);
    println!(
        "json preview rows {:8.1} ms ({} rows)",
        t.elapsed().as_secs_f64() * 1000.0,
        rows.len()
    );
    // The virtualized path a big JSON actually takes: a per-node plan and then
    // a band per frame, instead of the whole-document list above.
    let t = Instant::now();
    let plan = layout::json_tree::build_value_plan(&value, &Default::default(), 1440.0, &ctx);
    println!(
        "json plan         {:8.1} ms ({} slots)",
        t.elapsed().as_secs_f64() * 1000.0,
        plan.slots.len()
    );
    let t = Instant::now();
    let band = layout::json_tree::layout_value_band(layout::json_tree::ValueBand {
        value: &value,
        plan: &plan,
        collapsed: &Default::default(),
        width: 1440.0,
        ctx: &ctx,
        top: plan.height * 0.5,
        bottom: plan.height * 0.5 + 900.0,
    });
    println!(
        "json band         {:8.1} ms ({} ops)",
        t.elapsed().as_secs_f64() * 1000.0,
        band.ops.len()
    );

    let md = synthetic(Kind::Markdown, TARGET_BYTES);
    let t = Instant::now();
    let doc = crate::parse::markdown::parse(&md);
    println!(
        "md parse          {:8.1} ms ({} blocks)",
        t.elapsed().as_secs_f64() * 1000.0,
        doc.blocks.len()
    );
    let t = Instant::now();
    let list = layout::markdown::layout(&doc, 1440.0, &ctx);
    println!(
        "md layout         {:8.1} ms ({} ops)",
        t.elapsed().as_secs_f64() * 1000.0,
        list.ops.len()
    );
    let t = Instant::now();
    let rows = PreviewView { list }.rows(&fonts);
    println!(
        "md preview rows   {:8.1} ms ({} rows)",
        t.elapsed().as_secs_f64() * 1000.0,
        rows.len()
    );
    let t = Instant::now();
    let plan = layout::markdown::plan(&doc, 1440.0, &ctx);
    println!(
        "md plan           {:8.1} ms ({} blocks)",
        t.elapsed().as_secs_f64() * 1000.0,
        doc.blocks.len()
    );
    let t = Instant::now();
    let band = layout::markdown::layout_band(
        &doc,
        &plan,
        1440.0,
        &ctx,
        plan.rows.height() * 0.5,
        plan.rows.height() * 0.5 + 900.0,
    );
    println!(
        "md band           {:8.1} ms ({} ops)",
        t.elapsed().as_secs_f64() * 1000.0,
        band.ops.len()
    );

    let csv = synthetic(Kind::Csv, TARGET_BYTES);
    let t = Instant::now();
    let sheet = crate::parse::csv::parse_with_ext(&csv, None);
    println!(
        "csv parse         {:8.1} ms ({} rows kept)",
        t.elapsed().as_secs_f64() * 1000.0,
        sheet.rows.len()
    );
    let t = Instant::now();
    let list = layout::csv::layout(&sheet, 1440.0, &ctx);
    println!(
        "csv layout        {:8.1} ms ({} ops)",
        t.elapsed().as_secs_f64() * 1000.0,
        list.ops.len()
    );

    // G3 · The editor pane's builders — the costs §3 ties most typing
    // work to. `layout_plain` is the no-wrap walk; T1's bands replaced the
    // whole-document installs, so this probe is the before/after reference
    // (`editor_band` + `widest_width` should sum well under `layout_plain`'s
    // here). Both run over the code fixture at the gate window's split
    // width (1440 × 1.0: right half = 720, per `split_panes`, whose right
    // pane subtracts no divider).
    let code_fixture = synthetic(Kind::Code, TARGET_BYTES);
    let mut buf = crate::core::buffer::Buffer::from_text(&code_fixture);
    let edit_w = 1440.0 * 0.5;
    let t = Instant::now();
    let elist = layout::text::layout_plain(&buf, edit_w, &ctx, false);
    println!(
        "editor install    {:8.1} ms ({} ops) — install_preview's builder",
        t.elapsed().as_secs_f64() * 1000.0,
        elist.ops.len()
    );
    let t = Instant::now();
    let (band_list, widest) = layout::text::layout_plain_edit(&buf, edit_w, &ctx);
    println!(
        "editor band+widest{:8.1} ms ({} ops) — relayout_band's builder, widest {:5.1} px",
        t.elapsed().as_secs_f64() * 1000.0,
        band_list.ops.len(),
        widest
    );

    // T5 · The find bar's costs on the same 8 MB text. `search` is the
    // full rescan the old path ran per key with the bar open; `Find::patch`
    // is what a keystroke takes now — the edited line's run plus the
    // list shift, not the document.
    let mut find = crate::ui::find::Find::default();
    find.query = "pub fn".into();
    find.rescan(&buf, crate::core::buffer::Pos::default());
    let t = Instant::now();
    let full = crate::ui::find::search(&buf, "pub fn");
    println!(
        "find rescan       {:8.1} ms ({} matches) — full document",
        t.elapsed().as_secs_f64() * 1000.0,
        full.len()
    );
    let last = buf.line_count() - 1;
    buf.insert(crate::core::buffer::Pos { line: last, col: 0 }, "pub fn");
    let edits = buf.take_edits().expect("the edit");
    let t = Instant::now();
    let _ = find.patch(&buf, &edits, crate::core::buffer::Pos::default());
    println!(
        "find edit patch   {:8.1} ms — the edited line only",
        t.elapsed().as_secs_f64() * 1000.0
    );
}

#[test]
fn synthetic_documents_are_valid_and_deterministic() {
    // Small targets here; the gate's own 8 MB build is not a unit test.
    for kind in Kind::all() {
        let small = synthetic(kind, 64 * 1024);
        assert!(small.len() >= 64 * 1024, "{} too small", kind.name());
        assert_eq!(
            small,
            synthetic(kind, 64 * 1024),
            "{} is not deterministic",
            kind.name()
        );
        match kind {
            Kind::Json => {
                crate::parse::json::parser::parse(&small).expect("valid JSON");
                assert!(small.trim_end().ends_with(']'));
            }
            Kind::Jsonl => {
                for line in small.lines() {
                    crate::parse::json::parser::parse(line).expect("valid JSONL line");
                }
            }
            Kind::Csv => {
                let rows = small.lines().count();
                assert!(rows > 100, "{} rows", rows);
                assert!(small.lines().next().unwrap().starts_with("id,name"));
            }
            Kind::Markdown => assert!(small.contains("## Section")),
            Kind::Log => assert!(small.contains("INFO")),
            Kind::Code => assert!(small.contains("pub fn")),
        }
    }
}
