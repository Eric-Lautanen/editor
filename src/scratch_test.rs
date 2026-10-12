// Scratch harness: reads a JSON tree's toggle hit-rects and clicks each one,
// printing what collapsed. Manual-debug helper, not part of the test suite's
// guarantees; it only compiles under `cargo test` and contains no assertions
// that could fail spuriously. Guarded by an env var so `cargo test` skips it.
use crate::app::App;
use crate::platform::{Handler, NullHost};
use crate::ui::event::{Event, MouseButton};

fn open(name: &str, content: &str) -> App {
    let dir = std::env::temp_dir().join(name);
    std::fs::write(&dir, content).unwrap();
    App::new(Some(dir)).unwrap()
}

#[test]
fn zz_scratch_nested_toggles() {
    if std::env::var("READITOR_SCRATCH").is_err() {
        return;
    }
    let mut app = open(
        "readitor_scratch.json",
        "{\"a\": {\"b\": [1, 2, 3]}, \"c\": true}",
    );
    let _ = app.render_headless(800, 600, 1.0);
    let mut host = NullHost::default();
    for (i, hit) in std::mem::take(&mut app.preview.list.hits)
        .into_iter()
        .enumerate()
    {
        let h = hit.rect;
        let crate::layout::display_list::HitAction::ToggleNode(id) = hit.action else {
            panic!("not a toggle");
        };
        app.on_event(
            Event::MouseDown {
                x: h.x + 1.0,
                y: h.y + 1.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
        let collapsed = app.collapsed.clone();
        println!(
            "hit {i}: id={id} rect={:?} collapsed_after={:?} n={}",
            h,
            collapsed,
            collapsed.len()
        );
        app.on_event(
            Event::MouseDown {
                x: h.x + 1.0,
                y: h.y + 1.0,
                button: MouseButton::Left,
            },
            &mut host,
        );
    }
}
