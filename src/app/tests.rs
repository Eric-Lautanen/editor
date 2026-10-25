use super::*;

fn open(name: &str, content: &str) -> App {
    let dir = crate::testing::temp_dir().join(name);
    std::fs::write(&dir, content).unwrap();
    App::new(Some(dir)).unwrap()
}

/// Paint the active panes at `scroll`, the way the platform does: a tick,
/// then a frame. `render_headless` cannot stand in here — it reports a
/// resize every call, which marks the layout dirty and would rebuild the
/// band index between the two frames being compared.
fn paint_at(app: &mut App, scroll: f32) -> Canvas {
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.set_scroll(scroll);
    app.on_event(Event::Tick, &mut host);
    Handler::paint(app, &mut canvas);
    canvas
}

/// The band index is paint culling, not a second painter — and that has to
/// hold for *every kind the app opens*, not just the tree. Each kind gets
/// a document big enough to carry an index; the reader and editor panes
/// are each painted twice, with and without it, and must match pixel for
/// pixel.
#[test]
fn every_kind_paints_the_same_with_and_without_the_band_index() {
    use crate::core::file_kind::FileKind;

    // One seed per kind; JSON is built as a single array below so it stays
    // valid however long it gets. Repeating a seed keeps the syntax real
    // (a repeated header row is harmless).
    let seeds: [(FileKind, &str, &str); 9] = [
            (
                FileKind::Markdown,
                "md",
                "# Heading\n\nSome **bold** text with `code` and a [link](https://example.com).\n\n- one\n- two\n\n",
            ),
            (
                FileKind::JsonLines,
                "jsonl",
                "{\"a\": 1, \"b\": [true, null], \"c\": \"text\"}\n",
            ),
            (
                FileKind::Csv,
                "csv",
                "id,name,score,note\n1,alpha,10,\"a, b\"\n2,beta,-20,gamma\n",
            ),
            (
                FileKind::Csv,
                "tsv",
                "id\tname\tscore\n1\talpha\t10\n2\tbeta\t20\n",
            ),
            (
                FileKind::Yaml,
                "yaml",
                "server:\n  port: 8080\n  hosts:\n    - alpha\n    - beta\n",
            ),
            (
                FileKind::Toml,
                "toml",
                "[server]\nport = 8080\nname = \"web\"\n",
            ),
            (
                FileKind::Ini,
                "ini",
                "[server]\nport=8080\n; note\nname=web\n",
            ),
            (
                FileKind::Log,
                "log",
                "2026-10-17T12:00:00.000Z INFO api [worker-0] request ok id=7\n",
            ),
            (
                FileKind::Code,
                "rs",
                "pub fn parse(input: &str) -> Option<String> {\n    // note\n    Some(input.to_string())\n}\n\n",
            ),
        ];

    let target = 128 * 1024;
    let mut files: Vec<(FileKind, &str, String)> = seeds
        .iter()
        .map(|(kind, ext, seed)| (*kind, *ext, seed.repeat(target / seed.len() + 1)))
        .collect();
    let mut json = String::from("[\n");
    while json.len() < target {
        json.push_str("  {\"id\": 1, \"name\": \"row\", \"tags\": [\"a\"], \"ok\": true},\n");
    }
    json.push_str("  {\"id\": -1}\n]\n");
    files.push((FileKind::Json, "json", json));
    files.push((
        FileKind::PlainText,
        "txt",
        "plain text line with words in it\n".repeat(4000),
    ));

    for (kind, ext, text) in files {
        let path = crate::testing::temp_dir().join(format!("editor_band_kind.{ext}"));
        std::fs::write(&path, &text).unwrap();
        let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
        assert_eq!(app.tabs[0].kind, kind, "{ext} opened as the wrong kind");
        let mut saw_index = false;
        for mode in [Mode::Preview, Mode::Edit] {
            app.set_mode(mode);
            let _ = paint_at(&mut app, 0.0);
            let end = app.tabs[0].scroll.content_height.max(0.0);
            for scroll in [0.0, end * 0.9] {
                // `set_mode` marks the layout dirty, so this frame rebuilds
                // the lists with their index.
                app.set_mode(mode);
                let indexed = paint_at(&mut app, scroll);
                assert!(
                    indexed.pixels.iter().any(|&p| p != 0),
                    "{ext} {mode:?} painted nothing"
                );
                saw_index |= app.preview.list.bands.is_some()
                    || app.tabs[0].editor_pane.list.bands.is_some();
                app.preview.list.bands = None;
                for tab in app.tabs.iter_mut() {
                    tab.editor_pane.list.bands = None;
                }
                let linear = paint_at(&mut app, scroll);
                assert_eq!(
                    indexed.pixels, linear.pixels,
                    "{ext} {mode:?} at {scroll}: the band index changed the frame"
                );
            }
        }
        // A virtualized reader pane — a big JSONL's records, a planned
        // JSON tree — paints a band per frame, so its op list is empty and
        // it has no index to exercise. The JSON document here is sized to
        // stay under the plan's threshold, so JSON must have taken the
        // whole-layout path (and carried the index) like every other kind;
        // `a_big_json_paints_a_band_and_toggles_inside_it` is what covers
        // the planned one.
        if kind == FileKind::Json {
            assert!(
                app.tabs[0].json_doc.is_none(),
                "{ext}: this document grew past the JSON plan threshold"
            );
        }
        // S1 · CSV joins JSONL in the planned class: the sheet paints bands
        // from a plan, so it carries no band index either.
        if kind != FileKind::JsonLines && kind != FileKind::Csv {
            assert!(saw_index, "{ext}: no pane carried a band index");
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// A JSON document of `rows` array entries: over the plan's threshold at
/// 3 600, and still small enough to build inside a test.
fn big_json(rows: usize) -> String {
    let mut text = String::from("{\"rows\": [\n");
    for i in 0..rows {
        text.push_str(&format!(
            "  {{\"id\": {i}, \"name\": \"row-{i}\", \"tags\": [\"a\", \"b\"], \
                 \"ok\": {}, \"meta\": {{\"note\": \"n\"}}}}{}\n",
            i % 2 == 0,
            if i + 1 == rows { "" } else { "," },
        ));
    }
    text.push_str("]}\n");
    text
}

/// A window's path for a big JSON: the pane shows the document laid out
/// plain while the tree is parsed and planned on the worker, then paints
/// bands of it. The inline path is covered by the test below; this is the
/// one a user's real window takes.
#[test]
fn a_big_json_plans_on_the_worker_and_then_paints_bands() {
    // Over `PREVIEW_ASYNC_LINES` lines, which is what puts a pane on the
    // worker — a minified document with as many nodes and one line does
    // not go there at all — and under the 1 MB the load streamer splits
    // at, so the open is whole.
    let path = crate::testing::temp_dir().join("editor_band_worker.json");
    let mut text = String::from("[\n");
    for i in 0..30_000 {
        text.push_str(&format!(
            "{{\"i\": {i}}}{}\n",
            if i == 29_999 { "" } else { "," }
        ));
    }
    text.push_str("]\n");
    std::fs::write(&path, &text).unwrap();
    // `headless = false`: the async switch the worker path is behind, and
    // the one `render_headless` runs without.
    let mut app = App::new_streaming(Some(path.clone()), false).expect("open");
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert!(
        app.preview_job_pending(),
        "a 60k-node tree must be planned on the worker"
    );
    // The frame before it lands is the document as tinted text, not a
    // blank pane: the wait is for the tree, not for something to read.
    let bg = app.theme.bg.pack();
    assert!(canvas.pixels.iter().any(|&p| p != bg), "blank first frame");
    assert!(
        !app.preview.list.ops.is_empty(),
        "the worker's frame must paint the document"
    );
    // Pump the heartbeat (a tick polls the worker) until the plan lands.
    // No resize is sent again: `render_headless` would mark the layout
    // dirty and start a fresh job every frame.
    let mut guard = 0;
    while app.preview_job_pending() {
        app.on_event(Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(guard < 4000, "the worker never planned the tree");
    }
    let doc = &app.tabs[0]
        .json_doc
        .as_ref()
        .expect("the worker's tree is kept")
        .1;
    assert!(doc.plan.is_virtual());
    // The frame that installed it paints bands of it.
    assert!(
        app.preview.list.ops.len() < 1_000,
        "the landed pane kept {} ops",
        app.preview.list.ops.len()
    );
    assert!(canvas.pixels.iter().any(|&p| p != bg), "blank banded frame");
    let _ = std::fs::remove_file(&path);
}

/// A JSON document too big to lay out whole is planned per node and painted
/// as a band: the pane holds ops for one screenful instead of one per row of
/// the document, scrolling paints the rows of the offset it moved to, and a
/// toggle inside the band collapses the node the full layout would have.
#[test]
fn a_big_json_paints_a_band_and_toggles_inside_it() {
    use crate::core::file_kind::FileKind;

    // ~21k nodes, so over the plan's threshold and still quick to parse.
    let path = crate::testing::temp_dir().join("editor_band_big.json");
    std::fs::write(&path, big_json(3_600)).unwrap();

    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::Json);
    let canvas = app.render_headless(1100, 800, 1.0);
    {
        let doc = &app.tabs[0]
            .json_doc
            .as_ref()
            .expect("the plan needs its tree")
            .1;
        assert!(
            doc.plan.is_virtual(),
            "a {} node document should be planned",
            doc.plan.slots.len()
        );
    }
    let bg = app.theme.bg.pack();
    assert!(
        canvas.pixels.iter().any(|&p| p != bg),
        "the planned pane painted nothing"
    );
    // A screenful, not a document: the whole tree is thousands of rows.
    assert!(
        app.preview.list.ops.len() < 1_000,
        "the pane kept {} ops for a 21k-node tree",
        app.preview.list.ops.len()
    );
    // The band is the rows at the offset: the top of the document, then
    // somewhere past the thousandth row.
    let rows_at = |list: &DisplayList| -> Vec<usize> {
        list.ops
            .iter()
            .filter_map(|op| match op {
                DrawOp::Text { text, .. } => text
                    .strip_prefix("\"row-")
                    .and_then(|t| t.trim_end_matches('"').parse::<usize>().ok()),
                _ => None,
            })
            .collect()
    };
    let top = rows_at(&app.preview.list);
    assert!(!top.is_empty(), "no rows painted at the top");
    assert!(*top.iter().max().unwrap() < 200, "top band is not the top");
    let height = app.tabs[0].scroll.content_height;
    let mid = paint_at(&mut app, height * 0.5);
    assert!(mid.pixels.iter().any(|&p| p != bg), "nothing at the middle");
    let seen = rows_at(&app.preview.list);
    assert!(!seen.is_empty(), "no rows painted at the middle");
    assert!(
        *seen.iter().min().unwrap() > 1_000,
        "the middle of the document painted rows {:?}",
        seen.iter().min()
    );
    assert!(
        app.preview.list.ops.len() < 1_000,
        "the scrolled band kept {} ops",
        app.preview.list.ops.len()
    );

    // A click on a toggle in the band collapses that node, and the pane
    // shortens by the subtree it hides.
    let _ = paint_at(&mut app, 0.0);
    let r = app
        .preview
        .list
        .hits
        .iter()
        .find(|h| {
            matches!(
                h.action,
                crate::layout::display_list::HitAction::ToggleNode(_)
            )
        })
        .map(|h| h.rect)
        .expect("a band must carry toggles");
    let open_h = app.preview.list.height;
    let mut host = NullHost::default();
    app.on_event(
        Event::MouseDown {
            x: r.x + 1.0,
            y: r.y + 1.0 + app.viewport().y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.collapsed.len(), 1, "the click collapsed nothing");
    let mut c = Canvas::new(1100, 800);
    app.paint(&mut c);
    assert!(
        app.preview.list.height < open_h,
        "collapsing a planned node did not shrink the tree"
    );
    assert!(
        c.pixels.iter().any(|&p| p != bg),
        "collapsed pane painted nothing"
    );
    let _ = std::fs::remove_file(&path);
}

/// One file, two spellings, one tab. Duplicate tabs over a single file are
/// not cosmetic: two buffers, two dirty flags, and whichever saves last
/// wins over the other's edits.
#[test]
fn the_same_file_spelled_differently_reuses_its_tab() {
    let dir = crate::testing::temp_dir().join("editor_dupe_spelling");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("dupe.md");
    std::fs::write(&file, "# one\n").unwrap();
    let mut app = App::new(None).unwrap();
    let mut host = NullHost::default();
    app.open_path(file.clone(), &mut host);
    assert_eq!(
        app.tabs.len(),
        1,
        "the placeholder tab should become the file"
    );
    // The same file by a detour through a subdirectory: a different string
    // (a `.` segment is normalised away by `PathBuf::join`, so it has to be
    // `..`) naming one file.
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let detour = dir.join("sub").join("..").join("dupe.md");
    assert_ne!(detour, file, "the test needs two distinct spellings");
    app.open_path(detour, &mut host);
    assert_eq!(app.tabs.len(), 1, "a second tab was opened over the first");
    assert_eq!(app.path.as_deref(), Some(file.as_path()));
    let _ = std::fs::remove_file(&file);
}

fn ctrl(key: char) -> Event {
    Event::KeyDown {
        key: Key::Char(key),
        mods: Modifiers {
            ctrl: true,
            alt: false,
            shift: false,
        },
    }
}

#[test]
fn recent_files_track_opens_and_the_menu_reopens_them() {
    let a = crate::testing::temp_dir().join("editor_test_recent_a.md");
    std::fs::write(&a, "a\n").unwrap();
    let b = crate::testing::temp_dir().join("editor_test_recent_b.md");
    std::fs::write(&b, "b\n").unwrap();
    let mut app = App::new(None).unwrap();
    let mut host = NullHost::default();
    // Opening notes the file; re-opening moves it back to the front.
    app.open_path(b.clone(), &mut host);
    assert_eq!(app.settings().recent, vec![b.clone()]);
    app.open_path(a.clone(), &mut host);
    assert_eq!(app.settings().recent, vec![a.clone(), b.clone()]);
    // The context menu lists them under a caption that does not click.
    let menu = menu::open(
        &app.fonts,
        10.0,
        10.0,
        false,
        &app.recent,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        1.0,
    );
    assert_eq!(menu.items[6].0, menu::Action::Header);
    assert_eq!(menu.items[7].0, menu::Action::Recent(0));
    assert_eq!(menu.items[7].1, "editor_test_recent_a.md");
    // Picking the second recent focuses that file's tab…
    app.run_menu_action(menu::Action::Recent(1), &mut host);
    assert_eq!(app.path.as_deref(), Some(b.as_path()));
    // …and bumps it back to the front of the list.
    assert_eq!(app.settings().recent, vec![b.clone(), a.clone()]);
}

#[test]
fn oversized_files_are_refused_with_a_message() {
    // The rule itself: anything at or under the limit opens, anything
    // over it names the size and the limit.
    assert_eq!(size_error(0), None);
    assert_eq!(size_error(MAX_OPEN_BYTES), None);
    let msg = size_error(MAX_OPEN_BYTES + 1).expect("over the limit refuses");
    assert!(msg.contains("64 MB"), "names the limit: {msg}");
    assert!(msg.contains("larger than"), "says which way: {msg}");
}

/// `Event::KeyDown` for a plain (unmodified) key.
fn plain(key: Key) -> Event {
    Event::KeyDown {
        key,
        mods: Modifiers::default(),
    }
}

/// `Event::KeyDown` for a key with only Shift held.
fn shifted(key: Key) -> Event {
    Event::KeyDown {
        key,
        mods: Modifiers {
            ctrl: false,
            alt: false,
            shift: true,
        },
    }
}

/// Everything a keymap probe might plausibly change. Text, caret and tab
/// count included: a probe that lands in the wrong branch but still edits
/// the buffer is a real binding, and should pass rather than be reported
/// as dead.
#[derive(Debug, PartialEq)]
struct KeymapSnapshot {
    mode: Mode,
    dark: bool,
    help: bool,
    about: bool,
    find: bool,
    zoom: u32,
    active: usize,
    tabs: usize,
    text: String,
    caret: (usize, usize),
    selection: bool,
    status: String,
    offset: u32,
    quit: bool,
    clipboard: Option<String>,
    opened_url: Option<String>,
}

fn keymap_snapshot(app: &App, host: &NullHost) -> KeymapSnapshot {
    KeymapSnapshot {
        mode: app.mode,
        dark: app.dark,
        help: app.help_open,
        about: app.about_open,
        find: app.find.is_some(),
        zoom: app.zoom.to_bits(),
        active: app.active,
        tabs: app.tabs.len(),
        text: app.buffer.text(),
        caret: (app.editor.caret.line, app.editor.caret.col),
        selection: app.editor.selection().is_some(),
        status: app.status_extra.clone(),
        offset: app.scroll.offset.to_bits(),
        quit: host.quit_requested,
        clipboard: host.clipboard.clone(),
        opened_url: host.opened_url.clone(),
    }
}

/// A window that can observe the most chords at once: two tabs (so the tab
/// chords move), Edit mode (so the editing chords act), motion reduction
/// on (so a scroll chord lands on the frame it is fired in), and a file
/// tall enough for the view to have somewhere to scroll to.
fn keymap_window(name: &str) -> (App, NullHost) {
    let dir = crate::testing::temp_dir();
    let a = dir.join(format!("editor_keymap_{name}_a.txt"));
    let b = dir.join(format!("editor_keymap_{name}_b.txt"));
    let body = (0..80).map(|i| format!("line {i}\n")).collect::<String>();
    std::fs::write(&a, &body).unwrap();
    std::fs::write(&b, &body).unwrap();
    let mut app = App::new(Some(a)).unwrap();
    let mut host = NullHost::default();
    app.open_path(b, &mut host);
    app.set_mode(Mode::Edit);
    app.set_reduce_motion(true);
    (app, host)
}

/// Every row the keymap marks with a probe must do something to a fresh
/// window. This is the guard against the legend and `on_key` drifting
/// apart — the failure mode this table was introduced to end.
#[test]
fn every_probed_keymap_chord_changes_something() {
    let mut probed = 0;
    for section in crate::ui::keymap::SECTIONS {
        for item in section.items {
            let Some((key, mods)) = item.probe else {
                continue;
            };
            probed += 1;
            let (mut app, mut host) = keymap_window("probe");
            let before = keymap_snapshot(&app, &host);
            app.on_event(Event::KeyDown { key, mods }, &mut host);
            let after = keymap_snapshot(&app, &host);
            assert_ne!(
                before, after,
                "\"{}\" ({}) does nothing: the legend and the dispatcher have drifted",
                item.keys, section.title
            );
        }
    }
    // A table that lost its probes would pass the loop above vacuously.
    assert!(probed >= 12, "only {probed} probed chords");
}

#[test]
fn find_bar_opens_types_and_selects_the_first_match() {
    let mut app = App::new(None).unwrap();
    app.buffer = Buffer::from_text("one two\nthree one\none");
    let mut host = NullHost::default();
    app.on_event(ctrl('f'), &mut host);
    assert!(app.find.is_some(), "Ctrl+F opens the bar");
    // Typing goes to the query, never the buffer.
    app.on_event(Event::Text("one".into()), &mut host);
    assert_eq!(app.buffer.text(), "one two\nthree one\none");
    assert!(!app.buffer.dirty);
    let f = app.find.as_ref().unwrap();
    assert_eq!(f.matches.len(), 3);
    assert_eq!(f.status(), "1/3");
    // The current match is the editor selection.
    assert_eq!(app.editor.selection(), Some(f.matches[0]));
    // Ctrl+F again keeps the query; Escape closes.
    app.on_event(ctrl('f'), &mut host);
    assert_eq!(app.find.as_ref().unwrap().query, "one");
    app.on_event(plain(Key::Escape), &mut host);
    assert!(app.find.is_none());
}

#[test]
fn find_bar_enter_and_f3_step_with_wrap() {
    let mut app = App::new(None).unwrap();
    app.buffer = Buffer::from_text("a a\nb\na");
    let mut host = NullHost::default();
    app.on_event(ctrl('f'), &mut host);
    app.on_event(Event::Text("a".into()), &mut host);
    app.on_event(plain(Key::Enter), &mut host);
    assert_eq!(app.find.as_ref().unwrap().status(), "2/3");
    app.on_event(plain(Key::Enter), &mut host);
    app.on_event(plain(Key::Enter), &mut host);
    assert_eq!(app.find.as_ref().unwrap().status(), "1/3", "wraps forward");
    app.on_event(shifted(Key::Enter), &mut host);
    assert_eq!(app.find.as_ref().unwrap().status(), "3/3", "wraps backward");
    app.on_event(plain(Key::F(3)), &mut host);
    assert_eq!(app.find.as_ref().unwrap().status(), "1/3", "F3 steps too");
}

#[test]
fn find_bar_reports_zero_and_backspace_edits_the_query() {
    let mut app = App::new(None).unwrap();
    app.buffer = Buffer::from_text("one two");
    let mut host = NullHost::default();
    app.on_event(ctrl('f'), &mut host);
    app.on_event(Event::Text("onex".into()), &mut host);
    assert_eq!(app.find.as_ref().unwrap().status(), "0 matches");
    app.on_event(plain(Key::Backspace), &mut host);
    let f = app.find.as_ref().unwrap();
    assert_eq!(f.query, "one");
    assert_eq!(f.status(), "1/1");
    assert_eq!(app.buffer.text(), "one two", "buffer untouched");
}

#[test]
fn find_bar_search_is_case_insensitive() {
    let mut app = App::new(None).unwrap();
    app.buffer = Buffer::from_text("Alpha ALPHA alPHA");
    let mut host = NullHost::default();
    app.on_event(ctrl('f'), &mut host);
    app.on_event(Event::Text("alpha".into()), &mut host);
    assert_eq!(app.find.as_ref().unwrap().matches.len(), 3);
}

#[test]
fn find_bar_in_preview_counts_and_jumps_to_the_row() {
    let text = format!("{}the needle\n", "filler row of text\n".repeat(80));
    let mut app = open("editor_test_findprev.txt", &text);
    let _ = app.render_headless(400, 300, 1.0);
    let mut host = NullHost::default();
    app.on_event(ctrl('f'), &mut host);
    app.on_event(Event::Text("needle".into()), &mut host);
    let f = app.find.as_ref().unwrap();
    assert_eq!(f.matches.len(), 1);
    assert_eq!(f.status(), "1/1");
    assert!(
        app.scroll.offset > 0.0,
        "preview should scroll to the match row, offset {}",
        app.scroll.offset
    );
    // Escape closes the bar there too.
    app.on_event(plain(Key::Escape), &mut host);
    assert!(app.find.is_none());
}

/// The middle of a rect, for mouse-event tests.
fn mid(r: Rect) -> (f32, f32) {
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

#[test]
fn typing_in_the_find_bar_keeps_the_current_match_and_view_still() {
    // The runtime probe caught the view jumping while a query was typed
    // quickly: `Find::land` compared match STARTS against the caret,
    // which `find_jump` puts at the current match's END, so every
    // keystroke skipped ahead one match — and once that landed below the
    // fold, `follow_caret` scrolled. One Text event per character is
    // exactly what WM_CHAR delivers.
    let mut lines = vec!["find probe document".to_string()];
    for i in 1..=300u32 {
        if i % 10 == 5 {
            lines.push(format!("alpha marker {i}"));
        } else {
            lines.push(format!("line {i} bravo charlie delta echo"));
        }
    }
    let mut app = open("editor_test_findfast.txt", &lines.join("\n"));
    let _ = app.render_headless(1100, 800, 1.0);
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    let _ = app.render_headless(1100, 800, 1.0);
    app.on_event(ctrl('f'), &mut host);
    for ch in "alpha".chars() {
        app.on_event(Event::Text(ch.to_string()), &mut host);
        assert_eq!(
            app.find.as_ref().unwrap().current,
            0,
            "typing '{ch}' must keep the current match"
        );
    }
    assert_eq!(app.editor.caret.line, 5, "the selection stays on the match");
    assert_eq!(
        app.scroll.offset, 0.0,
        "typing in the find bar must not scroll the editor"
    );
}

#[test]
fn find_button_and_bar_close_button_toggle_the_bar() {
    let mut app = App::new(None).unwrap();
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    let (fx, fy) = mid(app
        .status_buttons
        .iter()
        .find(|b| b.id == 7)
        .expect("Find")
        .rect);
    app.on_event(
        Event::MouseDown {
            x: fx,
            y: fy,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.find.is_some(), "the Find button opens the bar");
    // The × on the bar's right edge closes it.
    let bar = app.find_bar_rect();
    let (cx, cy) = mid(find::close_rect(bar, 1.0));
    app.on_event(
        Event::MouseDown {
            x: cx,
            y: cy,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.find.is_none(), "the × closes the bar");
    // The Find button toggles both ways.
    app.on_event(
        Event::MouseDown {
            x: fx,
            y: fy,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.find.is_some());
    app.on_event(
        Event::MouseDown {
            x: fx,
            y: fy,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.find.is_none());
}

#[test]
fn double_tap_drag_extends_the_selection_like_double_click() {
    let mut app = App::new(None).unwrap();
    app.buffer = Buffer::from_text("hello world\nsecond line here\n");
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    let down = Event::MouseDown {
        x: 100.0,
        y: 60.0,
        button: MouseButton::Left,
    };
    let up = Event::MouseUp {
        x: 100.0,
        y: 60.0,
        button: MouseButton::Left,
    };
    // Tap, tap: the second press is a double-tap and selects the word.
    app.on_event(down.clone(), &mut host);
    app.on_event(up.clone(), &mut host);
    app.on_event(down.clone(), &mut host);
    app.on_event(up.clone(), &mut host);
    assert!(
        !app.editor.sel_text(&app.buffer).is_empty(),
        "double-tap selects a word"
    );
    // The finger drags: Pan extends the selection instead of scrolling.
    let before = app.scroll.offset;
    app.on_event(
        Event::Pan {
            dx: 0.0,
            dy: 120.0,
            x: 0.0,
            y: 0.0,
        },
        &mut host,
    );
    let sel = app.editor.selection().expect("selection survives the drag");
    assert!(sel.1.line >= 1, "drag extended onto a later line");
    assert_eq!(
        app.scroll.offset, before,
        "a selection drag does not scroll"
    );
    // PanEnd retires the gesture without a coast.
    app.on_event(Event::PanEnd { vx: 0.0, vy: -3.0 }, &mut host);
    assert!(app.touch_sel.is_none());
    assert_eq!(app.scroll.offset, before, "no coast after a selection drag");
}

#[test]
fn copy_button_follows_the_data_and_says_copied_after_a_press() {
    let mut app = App::new(None).unwrap();
    app.buffer = Buffer::from_text("hello world\n");
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.copy_btn.is_none(), "no selection, no button");
    app.on_event(ctrl('a'), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
    let cb = app.copy_btn.expect("Copy button with a selection").rect;
    // Not the far left any more: the document's own facts are, and the
    // button sits after them.
    let (info, _) = app.status_bar_text();
    let info_w = app.fonts.measure(FontStyle::Regular, &info, 12.5);
    assert!(
        cb.x > 14.0 + info_w,
        "the button follows the data ({info_w} px of text), at {}",
        cb.x
    );
    assert_eq!(cb.h, statusbar::HEIGHT, "full bar height for a finger");
    assert_eq!(app.copy_btn.unwrap().label, "Copy");
    let (cx, cy) = mid(cb);
    app.on_event(
        Event::MouseDown {
            x: cx,
            y: cy,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(host.clipboard.as_deref(), Some("hello world\n"));
    // The press is acknowledged where the press happened — the button
    // itself reads "Copied" — not as a message in some other corner.
    let _ = app.render_headless(800, 600, 1.0);
    assert_eq!(app.copy_btn.unwrap().label, "Copied");
    assert!(app.status_extra.is_empty(), "status: {}", app.status_extra);
    // Any new press can change the selection, so the flash ends with it —
    // and with the selection gone, the button goes too.
    app.on_event(
        Event::MouseDown {
            x: 200.0,
            y: 150.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.copy_btn.is_none(), "deselecting hides the button");
}

/// A preview click behaves like an editor click: one clears, two take the
/// word under the pointer, three the row it is on.
#[test]
fn preview_clicks_clear_select_a_word_and_select_a_row() {
    let mut app = open(
        "editor_test_preview_clicks.txt",
        "Alpha beta gamma\nsecond line here\nthird line\n",
    );
    let mut host = NullHost::default();
    let _ = app.render_headless(900, 600, 1.0);
    assert_eq!(app.mode, Mode::Preview);
    assert!(app.preview_rows.len() >= 3, "want a row per line");
    let row = app.preview_rows[0].clone();
    let dy = (row.top + row.bottom) / 2.0;
    // Find "beta" the way the app does, by asking it where the word is.
    let mut beta = None;
    let mut x = row.x0;
    while x < row.x1 {
        if let Some(w) = app.preview.word_at(&app.fonts, &app.preview_rows, x, dy) {
            if w.text == "beta" {
                beta = Some(w);
                break;
            }
        }
        x += 2.0;
    }
    let beta = beta.expect("the word beta in the first row");
    let (bx, by) = ((beta.x0 + beta.x1) / 2.0, app.viewport().y + dy);
    fn click(app: &mut App, host: &mut NullHost, x: f32, y: f32) {
        app.on_event(
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
            },
            host,
        );
        app.on_event(
            Event::MouseUp {
                x,
                y,
                button: MouseButton::Left,
            },
            host,
        );
    }
    // Two clicks: the word, not the row.
    click(&mut app, &mut host, bx, by);
    click(&mut app, &mut host, bx, by);
    assert_eq!(
        app.preview_selection_text().as_deref(),
        Some("beta"),
        "double-click takes the word"
    );
    let _ = app.render_headless(900, 600, 1.0);
    let cb = app.copy_btn.expect("Copy button for a word selection");
    assert_eq!(cb.label, "Copy");
    let (cx, cy) = mid(cb.rect);
    app.on_event(
        Event::MouseDown {
            x: cx,
            y: cy,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(
        host.clipboard.as_deref(),
        Some("beta"),
        "copy takes the word"
    );
    let _ = app.render_headless(900, 600, 1.0);
    assert_eq!(app.copy_btn.unwrap().label, "Copied");
    // A third click (same spot, fast): the whole row, and the flash is
    // back to "Copy" because the selection is no longer what was copied.
    click(&mut app, &mut host, bx, by);
    assert!(app.preview_word.is_none());
    assert_eq!(app.preview_sel, Some((0, 0)));
    assert_eq!(
        app.preview_selection_text().as_deref(),
        Some("Alpha beta gamma")
    );
    let _ = app.render_headless(900, 600, 1.0);
    assert_eq!(
        app.copy_btn.unwrap().label,
        "Copy",
        "a new selection resets"
    );
    // A fourth click: nothing selected, and the button is gone.
    click(&mut app, &mut host, bx, by);
    assert!(app.preview_sel.is_none());
    assert!(app.preview_word.is_none());
    assert!(app.preview_selection_text().is_none());
    let _ = app.render_headless(900, 600, 1.0);
    assert!(app.copy_btn.is_none(), "no selection, no button");
}

/// The touch counterpart: a double-tap selects, and the finger drag that
/// follows extends the selection instead of scrolling the page.
#[test]
fn a_finger_drag_after_a_preview_double_tap_selects_rows() {
    let mut app = open(
        "editor_test_preview_touch.txt",
        "one\ntwo\nthree\nfour\nfive\nsix\n",
    );
    let mut host = NullHost::default();
    let _ = app.render_headless(900, 600, 1.0);
    let row = app.preview_rows[0].clone();
    let (x, y) = (
        row.x0 + 10.0,
        app.viewport().y + (row.top + row.bottom) / 2.0,
    );
    for _ in 0..2 {
        app.on_event(
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
            },
            &mut host,
        );
        app.on_event(
            Event::MouseUp {
                x,
                y,
                button: MouseButton::Left,
            },
            &mut host,
        );
    }
    assert!(
        app.preview_selection_text().is_some(),
        "a double-tap selects something"
    );
    let before = app.scroll.offset;
    // A moving finger reports Pan, never MouseMove.
    app.on_event(
        Event::Pan {
            dx: 0.0,
            dy: 40.0,
            x: 0.0,
            y: 0.0,
        },
        &mut host,
    );
    let (a, b) = app.preview_sel.expect("rows selected by the drag");
    assert!(a < b, "the drag extended past the tapped row ({a}..{b})");
    assert_eq!(
        app.scroll.offset, before,
        "a selection drag does not scroll"
    );
    app.on_event(Event::PanEnd { vx: 0.0, vy: -3.0 }, &mut host);
    assert!(app.preview_touch.is_none(), "the gesture is retired");
}

/// A tap selects nothing and arms nothing: the flick that follows it is a
/// scroll, with the coast the finger threw. The gesture used to be armed by
/// any press on a row, so a tap followed by a flick inside the tap's second
/// became a row selection that scrolled nothing and coasted nowhere.
#[test]
fn a_tap_before_a_flick_does_not_turn_it_into_a_selection() {
    let mut app = open("editor_test_tap_flick.txt", &"one\ntwo\n".repeat(400));
    let mut host = NullHost::default();
    let _ = app.render_headless(900, 600, 1.0);
    let row = app.preview_rows[2].clone();
    let (x, y) = (
        row.x0 + 10.0,
        app.viewport().y + (row.top + row.bottom) / 2.0,
    );
    app.on_event(
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    app.on_event(
        Event::MouseUp {
            x,
            y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(
        app.preview_touch.is_none(),
        "a single tap must not arm a selection drag"
    );
    let before = app.scroll.offset;
    for _ in 0..5 {
        app.on_event(
            Event::Pan {
                dx: 0.0,
                dy: -20.0,
                x,
                y,
            },
            &mut host,
        );
    }
    assert!(
        (app.scroll.offset - before - 100.0).abs() < 0.05,
        "the flick after a tap must scroll, moved {}",
        app.scroll.offset - before
    );
    assert!(
        app.preview_sel.is_none(),
        "and must not leave a selection behind"
    );
    app.on_event(Event::PanEnd { vx: 0.0, vy: -2.0 }, &mut host);
    assert!(
        app.scroll.coast != 0.0,
        "the flick after a tap must coast like any other"
    );
}

/// The armed gesture belongs to the pane the finger is on, not to whatever a
/// previous tap left armed: a double-tap in the editor halves, then a flick
/// over the reader half, is a scroll of the reader — and it coasts. The
/// editor arm used to consume every pan it saw (a drag off its pane spends no
/// delta but still counts as the selection's), and `PanEnd` then retired the
/// gesture without a coast, so the reader half was dead to touch for as long
/// as the arm lived.
#[test]
fn an_armed_editor_selection_does_not_swallow_a_flick_on_the_reader_half() {
    let text =
        "# heading\n\na paragraph that is long enough to fill a row of the split\n\n".repeat(120);
    let mut app = open("editor_test_armed_split.md", &text);
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let _ = app.render_headless(1000, 600, 1.0);
    let (left, right) = app.split_panes();
    let (ex, ey) = (right.x + right.w / 2.0, right.y + right.h / 2.0);
    for _ in 0..2 {
        app.on_event(
            Event::MouseDown {
                x: ex,
                y: ey,
                button: MouseButton::Left,
            },
            &mut host,
        );
        app.on_event(
            Event::MouseUp {
                x: ex,
                y: ey,
                button: MouseButton::Left,
            },
            &mut host,
        );
    }
    assert!(
        app.touch_sel.is_some(),
        "the double-tap arms the editor's selection drag"
    );
    let (px, py) = (left.x + left.w / 2.0, left.y + left.h / 2.0);
    let before = app.preview_scroll();
    for _ in 0..5 {
        app.on_event(
            Event::Pan {
                dx: 0.0,
                dy: -20.0,
                x: px,
                y: py,
            },
            &mut host,
        );
    }
    assert!(
        app.preview_scroll() > before + 50.0,
        "a finger on the reader half scrolls it: moved {}",
        app.preview_scroll() - before
    );
    app.on_event(Event::PanEnd { vx: 0.0, vy: -2.0 }, &mut host);
    assert!(app.touch_sel.is_none(), "the stale arm is dropped");
    assert!(
        app.scroll.coast != 0.0,
        "and the flick coasts the pane it happened on"
    );
}

/// The other direction of the same rule: a reader-half tap arms a reader
/// selection, and a flick on the *editor* half is still the editor's scroll.
#[test]
fn an_armed_reader_selection_does_not_swallow_a_flick_on_the_editor_half() {
    let text =
        "# heading\n\na paragraph that is long enough to fill a row of the split\n\n".repeat(120);
    let mut app = open("editor_test_armed_reader.md", &text);
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let _ = app.render_headless(1000, 600, 1.0);
    let (left, right) = app.split_panes();
    let row = app
        .preview_rows
        .iter()
        .find(|r| r.top > left.y)
        .expect("a reader row to tap")
        .clone();
    let (px, py) = (
        row.x0 + 10.0,
        left.y + (row.top + row.bottom) / 2.0 - app.preview_scroll(),
    );
    for _ in 0..2 {
        app.on_event(
            Event::MouseDown {
                x: px,
                y: py,
                button: MouseButton::Left,
            },
            &mut host,
        );
        app.on_event(
            Event::MouseUp {
                x: px,
                y: py,
                button: MouseButton::Left,
            },
            &mut host,
        );
    }
    assert!(
        app.preview_touch.is_some(),
        "the reader double-tap arms its selection drag"
    );
    let (ex, ey) = (right.x + right.w / 2.0, right.y + right.h / 2.0);
    let before = app.editor_scroll();
    for _ in 0..5 {
        app.on_event(
            Event::Pan {
                dx: 0.0,
                dy: -20.0,
                x: ex,
                y: ey,
            },
            &mut host,
        );
    }
    assert!(
        app.editor_scroll() > before + 50.0,
        "a finger on the editor half scrolls it: moved {}",
        app.editor_scroll() - before
    );
    app.on_event(Event::PanEnd { vx: 0.0, vy: -2.0 }, &mut host);
    assert!(app.preview_touch.is_none(), "the stale arm is dropped");
    assert!(
        app.scroll.coast != 0.0,
        "and the flick coasts the pane it happened on"
    );
}

#[test]
fn headless_render_draws_text_and_statusbar() {
    let mut app = open("editor_test_a.txt", "Hello Editor\nsecond line");
    let c = app.render_headless(640, 400, 1.0);
    let bg = app.theme.bg.pack() | 0xFF00_0000;
    let dark_px = c.pixels[..640 * 372].iter().filter(|&&p| p != bg).count();
    assert!(dark_px > 100, "preview area is blank");
    // status bar region differs from page background
    assert_ne!(c.pixels[390 * 640 + 300], bg);
}

#[test]
fn zoom_changes_layout_height() {
    let mut app = App::new(None).unwrap();
    app.buffer = Buffer::from_text(&"line\n".repeat(50));
    let _ = app.render_headless(800, 600, 1.0);
    let h1 = app.preview.list.height;
    let mut host = NullHost::default();
    app.on_event(ctrl('+'), &mut host);
    // Zooming in Edit mode defers the wrapped list (it is not painted
    // there); the pane-switch button relayouts before the pane shows it.
    app.mode = Mode::Preview;
    app.layout_dirty = true;
    let mut c = Canvas::new(800, 600);
    app.paint(&mut c);
    assert!(app.preview.list.height > h1);
}

#[test]
fn json_renders_tinted_tree() {
    use crate::layout::display_list::DrawOp;
    let mut app = open("editor_test_b.json", "{\"name\": \"editor\", \"n\": 42}");
    assert_eq!(app.kind, FileKind::Json);
    let c = app.render_headless(800, 600, 1.0);
    assert!(app.status_extra.is_empty(), "valid JSON flagged");
    let bg = app.theme.bg.pack();
    let ink = c.pixels.iter().filter(|&&p| p != bg).count();
    assert!(ink > 200, "tree area is blank");
    // Key tint is in the laid-out ops (pixels are AA-blended, not exact).
    let tints: Vec<u32> = app
        .preview
        .list
        .ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Text { color, .. } => Some(color.pack()),
            _ => None,
        })
        .collect();
    assert!(tints.contains(&app.theme.json_key.pack()));
    assert!(tints.contains(&app.theme.json_number.pack()));
    assert!(tints.contains(&app.theme.json_string.pack()));
}

#[test]
fn invalid_json_shows_error_banner() {
    use crate::layout::display_list::DrawOp;
    let mut app = open("editor_test_c.json", "{\"a\": }");
    let c = app.render_headless(800, 600, 1.0);
    assert!(
        app.status_extra.contains("error"),
        "status: {}",
        app.status_extra
    );
    assert!(
        app.preview
            .list
            .ops
            .iter()
            .any(|op| matches!(op, DrawOp::RoundedRect { .. })),
        "error banner missing"
    );
    let bg = app.theme.bg.pack();
    assert!(c.pixels.iter().any(|&p| p != bg), "banner did not paint");
}

#[test]
fn clicking_toggle_collapses_tree() {
    let mut app = open(
        "editor_test_d.json",
        "{\"a\": {\"b\": [1, 2, 3]}, \"c\": true}",
    );
    let _ = app.render_headless(800, 600, 1.0);
    let open_h = app.preview.list.height;
    assert!(!app.preview.list.hits.is_empty());
    let r = app.preview.list.hits[0].rect;
    let mut host = NullHost::default();
    app.on_event(
        Event::MouseDown {
            x: r.x + 1.0,
            y: r.y + 1.0 + app.viewport().y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.collapsed.len(), 1);
    let mut c = Canvas::new(800, 600);
    app.paint(&mut c);
    assert!(app.preview.list.height < open_h, "collapse did not shrink");
}

#[test]
fn mode_key_cycles_preview_edit_split() {
    let mut app = App::new(None).unwrap();
    let mut host = NullHost::default();
    // An Untitled buffer opens straight in Edit mode.
    assert_eq!(app.mode, Mode::Edit);
    app.on_event(ctrl('e'), &mut host);
    assert_eq!(app.mode, Mode::Split);
    app.on_event(ctrl('e'), &mut host);
    assert_eq!(app.mode, Mode::Preview);
    app.on_event(ctrl('e'), &mut host);
    assert_eq!(app.mode, Mode::Edit);
    // Edit/Split panes lay out without panicking.
    app.on_event(ctrl('e'), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
}

#[test]
fn untitled_tabs_switch_to_edit_mode() {
    // Closing the last tab leaves a fresh Untitled, which drops into Edit.
    let mut app = open("editor_test_untitled.txt", "hello");
    let mut host = NullHost::default();
    assert_eq!(app.mode, Mode::Preview);
    app.on_event(ctrl('w'), &mut host);
    assert!(app.path.is_none(), "last tab is a fresh Untitled");
    assert_eq!(app.mode, Mode::Edit);
    // Give it content so the next open stacks a tab instead of replacing it.
    app.on_event(Event::Text("draft".into()), &mut host);
    app.mode = Mode::Preview; // as if the user flipped to Preview
    let out = crate::testing::temp_dir().join("editor_test_untitled2.txt");
    std::fs::write(&out, "x").unwrap();
    app.open_path(out.clone(), &mut host);
    assert_eq!(app.path.as_ref(), Some(&out));
    assert_eq!(app.mode, Mode::Preview, "a file tab keeps the mode");
    // Cycling back onto the Untitled tab forces Edit again.
    app.on_event(
        Event::KeyDown {
            key: Key::Tab,
            mods: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        },
        &mut host,
    );
    assert!(app.path.is_none(), "cycled back to the Untitled tab");
    assert_eq!(app.mode, Mode::Edit);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn markdown_renders_document() {
    let mut app = open(
        "editor_test_e.md",
        "# Title\n\nHello **world**.\n\n- a\n- b\n",
    );
    assert_eq!(app.kind, FileKind::Markdown);
    let c = app.render_headless(800, 600, 1.0);
    assert!(app.status_extra.is_empty());
    let bg = app.theme.bg.pack();
    assert!(
        c.pixels.iter().filter(|&&p| p != bg).count() > 200,
        "document area is blank"
    );
}

#[test]
fn new_kinds_render_with_status() {
    // CSV: table + row/col counts.
    let mut app = open("editor_test_kinds.csv", "a,b\n1,2\n3,4\n");
    assert_eq!(app.kind, FileKind::Csv);
    let c = app.render_headless(800, 600, 1.0);
    assert!(app.status_extra.contains("2 rows"), "{}", app.status_extra);
    let bg = app.theme.bg.pack();
    assert!(c.pixels.iter().filter(|&&p| p != bg).count() > 200);
    // Configs: tinted preview, clean bar.
    for (name, content, kind) in [
        ("editor_test_kinds.yaml", "port: 8080\n", FileKind::Yaml),
        ("editor_test_kinds.toml", "port = 8080\n", FileKind::Toml),
        ("editor_test_kinds.ini", "[s]\nport = 8080\n", FileKind::Ini),
        ("editor_test_kinds.rs", "fn main() {}\n", FileKind::Code),
    ] {
        let mut app = open(name, content);
        assert_eq!(app.kind, kind);
        let c = app.render_headless(800, 600, 1.0);
        assert!(
            c.pixels.iter().filter(|&&p| p != bg).count() > 50,
            "{name} blank"
        );
    }
    // Logs: level counts in the bar.
    let mut app = open("editor_test_kinds.log", "INFO up\nERROR boom\nWARN slow\n");
    assert_eq!(app.kind, FileKind::Log);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.status_extra.contains("1 error"), "{}", app.status_extra);
}

#[test]
fn hovering_a_link_names_it_in_the_status_bar() {
    use crate::layout::display_list::HitAction;
    let mut app = open(
        "editor_test_f.md",
        "See [example](https://example.com/x).\n",
    );
    let _ = app.render_headless(800, 600, 1.0);
    let (rect, href) = app
        .preview
        .list
        .hits
        .iter()
        .find_map(|h| match &h.action {
            HitAction::Link(href) => Some((h.rect, href.clone())),
            _ => None,
        })
        .expect("no link hit region");
    let mut host = NullHost::default();
    let (lx, ly) = (
        rect.x + rect.w / 2.0,
        rect.y + rect.h / 2.0 + app.viewport().y,
    );
    app.on_event(Event::MouseMove { x: lx, y: ly }, &mut host);
    assert_eq!(
        app.hover_link.as_deref(),
        Some(href.as_str()),
        "the bar names the address under the pointer"
    );
    // Moving off the link stops naming it: the bar describes what the
    // pointer is on, it is not a log of links that were visited.
    app.on_event(Event::MouseMove { x: 20.0, y: ly }, &mut host);
    assert!(app.hover_link.is_none());
    // Clicking still opens it, and writes nothing extra to the bar.
    app.on_event(
        Event::MouseDown {
            x: lx,
            y: ly,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(host.opened_url.as_deref(), Some(href.as_str()));
    assert!(app.status_extra.is_empty(), "status: {}", app.status_extra);
}

#[test]
fn footnote_markers_jump_to_the_note_and_back() {
    use crate::layout::display_list::HitAction;
    // Enough prose that the notes are off-screen at the top.
    let mut text = String::from("A claim[^1].\n\n");
    for i in 0..40 {
        text.push_str(&format!(
            "Paragraph {i} has enough words to fill a line of the reading column, so the page scrolls.\n\n"
        ));
    }
    text.push_str("[^1]: The note at the bottom.\n");
    let mut app = open("editor_test_footnote_jump.md", &text);
    let _ = app.render_headless(800, 600, 1.0);
    let refs: Vec<Rect> = app
        .preview
        .list
        .hits
        .iter()
        .filter_map(|h| match &h.action {
            HitAction::FootnoteRef(1) => Some(h.rect),
            _ => None,
        })
        .collect();
    assert_eq!(refs.len(), 1, "one marker hit region");
    let note_top = app
        .preview
        .list
        .hits
        .iter()
        .filter_map(|h| match &h.action {
            HitAction::FootnoteBack(1) => Some(h.rect),
            _ => None,
        })
        .map(|r| r.y)
        .fold(f32::INFINITY, f32::min);
    let mut host = NullHost::default();
    let vp = app.viewport();
    let click = |app: &App, rect: &Rect| {
        (
            vp.x + rect.x + rect.w / 2.0,
            vp.y + rect.y + rect.h / 2.0 - app.scroll.offset,
        )
    };
    let (x, y) = click(&app, &refs[0]);
    app.on_event(
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    // A note near the end lands as far as the page can scroll.
    let landed = (note_top - 32.0 * app.scale).max(0.0).min(app.scroll.max());
    assert!(
        (app.scroll.offset - landed).abs() < 1.0,
        "marker jumped to {}, want {landed}",
        app.scroll.offset
    );
    assert!(
        app.scroll.offset <= note_top && note_top < app.scroll.offset + vp.h,
        "the note is not in view at {}",
        app.scroll.offset
    );
    // The return arrow at the end of the note goes back to the marker.
    let arrow = app
        .preview
        .list
        .hits
        .iter()
        .filter_map(|h| match &h.action {
            HitAction::FootnoteBack(1) => Some(h.rect),
            _ => None,
        })
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .expect("return arrow");
    let (x, y) = click(&app, &arrow);
    app.on_event(
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    let back = (refs[0].y - 32.0 * app.scale).max(0.0);
    assert!(
        (app.scroll.offset - back).abs() < 1.0,
        "arrow returned to {}, want {back}",
        app.scroll.offset
    );
}

#[test]
fn a_planned_markdown_pane_jumps_footnotes_by_block() {
    // Below the plan threshold this would be one display list; here the pane
    // paints bands, so the jump targets come from the plan's block rows.
    let mut text = String::from("Top[^1].\n\n");
    for i in 0..60 {
        text.push_str(&format!(
            "Paragraph {i} has enough words to fill a line of the reading column, so the page scrolls.\n\n"
        ));
    }
    text.push_str("[^1]: The note at the bottom.\n");
    let mut app = planned_app("footnote_jump", "md", &text, 20);
    let _ = app.render_headless(800, 600, 1.0);
    let (note_top, ref_top) = {
        let Some(layout::PanePlan::Markdown(p)) = &app.pane_plan else {
            panic!("the fixture must be planned");
        };
        let doc = &app.markdown_doc.as_ref().expect("the parsed document").1;
        let note = doc.footnote_block(1).expect("note 1");
        let reference = doc.footnote_reference_block(1).expect("its reference");
        assert!(note > reference, "notes print after the prose");
        (p.rows.top(note), p.rows.top(reference))
    };
    app.jump_to_footnote(1, true);
    let landed = (note_top - 32.0 * app.scale).max(0.0).min(app.scroll.max());
    assert!(
        (app.scroll.offset - landed).abs() < 1.0,
        "planned jump landed at {}, want {landed}",
        app.scroll.offset
    );
    app.jump_to_footnote(1, false);
    let back = (ref_top - 32.0 * app.scale).max(0.0);
    assert!(
        (app.scroll.offset - back).abs() < 1.0,
        "planned return landed at {}, want {back}",
        app.scroll.offset
    );
}

#[test]
fn typing_edits_buffer_in_edit_mode() {
    let mut app = open("editor_test_g.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    assert_eq!(app.mode, Mode::Edit);
    app.on_event(Event::Text("X".into()), &mut host);
    assert_eq!(app.buffer.text(), "Xhello");
    assert!(app.buffer.dirty);
    assert_eq!(app.editor.caret, Pos { line: 0, col: 1 });
    // Undo removes the typing, redo re-applies it.
    app.on_event(ctrl('z'), &mut host);
    assert_eq!(app.buffer.text(), "hello");
    app.on_event(ctrl('y'), &mut host);
    assert_eq!(app.buffer.text(), "Xhello");
}

#[test]
fn typing_in_preview_mode_is_ignored() {
    let mut app = open("editor_test_h.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(Event::Text("X".into()), &mut host);
    assert_eq!(app.buffer.text(), "hello");
    assert!(!app.buffer.dirty);
}

#[test]
fn enter_and_backspace_split_join_lines() {
    let mut app = open("editor_test_i.txt", "one two");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.editor.caret = Pos { line: 0, col: 3 };
    app.on_event(
        Event::KeyDown {
            key: Key::Enter,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert_eq!(app.buffer.text(), "one\n two");
    app.on_event(
        Event::KeyDown {
            key: Key::Backspace,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    // Backspace at {1, 0} removes the newline: "one" + " two".
    assert_eq!(app.buffer.text(), "one two");
    app.on_event(ctrl('z'), &mut host);
    assert_eq!(app.buffer.text(), "one\n two");
}

#[test]
fn clipboard_copy_cut_paste_flow() {
    let mut app = open("editor_test_j.txt", "hello world");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    let tab = &mut *app;
    tab.editor.select_all(&tab.buffer);
    app.on_event(ctrl('c'), &mut host);
    assert_eq!(host.clipboard.as_deref(), Some("hello world"));
    app.on_event(ctrl('x'), &mut host);
    assert_eq!(app.buffer.text(), "");
    assert!(app.buffer.dirty);
    app.on_event(ctrl('v'), &mut host);
    assert_eq!(app.buffer.text(), "hello world");
}

#[test]
fn save_marks_buffer_clean() {
    let mut app = open("editor_test_k.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(Event::Text("!".into()), &mut host);
    // The dirty marker lives on the tab's save button now, not the title.
    assert!(app.buffer.dirty);
    assert_eq!(app.title(), "editor_test_k.txt — Editor");
    assert_eq!(app.buffer.text(), "!hello");
    app.on_event(ctrl('s'), &mut host);
    assert!(!app.buffer.dirty);
    assert_eq!(app.title(), "editor_test_k.txt — Editor");
    assert_eq!(
        std::fs::read_to_string(app.path.as_ref().unwrap()).unwrap(),
        "!hello"
    );
}

/// T7 · The dirty compare starts at the edit watermark, and its answer
/// must equal a full line-by-line comparison's: an edit at the end of a
/// long buffer walks no line above it, an edit at the top walks all of
/// them, and undoing back to the snapshot is clean again.
#[test]
fn dirty_compare_starts_at_the_edit_watermark() {
    let long = "x\n".repeat(20_000);
    let mut app = open("editor_test_wm.txt", &long);
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);

    // Nothing edited: the whole document is the snapshot, nothing to walk.
    assert!(!app.buffer.dirty);
    assert_eq!(app.edited_from, usize::MAX);

    // A keystroke at the very end. The watermark drops to the last line,
    // and `dirty` must agree with the same compare walked from line zero.
    let last = app.buffer.line_count() - 1;
    app.editor.caret = Pos { line: last, col: 0 };
    app.on_event(Event::Text("y".into()), &mut host);
    assert!(app.edited_from < app.buffer.line_count());
    let full = !super::same_lines(&app.buffer, &app.saved_text, app.saved_lines, 0);
    assert_eq!(app.buffer.dirty, full, "watermarked compare diverged");
    assert!(app.buffer.dirty);

    // Undoing restores the exact text: clean again, compare walked from
    // the same watermark.
    app.on_event(ctrl('z'), &mut host);
    assert!(!app.buffer.dirty);

    // An edit at the top: the watermark cannot skip anything, so the
    // walk is the full one — and its answer still matches the oracle.
    app.editor.caret = Pos { line: 0, col: 0 };
    app.on_event(Event::Text("y".into()), &mut host);
    assert_eq!(app.edited_from, 0);
    let full = !super::same_lines(&app.buffer, &app.saved_text, app.saved_lines, 0);
    assert_eq!(app.buffer.dirty, full, "top-edit compare diverged");
    assert!(app.buffer.dirty);
}

/// T4 · The JSONL status keeps its errored-record count running: the
/// patch reports the replaced records' delta, and the status
/// equals what a recount of the same records says.
#[test]
fn jsonl_errors_count_with_the_records_patch() {
    use crate::core::file_kind::FileKind;

    let path = crate::testing::temp_dir().join("editor_errors_count.jsonl");
    // Over the virtual threshold, with one malformed line early on.
    let mut text = String::new();
    for i in 0..6_000 {
        if i == 7 {
            text.push_str("{\"id\": 7\n");
        } else {
            text.push_str(&format!(
                "{{\"id\": {i}, \"tags\": [\"a\", \"b\"], \"note\": \"row {i}\"}}\n"
            ));
        }
    }
    std::fs::write(&path, text).unwrap();
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::JsonLines);
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    let mut guard = 0;
    while app.preview_job_pending() || app.tabs[0].plan.slots.is_empty() {
        app.on_event(Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(guard < 4000, "the JSONL records never landed");
    }

    // Baseline: the counter is the recount, and the malformed line is in.
    let baseline_errors = record_errors(&app.tabs[0].records);
    assert!(baseline_errors >= 1, "the fixture carries an error");
    assert_eq!(app.status_extra, record_count_status(&app.records));
    assert_eq!(app.tabs[0].records_errors, baseline_errors);

    // Typing in a good row: no error delta, status equals the recount.
    // The caret goes into a note string, where the key stays valid JSON.
    let good = 1000;
    let col = app.buffer.line(good).find("row 1000").unwrap() + "row ".len();
    app.editor.caret = Pos { line: good, col };
    app.on_event(Event::Text("x".into()), &mut host);
    assert_eq!(app.status_extra, record_count_status(&app.records));
    assert_eq!(
        app.tabs[0].records_errors,
        record_errors(&app.tabs[0].records)
    );

    // Healing the broken line: the delta moves and a recount agrees.
    let broken = (0..app.tabs[0].records.len())
        .find(|&i| app.tabs[0].records[i].value.is_err())
        .expect("the broken record");
    let line = app.tabs[0].records[broken].line - 1;
    let col = app.buffer.line(line).len();
    app.editor.caret = Pos { line, col };
    app.on_event(Event::Text("}".into()), &mut host);
    assert_eq!(app.status_extra, record_count_status(&app.records));
    assert_eq!(
        app.tabs[0].records_errors,
        record_errors(&app.tabs[0].records)
    );
    assert!(
        record_errors(&app.tabs[0].records) < baseline_errors,
        "the heal dropped an error"
    );

    // Breaking a fresh line the same way: the delta moves back up.
    let fresh = (0..app.tabs[0].records.len())
        .find(|&i| !app.tabs[0].records[i].value.is_err())
        .expect("a good record");
    let line = app.tabs[0].records[fresh].line - 1;
    let col = app.buffer.line(line).len();
    app.editor.caret = Pos { line, col };
    app.on_event(Event::Text("{".into()), &mut host);
    assert_eq!(app.status_extra, record_count_status(&app.records));
    assert_eq!(
        app.tabs[0].records_errors,
        record_errors(&app.tabs[0].records)
    );
    assert_eq!(
        record_errors(&app.tabs[0].records),
        baseline_errors,
        "the break restored it"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn save_as_writes_an_untitled_buffer() {
    // An Untitled buffer has no path: Ctrl+S must ask the host where to
    // put it, then adopt that path for the tab.
    let mut app = App::new(None).unwrap();
    assert!(app.path.is_none());
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(Event::Text("draft".into()), &mut host);
    assert!(app.buffer.dirty);
    // Cancelling changes nothing: still Untitled, still dirty.
    app.on_event(ctrl('s'), &mut host);
    assert!(app.path.is_none());
    assert!(app.buffer.dirty);
    assert_eq!(app.status_extra, "save cancelled");
    // Choosing a path writes the file and re-homes the tab.
    let out = crate::testing::temp_dir().join("editor_test_saveas.md");
    let _ = std::fs::remove_file(&out);
    host.save_file = Some(out.clone());
    app.on_event(ctrl('s'), &mut host);
    assert_eq!(app.path.as_ref(), Some(&out));
    assert_eq!(app.kind, FileKind::Markdown, "kind follows the suffix");
    assert!(!app.buffer.dirty);
    assert_eq!(app.title(), "editor_test_saveas.md — Editor");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "draft");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn tab_save_button_saves_the_tab() {
    let mut app = open("editor_test_savebtn.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(Event::Text("!".into()), &mut host);
    assert!(app.buffer.dirty);
    let _ = app.render_headless(800, 600, 1.0);
    let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
    let save = lay.saves[0].expect("dirty tab shows a save button");
    assert!(
        save.right() <= lay.closes[0].x + 0.5,
        "save button must sit left of the close ×"
    );
    assert_eq!(
        titlebar::hit_test(&lay, save.x + 2.0, save.y + 2.0),
        TitleHit::TabSave(0)
    );
    app.on_event(
        Event::MouseDown {
            x: save.x + 2.0,
            y: save.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(!app.buffer.dirty, "clicking the tab save button saves");
    assert_eq!(
        std::fs::read_to_string(app.path.as_ref().unwrap()).unwrap(),
        "!hello"
    );
    // A clean tab carries no save button.
    let _ = app.render_headless(800, 600, 1.0);
    let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
    assert!(lay.saves[0].is_none());
}

#[test]
fn undo_back_to_saved_clears_the_save_button() {
    // The report: type, see the save button pop up, undo it all — and
    // the button stayed up. Dirty now means "differs from saved",
    // recomputed on every edit, so it clears again.
    let mut app = open("editor_test_undoclean.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host); // Edit
    assert!(!app.buffer.dirty, "a just-opened file is clean");
    app.on_event(Event::Text("!".into()), &mut host);
    assert!(app.buffer.dirty);
    let _ = app.render_headless(800, 600, 1.0);
    let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
    assert!(lay.saves[0].is_some(), "dirty tab shows a save button");
    // Undo the keystroke: the content matches the file again.
    app.on_event(ctrl('z'), &mut host);
    assert_eq!(app.buffer.text(), "hello");
    assert!(!app.buffer.dirty, "undo-to-saved must clear dirty");
    let _ = app.render_headless(800, 600, 1.0);
    let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, app.scale);
    assert!(lay.saves[0].is_none(), "the save button must go away");
    // And re-editing raises it again.
    app.on_event(Event::Text("?".into()), &mut host);
    assert!(app.buffer.dirty);
}

#[test]
fn status_bar_open_button_opens_a_file() {
    let mut app = App::new(None).unwrap();
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    let open_btn = app
        .status_buttons
        .iter()
        .find(|b| b.id == 5)
        .expect("Open button")
        .rect;
    // Leftmost in the row: Open Find | Preview Edit Split | Help | Theme.
    let find_btn = app
        .status_buttons
        .iter()
        .find(|b| b.id == 7)
        .expect("Find button")
        .rect;
    let leftmost = app
        .status_buttons
        .iter()
        .map(|b| b.rect.x)
        .fold(f32::MAX, f32::min);
    assert!((open_btn.x - leftmost).abs() < 0.5, "Open is leftmost");
    assert!(
        (find_btn.x - open_btn.right()).abs() < 8.0,
        "Find sits right of Open"
    );
    let out = crate::testing::temp_dir().join("editor_test_openbtn.txt");
    std::fs::write(&out, "opened").unwrap();
    host.picked_file = Some(out.clone());
    app.on_event(
        Event::MouseDown {
            x: open_btn.x + 2.0,
            y: open_btn.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.path.as_ref(), Some(&out));
    assert_eq!(app.buffer.text(), "opened");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn arrow_keys_move_caret_in_edit_mode() {
    let mut app = open("editor_test_l.txt", "first\nsecond");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(
        Event::KeyDown {
            key: Key::Down,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert_eq!(app.editor.caret, Pos { line: 1, col: 0 });
    app.on_event(
        Event::KeyDown {
            key: Key::Right,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert_eq!(app.editor.caret.col, 1);
}

#[test]
fn shift_select_and_delete_selection() {
    let mut app = open("editor_test_m.txt", "abcdef");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(
        Event::KeyDown {
            key: Key::Right,
            mods: Modifiers {
                ctrl: false,
                alt: false,
                shift: true,
            },
        },
        &mut host,
    );
    app.on_event(
        Event::KeyDown {
            key: Key::Right,
            mods: Modifiers {
                ctrl: false,
                alt: false,
                shift: true,
            },
        },
        &mut host,
    );
    assert_eq!(
        app.editor.selection(),
        Some((Pos { line: 0, col: 0 }, Pos { line: 0, col: 2 }))
    );
    app.on_event(
        Event::KeyDown {
            key: Key::Delete,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert_eq!(app.buffer.text(), "cdef");
}

#[test]
fn split_renders_both_panes_and_keeps_place() {
    let mut app = open("editor_test_n.txt", &"word line\n".repeat(40));
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host); // Edit
    app.on_event(ctrl('e'), &mut host); // Split
    let mut c = app.render_headless(800, 600, 1.0);
    let bg = app.theme.bg.pack();
    // Both panes painted something.
    assert!(
        c.pixels.iter().filter(|&&p| p != bg).count() > 400,
        "split panes blank"
    );
    // Editing in split re-renders the document pane (height changes).
    let h_before = app.preview.list.height;
    app.on_event(Event::Text("X".into()), &mut host);
    app.paint(&mut c);
    assert!(app.preview.list.height >= h_before);
    assert!(app.buffer.text().starts_with("Xword"));
}

#[test]
fn nested_toggles_collapse_and_reexpand() {
    let mut app = open(
        "editor_test_p.json",
        "{\"a\":{\"x\":1},\"list\":[{\"in\":true}],\"s\":\"end\"}",
    );
    let _ = app.render_headless(800, 600, 1.0);
    let hits: Vec<(Rect, usize)> = app
        .preview
        .list
        .hits
        .iter()
        .filter_map(|h| match &h.action {
            crate::layout::display_list::HitAction::ToggleNode(id) => Some((h.rect, *id)),
            _ => None,
        })
        .collect();
    assert!(hits.len() >= 3, "want root + 2 nested toggles");
    let mut host = NullHost::default();
    for (i, &(r, id)) in hits.iter().enumerate().skip(1) {
        let open_h = app.preview.list.height;
        // Click on the [+]/[-] glyph itself (end of the hit rect): the
        // marker sits after the key text on nested rows.
        let (mx, my) = (r.right() - 2.0, r.y + 2.0 + app.viewport().y);
        app.on_event(
            Event::MouseDown {
                x: mx,
                y: my,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(
            app.collapsed.contains(&id),
            "click on toggle {i} (id {id}) did not collapse"
        );
        app.paint(&mut Canvas::new(800, 600));
        assert!(
            app.preview.list.height < open_h,
            "collapsing toggle {i} (id {id}) did not shrink the tree"
        );
        // Click again: re-expand.
        app.on_event(
            Event::MouseDown {
                x: mx,
                y: my,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(!app.collapsed.contains(&id), "re-expand of {id} failed");
        app.paint(&mut Canvas::new(800, 600));
    }
}

#[test]
fn tabs_open_dedupe_and_cycle() {
    let mut app = open("editor_test_q.txt", "one");
    let second = crate::testing::temp_dir().join("editor_test_r.md");
    std::fs::write(&second, "# two\n").unwrap();
    let mut host = NullHost::default();
    assert_eq!(app.tabs.len(), 1);
    app.on_event(Event::FileOpen(second.clone()), &mut host);
    assert_eq!(app.tabs.len(), 2, "drop should open a new tab");
    assert_eq!(app.active, 1);
    assert_eq!(app.kind, FileKind::Markdown);
    // Re-opening the same path focuses the existing tab instead of adding.
    app.on_event(ctrl('e'), &mut host); // leave tab 1 in Edit, then switch
    app.on_event(Event::FileOpen(second.clone()), &mut host);
    assert_eq!(app.tabs.len(), 2, "re-open must not duplicate");
    assert_eq!(app.active, 1);
    assert_eq!(app.mode, Mode::Edit, "per-tab state kept");
    // Ctrl+Tab cycles; Shift goes back.
    app.on_event(
        Event::KeyDown {
            key: Key::Tab,
            mods: Modifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        },
        &mut host,
    );
    assert_eq!(app.active, 0);
    app.on_event(
        Event::KeyDown {
            key: Key::Tab,
            mods: Modifiers {
                ctrl: true,
                alt: false,
                shift: true,
            },
        },
        &mut host,
    );
    assert_eq!(app.active, 1);
}

#[test]
fn close_tab_warns_when_dirty_then_discards() {
    let mut app = open("editor_test_s.txt", "keep me");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(Event::Text("X".into()), &mut host);
    assert!(app.buffer.dirty);
    app.on_event(ctrl('w'), &mut host);
    assert_eq!(app.tabs.len(), 1, "dirty tab must warn, not close");
    assert!(app.status_extra.contains("unsaved"));
    app.on_event(ctrl('w'), &mut host);
    assert_eq!(app.tabs.len(), 1, "last tab becomes a fresh Untitled");
    assert!(!app.buffer.dirty);
    assert_eq!(app.buffer.text(), "");
    assert_eq!(app.name(), "Untitled");
}

#[test]
fn ctrl_o_opens_host_picked_file() {
    let mut app = App::new(None).unwrap();
    let pick = crate::testing::temp_dir().join("editor_test_t.json");
    std::fs::write(&pick, "{\"a\": 1}").unwrap();
    let mut host = NullHost {
        picked_file: Some(pick.clone()),
        ..NullHost::default()
    };
    app.on_event(ctrl('o'), &mut host);
    assert_eq!(app.tabs.len(), 1, "untitled app had no file tab yet");
    assert_eq!(app.active, 0);
    assert_eq!(app.path.as_deref(), Some(pick.as_path()));
    assert_eq!(app.kind, FileKind::Json);
    // A second Ctrl+O with no pick (dialog cancelled) changes nothing.
    app.on_event(ctrl('o'), &mut host);
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn help_overlay_toggles_and_click_closes() {
    let mut app = open("editor_test_u.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(
        Event::KeyDown {
            key: Key::F(1),
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert!(app.help_open);
    // Any click dismisses it (and does not place a caret).
    app.on_event(
        Event::MouseDown {
            x: 100.0,
            y: 100.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(!app.help_open);
    // Escape closes it too.
    app.on_event(
        Event::KeyDown {
            key: Key::F(1),
            mods: Modifiers::default(),
        },
        &mut host,
    );
    app.on_event(
        Event::KeyDown {
            key: Key::Escape,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert!(!app.help_open);
}

/// The About box's manual update check: the button is real estate the
/// click handler uses, it sits under the toggle, and a press elsewhere on
/// the overlay still dismisses the box. The button's own press starts a
/// network check on another thread (and answers with a native box), so what
/// is pinned here is the part that could silently rot — the geometry.
#[test]
fn about_box_has_a_check_now_button_under_the_toggle() {
    let mut app = open("editor_test_ab_check.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(
        Event::KeyDown {
            key: Key::F(2),
            mods: Modifiers::default(),
        },
        &mut host,
    );
    let _ = app.render_headless(900, 640, 1.0);
    let toggle = app.about_toggle.expect("toggle rect");
    let button = app.about_check.expect("check-now button rect");
    assert!(
        button.y >= toggle.y + toggle.h,
        "button at {} not under the toggle ending at {}",
        button.y,
        toggle.y + toggle.h
    );
    assert!(button.w > 0.0 && button.h > 0.0, "button rect empty");
    assert!(
        button.x + button.w <= 900.0 && button.y + button.h <= 640.0,
        "button off the window"
    );
    // Anywhere else on the overlay dismisses it, as before.
    app.on_event(
        Event::MouseDown {
            x: 6.0,
            y: 6.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(!app.about_open);
    assert!(app.about_check.is_none(), "rects clear with the box");
}

#[test]
fn about_overlay_toggles_with_f2_and_closes() {
    let mut app = open("editor_test_ab.txt", "hello");
    let mut host = NullHost::default();
    let f2 = || Event::KeyDown {
        key: Key::F(2),
        mods: Modifiers::default(),
    };
    app.on_event(f2(), &mut host);
    assert!(app.about_open);
    // F2 toggles it back off.
    app.on_event(f2(), &mut host);
    assert!(!app.about_open);
    // Escape closes it too.
    app.on_event(f2(), &mut host);
    assert!(app.about_open);
    app.on_event(
        Event::KeyDown {
            key: Key::Escape,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert!(!app.about_open);
}

#[test]
fn about_and_help_overlays_are_mutually_exclusive() {
    let mut app = open("editor_test_ab2.txt", "hello");
    let mut host = NullHost::default();
    let key = |k| Event::KeyDown {
        key: k,
        mods: Modifiers::default(),
    };
    app.on_event(key(Key::F(1)), &mut host);
    assert!(app.help_open && !app.about_open);
    // F2 swaps help for the about box rather than stacking them.
    app.on_event(key(Key::F(2)), &mut host);
    assert!(app.about_open && !app.help_open);
    app.on_event(key(Key::F(1)), &mut host);
    assert!(app.help_open && !app.about_open);
    // Any click dismisses whichever is up.
    app.on_event(
        Event::MouseDown {
            x: 100.0,
            y: 100.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(!app.help_open && !app.about_open);
}

#[test]
fn status_bar_about_button_opens_the_box() {
    let mut app = open("editor_test_ab3.txt", "hello");
    let mut host = NullHost::default();
    let _ = app.render_headless(900, 600, 1.0);
    let about = app
        .status_buttons
        .iter()
        .find(|b| b.label == "About")
        .expect("About button")
        .rect;
    app.on_event(
        Event::MouseDown {
            x: about.x + 2.0,
            y: about.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.about_open);
}

#[test]
fn menu_about_item_opens_the_box() {
    let mut app = open("editor_test_ab4.txt", "hello");
    let mut host = NullHost::default();
    let _ = app.render_headless(900, 600, 1.0);
    app.on_event(
        Event::MouseDown {
            x: 300.0,
            y: 300.0,
            button: MouseButton::Right,
        },
        &mut host,
    );
    let menu = app.menu.as_ref().expect("right-click opens a menu");
    let item = menu
        .rects
        .iter()
        .enumerate()
        .find(|(i, _)| menu.items[*i].0 == menu::Action::About)
        .map(|(_, r)| *r)
        .expect("menu offers About");
    app.on_event(
        Event::MouseDown {
            x: item.x + 4.0,
            y: item.y + 4.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.about_open);
    assert!(app.menu.is_none());
}

#[test]
fn right_click_menu_copy_all_and_dismiss() {
    let mut app = open("editor_test_v.txt", "whole document");
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    app.on_event(
        Event::MouseDown {
            x: 300.0,
            y: 300.0,
            button: MouseButton::Right,
        },
        &mut host,
    );
    let menu = app.menu.as_ref().expect("right-click opens a menu");
    let copy_all = menu
        .rects
        .iter()
        .enumerate()
        .find(|(i, _)| menu.items[*i].0 == menu::Action::CopyAll)
        .map(|(i, r)| (i, *r))
        .expect("preview menu offers Copy all");
    // Clicking the item runs it and closes the menu.
    app.on_event(
        Event::MouseDown {
            x: copy_all.1.x + 4.0,
            y: copy_all.1.y + 4.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.menu.is_none());
    assert_eq!(host.clipboard.as_deref(), Some("whole document"));
    // Re-open and click outside: dismissed without action.
    app.on_event(
        Event::MouseDown {
            x: 300.0,
            y: 300.0,
            button: MouseButton::Right,
        },
        &mut host,
    );
    assert!(app.menu.is_some());
    app.on_event(
        Event::MouseDown {
            x: 5.0,
            y: 5.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.menu.is_none());
    let _ = copy_all.0;
}

#[test]
fn title_bar_clicks_switch_and_close_tabs() {
    let mut app = open("editor_test_w.txt", "one");
    let second = crate::testing::temp_dir().join("editor_test_x.txt");
    std::fs::write(&second, "two").unwrap();
    let mut host = NullHost::default();
    app.on_event(Event::FileOpen(second), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
    let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, 1.0);
    // Click tab 0: switch.
    app.on_event(
        Event::MouseDown {
            x: lay.tabs[0].x + 4.0,
            y: lay.tabs[0].y + 4.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.active, 0);
    assert_eq!(app.buffer.text(), "one");
    // Click its ×: clean tab closes immediately.
    app.on_event(
        Event::MouseDown {
            x: lay.closes[0].x + 2.0,
            y: lay.closes[0].y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.buffer.text(), "two");
}

#[test]
fn cursor_shapes_over_panes_and_chrome() {
    let mut app = open("editor_test_y.json", "{\"a\": {\"b\": 1}, \"c\": [1, 2]}");
    let _ = app.render_headless(800, 600, 1.0);
    // Hand over a tree toggle in Preview.
    let toggle = app.preview.list.hits[0].rect;
    assert_eq!(
        app.cursor_at(toggle.x + 2.0, toggle.y + 2.0 + app.viewport().y),
        CursorShape::Hand
    );
    // Arrow over empty preview space.
    assert_eq!(app.cursor_at(400.0, 500.0), CursorShape::Arrow);
    // I-beam over the editor pane in Edit mode.
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    assert_eq!(app.cursor_at(400.0, 300.0), CursorShape::IBeam);
    // Hand over the window close button in the title bar.
    let lay = titlebar::layout(&app.fonts, &app.tab_labels(), 800.0, 1.0);
    assert_eq!(app.cursor_at(lay.close.x + 10.0, 10.0), CursorShape::Hand);
}

#[test]
fn caret_bar_paints_in_edit_mode() {
    let mut app = open("editor_test_caret.txt", "hello world");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    let c = app.render_headless(800, 600, 1.0);
    let ctx = app.ctx();
    let (cx, cy) = app.editor.caret_xy(&app.buffer, 800.0, &ctx);
    let line_h = app.editor.line_height(800.0, &ctx);
    let view = app.viewport();
    // The caret bar is a thin fg-colored rect at the caret position.
    let px = |x: f32, y: f32| c.pixels[y as usize * 800 + x as usize];
    let fg = app.theme.fg.pack();
    let mid_y = (view.y + cy + line_h / 2.0) as usize;
    // Content (and the caret with it) is inset by the gutter rail.
    let rail = gutter::rail_w(&app.fonts, &ctx, app.buffer.line_count());
    let x0 = (view.x + cx + rail - app.h_scroll).round() as usize;
    let found = (0..3).any(|dx| px((x0 + dx) as f32, mid_y as f32) == fg);
    assert!(
        found,
        "no caret pixels at ({x0},{mid_y}); colors: {:?}",
        (0..3)
            .map(|dx| px((x0 + dx) as f32, mid_y as f32))
            .collect::<Vec<u32>>()
    );
}

#[test]
fn click_in_edit_pane_places_caret() {
    let mut app = open("editor_test_o.txt", &"second line of text\n".repeat(20));
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
    // Click near the start of the visible area (first row).
    app.on_event(
        Event::MouseDown {
            x: 50.0,
            y: 40.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    // Caret moved to a real buffer position near the top of the document.
    assert!(app.editor.caret.line < 5, "caret: {:?}", app.editor.caret);
    app.on_event(
        Event::MouseUp {
            x: 50.0,
            y: 40.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.drag_anchor.is_none());
}

/// The OS accessibility switch has to change the *ride*, not the distance:
/// a wheel notch must land where it would have landed, just without the
/// frames getting there. Direct manipulation (a finger, a thumb) is direct
/// either way and is covered by its own tests.
#[test]
fn reduced_motion_applies_a_wheel_step_at_once() {
    let body = "a line of text\n".repeat(400);
    let mut host = NullHost::default();
    // Motion on (the default): a notch glides, so nothing moves on the
    // frame it arrives — that is what `scroll_smooth` is for.
    let mut gliding = open("editor_test_reduce_a.txt", &body);
    let _ = gliding.render_headless(800, 600, 1.0);
    gliding.on_event(
        Event::Scroll {
            dx: 0.0,
            dy: -480.0,
        },
        &mut host,
    );
    assert_eq!(gliding.scroll.offset, 0.0, "a notch must glide, not jump");
    // Motion reduced: the same notch is applied immediately, and there is
    // nothing left for a frame clock to animate.
    let mut reduced = open("editor_test_reduce_b.txt", &body);
    let _ = reduced.render_headless(800, 600, 1.0);
    reduced.set_reduce_motion(true);
    reduced.on_event(
        Event::Scroll {
            dx: 0.0,
            dy: -480.0,
        },
        &mut host,
    );
    assert_eq!(
        reduced.scroll.offset, 480.0,
        "the step still scrolls the full distance"
    );
    assert!(!reduced.scroll.animating(), "and nothing is animating");
}

#[test]
fn preview_drag_selects_rows_and_copies() {
    let mut app = open("editor_test_sel.txt", "alpha\nbeta\ngamma\ndelta\n");
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.preview_rows.len() >= 4, "want a row per line");
    let mid = |app: &App, i: usize| {
        app.viewport().y + (app.preview_rows[i].top + app.preview_rows[i].bottom) / 2.0
    };
    let (y0, y2) = (mid(&app, 0), mid(&app, 2));
    // Drag from row 0 down to row 2.
    app.on_event(
        Event::MouseDown {
            x: 100.0,
            y: y0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    app.on_event(Event::MouseMove { x: 100.0, y: y2 }, &mut host);
    app.on_event(
        Event::MouseUp {
            x: 100.0,
            y: y2,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.preview_sel, Some((0, 2)));
    // Ctrl+C copies the selected rows; the selection survives the copy.
    app.on_event(ctrl('c'), &mut host);
    assert_eq!(host.clipboard.as_deref(), Some("alpha\nbeta\ngamma"));
    // Switching modes clears it.
    app.on_event(ctrl('e'), &mut host);
    assert_eq!(app.preview_sel, None);
}

#[test]
fn status_bar_buttons_switch_modes_and_help() {
    let mut app = open("editor_test_sb.txt", "hello");
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    let edit = app
        .status_buttons
        .iter()
        .find(|b| b.label == "Edit")
        .expect("Edit button")
        .rect;
    let help = app
        .status_buttons
        .iter()
        .find(|b| b.label == "Help")
        .expect("Help button")
        .rect;
    app.on_event(
        Event::MouseDown {
            x: edit.x + 2.0,
            y: edit.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.mode, Mode::Edit);
    app.on_event(
        Event::MouseDown {
            x: help.x + 2.0,
            y: help.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.help_open);
}

#[test]
fn clean_file_reloads_when_changed_on_disk() {
    let path = crate::testing::temp_dir().join("editor_test_rc.txt");
    std::fs::write(&path, "original").unwrap();
    let mut app = App::new(Some(path.clone())).unwrap();
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    std::fs::write(&path, "changed externally").unwrap();
    // ~2 s of ticks: the periodic check reloads the clean buffer.
    for _ in 0..50 {
        app.on_event(Event::Tick, &mut host);
    }
    assert_eq!(app.buffer.text(), "changed externally");
    assert!(app.status_extra.contains("reloaded"));
    // Ctrl+R works directly too.
    std::fs::write(&path, "third version").unwrap();
    app.on_event(ctrl('r'), &mut host);
    assert_eq!(app.buffer.text(), "third version");
}

#[test]
fn dirty_file_gets_reload_hint_not_silent_reload() {
    let path = crate::testing::temp_dir().join("editor_test_rh.txt");
    std::fs::write(&path, "original").unwrap();
    let mut app = App::new(Some(path.clone())).unwrap();
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(Event::Text("X".into()), &mut host);
    assert!(app.buffer.dirty);
    std::fs::write(&path, "changed externally").unwrap();
    for _ in 0..50 {
        app.on_event(Event::Tick, &mut host);
    }
    // Unsaved edits win: a hint appears, the buffer is untouched.
    assert_eq!(app.buffer.text(), "Xoriginal");
    assert!(app.status_extra.contains("Ctrl+R"));
    // Explicit Ctrl+R discards the edits and reloads.
    app.on_event(ctrl('r'), &mut host);
    assert_eq!(app.buffer.text(), "changed externally");
    assert!(!app.buffer.dirty);
}

#[test]
fn theme_follows_os_until_manual_override() {
    let mut app = open("editor_test_theme.txt", "hello");
    let mut host = NullHost::default();
    assert!(!app.dark);
    app.on_event(Event::ThemeChanged { dark: true }, &mut host);
    assert!(app.dark, "OS dark preference should be followed");
    // After Ctrl+T the app stops following the OS.
    app.on_event(ctrl('t'), &mut host);
    assert!(!app.dark);
    app.on_event(Event::ThemeChanged { dark: true }, &mut host);
    assert!(!app.dark, "manual override must stick");
}

#[test]
fn tick_auto_scrolls_editor_drag() {
    let mut app = open("editor_test_tick.txt", &"line\n".repeat(200));
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
    // Start a selection drag and hold the mouse near the bottom edge.
    app.on_event(
        Event::MouseDown {
            x: 100.0,
            y: 100.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    app.on_event(Event::MouseMove { x: 100.0, y: 560.0 }, &mut host);
    let before = app.scroll.offset;
    app.on_event(Event::Tick, &mut host);
    assert!(app.scroll.offset > before, "tick should auto-scroll down");
    assert!(app.editor.selection().is_some(), "selection extends");
}

#[test]
fn caret_blinks_on_tick_and_resets_on_activity() {
    let mut app = open("editor_test_blink.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    assert!(app.caret_visible(), "caret starts solid");
    for _ in 0..12 {
        app.on_event(Event::Tick, &mut host);
    }
    assert!(
        !app.caret_visible(),
        "caret should hide after a half-period"
    );
    for _ in 0..12 {
        app.on_event(Event::Tick, &mut host);
    }
    assert!(app.caret_visible(), "and come back on the next one");
    for _ in 0..12 {
        app.on_event(Event::Tick, &mut host);
    }
    assert!(!app.caret_visible());
    // Typing restarts the blink, solid.
    app.on_event(Event::Text("X".into()), &mut host);
    assert!(app.caret_visible());
    // So does moving the caret.
    for _ in 0..12 {
        app.on_event(Event::Tick, &mut host);
    }
    assert!(!app.caret_visible());
    app.on_event(
        Event::KeyDown {
            key: Key::Right,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert!(app.caret_visible());
}

#[test]
fn theme_button_toggles_dark() {
    let mut app = open("editor_test_themebtn.txt", "hello");
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    let btn = app
        .status_buttons
        .iter()
        .find(|b| b.id == 4)
        .expect("theme button");
    assert_eq!(btn.label, "Dark", "light theme offers to go dark");
    app.on_event(
        Event::MouseDown {
            x: btn.rect.x + 2.0,
            y: btn.rect.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.dark, "click should switch to dark");
    assert!(app.theme_manual, "button is a manual override");
    // The label now offers the way back.
    let _ = app.render_headless(800, 600, 1.0);
    let btn = app
        .status_buttons
        .iter()
        .find(|b| b.id == 4)
        .expect("theme button");
    assert_eq!(btn.label, "Light");
    app.on_event(
        Event::MouseDown {
            x: btn.rect.x + 2.0,
            y: btn.rect.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(!app.dark);
}

#[test]
fn split_panes_are_half_half_with_divider() {
    let mut app = open("editor_test_splitdiv.txt", "hello");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(ctrl('e'), &mut host); // Split
    let _ = app.render_headless(1000, 600, 1.0);
    let (left, right) = app.split_panes();
    assert!(
        (left.w - right.w).abs() <= 2.0,
        "panes should be ~equal: {left:?} {right:?}"
    );
    let div = app.split_divider();
    assert!(div.w >= 1.0, "divider must be visible");
    assert_eq!(div.right(), right.x, "divider sits at the seam");
    assert!(
        div.x >= left.right() - 0.5,
        "divider starts at the left pane edge"
    );
}

#[test]
fn split_preview_wraps_at_pane_width() {
    // A long single line: in Split the preview column must fit the left
    // pane (it used to lay out at the full window width and clip).
    let mut app = open("editor_test_splitwrap.txt", "word ".repeat(120).trim_end());
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(ctrl('e'), &mut host); // Split
    let _ = app.render_headless(1000, 600, 1.0);
    let left = app.split_panes().0;
    let ctx = app.ctx();
    for op in &app.preview.list.ops {
        if let crate::layout::display_list::DrawOp::Text {
            x,
            text,
            size,
            style,
            ..
        } = op
        {
            let w = ctx.fonts.measure(*style, text, *size);
            assert!(
                x + w <= left.right() + 0.5,
                "preview text overflows the left pane: x={x} w={w}"
            );
        }
    }
    // And the column is centered in the pane, not the window: laid out at
    // the old full-window width the column would start at x≈90 (window
    // centering of the 820px cap); at pane width it hugs the pane padding.
    let xs = app
        .preview
        .list
        .ops
        .iter()
        .filter_map(|op| match op {
            crate::layout::display_list::DrawOp::Text { x, .. } => Some(*x),
            _ => None,
        })
        .collect::<Vec<_>>();
    let min = xs.iter().cloned().fold(f32::MAX, f32::min);
    assert!(
        min - left.x < 60.0,
        "column should be centered in the pane, not the window (min x {min} vs pane {left:?})"
    );
}

/// Long wrapping paragraphs: the preview runs much taller than the
/// source lines, which is the Split case the ratio lock exists for.
fn open_long_preview(name: &str) -> App {
    let para = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do ";
    let text = (0..40)
        .map(|_| para.repeat(6).trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut app = open(name, &format!("# Long\n\n{text}\n"));
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(ctrl('e'), &mut host); // Split
    app
}

#[test]
fn split_scroll_shares_a_ratio_not_pixels() {
    // The report: with a long preview the shared pixel offset sailed the
    // short editor pane past its own end (phantom gutter rows, pinned
    // thumb). Each pane now paints its own share of one ratio.
    let mut app = open_long_preview("editor_test_splitratio.md");
    let _ = app.render_headless(1000, 700, 1.0);
    assert!(
        app.preview_max() > app.editor_max() * 1.5,
        "setup needs a much longer preview: {} vs {}",
        app.preview_max(),
        app.editor_max()
    );
    // Bottom: the editor stops at its own end while the ratios agree.
    app.scroll.offset = app.scroll.max();
    assert!(
        app.editor_scroll() <= app.editor_max() + 1.0,
        "editor past its end: {} > {}",
        app.editor_scroll(),
        app.editor_max()
    );
    for (got, name) in [
        (app.scroll_ratio(), "shared"),
        (app.preview_scroll() / app.preview_max(), "preview"),
        (app.editor_scroll() / app.editor_max(), "editor"),
    ] {
        assert!(
            (got - 1.0).abs() < 0.01,
            "{name} ratio should be 1 at the bottom: {got}"
        );
    }
    // Middle: both panes sit halfway through their own content, so the
    // preview covers more ground over the same thumb travel.
    app.scroll.offset = app.scroll.max() / 2.0;
    let preview_ratio = app.preview_scroll() / app.preview_max();
    let editor_ratio = app.editor_scroll() / app.editor_max();
    assert!((preview_ratio - 0.5).abs() < 0.02, "{preview_ratio}");
    assert!((editor_ratio - 0.5).abs() < 0.02, "{editor_ratio}");
    assert!(
        app.preview_scroll() > app.editor_scroll() * 1.5,
        "preview should move further per ratio point"
    );
    // Both thumbs ride at the same relative travel.
    let pv = app.sb_geom(false, false).expect("preview bar");
    let ed = app.sb_geom(true, false).expect("editor bar");
    let rel = |g: &crate::ui::preview_view::SbGeom| (g.thumb.y - g.start) / g.travel.max(1.0);
    assert!(
        (rel(&pv) - rel(&ed)).abs() < 0.05,
        "thumbs must stay locked: {} vs {}",
        rel(&pv),
        rel(&ed)
    );
}

#[test]
fn split_thumb_drag_drives_both_panes_by_ratio() {
    // Dragging either thumb sets the shared ratio, so the other pane
    // follows: grab the short editor bar and pull it to the bottom.
    let mut app = open_long_preview("editor_test_splitdrag.md");
    let mut host = NullHost::default();
    let _ = app.render_headless(1000, 700, 1.0);
    let g = app.sb_geom(true, false).expect("editor bar");
    app.on_event(
        Event::MouseDown {
            x: g.thumb.x + 1.0,
            y: g.thumb.y + 1.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.sb_drag.is_some(), "thumb press starts a drag");
    app.on_event(
        Event::MouseMove {
            x: g.thumb.x + 1.0,
            y: g.start + g.travel,
        },
        &mut host,
    );
    assert!(
        app.scroll_ratio() > 0.95,
        "dragging the editor thumb to its end must reach the document end: {}",
        app.scroll_ratio()
    );
    // A 1 px grab offset keeps the last pixel of travel out of reach,
    // so allow a relative epsilon instead of exactness.
    assert!(
        (app.preview_scroll() - app.preview_max()).abs() <= app.preview_max() * 0.01 + 1.0,
        "preview follows to its own end: {} vs {}",
        app.preview_scroll(),
        app.preview_max()
    );
    app.on_event(
        Event::MouseUp {
            x: 0.0,
            y: 0.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.sb_drag.is_none());
}

/// Leftmost and rightmost edge (physical px) of the preview's text.
fn preview_text_extent(app: &App) -> (f32, f32) {
    let ctx = app.ctx();
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    for op in &app.preview.list.ops {
        if let crate::layout::display_list::DrawOp::Text {
            x,
            text,
            size,
            style,
            ..
        } = op
        {
            min = min.min(*x);
            max = max.max(*x + ctx.fonts.measure(*style, text, *size));
        }
    }
    (min, max)
}

#[test]
fn a_wider_window_gets_a_wider_reading_column() {
    // Regression for "maximizing doesn't readjust": the wrapped panes kept a
    // fixed 820 px measure however wide the window got, so maximizing left
    // most of the screen empty instead of reflowing the text into it.
    let mut app = open(
        "editor_test_column.txt",
        &format!("# Wide\n\n{}\n", "word ".repeat(400)),
    );
    let _ = app.render_headless(1100, 800, 1.0);
    let (n_min, n_max) = preview_text_extent(&app);
    let _ = app.render_headless(1920, 1168, 1.0);
    let (w_min, w_max) = preview_text_extent(&app);
    assert!(
        w_max - w_min > (n_max - n_min) * 1.5,
        "the column must reflow into a maximized window: {n_min}..{n_max} -> {w_min}..{w_max}"
    );
    // Centered, and inside the pane: a wider column must not run under the
    // window edge or the overlay scrollbar.
    let pane = app.viewport();
    assert!(w_min > pane.x + 100.0 && w_max < pane.right() - 100.0);
    // The right margin is a word short of the left one at most.
    assert!(
        (w_min - pane.x - (pane.right() - w_max)).abs() < 60.0,
        "not centered: {w_min} .. {w_max} in {pane:?}"
    );
    // A first row of the document is still the heading, i.e. the whole page
    // was laid out again for the new width rather than scaled or clipped.
    assert!(w_max - w_min > 0.0);
}

/// How long one frame costs, for the scroll-smoothness work: run with
/// `cargo test --release -- --ignored --nocapture paint_cost`.
#[test]
#[ignore]
fn paint_cost() {
    let body = "word ".repeat(60);
    let doc = (0..1200)
        .map(|i| format!("## Section {i}\n\n{body}\n"))
        .collect::<String>();
    let mut app = open("editor_bench.md", &doc);
    let mut host = NullHost::default();
    for (w, h) in [(1100usize, 800usize), (1920, 1168)] {
        let _ = app.render_headless(w, h, 1.0);
        let n = 60;
        let t = std::time::Instant::now();
        for i in 0..n {
            app.set_scroll((i * 37) as f32);
            app.on_event(Event::Tick, &mut host);
            let _ = app.render_headless(w, h, 1.0);
        }
        let per = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
        println!("paint {w}x{h}: {per:.2} ms/frame ({:.0} fps)", 1000.0 / per);
        println!(
            "  preview ops {}, status buttons {}, rows {}",
            app.preview.list.ops.len(),
            app.status_buttons.len(),
            app.preview_rows.len()
        );
        // Split: canvas allocation vs the painted content.
        let mut c = crate::gfx::canvas::Canvas::new(w, h);
        let t_alloc = t.elapsed();
        let t2 = std::time::Instant::now();
        crate::platform::Handler::paint(&mut app, &mut c);
        let one = t2.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  one paint {one:.2} ms (loop total {:.2})",
            t_alloc.as_secs_f64() * 1000.0
        );
    }
}

#[test]
fn cursor_over_scrollbars_is_arrow_not_ibeam() {
    let mut app = open(
        "editor_test_sbcur.txt",
        &"long line without scrolling\n".repeat(60),
    );
    app.on_event(ctrl('e'), &mut NullHost::default()); // Edit (=> editor pane)
    let _ = app.render_headless(800, 600, 1.0);
    // Over the vertical overlay scrollbar zone (which sits a window-edge
    // grip clear of the border so the OS resize border cannot eat it).
    let view = app.viewport();
    let v = app
        .editor_pane
        .v_sb(view, app.scroll.offset, app.metrics())
        .expect("setup: editor pane must scroll");
    let sb_x = v.zone.x + 1.0;
    let y_mid = view.y + view.h / 2.0;
    assert_eq!(app.cursor_at(sb_x, y_mid), CursorShape::Arrow);
    // Just inside the text area, same row: still I-beam.
    assert_eq!(
        app.cursor_at(view.right() - v.zone.w - 30.0, y_mid),
        CursorShape::IBeam
    );
    // Horizontal bar zone (long line in Edit => h_max > 0). One file:
    // re-use with a wide line via a fresh tab.
    let wide = crate::testing::temp_dir().join("editor_test_sbcursor_h.txt");
    std::fs::write(&wide, format!("x{}", "y".repeat(300))).unwrap();
    app.open_path(wide, &mut NullHost::default());
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.h_max > 0.0, "setup: need a wide line");
    let h = app
        .editor_pane
        .h_sb(
            view,
            PaneScroll::editor(app.scroll.offset, app.h_scroll, app.h_max, 0.0),
            app.metrics(),
        )
        .expect("setup: wide line must overflow");
    assert_eq!(
        app.cursor_at(view.x + 200.0, h.zone.y + 1.0),
        CursorShape::Arrow
    );
}

#[test]
fn editor_does_not_wrap_and_scrolls_horizontally() {
    let long = format!("{} end", "word ".repeat(200));
    let mut app = open("editor_test_nw.txt", &long);
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host); // Edit
    let _ = app.render_headless(600, 400, 1.0);
    // One row per line, no folding.
    assert_eq!(app.editor_pane.list.ops.len(), 1);
    assert!(app.h_max > 0.0, "long line must create scroll range");
    // Horizontal wheel pans the editor pane with a glide, like vertical
    // notches: nothing lands on the event's own frame.
    app.on_event(
        Event::Scroll {
            dx: -120.0,
            dy: 0.0,
        },
        &mut host,
    );
    assert_eq!(app.h_scroll, 0.0, "a notch must not land in one jump");
    assert_eq!(app.h_target, 120.0);
    assert!(app.scrolling, "a glide needs the frame clock");
    for _ in 0..600 {
        if !app.scrolling {
            break;
        }
        app.on_event(Event::Frame, &mut host);
    }
    assert_eq!(app.h_scroll, 120.0);
    assert!(!app.scrolling);
    // Clamped at the max.
    app.on_event(
        Event::Scroll {
            dx: -1_000_000.0,
            dy: 0.0,
        },
        &mut host,
    );
    assert_eq!(app.h_target, app.h_max);
    app.on_event(
        Event::Scroll {
            dx: 1_000_000.0,
            dy: 0.0,
        },
        &mut host,
    );
    assert_eq!(app.h_target, 0.0);
    for _ in 0..600 {
        if !app.scrolling {
            break;
        }
        app.on_event(Event::Frame, &mut host);
    }
    assert_eq!(app.h_scroll, 0.0);
    // End key puts the caret past the pane edge; follow_caret pans to it.
    app.on_event(
        Event::KeyDown {
            key: Key::End,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert!(app.h_scroll > 0.0, "caret at line end should pan right");
    // Home pans back.
    app.on_event(
        Event::KeyDown {
            key: Key::Home,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert_eq!(app.h_scroll, 0.0);
}

/// Long-line setup with a real horizontal scroll range, in Edit mode.
fn open_wide(name: &str) -> (App, NullHost) {
    let long = format!("{} end", "word ".repeat(200));
    let mut app = open(name, &long);
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host); // Edit
    let _ = app.render_headless(600, 400, 1.0);
    assert!(app.h_max > 100.0, "setup: need room to glide");
    (app, host)
}

#[test]
fn horizontal_tilt_glides_over_frames_instead_of_jumping() {
    // The report: the horizontal bar felt choppy next to the vertical
    // one, because every tilt landed in one jump while vertical notches
    // glide. Same easing, same clock discipline now.
    let (mut app, mut host) = open_wide("editor_test_hglide.txt");
    app.on_event(Event::Scroll { dx: -90.0, dy: 0.0 }, &mut host);
    assert_eq!(app.h_scroll, 0.0, "a tilt must not land in one jump");
    assert_eq!(app.h_target, 90.0);
    assert!(host.animating, "a glide needs the frame clock");
    let mut seen = vec![app.h_scroll];
    for _ in 0..600 {
        if !app.scrolling {
            break;
        }
        app.on_event(Event::Frame, &mut host);
        seen.push(app.h_scroll);
    }
    assert!(
        seen.len() > 5,
        "a tilt should arrive over several frames, not {}: {seen:?}",
        seen.len()
    );
    for w in seen.windows(2) {
        assert!(
            w[1] > w[0],
            "each frame moves a little, never back: {seen:?}"
        );
    }
    assert_eq!(app.h_scroll, 90.0);
    assert_eq!(app.h_target, 90.0);
    assert!(!app.scrolling);
    assert!(!host.animating);
}

#[test]
fn fine_horizontal_tilt_applies_directly() {
    // Trackpad-grade deltas are already smooth: gliding them would only
    // add latency, exactly like the vertical fine path.
    let (mut app, mut host) = open_wide("editor_test_hfine.txt");
    app.on_event(Event::Scroll { dx: -3.0, dy: 0.0 }, &mut host);
    assert_eq!(app.h_scroll, 3.0);
    assert_eq!(app.h_target, 3.0);
    assert!(!app.scrolling, "no glide stands behind a fine delta");
}

#[test]
fn touch_pan_stays_on_the_finger_and_ends_the_h_glide() {
    // A finger landing mid-glide takes the view over horizontally too:
    // the tilt's leftover travel is dropped, not resumed under the finger.
    let (mut app, mut host) = open_wide("editor_test_hpan.txt");
    app.on_event(Event::Scroll { dx: -90.0, dy: 0.0 }, &mut host);
    assert!(app.scrolling);
    // The finger is over the editor pane: the area is reachable at a
    // mid-pane position inside the 600×400 frame the fixture painted.
    let area = app.editor_area();
    let fx = area.x + area.w / 2.0;
    let fy = area.y + area.h / 2.0;
    app.on_event(
        Event::Pan {
            dx: -17.0,
            dy: 0.0,
            x: fx,
            y: fy,
        },
        &mut host,
    );
    assert_eq!(app.h_scroll, 17.0);
    assert_eq!(app.h_target, 17.0, "the finger owns the view now");
}

#[test]
fn horizontal_thumb_drag_tracks_the_pointer_exactly() {
    // Pointer-driven input never glides: the thumb is the pointer.
    let (mut app, mut host) = open_wide("editor_test_hdrag.txt");
    let g = app.sb_geom(true, true).expect("horizontal bar");
    app.on_event(
        Event::MouseDown {
            x: g.thumb.x + 1.0,
            y: g.thumb.y + 1.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.sb_drag.is_some(), "thumb press starts a drag");
    let px = g.start + g.travel / 2.0;
    app.on_event(
        Event::MouseMove {
            x: px,
            y: g.thumb.y + 1.0,
        },
        &mut host,
    );
    let want = g.offset_for(px - 1.0);
    assert_eq!(app.h_scroll, want);
    assert_eq!(app.h_target, want, "no glide behind a drag");
}

#[test]
fn h_glide_target_clamps_when_the_line_shrinks() {
    // A relayout or frame that narrows the range must not leave the
    // target unreachable past the end.
    let (mut app, mut host) = open_wide("editor_test_hclamp.txt");
    app.h_scroll = app.h_max - 100.0;
    app.h_target = app.h_max + 500.0;
    app.scrolling = true;
    app.on_event(Event::Frame, &mut host);
    assert_eq!(
        app.h_target, app.h_max,
        "the frame clamps an unreachable target first"
    );
    for _ in 0..600 {
        if !app.scrolling {
            break;
        }
        app.on_event(Event::Frame, &mut host);
    }
    assert_eq!(app.h_scroll, app.h_max);
    assert_eq!(app.h_target, app.h_max);
    assert!(!app.scrolling);
}

#[test]
fn preview_still_wraps_in_split() {
    let mut app = open("editor_test_pw.txt", "word ".repeat(120).trim_end());
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    app.on_event(ctrl('e'), &mut host); // Split
    let _ = app.render_headless(800, 600, 1.0);
    // The preview pane wraps (many rows); the editor pane does not (1 row).
    assert!(app.preview_rows.len() > 1, "preview should wrap");
    assert_eq!(app.editor_pane.list.ops.len(), 1, "editor must not wrap");
}

#[test]
fn vertical_scrollbar_clears_the_resize_grip_in_every_mode() {
    // Regression: the overlay scrollbar sat flush with the window's right
    // edge, inside the strip the OS keeps for resizing, so pressing the
    // thumb started a window resize instead of scrolling. Preview, Edit
    // and Split reach the bar through different paths (`sb_geom`, `sb_at`,
    // `paint_panes` all branch on mode), so every mode is checked: the
    // zone and thumb clear the grip, the pointer is an arrow there, a
    // press on the visible thumb starts a drag, and the painted bar never
    // shows up inside the strip.
    let band = platform::edge_grip();
    let (w, h) = (800usize, 600usize);
    // One block per paragraph guarantees a document taller than any pane.
    let content = "a paragraph of text\n\n".repeat(120);
    for (presses, mode) in [(0u8, Mode::Preview), (1, Mode::Edit), (2, Mode::Split)] {
        let mut app = open("editor_test_sbedge.txt", &content);
        let mut host = NullHost::default();
        for _ in 0..presses {
            app.on_event(ctrl('e'), &mut host); // Preview -> Edit -> Split
        }
        assert_eq!(app.mode, mode, "setup: mode after {presses} Ctrl+E");
        assert_eq!(
            app.edge,
            band * app.scale,
            "{mode:?}: app tracks the platform band (physical px)"
        );
        // The strip the bars clear is derived from a bar's own thickness
        // under the platform band, so it can never grow into the bar.
        let grip = app.bar_grip();
        assert_eq!(
            grip,
            preview_view::bar_grip(app.scale, app.edge),
            "{mode:?}: bar-safe strip is derived, not chosen"
        );
        assert!(
            grip <= preview_view::BAR_W_LOGICAL * preview_view::GRIP_SHARE * app.scale + 0.001,
            "{mode:?}: strip {grip} eats into the bar"
        );
        // Lay out first: the bar exists only once the pane knows its
        // content height.
        let canvas = app.render_headless(w, h, 1.0);
        // The pane that owns the window's right edge: the preview when it
        // fills the window, otherwise the editor pane.
        let editor = mode != Mode::Preview;
        let area = if editor {
            app.editor_area()
        } else {
            app.viewport()
        };
        let g = app
            .sb_geom(editor, false)
            .unwrap_or_else(|| panic!("{mode:?}: expected a vertical scrollbar"));
        assert!(
            g.thumb.right() <= area.right() - grip,
            "{mode:?}: thumb leaves no room for the resize strip: {g:?}"
        );
        assert!(
            g.zone.right() <= area.right() - grip + 0.001,
            "{mode:?}: interactive zone reaches into the strip: {g:?}"
        );
        // The bar still hugs the window edge — the gap is the resize
        // strip, not a wide inset that reads as a floating bar.
        assert!(
            area.right() - g.thumb.right() <= grip.max(3.0 * app.scale) + 0.001,
            "{mode:?}: bar floats {} px from the edge",
            area.right() - g.thumb.right()
        );
        let (px, py) = (g.thumb.x + 1.0, g.thumb.y + 2.0);
        assert_eq!(
            app.cursor_at(px, py),
            CursorShape::Arrow,
            "{mode:?}: the pointer over the bar is an arrow"
        );
        app.on_event(
            Event::MouseDown {
                x: px,
                y: py,
                button: MouseButton::Left,
            },
            &mut host,
        );
        assert!(
            app.sb_drag.is_some(),
            "{mode:?}: a press on the visible thumb must start a scroll drag"
        );
        // Painted pixels agree with the geometry *and* with the strip that
        // applies at each of them: the strip is asked for per point, so a
        // painted bar column must be a column the platform does not claim
        // at that row — anywhere else the pixel would be unreachable.
        let bar = canvas.pixels[(py as usize) * w + px as usize];
        let samples: Vec<(usize, usize)> = (48..h - 48)
            .step_by(7)
            .flat_map(|y| ((w - 24)..w).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.pixels[y * w + x] == bar)
            .collect();
        for &(x, y) in &samples {
            let g = app.grip_at(x as f32, y as f32, band);
            assert!(
                    x as f32 + g <= w as f32,
                    "{mode:?}: bar pixel at ({x}, {y}) is inside the {g} px strip the platform claims there"
                );
        }
        assert!(
            samples.len() > 20,
            "{mode:?}: sampled colour is not the bar ({} px)",
            samples.len()
        );
        let inside = (40..h - 40)
            .flat_map(|y| ((w - grip as usize - 20)..(w - grip as usize)).map(move |x| (x, y)))
            .filter(|&(x, y)| canvas.pixels[y * w + x] == bar)
            .count();
        assert!(
            inside > 20,
            "{mode:?}: sampled colour is not the bar ({inside} px)"
        );
    }
}

#[test]
fn resize_grip_self_tunes_to_what_is_at_the_edge() {
    // The strip is asked for per point, not chosen once: the platform's full
    // band wherever the app draws nothing, and only the bar-safe share over
    // a bar. Both halves matter — widening an edge is only safe as long as
    // the bars still get their pixels back — so each is asserted in every
    // mode the bar is reachable through.
    let band = platform::edge_grip();
    let (w, h) = (800usize, 600usize);
    let mid = h as f32 / 2.0;
    let near_edge = w as f32 - 1.0;
    // A document that fits: no bar at all, so the whole edge is resizable
    // at the platform's full band rather than the bar-safe share.
    let mut short = open("editor_test_grip.txt", "one short line\n");
    let _ = short.render_headless(w, h, 1.0);
    assert!(
        short.sb_geom(false, false).is_none(),
        "setup: a short document needs no scrollbar"
    );
    assert_eq!(
        short.grip_at(near_edge, mid, band),
        band,
        "no bar at the edge: it keeps the platform's full band"
    );
    let content = "a paragraph of text\n\n".repeat(120);
    for (presses, mode) in [(0u8, Mode::Preview), (1, Mode::Edit), (2, Mode::Split)] {
        let mut app = open("editor_test_grip2.txt", &content);
        let mut host = NullHost::default();
        for _ in 0..presses {
            app.on_event(ctrl('e'), &mut host); // Preview -> Edit -> Split
        }
        let _ = app.render_headless(w, h, 1.0);
        let editor = mode != Mode::Preview;
        let grip = app.bar_grip();
        // Never wider than the band the platform claims, and strictly
        // narrower over a bar — the second half only means something when
        // there is a band at all: macOS reports `0.0` (no edge grip) until
        // the AppKit backend lands (M9), and then there is nothing for a
        // bar to hand back, which the `<=` pins exactly.
        assert!(
            grip <= band * app.scale,
            "{mode:?}: the bar-safe strip is never wider than the platform's band"
        );
        if band > 0.0 {
            assert!(
                grip < band * app.scale,
                "{mode:?}: the bar-safe strip must be the narrower one"
            );
        }
        assert_eq!(
            grip,
            preview_view::bar_grip(app.scale, band),
            "{mode:?}: the bar-safe strip follows the band it is given"
        );
        let g = app
            .sb_geom(editor, false)
            .unwrap_or_else(|| panic!("{mode:?}: expected a vertical scrollbar"));
        // Over the bar the app keeps its pixels; the columns between the
        // bar and the edge stay available for resizing, which is what the
        // platform does with the band it gets back there.
        for (name, x) in [
            ("thumb", g.thumb.x + 1.0),
            ("bar's outer column", g.thumb.right() - 1.0),
        ] {
            assert_eq!(
                app.grip_at(x, mid, band),
                grip,
                "{mode:?}: {name} must be handed back to the app"
            );
        }
        assert_eq!(
            app.grip_at(near_edge, mid, band),
            band,
            "{mode:?}: the outermost columns stay the platform's to resize"
        );
        // Away from the bar the whole band is available: a point on the far
        // edge is inside the band's width but nothing the app draws on.
        assert_eq!(
            app.grip_at(1.0, mid, band),
            band,
            "{mode:?}: the far edge is untouched by the bar"
        );
    }
}

#[test]
fn a_wheel_notch_glides_over_frames_instead_of_jumping() {
    // The choppiness is the *step*, not the speed: a notch that lands in
    // one frame is four or five visible jumps. Nothing moves on the
    // notch's own frame, the host is asked for a clock fast enough to
    // animate on, and the travel arrives over several frames.
    let mut app = open("editor_test_glide.txt", &"line\n".repeat(400));
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    assert_eq!(app.scroll.offset, 0.0);
    app.on_event(Event::Scroll { dx: 0.0, dy: -48.0 }, &mut host);
    assert_eq!(app.scroll.offset, 0.0, "a notch must not land in one jump");
    assert!(
        host.animating,
        "a glide needs frames the heartbeat cannot give"
    );
    let mut seen = vec![app.scroll.offset];
    for _ in 0..600 {
        if !app.scrolling {
            break;
        }
        app.on_event(Event::Frame, &mut host);
        seen.push(app.scroll.offset);
    }
    assert!(
        seen.len() > 5,
        "a notch should arrive over several frames, not {}: {seen:?}",
        seen.len()
    );
    for w in seen.windows(2) {
        assert!(
            w[1] > w[0],
            "each frame must move a little, and never backwards: {seen:?}"
        );
    }
    // It lands exactly where the notch pointed, and the clock stops with
    // it — a host left animating forever is a host repainting forever.
    assert_eq!(app.scroll.offset, 48.0);
    assert!(!app.scrolling);
    assert!(!host.animating);
}

#[test]
fn a_finger_pan_follows_the_input_and_drops_a_glide() {
    let mut app = open("editor_test_pan.txt", &"line\n".repeat(400));
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    // Direct manipulation tracks the input exactly: gliding a finger's own
    // movement would read as lag, not as smoothness.
    app.on_event(
        Event::Pan {
            dx: 0.0,
            dy: -17.0,
            x: 0.0,
            y: 0.0,
        },
        &mut host,
    );
    assert_eq!(app.scroll.offset, 17.0);
    assert!(
        !app.scrolling,
        "no glide stands between the finger and the view"
    );
    // A finger landing mid-glide takes the view over: the notch's leftover
    // travel is dropped rather than resumed under the finger.
    app.on_event(Event::Scroll { dx: 0.0, dy: -48.0 }, &mut host);
    assert!(app.scrolling);
    app.on_event(
        Event::Pan {
            dx: 0.0,
            dy: -5.0,
            x: 0.0,
            y: 0.0,
        },
        &mut host,
    );
    assert_eq!(app.scroll.offset, 22.0);
    assert_eq!(
        app.scroll.target, 22.0,
        "the glide is abandoned, not pending"
    );
    assert!(!app.scrolling);
    assert!(!host.animating, "the frame clock goes with the glide");
}

#[test]
fn a_flick_coasts_past_the_finger() {
    // The report: touch scrolling should "coast a little after scroll".
    // PanEnd carries the finger's release velocity; the app hands it to
    // the scroller and runs the frame clock, and the view keeps moving
    // with no further input until the momentum is spent.
    let mut app = open("editor_test_fling.txt", &"line\n".repeat(400));
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    // Drag down (dy < 0 → content down … see Pan's sign) then lift with
    // the motion still going: a solid 1.5 px/ms upward flick.
    for _ in 0..5 {
        app.on_event(
            Event::Pan {
                dx: 0.0,
                dy: -30.0,
                x: 0.0,
                y: 0.0,
            },
            &mut host,
        );
    }
    assert_eq!(app.scroll.offset, 150.0);
    app.on_event(Event::PanEnd { vx: 0.0, vy: -1.5 }, &mut host);
    assert!(app.scrolling, "a flick needs the frame clock");
    assert!(host.animating);
    assert!(app.scroll.coast != 0.0);
    assert_eq!(app.scroll.target, 150.0, "a coast has no destination");
    // Frames keep the ride moving past the finger's last touch, in the
    // same direction, decaying — never reversing, never snapping to a
    // destination.
    let mut prev = app.scroll.offset;
    assert!(prev > 0.0);
    for _ in 0..12 {
        app.on_event(Event::Frame, &mut host);
        assert!(
            app.scroll.offset >= prev,
            "a coast never reverses: {} after {}",
            app.scroll.offset,
            prev
        );
        prev = app.scroll.offset;
    }
    assert!(prev > 160.0, "the coast travelled past the finger: {prev}");
    // The clock stops when the ride is over, and the last frame lands.
    while app.scrolling {
        app.on_event(Event::Frame, &mut host);
    }
    assert!(!host.animating);
    assert_eq!(app.scroll.offset, app.scroll.target);
}

#[test]
fn a_flick_keeps_its_coast_when_the_editor_band_rebuilds() {
    // The report: touch inertia works in Preview and dies in Edit. The
    // editor pane is a *band* (T1): a coast long enough to leave the rows it
    // covers makes the next paint rebuild it, and the geometry install that
    // follows used to end every ride — `scroll_by(0.0)` resets the coast —
    // which stops the flick dead about a screen in. Preview keeps a small
    // file's whole list, so it never rebuilds and never showed the bug; Split
    // rebuilds the editor half and stutters. Every mode is walked here
    // because the two pans and the two relayout paths must agree.
    for mode in [Mode::Edit, Mode::Split] {
        let mut app = open(
            &format!("editor_test_fling_band_{mode:?}.txt"),
            &"line\n".repeat(2000),
        );
        app.set_mode(mode);
        let mut host = NullHost::default();
        let mut canvas = app.render_headless(800, 600, 1.0);
        assert!(
            !app.editor_band_stale(),
            "{mode:?}: the first paint builds the band"
        );

        app.on_event(
            Event::Pan {
                dx: 0.0,
                dy: -40.0,
                x: 400.0,
                y: 300.0,
            },
            &mut host,
        );
        app.on_event(Event::PanEnd { vx: 0.0, vy: -2.0 }, &mut host);
        assert!(
            app.scroll.coast != 0.0,
            "{mode:?}: the flick starts a coast"
        );

        let start = app.scroll.offset;
        let mut frames = 0;
        let mut rebuilt = false;
        while app.scrolling && frames < 600 {
            app.on_event(Event::Frame, &mut host);
            let stale = app.editor_band_stale();
            app.paint(&mut canvas);
            rebuilt |= stale;
            frames += 1;
            assert!(
                app.scroll.coast != 0.0 || !app.scrolling,
                "{mode:?}: the coast died under a band rebuild {} px in",
                app.scroll.offset - start
            );
        }
        assert!(
            rebuilt,
            "{mode:?}: the flick never crossed a band boundary, so this proves nothing"
        );
        assert!(
            app.scroll.offset - start > 600.0,
            "{mode:?}: the flick coasted only {} px",
            app.scroll.offset - start
        );
    }
}

#[test]
fn a_wheel_glide_survives_a_band_rebuild() {
    // The same install that killed a coast took a glide's destination with it:
    // a PageDown in Edit mode crosses the editor band, the rebuild re-installed
    // the scroll geometry and the remaining travel was dropped mid-ride. The
    // destination is the wheel's, and re-installing the same document at the
    // same place must leave it alone.
    let mut app = open("editor_test_pagedown_band.txt", &"line\n".repeat(2000));
    app.set_mode(Mode::Edit);
    let mut host = NullHost::default();
    let mut canvas = app.render_headless(800, 600, 1.0);
    app.on_event(
        Event::Scroll {
            dx: 0.0,
            dy: -2000.0,
        },
        &mut host,
    );
    assert_eq!(
        app.scroll.offset, 0.0,
        "a big step glides, it does not jump"
    );
    assert_eq!(app.scroll.target, 2000.0);
    let mut rebuilt = false;
    for _ in 0..600 {
        if !app.scrolling {
            break;
        }
        app.on_event(Event::Frame, &mut host);
        rebuilt |= app.editor_band_stale();
        app.paint(&mut canvas);
    }
    assert!(
        rebuilt,
        "the glide never crossed a band boundary, so this proves nothing"
    );
    assert_eq!(
        app.scroll.offset, 2000.0,
        "the wheel's destination is still where it lands"
    );
    assert!(!app.scrolling, "and the clock ends with the ride");
}

#[test]
fn a_split_finger_moves_the_pane_under_it_one_pixel_per_pixel() {
    // Split links its halves by shared *progress*, not by pixels: each pane
    // paints a ratio of its own range, and a range is only a share of the
    // longer one. A finger's `dy` is in the pane it is over, so that share
    // has to be divided out — measured before the fix, a wrapping Markdown
    // preview put the editor at 0.63, and the text under the finger travelled
    // 63 px for 100 px of finger while the far half kept up.
    let text = "# heading

some words in a paragraph that wraps because markdown                 wraps at the pane

"
    .repeat(400);
    let mut app = open("editor_test_split_pan.md", &text);
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    let editor_share = app.pane_share(true);
    let preview_share = app.pane_share(false);
    assert!(
        editor_share < 0.9 && preview_share > editor_share,
        "the fixture needs unequal panes, or this proves nothing:          editor {editor_share}, preview {preview_share}"
    );
    // The finger is over the editor half: the editor is what must keep up.
    let area = app.editor_area();
    let (fx, fy) = (area.x + area.w / 2.0, area.y + area.h / 2.0);
    let before = app.editor_scroll();
    for _ in 0..5 {
        app.on_event(
            Event::Pan {
                dx: 0.0,
                dy: -20.0,
                x: fx,
                y: fy,
            },
            &mut host,
        );
    }
    assert!(
        (app.editor_scroll() - before - 100.0).abs() < 0.05,
        "the pane under the finger tracks it: moved {} px for 100 px of finger",
        app.editor_scroll() - before
    );
    // And the flick it threw coasts at the speed the finger let go at, in
    // the same pane's pixels — not at a share of it.
    app.on_event(Event::PanEnd { vx: 0.0, vy: -2.0 }, &mut host);
    assert!(
        (app.scroll.coast * editor_share - 2.0).abs() < 0.05,
        "a 2 px/ms flick coasts the editor at {} px/ms",
        app.scroll.coast * editor_share
    );

    // The other half is the same rule: a finger over the preview tracks the
    // preview, and the editor follows at its own share of the progress.
    let view = app.viewport();
    let (px, py) = (view.x + view.w / 4.0, view.y + view.h / 2.0);
    let before = app.preview_scroll();
    for _ in 0..5 {
        app.on_event(
            Event::Pan {
                dx: 0.0,
                dy: -20.0,
                x: px,
                y: py,
            },
            &mut host,
        );
    }
    assert!(
        (app.preview_scroll() - before - 100.0).abs() < 0.05,
        "the preview tracks a finger over it: moved {} px for 100 px of finger",
        app.preview_scroll() - before
    );
}

#[test]
fn a_release_that_held_still_stops_the_ride() {
    // The finger arrived, paused, then lifted: ~0 velocity, no coast. The
    // clock ends and the view stays exactly where the finger left it.
    let mut app = open("editor_test_stop.txt", &"line\n".repeat(400));
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    app.on_event(
        Event::Pan {
            dx: 0.0,
            dy: -30.0,
            x: 0.0,
            y: 0.0,
        },
        &mut host,
    );
    assert_eq!(app.scroll.offset, 30.0);
    app.on_event(Event::PanEnd { vx: 0.0, vy: 0.0 }, &mut host);
    assert!(!app.scrolling);
    assert!(app.scroll.coast == 0.0);
    assert_eq!(app.scroll.offset, 30.0);
}

#[test]
fn a_wheel_notch_ends_a_coast_rather_than_fighting_it() {
    // Mid-coast, the wheel speaks: the ride must end and the glide start
    // *from where the view now is* — a glide to the coast's stale target
    // would yank the view backwards under the user's hand.
    let mut app = open("editor_test_wc.txt", &"line\n".repeat(400));
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    app.on_event(Event::PanEnd { vx: 0.0, vy: -2.0 }, &mut host);
    assert!(app.scroll.coast != 0.0);
    app.on_event(Event::Frame, &mut host);
    let mid_coast = app.scroll.offset;
    assert!(mid_coast > 0.0);
    app.on_event(Event::Scroll { dx: 0.0, dy: -48.0 }, &mut host);
    assert!(app.scroll.coast == 0.0, "the wheel ends the ride");
    assert!(app.scrolling, "and runs its own clock");
    assert_eq!(app.scroll.target, mid_coast + 48.0);
    while app.scrolling {
        app.on_event(Event::Frame, &mut host);
    }
    assert_eq!(
        app.scroll.offset,
        mid_coast + 48.0,
        "landed ahead, not back"
    );
}

#[test]
fn scrollbar_thumb_drag_scrolls() {
    let mut app = open("editor_test_sbd.txt", &"line\n".repeat(200));
    let _ = app.render_headless(800, 600, 1.0);
    let area = app.viewport();
    let g = app
        .preview
        .v_sb(area, app.scroll.offset, app.metrics())
        .unwrap();
    assert!(app.scroll.offset < 10.0, "start at top");
    let mut host = NullHost::default();
    // Press on the thumb, drag to the bottom of the track.
    app.on_event(
        Event::MouseDown {
            x: g.thumb.x + 2.0,
            y: g.thumb.y + 4.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.sb_drag.is_some(), "thumb press starts a drag");
    app.on_event(
        Event::MouseMove {
            x: g.thumb.x + 2.0,
            y: g.start + g.travel,
        },
        &mut host,
    );
    assert!(
        app.scroll.offset > g.max * 0.9,
        "drag to bottom: {} / {}",
        app.scroll.offset,
        g.max
    );
    // Release ends the drag.
    app.on_event(
        Event::MouseUp {
            x: g.thumb.x + 2.0,
            y: g.start + g.travel,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.sb_drag.is_none());
    // A track click below the thumb pages down and grabs the thumb.
    app.scroll.offset = 0.0;
    let g2 = app.preview.v_sb(area, 0.0, app.metrics()).unwrap();
    app.on_event(
        Event::MouseDown {
            x: g2.thumb.x + 2.0,
            y: g2.thumb.y + g2.thumb.h + 20.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(app.scroll.offset > 100.0, "track click pages down");
    assert!(app.sb_drag.is_some(), "track click grabs the thumb");
}

#[test]
fn a_maximized_window_keeps_the_rect_it_restores_to() {
    // The window the user reopens wants the size and place they were using
    // *before* maximizing: a maximized report carries the work area, and
    // restoring that would hand back a screen-sized window as soon as it
    // was un-maximized.
    let mut app = open("editor_test_win.txt", "hello\n");
    let mut host = NullHost::default();
    let normal = WindowGeometry {
        x: -1200,
        y: 60,
        width: 1100,
        height: 800,
        maximized: false,
    };
    app.on_event(Event::WindowGeometry(normal), &mut host);
    assert_eq!(app.settings().window, Some((-1200, 60, 1100, 800)));
    assert!(!app.settings().maximized);
    app.on_event(
        Event::WindowGeometry(WindowGeometry {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
            maximized: true,
        }),
        &mut host,
    );
    assert_eq!(
        app.settings().window,
        Some((-1200, 60, 1100, 800)),
        "maximizing must not overwrite the rect to restore at"
    );
    assert!(app.settings().maximized);
    // Un-maximizing reports the real rect again, and a later move updates
    // it — the layout the OS restored is authoritative either way.
    let moved = WindowGeometry {
        x: 300,
        y: 200,
        width: 900,
        height: 700,
        maximized: false,
    };
    app.on_event(Event::WindowGeometry(moved), &mut host);
    assert_eq!(app.settings().window, Some((300, 200, 900, 700)));
    assert!(!app.settings().maximized);
}

#[test]
fn leaving_remembers_the_window() {
    // `persist` runs from many places, but the window's rect is only
    // readable while the window exists, so the way *out* is the one that
    // has to write the last one.
    let mut app = open("editor_test_quit.txt", "hello\n");
    let mut host = NullHost::default();
    app.on_event(
        Event::WindowGeometry(WindowGeometry {
            x: 300,
            y: 200,
            width: 900,
            height: 700,
            maximized: false,
        }),
        &mut host,
    );
    app.quit(&mut host);
    assert!(host.quit_requested, "quitting still asks the host to quit");
    assert_eq!(app.settings().window, Some((300, 200, 900, 700)));
}

/// The rect a test's window is reported at, normal (not maximized).
fn geometry(x: i32, y: i32, width: u32, height: u32) -> WindowGeometry {
    WindowGeometry {
        x,
        y,
        width,
        height,
        maximized: false,
    }
}

#[test]
fn a_moved_window_is_written_without_a_clean_exit() {
    // The session is written on the way out, but a killed process, a crash
    // or a power cut never gets there — so the heartbeat has to write a
    // reported geometry on its own, or the next launch reopens at the rect
    // of the last *clean* exit.
    let mut app = open("editor_test_geom_beat.txt", "hello\n");
    let mut host = NullHost::default();
    app.on_event(
        Event::WindowGeometry(geometry(300, 200, 900, 700)),
        &mut host,
    );
    assert!(app.geometry_dirty, "a report is queued for writing");
    for _ in 0..GEOMETRY_SAVE_TICKS - 1 {
        app.on_event(Event::Tick, &mut host);
    }
    assert!(
        app.geometry_dirty,
        "not written while the window is still settling"
    );
    app.on_event(Event::Tick, &mut host);
    assert!(!app.geometry_dirty, "written once the window held still");
    assert_eq!(app.settings().window, Some((300, 200, 900, 700)));
}

#[test]
fn a_repeated_report_cannot_starve_the_write() {
    // Some platforms re-report a rect they have already reported (a state
    // change the window manager echoes back). If every report restarted
    // the wait, a window that only ever re-reported would never reach the
    // file at all.
    let mut app = open("editor_test_geom_repeat.txt", "hello\n");
    let mut host = NullHost::default();
    let g = geometry(10, 20, 800, 600);
    app.on_event(Event::WindowGeometry(g), &mut host);
    for _ in 0..GEOMETRY_SAVE_TICKS {
        app.on_event(Event::WindowGeometry(g), &mut host);
        app.on_event(Event::Tick, &mut host);
    }
    assert!(
        !app.geometry_dirty,
        "the same rect re-reported is still one pending write"
    );
    assert_eq!(app.settings().window, Some((10, 20, 800, 600)));
}

#[test]
fn a_drag_writes_once_at_the_rect_it_ended_on() {
    // A move reports every intermediate rect. Each one defers the write
    // (so a long drag is not a write per frame), and the one that lands is
    // the rect the gesture ended on.
    let mut app = open("editor_test_geom_drag.txt", "hello\n");
    let mut host = NullHost::default();
    for i in 0..6 {
        app.on_event(
            Event::WindowGeometry(geometry(100 + i * 40, 150 + i * 10, 900, 700)),
            &mut host,
        );
        for _ in 0..5 {
            app.on_event(Event::Tick, &mut host);
        }
        assert!(app.geometry_dirty, "mid-drag the write is still pending");
    }
    for _ in 0..GEOMETRY_SAVE_TICKS {
        app.on_event(Event::Tick, &mut host);
    }
    assert_eq!(
        app.settings().window,
        Some((300, 200, 900, 700)),
        "the rect the drag ended on is the one remembered"
    );
}

#[test]
fn a_maximized_window_is_written_even_though_its_rect_is_not() {
    // Maximizing changes what the next session does (open maximized)
    // without changing the rect it reopens at, so the change has to be
    // queued on its own — the rect of a maximized report is the work area,
    // which must not replace the one to restore.
    let mut app = open("editor_test_geom_max.txt", "hello\n");
    let mut host = NullHost::default();
    app.restore_window(Some((120, 80, 1100, 800)), false);
    app.on_event(
        Event::WindowGeometry(WindowGeometry {
            maximized: true,
            ..geometry(0, 0, 1920, 1040)
        }),
        &mut host,
    );
    assert!(app.geometry_dirty, "the maximize is queued for writing");
    for _ in 0..GEOMETRY_SAVE_TICKS {
        app.on_event(Event::Tick, &mut host);
    }
    assert!(!app.geometry_dirty);
    assert_eq!(app.settings().window, Some((120, 80, 1100, 800)));
    assert!(app.settings().maximized);
    // A first report that only confirms the restored state is not a change,
    // so a session reopened maximized does not rewrite itself on launch.
    let mut again = open("editor_test_geom_max2.txt", "hello\n");
    again.restore_window(Some((120, 80, 1100, 800)), true);
    again.on_event(
        Event::WindowGeometry(WindowGeometry {
            maximized: true,
            ..geometry(0, 0, 1920, 1040)
        }),
        &mut host,
    );
    assert!(!again.geometry_dirty);
}

#[test]
fn a_saved_window_is_what_the_platform_is_asked_to_open_at() {
    let mut app = open("editor_test_win2.txt", "hello\n");
    let mut host = NullHost::default();
    // Nothing saved yet (a first run): the platform places the window.
    assert_eq!(app.initial_window(), None);
    app.restore_window(Some((120, 80, 1100, 800)), true);
    assert_eq!(
        app.initial_window(),
        Some(WindowGeometry {
            x: 120,
            y: 80,
            width: 1100,
            height: 800,
            maximized: true,
        })
    );
    // And the whole thing survives a settings round trip, which is what
    // the next launch reads.
    let round_tripped = app.settings();
    assert_eq!(round_tripped.window, Some((120, 80, 1100, 800)));
    assert!(round_tripped.maximized);
    let mut next = open("editor_test_win3.txt", "hello\n");
    next.restore_window(round_tripped.window, round_tripped.maximized);
    next.on_event(
        Event::WindowGeometry(WindowGeometry {
            x: 0,
            y: 0,
            width: 2560,
            height: 1400,
            maximized: true,
        }),
        &mut host,
    );
    assert_eq!(
        next.initial_window(),
        Some(WindowGeometry {
            x: 120,
            y: 80,
            width: 1100,
            height: 800,
            maximized: true,
        }),
        "a maximized first report must not lose the saved rect"
    );
}

#[test]
fn a_stranded_scroll_drag_ends_on_the_next_press() {
    // Regression: the thumb follows the pointer for as long as a drag is
    // live, and only `MouseUp` ends it. A release the platform cannot
    // deliver — the pointer let go outside the client area, the window
    // deactivated mid-drag — used to leave the bar glued to the pointer,
    // scrolling on every move with no button held, until a later click
    // happened to free it. The click that starts the next gesture is where
    // that has to end, so a stranded drag can never outlive one press.
    let mut app = open("editor_test_stranded.txt", &"line\n".repeat(200));
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    let area = app.viewport();
    let g = app
        .preview
        .v_sb(area, app.scroll.offset, app.metrics())
        .unwrap();
    app.on_event(
        Event::MouseDown {
            x: g.thumb.x + 2.0,
            y: g.thumb.y + 4.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(
        app.sb_drag.is_some(),
        "setup: the press starts a thumb drag"
    );
    app.on_event(
        Event::MouseMove {
            x: g.thumb.x + 2.0,
            y: g.start + g.travel,
        },
        &mut host,
    );
    let dragged = app.scroll.offset;
    assert!(dragged > 100.0, "setup: the drag scrolls to the bottom");
    // The release never arrives. The user presses a line of text instead.
    app.on_event(
        Event::MouseDown {
            x: 40.0,
            y: 200.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert!(
        app.sb_drag.is_none(),
        "the next press must free a drag whose release was lost"
    );
    // ...and from there the pointer moves without touching the bar.
    app.on_event(
        Event::MouseMove {
            x: g.thumb.x + 2.0,
            y: g.start,
        },
        &mut host,
    );
    assert_eq!(
        app.scroll.offset, dragged,
        "a pointer with no button on it must not drag the bar"
    );
}

#[test]
fn gutter_click_does_not_place_caret() {
    let mut app = open("editor_test_gut.txt", "hello world\nsecond line");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
    app.on_event(
        Event::KeyDown {
            key: Key::Right,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    assert_eq!(app.editor.caret.col, 1);
    // A click inside the line-number rail must not move the caret.
    let y = app.viewport().y + 10.0;
    app.on_event(
        Event::MouseDown {
            x: 5.0,
            y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.editor.caret.col, 1, "gutter click must be ignored");
    // The pointer is an arrow over the rail, not an I-beam.
    assert_eq!(app.cursor_at(5.0, y), CursorShape::Arrow);
}

#[test]
fn double_and_triple_click_select_word_and_line() {
    let mut app = open("editor_test_dbl.txt", "hello world\nsecond line");
    let mut host = NullHost::default();
    app.on_event(ctrl('e'), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
    let ctx = app.ctx();
    let p = crate::layout::text::plain_params(800.0, &ctx, false);
    let rail = gutter::rail_w(&app.fonts, &ctx, app.buffer.line_count());
    // Middle of "world" (6.5 mono cells past the line start).
    let adv = app
        .fonts
        .advance(crate::gfx::font::FontStyle::Mono, 'o', p.size);
    let x = app.viewport().x + rail + p.x0 + 6.5 * adv;
    let y = app.viewport().y + p.pad + p.line_h / 2.0;
    let click = |app: &mut App, host: &mut NullHost| {
        app.on_event(
            Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
            },
            host,
        );
    };
    click(&mut app, &mut host); // single: places the caret
    assert!(app.editor.selection().is_none());
    click(&mut app, &mut host); // double: selects the word
    assert_eq!(app.editor.sel_text(&app.buffer), "world");
    click(&mut app, &mut host); // triple: selects the whole line
    assert_eq!(app.editor.sel_text(&app.buffer), "hello world");
}

#[test]
fn a_horizontal_pan_over_the_split_preview_pane_does_not_drive_the_editor() {
    let mut app = open("editor_test_hsplit.md", "# one\nwordy line\n");
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    // The pointer rests over the preview (left) half.
    let (left, _) = app.split_panes();
    app.on_event(
        Event::MouseMove {
            x: left.x + left.w / 2.0,
            y: left.y + left.h / 2.0,
        },
        &mut host,
    );
    let before = app.h_scroll;
    app.on_event(
        Event::Pan {
            dx: -60.0,
            dy: 0.0,
            x: left.x + left.w / 2.0,
            y: left.y + left.h / 2.0,
        },
        &mut host,
    );
    assert_eq!(
        app.h_scroll, before,
        "a touch pan over the preview half must not scroll the editor pane sideways"
    );
    // Over the editor half the same gesture does drive it (when the
    // editor has something to scroll: a wide no-fold line). The buffer
    // and size settle first, then the finger lands on the right half.
    let long = "z".repeat(400);
    app.buffer = Buffer::from_text(&format!("# one\n{long}\n"));
    app.layout_dirty = true;
    let _ = app.render_headless(800, 600, 1.0);
    assert!(
        app.h_max > 0.0,
        "the wide no-wrap line must give the editor pane room to scroll"
    );
    let (_, right) = app.split_panes();
    let ex = right.x + right.w / 2.0;
    app.on_event(
        Event::MouseMove {
            x: ex,
            y: right.y + right.h / 2.0,
        },
        &mut host,
    );
    app.on_event(
        Event::Pan {
            dx: -60.0,
            dy: 0.0,
            x: ex,
            y: right.y + right.h / 2.0,
        },
        &mut host,
    );
    assert!(
        app.h_scroll > before,
        "a touch pan over the editor half must scroll the editor pane"
    );
}

#[test]
fn a_wheel_horizontal_over_the_split_preview_pane_is_ignored_too() {
    let mut app = open("editor_test_hwheel.md", "# one\nwordy line\n");
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let (left, _) = app.split_panes();
    app.on_event(
        Event::MouseMove {
            x: left.x + left.w / 2.0,
            y: left.y + left.h / 2.0,
        },
        &mut host,
    );
    let before = app.h_scroll;
    app.on_event(Event::Scroll { dx: -40.0, dy: 0.0 }, &mut host);
    assert_eq!(
        app.h_scroll, before,
        "a horizontal wheel over the preview half must not scroll the editor"
    );
}

/// R1's verify: the widest-line rule must give the same answer through
/// all three install paths — `relayout`'s Edit branch, `install_preview`
/// in Split, and the band path inside `relayout_band` — on a fixture
/// mixing the shapes that made the hand-rolled scans disagree (long
/// ASCII, tab-expanded rows, accented Latin, CJK).
#[test]
fn r1_widest_rule_agrees_across_the_three_installs() {
    let text = format!(
        "{}\n\t\t@GetMapping\t\tvalue\n{}\n{}\ntail\n",
        "x".repeat(300),
        "é".repeat(240),
        "日本".repeat(120),
    );
    let mut app = open("r1_widest_test.rs", &text);
    let mut host = NullHost::default();
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    // The Edit branch (widest_width through relayout).
    app.set_mode(Mode::Edit);
    let _ = app.render_headless(1100, 800, 1.0);
    let h_edit = app.h_max;
    let ew_edit = app.editor_area().w;
    // install_preview's builder in Split (widest_width).
    app.set_mode(Mode::Split);
    let _ = app.render_headless(1100, 800, 1.0);
    let h_split = app.h_max;
    // The band path (widest_width through the `tab.widest` cache) —
    // called in Split, where its `mode != Preview` guard lets it build
    // h_max.
    let view = app.viewport();
    app.relayout_band(1100.0, view.h, app.editor_area().w, 0.0, false);
    let h_band = app.h_max;
    // The shared quantity is widest + rail + 32 px; each mode stores
    // h_max minus its own editor width, so compare h_max + edit_w.
    let total_edit = h_edit + ew_edit;
    let total_split = h_split + app.editor_area().w;
    let total_band = h_band + app.editor_area().w;
    assert!(
        total_edit.is_finite() && total_edit > 0.0,
        "the Edit branch computed a nonsensical extent"
    );
    assert!(
        (total_edit - total_split).abs() <= 0.5,
        "Edit vs install_preview disagree on the widest row: {total_edit} vs {total_split}"
    );
    assert!(
        (total_split - total_band).abs() <= 0.5,
        "install_preview vs the band disagree on the widest row: {total_split} vs {total_band}"
    );
}

#[test]
fn a_streaming_tab_spins_in_its_preview_until_the_seal_lands() {
    let dir = crate::testing::temp_dir().join("editor_stream_gauge");
    std::fs::create_dir_all(&dir).unwrap();
    // Over the 1 MB head threshold, so the tab carries an in-flight
    // streamer rather than a finished buffer.
    let file = dir.join("big.txt");
    let body = "line\n".repeat(220_000);
    std::fs::write(&file, &body).unwrap();
    let mut app = App::new_streaming(Some(file.clone()), false).expect("open streaming");
    assert!(app.streamer.is_some(), "fixture must be streaming");
    // Preview is the default mode: the pane paints, and the corner where
    // the gauge sits carries ink while the load is in flight.
    let mut host = NullHost::default();
    let c = app.render_headless(800, 600, 1.0);
    let corner = gauge_pixels(&c);
    assert!(
        corner > 0,
        "a streaming tab must show the load gauge in the preview pane"
    );
    // While streaming, the preview is the FAST band pass, not the kind's
    // whole-document layout: the status stays bare until the seal (the
    // kind pane writes its own status_extra; the fast path clears it).
    assert_eq!(
        app.status_extra, "",
        "the kind layout must not run while the stream is live"
    );
    // Pump the stream to the end through the real tick path: the seal
    // clears the streamer, and the next frame's corner must be bare.
    let full = body.len();
    let mut guard = 0;
    while app.streamer.is_some() {
        app.on_event(Event::Tick, &mut host);
        guard += 1;
        assert!(guard < 4000, "the pump never sealed (full={full})");
    }
    let c = app.render_headless(800, 600, 1.0);
    assert_eq!(gauge_pixels(&c), 0, "a sealed tab must not keep spinning");
}

#[test]
fn a_sealed_stream_turns_the_plain_band_into_the_kind_layout() {
    let dir = crate::testing::temp_dir().join("editor_stream_seal_layout");
    std::fs::create_dir_all(&dir).unwrap();
    // Over the 1 MB head, so the tab streams; code carries three colour
    // classes (comment, keyword, string), which makes the plain band
    // unmistakable from the highlighted pane.
    let file = dir.join("big.rs");
    let body = "// note\nfn f(x: &str) -> usize { x.len() }\n".repeat(30_000);
    std::fs::write(&file, &body).unwrap();
    let mut app = App::new_streaming(Some(file), false).expect("open streaming");
    assert!(app.streamer.is_some(), "fixture must be streaming");
    let mut host = NullHost::default();
    // The frame the open paints: the fast band, one viewport of plain rows.
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.streamer.is_some(), "the open frame must not seal");
    let band_ops = app.preview.list.ops.len();
    let band_colours = text_colours(&app.preview.list);
    assert!(
        band_ops < 400,
        "the band must be the viewport, not the document ({band_ops} ops)"
    );
    assert!(
        band_colours <= 1,
        "the band paints one colour, the theme fg ({band_colours})"
    );
    // Pump to the seal, repainting after every tick — the real app's
    // cadence, where a dirty flag only lives until the next paint. The
    // seal's relayout must still reach the screen.
    let mut guard = 0;
    while app.streamer.is_some() {
        app.on_event(Event::Tick, &mut host);
        guard += 1;
        assert!(guard < 4000, "the pump never sealed");
        let _ = app.render_headless(800, 600, 1.0);
    }
    // The seal starts the code pane's plan on the worker (C1): the plan's
    // tint checkpoints are whole-document work, and a 60k-line file is one,
    // while the tinted prefix keeps the seal frame's colours on screen.
    assert!(
        app.preview_job_pending(),
        "a big code pane's plan builds on the worker with its checkpoints filled"
    );
    // Until it lands the pane still paints its band, not a blank.
    assert!(
        app.preview.list.height > 500_000.0,
        "the waiting pane still carries the document's height ({:.0} px)",
        app.preview.list.height
    );
    let mut guard = 0;
    while app.preview_job_pending() {
        app.on_event(Event::Tick, &mut host);
        guard += 1;
        assert!(guard < 4000, "the code plan never landed");
        let _ = app.render_headless(800, 600, 1.0);
    }
    assert!(app.pane_plan.is_some(), "the sealed pane must be planned");
    let list = &app.preview.list;
    assert!(
        list.height > 500_000.0,
        "the plan must cover the whole document ({:.0} px)",
        list.height
    );
    assert!(
        list.ops.len() < 400,
        "the sealed preview must hold the viewport, not every line ({} ops)",
        list.ops.len()
    );
    assert!(
        text_colours(list) >= 3,
        "the sealed preview must keep its syntax colours ({})",
        text_colours(list)
    );
    // Scrolling to the end paints the end: the band is read from the plan,
    // so nothing above it is ever laid out.
    let height = app.tabs[app.active].scroll.content_height;
    app.set_scroll(height - 500.0);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(
        app.preview.list.ops.len() < 400,
        "a scrolled band stays bounded ({} ops)",
        app.preview.list.ops.len()
    );
    assert!(
        app.preview
            .list
            .ops
            .iter()
            .any(|op| matches!(op, DrawOp::Text { text, .. } if text.contains("fn"))),
        "the far band must paint real code rows"
    );
}

/// Distinct text colours in a display list: the quickest way to tell a
/// plain band (one colour) from the kind's highlighted pane.
fn text_colours(list: &crate::layout::display_list::DisplayList) -> usize {
    use std::collections::HashSet;
    list.ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Text { color, .. } => Some((color.r, color.g, color.b)),
            _ => None,
        })
        .collect::<HashSet<_>>()
        .len()
}

/// Ink in the bottom-right corner square where the load gauge paints
/// (inside the pane, clear of the scrollbar and the status bar).
fn gauge_pixels(c: &Canvas) -> usize {
    let mut n = 0;
    for y in 490..540 {
        for x in 730..770 {
            let p = c.pixels[y * 800 + x];
            let r = (p >> 16) & 0xFF;
            let g = (p >> 8) & 0xFF;
            let b = p & 0xFF;
            if (r + g + b) / 3 < 160 {
                n += 1;
            }
        }
    }
    n
}

/// Open a document with the plan threshold lowered, so a small fixture
/// exercises the banded path the threshold normally reserves for big
/// files.
fn planned_app(name: &str, ext: &str, body: &str, units: usize) -> App {
    let dir = crate::testing::temp_dir().join("editor_planned_panes");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.{ext}"));
    std::fs::write(&file, body).unwrap();
    let mut app = App::new_streaming(Some(file), true).expect("open");
    app.virtual_units = units;
    app.layout_dirty = true;
    app
}

#[test]
fn a_big_plain_text_pane_is_planned_and_banded() {
    let mut body = String::new();
    for i in 0..3_000 {
        body.push_str(&format!("row {i} {}\n", "text ".repeat(i % 12)));
    }
    let mut app = planned_app("planned_plain", "txt", &body, 500);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.pane_plan.is_some(), "above the threshold: planned");
    let height = app.preview.list.height;
    assert!(height > 10_000.0, "the plan covers the document: {height}");
    assert!(
        app.preview.list.ops.len() < 200,
        "a band, not the document ({} ops)",
        app.preview.list.ops.len()
    );
    // The far end paints the last rows, from the plan alone.
    app.set_scroll(height - 400.0);
    let _ = app.render_headless(800, 600, 1.0);
    let last = "row 2999";
    assert!(
        app.preview
            .list
            .ops
            .iter()
            .any(|op| matches!(op, DrawOp::Text { text, .. } if text.contains(last))),
        "the band at the end must show the last lines"
    );
}

#[test]
fn a_big_log_is_planned_banded_and_keeps_its_counts() {
    let mut body = String::new();
    for i in 0..4_000 {
        body.push_str(&format!(
            "2026-10-17T12:00:00Z {} [worker-1] request {i} finished\n",
            if i % 5 == 0 { "ERROR" } else { "INFO" }
        ));
    }
    let mut app = planned_app("planned_log", "log", &body, 500);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.pane_plan.is_some());
    assert!(
        app.tabs[app.active].status_extra.contains("error"),
        "the status line still counts levels: {}",
        app.tabs[app.active].status_extra
    );
    assert!(app.preview.list.ops.len() < 200);
    // Logs are tinted by level: the band carries more than one colour.
    assert!(text_colours(&app.preview.list) >= 2);
}

#[test]
fn a_big_markdown_document_is_planned_and_banded() {
    let mut body = String::new();
    for i in 0..80 {
        body.push_str(&format!("## Section {i}\n\n"));
        body.push_str(
            "A paragraph with **bold**, `code` and a [link](https://example.com) that is \
                 long enough to wrap across the reading column at least once.\n\n",
        );
        body.push_str(&format!("```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n\n"));
    }
    let mut app = planned_app("planned_md", "md", &body, 40);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.pane_plan.is_some(), "80 blocks: planned");
    let height = app.preview.list.height;
    assert!(height > 4_000.0, "the plan covers the document: {height}");
    assert!(
        app.preview.list.ops.len() < 600,
        "a band of blocks, not the document ({} ops)",
        app.preview.list.ops.len()
    );
    // A block in the middle of the document is painted at its planned y.
    app.set_scroll(height * 0.5);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(
        app.preview
            .list
            .ops
            .iter()
            .any(|op| matches!(op, DrawOp::Text { text, .. } if text.starts_with("Section"))),
        "the middle band must show a heading"
    );
}

/// S2 · Scrolling inside one markdown band reuses the cached ops: the first
/// scroll builds (1), further scrolls within its covered units build none,
/// a scroll past its end builds again, and an edit invalidates the cache
/// (the band's revision no longer names the buffer).
#[test]
fn a_markdown_band_stays_cached_between_frames_inside_its_range() {
    let mut body = String::new();
    for i in 0..120 {
        body.push_str(&format!("## Section {i}\n\n"));
        body.push_str("A paragraph long enough to wrap more than once in the reading column.\n\n");
    }
    let mut app = planned_app("md_band_cache", "md", &body, 40);
    let mut host = NullHost::default();
    let _ = app.render_headless(800, 600, 1.0);
    assert!(app.pane_plan.is_some(), "120 blocks: planned");
    let built0 = app.md_band_built();
    assert!(built0 >= 1, "the first frame built a band");
    let height = app.preview.list.height;

    // A band built for the viewport [s, s+600) covers every unit with a top
    // at or below s+600, so the next frame's scroll reuses it while its own
    // window ends before that first uncovered top. The slack is that band's
    // overhang below its own bottom edge (a covered unit is taller than the
    // pixels of it in view); read it from the cache instead of guessing a
    // block height.
    fn in_range_scroll(app: &App, vport: f32, base: f32) -> f32 {
        let tab = &app.tabs[0];
        let plan = match &tab.pane_plan {
            Some(crate::layout::PanePlan::Markdown(p)) => p,
            _ => panic!("the pane is a planned markdown pane"),
        };
        let c = tab.md_band.as_ref().expect("the band is cached");
        let end_top = plan.rows.top(c.units.end);
        let slack = end_top - base - vport;
        (slack * 0.5).max(0.0)
    }
    // Scroll within the band the first frame covered: cache holds.
    let s1 = in_range_scroll(&app, 600.0, 0.0);
    app.set_scroll(s1);
    let _ = app.render_headless(800, 600, 1.0);
    assert_eq!(
        app.md_band_built(),
        built0,
        "a scroll inside the covered units must reuse the cached band"
    );

    // A scroll past the band's end needs a new band, which becomes the cache.
    app.set_scroll(height * 0.9);
    let _ = app.render_headless(800, 600, 1.0);
    let built1 = app.md_band_built();
    assert!(
        built1 > built0,
        "a scroll past the covered units builds a fresh band"
    );
    // The new cache serves itself too.
    let s2 = height * 0.9 + in_range_scroll(&app, 600.0, height * 0.9);
    app.set_scroll(s2);
    let _ = app.render_headless(800, 600, 1.0);
    assert_eq!(
        app.md_band_built(),
        built1,
        "the fresh band serves in-range scrolls"
    );

    // An edit changes the revision: the cached band must not paint from it.
    app.set_mode(crate::app::Mode::Split);
    let _ = app.render_headless(800, 600, 1.0);
    app.editor.caret = crate::core::buffer::Pos { line: 0, col: 0 };
    app.on_event(crate::ui::event::Event::Text("typed ".into()), &mut host);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(
        app.md_band_built() > built1,
        "an edit invalidates the cached band"
    );

    // And the pane shows the edit: the fresh band carries the typed text.
    app.set_scroll(0.0);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(
        app.preview
            .list
            .ops
            .iter()
            .any(|op| matches!(op, DrawOp::Text { text, .. } if text.contains("typed"))),
        "the pane after the edit shows the typed text"
    );
}

#[test]
fn a_small_pane_keeps_its_full_list_and_row_index() {
    let body = "fn main() {\n    println!(\"hi\");\n}\n".repeat(20);
    let mut app = planned_app("small_code", "rs", &body, 500);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(
        app.pane_plan.is_none(),
        "below the threshold the pane keeps its full list"
    );
    assert!(
        !app.preview_rows.is_empty(),
        "and its row index, which selection and copy need"
    );
}

/// Every reader kind must be banded above the threshold, or bounded by
/// construction. This is the rule that makes the plan universal: a new
/// `FileKind` either joins a planned family here, or this test fails and
/// its author has to decide how it will be painted.
#[test]
fn every_file_kind_plans_or_is_bounded_by_its_parser() {
    use crate::core::file_kind::FileKind::*;
    let all = [
        Markdown, Json, JsonLines, Csv, Yaml, Toml, Ini, Log, Code, PlainText,
    ];
    for kind in all {
        let planned = match kind {
            // Structure-keyed plans of their own.
            Markdown | Json | JsonLines | Csv => true,
            // The line family's shared plan.
            _ => line_kind(kind).is_some(),
        };
        assert!(planned, "{kind:?} has no banded path");
    }
}

/// The worker path a multi-megabyte Markdown document takes: the band
/// paints while the plan is measured off-thread, and the plan that lands
/// carries both the document and its per-block index.
#[test]
fn a_big_markdown_plans_on_the_worker_and_then_paints_bands() {
    let mut body = String::new();
    for i in 0..30 {
        body.push_str(&format!("## Section {i}\n\n"));
        body.push_str(
            "A paragraph with **bold**, `code` and a [link](https://example.com) that is \
                 long enough to wrap across the reading column at least once.\n\n",
        );
        body.push_str(&format!("```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n\n"));
    }
    let dir = crate::testing::temp_dir().join("editor_planned_panes");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("worker_md.md");
    std::fs::write(&path, &body).unwrap();
    // `headless = false`: the async switch the worker path is behind.
    let mut app = App::new_streaming(Some(path), false).expect("open");
    app.virtual_units = 40;
    app.async_lines = 1;
    app.layout_dirty = true;
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert!(
        app.preview_job_pending(),
        "a big document's plan belongs on the worker"
    );
    // The wait is the plain band, not a blank pane.
    assert!(!app.preview.list.ops.is_empty());
    let mut guard = 0;
    while app.preview_job_pending() {
        app.on_event(Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(guard < 4000, "the worker never planned the document");
    }
    assert!(app.pane_plan.is_some(), "the plan landed on the tab");
    assert!(
        app.markdown_doc.is_some(),
        "the parsed document landed with it"
    );
    assert!(
        app.preview.list.ops.len() < 600,
        "the landed pane paints a band ({} ops)",
        app.preview.list.ops.len()
    );
}

/// 24 000 lines of code: over the plan threshold, under the load streamer's
/// split, so the whole document is open and planned.
fn big_code(path: &std::path::Path, lines: usize) {
    let mut text = String::with_capacity(lines * 60);
    for i in 0..lines {
        text.push_str(&format!(
            "pub fn item_{i}(x: u32) -> u32 {{ let y = x * 3 + {i}; y }}\n"
        ));
    }
    std::fs::write(path, text).unwrap();
}

/// Typing in Split patches the plan instead of laying the document out
/// again: one keystroke measures the one line it changed, and the index it
/// leaves is the one a fresh plan of the edited buffer has. Before this,
/// every keystroke re-counted every line's wrapped rows.
#[test]
fn typing_in_split_patches_the_code_plan_instead_of_rebuilding_it() {
    use crate::core::file_kind::FileKind;

    let path = crate::testing::temp_dir().join("editor_patch_code.rs");
    big_code(&path, 24_000);
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::Code);
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert!(app.tabs[0].pane_plan.is_some(), "the pane is planned");
    let before = app.plan_patches();

    // A character in the first line, a newline (which splits it), and a
    // backspace: growth, a line-count change and a shrink.
    app.on_event(Event::Text("x".into()), &mut host);
    Handler::paint(&mut app, &mut canvas);
    app.on_event(
        Event::KeyDown {
            key: Key::Enter,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    app.on_event(
        Event::KeyDown {
            key: Key::Backspace,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert_eq!(app.plan_patches(), before + 3, "each edit patched the plan");

    // And the patch is exact: the pane's height is a fresh plan's.
    let ctx = Ctx {
        fonts: &app.fonts,
        theme: &app.theme,
        scale: app.scale,
        zoom: app.zoom,
    };
    let fresh = layout::highlight::build_plan(
        &app.tabs[0].buffer,
        app.tabs[0].plan_width,
        &ctx,
        layout::highlight::Kind::Code,
    );
    assert_eq!(
        app.tabs[0].pane_plan.as_ref().unwrap().height(),
        fresh.rows.height()
    );
    assert_eq!(app.tabs[0].pane_rev, app.tabs[0].buffer.revision());
    let _ = std::fs::remove_file(&path);
}

/// C1 · The width hand-off keeps both of its contracts. A width change with
/// the plan current for this revision lands a plan the live buffer describes
/// (the hand-off: recount + state adoption, no edit patch); a width change
/// with a plan the buffer moved past — an edit landed while the reader pane
/// had no patcher, here Edit mode, which skips the edit-time patch — must
/// not transplant the stale rows: the rebuilt plan matches a fresh plan of
/// the edited buffer at the new width. Both painted ends equal exactly the
/// same fresh-plan heights, so a regression on either side shows up as a
/// wrong pane, not just a wrong cost.
#[test]
fn a_width_change_hands_the_plan_over_and_a_stale_plan_rebuilds() {
    use crate::core::file_kind::FileKind;

    let path = crate::testing::temp_dir().join("editor_width_handoff.rs");
    big_code(&path, 24_000);
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::Code);
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert!(matches!(
        app.tabs[0].pane_plan,
        Some(layout::PanePlan::Lines(_))
    ));
    let revision = app.tabs[0].buffer.revision();
    assert_eq!(
        app.tabs[0].pane_rev, revision,
        "the load plan is current for its revision"
    );
    let patches0 = app.plan_patches();

    // The hand-off side: a narrower window recounts rows at the new width
    // and lands the height a fresh plan of the same buffer has. No edit is
    // involved, so the patch counter must not move.
    app.on_event(
        Event::Resized {
            width: 900,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert_eq!(app.tabs[0].pane_rev, revision, "the plan stays current");
    assert_eq!(
        app.plan_patches(),
        patches0,
        "width work is not an edit patch"
    );
    let ctx = Ctx {
        fonts: &app.fonts,
        theme: &app.theme,
        scale: app.scale,
        zoom: app.zoom,
    };
    let fresh = layout::highlight::build_plan(
        &app.tabs[0].buffer,
        app.tabs[0].plan_width,
        &ctx,
        layout::highlight::Kind::Code,
    );
    assert_eq!(
        app.tabs[0].pane_plan.as_ref().unwrap().height(),
        fresh.rows.height(),
        "the hand-off lands the fresh plan's height"
    );

    // The stale side: an edit in Edit mode (no reader patch there), then
    // back to Split at a third width. The hand-off must not fire on the
    // stale plan: the pane ends up describing the *edited* buffer.
    app.set_mode(Mode::Edit);
    app.on_event(
        Event::KeyDown {
            key: Key::Enter,
            mods: Modifiers::default(),
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert_ne!(
        app.tabs[0].buffer.revision(),
        revision,
        "the edit moved the buffer"
    );
    app.set_mode(Mode::Split);
    app.on_event(
        Event::Resized {
            width: 1000,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert_eq!(
        app.tabs[0].pane_rev,
        app.tabs[0].buffer.revision(),
        "the pane is current for the edited buffer"
    );
    assert_eq!(
        app.plan_patches(),
        patches0,
        "an Edit-mode edit patches no reader plan"
    );
    let ctx = Ctx {
        fonts: &app.fonts,
        theme: &app.theme,
        scale: app.scale,
        zoom: app.zoom,
    };
    let fresh2 = layout::highlight::build_plan(
        &app.tabs[0].buffer,
        app.tabs[0].plan_width,
        &ctx,
        layout::highlight::Kind::Code,
    );
    assert_eq!(
        app.tabs[0].pane_plan.as_ref().unwrap().height(),
        fresh2.rows.height(),
        "the rebuilt pane matches the edited document at the new width"
    );
    let _ = std::fs::remove_file(&path);
}

/// The same for a Markdown document: the edit re-parses the document (the
/// block list is the parser's output) but the plan measures only the block
/// run that changed, and lands on a fresh plan's heights.
#[test]
fn typing_in_split_patches_the_markdown_plan() {
    let dir = crate::testing::temp_dir().join("editor_patch_md");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("patch.md");
    let mut text = String::new();
    for i in 0..30 {
        text.push_str(&format!("## Section {i}\n\n"));
        text.push_str(
            "A paragraph with **bold**, `code` and a [link](https://example.com) that is \
                 long enough to wrap across the reading column at least once.\n\n",
        );
    }
    std::fs::write(&path, text).unwrap();
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    // A block count the fixture clears, without a multi-megabyte document.
    app.virtual_units = 40;
    app.set_mode(Mode::Split);
    app.layout_dirty = true;
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert!(matches!(
        app.tabs[0].pane_plan,
        Some(layout::PanePlan::Markdown(_))
    ));
    assert!(app.tabs[0].markdown_doc.is_some());
    let before = app.plan_patches();

    app.on_event(Event::Text("z".into()), &mut host);
    Handler::paint(&mut app, &mut canvas);
    assert_eq!(app.plan_patches(), before + 1, "the edit patched the plan");

    let revision = app.tabs[0].buffer.revision();
    assert_eq!(
        app.tabs[0].markdown_doc.as_ref().map(|(r, _)| *r),
        Some(revision),
        "the re-parsed document rides with the plan"
    );
    let (_, doc) = app.tabs[0].markdown_doc.as_ref().unwrap();
    let ctx = Ctx {
        fonts: &app.fonts,
        theme: &app.theme,
        scale: app.scale,
        zoom: app.zoom,
    };
    let fresh = layout::markdown::plan(doc, app.tabs[0].plan_width, &ctx);
    assert_eq!(
        app.tabs[0].pane_plan.as_ref().unwrap().height(),
        fresh.rows.height()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A JSONL file of `lines` records, each with a nested array so a record
/// walks more than one node.
fn big_jsonl(path: &std::path::Path, lines: usize) {
    let mut text = String::with_capacity(lines * 64);
    for i in 0..lines {
        text.push_str(&format!(
            "{{\"id\": {i}, \"tags\": [\"a\", \"b\"], \"note\": \"row {i}\"}}\n"
        ));
    }
    std::fs::write(path, text).unwrap();
}

/// Typing into a very large JSONL in Split patches its records and the
/// index beside them: the touched line is parsed again and the index below
/// it shifts, the pane never waiting on a whole re-parse. Before this, an
/// edit dropped the records and the worker parsed the file again — so the
/// test checks the path taken (a patch, and no job started) as well as the
/// result (the index a fresh build of those records gives).
#[test]
fn typing_in_split_patches_the_jsonl_records_and_index() {
    use crate::core::file_kind::FileKind;

    let path = crate::testing::temp_dir().join("editor_patch_big.jsonl");
    big_jsonl(&path, 6_000);
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::JsonLines);
    // The worker threshold, lowered so a re-parse here would visibly take
    // the off-thread path this patch replaces.
    app.async_lines = 1_000;
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    let mut guard = 0;
    while app.preview_job_pending() || app.tabs[0].plan.slots.is_empty() {
        app.on_event(Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(guard < 4000, "the JSONL records never landed");
    }
    assert!(
        app.tabs[0].records.len() > 4_000,
        "the fixture should be over the virtual threshold"
    );
    let before = app.plan_patches();

    app.on_event(Event::Text("x".into()), &mut host);
    Handler::paint(&mut app, &mut canvas);
    assert_eq!(app.plan_patches(), before + 1, "the edit patched the index");
    assert!(
        !app.preview_job_pending(),
        "the pane re-parsed the file instead of patching it"
    );
    assert_eq!(app.tabs[0].records_rev, app.tabs[0].buffer.revision());

    // And the patch is exact: the pane's index is the one a fresh build of
    // its own records gives, slot for slot.
    let ctx = Ctx {
        fonts: &app.fonts,
        theme: &app.theme,
        scale: app.scale,
        zoom: app.zoom,
    };
    let fresh = layout::json_tree::build_plan(
        &app.tabs[0].records,
        &app.tabs[0].collapsed,
        app.tabs[0].plan_width,
        &ctx,
        true,
    );
    let patched = &app.tabs[0].plan;
    assert_eq!(patched.slots.len(), fresh.slots.len());
    for (a, b) in patched.slots.iter().zip(fresh.slots.iter()) {
        assert_eq!(
            (a.record, a.id_base, a.nodes),
            (b.record, b.id_base, b.nodes)
        );
        assert!((a.top - b.top).abs() < 0.01, "top {} != {}", a.top, b.top);
        assert!((a.h - b.h).abs() < 0.01, "height {} != {}", a.h, b.h);
    }
    assert!((patched.height - fresh.height).abs() < 0.01);
    assert_eq!(app.tabs[0].records[0].line, 1, "records stay line-numbered");

    // What the user sees: the character landed in the first record, so the
    // patched record is the error one — and the band the pane painted says
    // so, which is the artifact all of the above is in service of.
    assert!(
        app.tabs[0].records[0].value.is_err(),
        "the typed character should have broken the first record"
    );
    assert!(
        app.preview.list.ops.iter().any(|op| matches!(
            op,
            DrawOp::Text { text, .. } if text.contains("record — error")
        )),
        "the pane did not paint the record the edit broke"
    );
    let _ = std::fs::remove_file(&path);
}

/// Typing into a very large JSON in Split patches the tree: the node the
/// caret is in is parsed and measured again, every other node keeps the
/// measurement it had, and the pane never re-parses the document. Before
/// this, an edit dropped the tree and the whole file was parsed and
/// re-planned — so the test pins the path taken (a patch, and no job
/// started) as well as the result (the plan a fresh build gives) and the
/// artifact (the band the pane painted).
#[test]
fn typing_in_split_patches_a_big_json_tree() {
    use crate::core::file_kind::FileKind;

    let path = crate::testing::temp_dir().join("editor_patch_big.json");
    std::fs::write(&path, big_json(3_600)).unwrap();
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::Json);
    // The worker threshold, lowered so a re-parse here would visibly take
    // the off-thread path this patch replaces.
    app.async_lines = 1_000;
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    let mut guard = 0;
    while app.preview_job_pending() || app.tabs[0].json_doc.is_none() {
        app.on_event(Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(guard < 4000, "the tree never landed");
    }
    let nodes = app.tabs[0].json_doc.as_ref().unwrap().1.plan.slots.len();
    assert!(nodes > 20_000, "the fixture should be a big tree: {nodes}");
    let before = app.plan_patches();

    // The caret goes inside the first row's name, where a keystroke is a
    // change to one string and nothing else.
    let line = 1;
    let col = app.tabs[0].buffer.line(line).find("row-0").unwrap() + 5;
    app.tabs[0].editor.caret = crate::core::buffer::Pos { line, col };
    app.on_event(Event::Text("x".into()), &mut host);
    Handler::paint(&mut app, &mut canvas);
    assert_eq!(app.plan_patches(), before + 1, "the edit patched the tree");
    assert!(
        !app.preview_job_pending(),
        "the pane re-parsed the document instead of patching it"
    );
    let revision = app.tabs[0].buffer.revision();
    assert_eq!(app.tabs[0].json_doc.as_ref().unwrap().0, revision);

    // And the patch is exact: the plan is the one a full measure of the
    // tree it now holds gives, slot for slot — positions, heights, spans
    // and toggle ids included.
    let ctx = Ctx {
        fonts: &app.fonts,
        theme: &app.theme,
        scale: app.scale,
        zoom: app.zoom,
    };
    let (_, doc) = app.tabs[0].json_doc.as_ref().unwrap();
    let fresh = layout::json_tree::build_value_plan(
        &doc.value,
        &app.tabs[0].collapsed,
        app.tabs[0].plan_width,
        &ctx,
    );
    assert_eq!(doc.plan.slots.len(), fresh.slots.len());
    assert_eq!(doc.plan.ids, fresh.ids);
    for (a, b) in doc.plan.slots.iter().zip(fresh.slots.iter()) {
        assert_eq!(
            (a.parent, a.next, a.ord, a.id),
            (b.parent, b.next, b.ord, b.id)
        );
        assert!((a.top - b.top).abs() < 0.01, "top {} != {}", a.top, b.top);
        assert!((a.h - b.h).abs() < 0.01, "height {} != {}", a.h, b.h);
    }
    assert!((doc.plan.height - fresh.height).abs() < 0.01);
    // The tree is the document: a fresh parse of the edited text is the
    // tree the patch now holds.
    let (whole, spans) = crate::parse::json::parser::parse_spanned(&app.tabs[0].buffer.text())
        .expect("the edited document parses");
    assert_eq!(doc.value, whole);
    assert_eq!(doc.spans, spans);

    // What the user sees: the character is in the first row's name, so the
    // band the pane painted shows it.
    assert!(
        app.preview.list.ops.iter().any(|op| matches!(
            op,
            DrawOp::Text { text, .. } if text.contains("row-0x")
        )),
        "the band did not paint the edited row"
    );
    let _ = std::fs::remove_file(&path);
}

/// A keystroke that leaves a big JSON unparseable — a dangling comma, a
/// half-written number, a key mid-word — no longer costs the tree. The pane
/// keeps every node it had and puts one error row where the text does not
/// parse, and the fixing keystroke patches that node back. Before this, each
/// such keystroke dropped the tree and the document was re-parsed whole.
#[test]
fn typing_a_dangling_comma_keeps_a_big_json_tree() {
    use crate::core::file_kind::FileKind;

    let path = crate::testing::temp_dir().join("editor_patch_broken.json");
    std::fs::write(&path, big_json(3_600)).unwrap();
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::Json);
    app.async_lines = 1_000;
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    let mut guard = 0;
    while app.preview_job_pending() || app.tabs[0].json_doc.is_none() {
        app.on_event(Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(guard < 4000, "the tree never landed");
    }
    let nodes = app.tabs[0].json_doc.as_ref().unwrap().1.plan.slots.len();
    let before = app.plan_patches();

    // A comma typed just inside the array's `[`: the document no longer
    // parses, and the array is the node it stopped describing.
    let col = app.tabs[0].buffer.line(0).find('[').unwrap() + 1;
    app.tabs[0].editor.caret = crate::core::buffer::Pos { line: 0, col };
    app.on_event(Event::Text(",".into()), &mut host);
    Handler::paint(&mut app, &mut canvas);

    assert_eq!(app.plan_patches(), before + 1, "the edit patched the tree");
    assert!(
        !app.preview_job_pending(),
        "an unparseable keystroke re-parsed the document"
    );
    assert_eq!(
        app.tabs[0].json_doc.as_ref().unwrap().0,
        app.tabs[0].buffer.revision()
    );
    {
        let doc = &app.tabs[0].json_doc.as_ref().unwrap().1;
        assert!(doc.broken.is_some(), "the tree should report the break");
        assert_eq!(
            doc.plan.slots.len(),
            2,
            "the root and the array it cannot describe: {} of {nodes} slots",
            doc.plan.slots.len()
        );
    }
    assert!(
        app.tabs[0].status_extra.starts_with("error"),
        "the status line should say so: {:?}",
        app.tabs[0].status_extra
    );
    let texts: Vec<&str> = app
        .preview
        .list
        .ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.starts_with("! ")),
        "the band did not paint the error row: {texts:?}"
    );

    // Deleting the comma makes the document parse again, and the node is
    // patched back from the one row it had become: the tree is the one a
    // full parse of the fixed text gives, and the error is gone from both
    // the tree and the status line.
    let after_break = app.plan_patches();
    app.on_event(plain(Key::Backspace), &mut host);
    Handler::paint(&mut app, &mut canvas);
    assert_eq!(app.plan_patches(), after_break + 1, "the fix patched too");
    assert!(!app.preview_job_pending());
    {
        let doc = &app.tabs[0].json_doc.as_ref().unwrap().1;
        assert_eq!(doc.broken, None, "the repaired document is whole");
        assert_eq!(doc.plan.slots.len(), nodes);
        let (value, spans) = crate::parse::json::parser::parse_spanned(&app.tabs[0].buffer.text())
            .expect("the fixed document parses");
        assert_eq!(doc.value, value);
        assert_eq!(doc.spans, spans);
    }
    assert_eq!(app.tabs[0].status_extra, "");
    assert!(
        !app.preview.list.ops.iter().any(|op| matches!(
            op,
            DrawOp::Text { text, .. } if text.starts_with("! ")
        )),
        "the error row outlived the error"
    );
    let _ = std::fs::remove_file(&path);
}

/// The bar's error names the node the tree cannot describe, so pressing it
/// goes there: the pane centres that row however far down the document it
/// is. A bar with nothing broken has no such stretch — the same pixels are a
/// fact — and Edit, which has no reader pane to move, offers no target
/// either even while the tree still carries the break.
#[test]
fn pressing_the_status_error_jumps_to_the_broken_node() {
    let path = crate::testing::temp_dir().join("editor_status_jump.json");
    // Blank lines between the records make the editor pane much taller than
    // the tree — the two panes share one progress, so the jump has to
    // convert, and this is the document where the wrong unit shows.
    let mut text = String::from("{\"rows\": [\n");
    for i in 0..3_600 {
        text.push_str(&format!(
            "  {{\"id\": {i}, \"name\": \"row-{i}\", \"tags\": [\"a\", \"b\"], \
                 \"ok\": true, \"meta\": {{\"note\": \"n\"}}}}{}\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n",
            if i == 3_599 { "" } else { "," }
        ));
    }
    text.push_str("]}\n");
    std::fs::write(&path, &text).unwrap();
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    app.async_lines = 1_000;
    app.set_mode(Mode::Split);
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(
        Event::Resized {
            width: 1100,
            height: 800,
            scale: 1.0,
        },
        &mut host,
    );
    let mut guard = 0;
    while app.preview_job_pending() || app.tabs[0].json_doc.is_none() {
        app.on_event(Event::Tick, &mut host);
        Handler::paint(&mut app, &mut canvas);
        guard += 1;
        assert!(guard < 4000, "the tree never landed");
    }
    // A whole document is a fact on the bar, not a target.
    Handler::paint(&mut app, &mut canvas);
    assert!(
        app.status_detail.is_none(),
        "there is no error to go to yet"
    );

    // A comma inside the `tags` array of a row halfway down: the array is
    // the node the tree stops describing, and it is far below the pane.
    assert!(
        app.scroll.max() - app.preview_max() > 1_000.0,
        "setup: the panes must not share a scale ({} vs {})",
        app.scroll.max(),
        app.preview_max()
    );
    let line = 1 + 17 * 1_800;
    let col = app.tabs[0].buffer.line(line).find("[\"a\"").unwrap() + 1;
    app.tabs[0].editor.caret = crate::core::buffer::Pos { line, col };
    app.on_event(Event::Text(",".into()), &mut host);
    Handler::paint(&mut app, &mut canvas);
    assert!(
        app.status_extra.starts_with("error"),
        "the bar should name the break: {:?}",
        app.status_extra
    );
    // Wherever the pane ended up after the keystroke, the press must still
    // be measured from the top of the document.
    app.set_scroll(0.0);
    Handler::paint(&mut app, &mut canvas);
    let detail = app.status_detail.expect("the error is a target");
    let y = detail.y + detail.h / 2.0;
    assert_eq!(app.cursor_at(detail.x + 1.0, y), CursorShape::Hand);
    assert_eq!(
        app.cursor_at(20.0, y),
        CursorShape::Arrow,
        "facts are not links"
    );

    let slot = app.json_error_slot().expect("the broken node");
    let (top, h, height) = {
        let doc = &app.tabs[0].json_doc.as_ref().unwrap().1;
        (
            doc.plan.slots[slot].top,
            doc.plan.slots[slot].h,
            doc.plan.height,
        )
    };
    let pane_h = app.split_panes().0.h;
    let want = (top - (pane_h - h).max(0.0) / 2.0).clamp(0.0, (height - pane_h).max(0.0));
    assert!(
        app.preview_scroll() < want - 40.0,
        "the node starts off screen ({} vs {want})",
        app.preview_scroll()
    );

    app.on_event(
        Event::MouseDown {
            x: detail.x + 2.0,
            y: detail.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert!(
        (app.preview_scroll() - want).abs() <= 1.0,
        "the pane should centre the node at {want}, not {}",
        app.preview_scroll()
    );
    assert!(
        (app.scroll.offset - want).abs() > 1_000.0,
        "the shared progress was left in preview pixels"
    );
    // And the row it names is the one on screen, not a nearby one.
    let texts: Vec<&str> = app
        .preview
        .list
        .ops
        .iter()
        .filter_map(|op| match op {
            DrawOp::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.starts_with("! ")),
        "the error row is not in the painted band: {texts:?}"
    );

    // A transient that took the message's place is a fact, not a link: the
    // bar is no longer naming the node, so it must not offer to go there.
    let error = app.status_extra.clone();
    app.status_extra = "reloaded".into();
    Handler::paint(&mut app, &mut canvas);
    assert!(app.status_detail.is_none(), "\"reloaded\" offered a jump");
    app.status_extra = error;

    // The whole-window pane works the same, on its own replanned tree.
    app.set_mode(Mode::Preview);
    Handler::paint(&mut app, &mut canvas);
    app.set_scroll(0.0);
    Handler::paint(&mut app, &mut canvas);
    let detail = app.status_detail.expect("Preview's error is a target too");
    let (top, h, height) = {
        let doc = &app.tabs[0].json_doc.as_ref().unwrap().1;
        let s = doc.plan.slots[app.json_error_slot().unwrap()];
        (s.top, s.h, doc.plan.height)
    };
    let pane_h = app.viewport().h;
    let want = (top - (pane_h - h).max(0.0) / 2.0).clamp(0.0, (height - pane_h).max(0.0));
    app.on_event(
        Event::MouseDown {
            x: detail.x + 2.0,
            y: detail.y + 2.0,
            button: MouseButton::Left,
        },
        &mut host,
    );
    Handler::paint(&mut app, &mut canvas);
    assert!(
        (app.preview_scroll() - want).abs() <= 1.0,
        "Preview should centre the node at {want}, not {}",
        app.preview_scroll()
    );

    // Edit has no reader pane to move, so the same words are a fact there.
    app.set_mode(Mode::Edit);
    Handler::paint(&mut app, &mut canvas);
    assert!(app.status_detail.is_none(), "Edit offered a jump");
    let _ = std::fs::remove_file(&path);
}

/// Collapsing a node inside a JSONL band still rebuilds the index: the
/// records are untouched, but a folded block is shorter, so the plan beside
/// them must be rebuilt rather than taken as current.
#[test]
fn collapsing_inside_a_jsonl_band_rebuilds_the_index() {
    use crate::core::file_kind::FileKind;

    let path = crate::testing::temp_dir().join("editor_band_big.jsonl");
    big_jsonl(&path, 5_000);
    let mut app = App::new_streaming(Some(path.clone()), true).expect("open");
    assert_eq!(app.tabs[0].kind, FileKind::JsonLines);
    let _ = app.render_headless(1100, 800, 1.0);
    assert!(
        !app.tabs[0].plan.slots.is_empty(),
        "the records are planned"
    );
    let open_h = app.tabs[0].plan.height;
    let r = app
        .preview
        .list
        .hits
        .iter()
        .find(|h| {
            matches!(
                h.action,
                crate::layout::display_list::HitAction::ToggleNode(_)
            )
        })
        .map(|h| h.rect)
        .expect("a JSONL band must carry toggles");
    let mut host = NullHost::default();
    app.on_event(
        Event::MouseDown {
            x: r.x + 1.0,
            y: r.y + 1.0 + app.viewport().y,
            button: MouseButton::Left,
        },
        &mut host,
    );
    assert_eq!(app.collapsed.len(), 1, "the click collapsed nothing");
    let mut canvas = Canvas::new(1100, 800);
    app.paint(&mut canvas);
    assert!(
        app.tabs[0].plan.height < open_h,
        "collapsing a planned record did not shrink the pane"
    );
    assert_eq!(
        app.tabs[0].plan_collapsed_gen, app.tabs[0].collapsed_gen,
        "the rebuilt index belongs to the new collapsed set"
    );
    let _ = std::fs::remove_file(&path);
}

/// The report: scroll a preview to the bottom, switch to the editor, and the
/// pane is blank until the view is toggled away and back. The switch settles
/// the view against the editor's own height, and the band the same relayout
/// built was measured at the *preview's* offset — an empty, inverted range the
/// staleness check then called fresh, so nothing ever rebuilt it.
#[test]
fn switching_from_a_scrolled_preview_paints_the_editor_at_once() {
    let body = "# Heading\n\nSome **bold** text with `code` and a [link](https://example.com).\n\n- one\n- two\n\n";
    let mut app = open("editor_test_switch_at_end.md", &body.repeat(40));
    let _ = app.render_headless(1100, 800, 1.0);
    app.set_mode(Mode::Preview);
    let _ = app.render_headless(1100, 800, 1.0);
    let preview_end = app.scroll.max();
    assert!(preview_end > 0.0, "the fixture must scroll");
    app.set_scroll(f32::MAX);
    assert_eq!(app.scroll.offset, preview_end, "at the preview's end");
    let mut host = NullHost::default();
    let mut canvas = Canvas::new(1100, 800);
    app.on_event(Event::Tick, &mut host);
    Handler::paint(&mut app, &mut canvas);
    // The switch itself, and one frame — no toggle away and back.
    app.set_mode(Mode::Edit);
    app.on_event(Event::Tick, &mut host);
    let mut canvas = Canvas::new(1100, 800);
    Handler::paint(&mut app, &mut canvas);
    assert!(
        !app.tabs[0].editor_pane.list.ops.is_empty(),
        "the editor pane must have text on the frame the switch paints"
    );
    assert!(
        !app.editor_band_stale(),
        "and its band must cover the rows it is about to show"
    );
    assert!(
        canvas.pixels.iter().any(|&p| p != 0),
        "the switch painted an empty pane"
    );
}

/// The other half of that bug: `editor_band_stale` answered with the test for
/// "does this band still cover the view?", so a covered band asked for a
/// rebuild on every frame (the paint path read `true` as stale) while a band
/// the view had jumped out of asked for none — the one case that needed it.
#[test]
fn the_editor_band_is_stale_only_once_the_view_leaves_it() {
    let mut app = open("editor_test_band_stale.txt", &"line\n".repeat(400));
    let _ = app.render_headless(800, 600, 1.0);
    app.set_mode(Mode::Edit);
    let _ = app.render_headless(800, 600, 1.0);
    assert!(
        !app.editor_band_stale(),
        "the band was built for the view it covers"
    );
    // A row or two of scroll stays inside the band's slack: no rebuild.
    let line = app.line_step_px();
    app.set_scroll(line * 2.0);
    assert!(
        !app.editor_band_stale(),
        "two rows are inside the band's slack"
    );
    // Far past it — the end of the document — the rows on screen are not the
    // ones the band holds.
    app.set_scroll(f32::MAX);
    assert!(
        app.editor_band_stale(),
        "the band cannot answer for rows it never measured"
    );
}

/// The report: a wheel notch barely moved the page and moved less the larger
/// the text got. A notch is a *line* device — three lines, as everywhere else
/// on the machine — so the distance comes from the text the pane is showing,
/// never from a fixed pixel count.
#[test]
fn a_wheel_notch_steps_by_three_lines_of_the_current_text() {
    let mut app = open("editor_test_wheel_step.txt", &"line\n".repeat(400));
    let _ = app.render_headless(800, 600, 1.0);
    let line_h = |app: &App| layout::text::plain_params(app.viewport().w, &app.ctx(), true).line_h;
    assert_eq!(
        app.line_step_px(),
        line_h(&app),
        "the step is one line of the pane's own text"
    );
    let at_100 = app.line_step_px();
    app.zoom_by(2.0);
    let at_200 = app.line_step_px();
    assert!(
        at_200 > at_100 * 1.9,
        "the step must grow with the text: {at_100} -> {at_200}"
    );
    assert_eq!(
        at_200,
        line_h(&app),
        "and stay exactly one line of it after zooming"
    );
    // Three of those is a notch: a full notch keeps moving three rows at
    // either size, which is the whole point of measuring in lines.
    assert!((crate::platform::WHEEL_LINES_PER_NOTCH * at_200 - 3.0 * line_h(&app)).abs() < 0.001);
}

/// The report named Split as a suspect too. There the two panes share one
/// progress rather than one pixel offset, so the editor's own offset moves by
/// more than either pane's raw scroll — and the band must follow it, not paint
/// the rows of wherever the previous frame's progress was.
#[test]
fn the_split_editor_pane_stays_painted_wherever_the_progress_lands() {
    let body = "# Heading\n\nSome **bold** text with `code` and a [link](https://example.com).\n\n- one\n- two\n\n";
    let mut app = open("editor_test_split_scroll.md", &body.repeat(40));
    let _ = app.render_headless(1100, 800, 1.0);
    app.set_mode(Mode::Split);
    let _ = app.render_headless(1100, 800, 1.0);
    let mut host = NullHost::default();
    let max = app.scroll.max();
    assert!(max > 0.0, "the fixture must scroll");
    for fraction in [0.0, 0.35, 0.9, 1.0] {
        app.set_scroll(max * fraction);
        app.on_event(Event::Tick, &mut host);
        let mut canvas = Canvas::new(1100, 800);
        Handler::paint(&mut app, &mut canvas);
        assert!(
            !app.tabs[0].editor_pane.list.ops.is_empty(),
            "the editor half is blank at {fraction} of the document"
        );
        assert!(
            !app.editor_band_stale(),
            "its band does not cover the rows at {fraction} of the document: {:?}",
            app.tabs[0].editor_band
        );
    }
}
