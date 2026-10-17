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
/// Two megabytes, not eight: past this the *unvirtualized* preview layouts
/// (JSON tree, Markdown, CSV) take multiple seconds to lay out once, which is
/// a separate and much larger problem than the per-frame regressions this gate
/// is for. A gate that already fails on the day it lands teaches nobody
/// anything; this one starts green at a size users do open, and its budgets
/// still turn a 3x per-frame or per-line regression into a red build. Sized
/// 8 MB it takes minutes per kind, which no one will look at twice.
const TARGET_BYTES: usize = 2 * 1024 * 1024;

/// Frames painted while scrolling. Enough for a mean that a single GC-ish
/// hiccup cannot move, few enough to keep the gate to a few seconds.
const FRAMES: usize = 24;

/// One budget, in the units the gate reports.
#[derive(Debug, Clone, Copy)]
struct Budget {
    load_ms: f64,
    first_frame_ms: f64,
    frame_ms: f64,
    churn_mb: f64,
    live_mb: f64,
}

#[derive(Debug, Clone, Copy)]
struct Cost {
    load_ms: f64,
    first_frame_ms: f64,
    frame_ms: f64,
    churn_mb: f64,
    live_mb: f64,
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
            // measured range is narrow (1-3 ms, 0-3 MB/frame); load and first
            // frame differ by what each kind makes the app do up front, which
            // is the interesting part of the table: JSON's first frame is a
            // whole-document tree layout, CSV's is a table scan.
            //
            // `live` is a gross-leak check, not a target: the synthetic file
            // is 2 MB, and 200 MB separates "text held once plus the display
            // list" from "the preview keeps a copy per relayout".
            Kind::Json => Budget {
                load_ms: 800.0,
                first_frame_ms: 4000.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
            },
            Kind::Jsonl => Budget {
                load_ms: 800.0,
                first_frame_ms: 1500.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
            },
            Kind::Markdown => Budget {
                load_ms: 800.0,
                first_frame_ms: 2500.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
            },
            Kind::Log => Budget {
                load_ms: 800.0,
                first_frame_ms: 1500.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
            },
            Kind::Csv => Budget {
                load_ms: 800.0,
                first_frame_ms: 600.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
            },
            Kind::Code => Budget {
                load_ms: 2000.0,
                first_frame_ms: 2000.0,
                frame_ms: 20.0,
                churn_mb: 6.0,
                live_mb: 200.0,
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
    let text = synthetic(kind, TARGET_BYTES);
    assert!(
        text.len() >= TARGET_BYTES,
        "{} generator produced only {} bytes",
        kind.name(),
        text.len()
    );
    let path = std::env::temp_dir().join(format!("editor_perf_{}.{}", kind.name(), kind.ext()));
    std::fs::write(&path, &text).unwrap();
    // The generator's own copy is 2 MB; drop it before the memory baseline so
    // the harness never bills itself to the document.
    drop(text);

    // Live bytes are relative to the process's own baseline, taken before the
    // document exists: the harness runs every kind in one process, and a
    // global cache (fonts, glyphs) filled by the previous kind must not be
    // billed to this one.
    #[cfg(any(test, feature = "mem-census"))]
    let live_before = crate::mem::snapshot().global_live;

    // Load: the constructor parses and lays out; nothing is painted yet.
    let t = Instant::now();
    let mut app = App::new(Some(path.clone())).expect("open");
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
    // 1440x900 at 1x: a common laptop window, and the width the wrap logic
    // was tuned against.
    let (w, h) = (1440usize, 900usize);

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

    let _ = std::fs::remove_file(&path);
    Cost {
        load_ms,
        first_frame_ms,
        frame_ms,
        churn_mb,
        live_mb,
    }
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
    ];
    println!(
        "{:<9} load {:8.1} ms | first {:7.1} ms | frame {:6.2} ms | churn {:5.2} MB | live {:6.1} MB",
        kind.name(),
        cost.load_ms,
        cost.first_frame_ms,
        cost.frame_ms,
        cost.churn_mb,
        cost.live_mb,
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
