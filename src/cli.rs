use crate::{
    app::{App, Mode},
    core::file_kind::FileKind,
    error::{Error, Result},
    parse::{json, jsonl},
    platform, settings, update,
};
use std::path::PathBuf;

const USAGE: &str = "usage: editor [FILE]\n\
  --version | -V       print version\n\
  --install | --uninstall   register/remove file associations (portable use)\n\
  --update             check GitHub and install the newest release\n\
  --no-update          skip the automatic update check for this launch\n\
  --pretty             print FILE (.json/.jsonl) canonicalized to stdout\n\
  dev: --render OUT.ppm [--size WxH] [--scale S] [--dark] [--scroll PX]   headless snapshot of FILE
  dev: --mem-report FILE                                                allocation census after load+layout (mem-census builds)";

struct Opts {
    file: Option<PathBuf>,
    render: Option<PathBuf>,
    pretty: bool,
    size: (usize, usize),
    scale: f32,
    dark: bool,
    scroll: f32,
    /// Print the allocation census after the file is loaded/laid out.
    mem_report: bool,
    /// Skip the background update check this launch (dev loop; the env
    /// equivalent, `EDITOR_NO_UPDATE_CHECK`, covers whole sessions).
    no_update: bool,
}

fn usage(msg: impl Into<String>) -> Error {
    Error::Usage(msg.into())
}

pub fn run(args: Vec<String>) -> Result<()> {
    let mut o = Opts {
        file: None,
        render: None,
        pretty: false,
        size: platform::DEFAULT_SIZE,
        scale: 1.0,
        dark: false,
        scroll: 0.0,
        mem_report: false,
        no_update: false,
    };
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| {
            it.next()
                .ok_or_else(|| usage(format!("{name} needs a value")))
        };
        match a.as_str() {
            "--version" | "-V" => {
                platform::print_line(&format!("editor {}", env!("CARGO_PKG_VERSION")));
                return Ok(());
            }
            "--help" | "-h" => {
                platform::print_line(USAGE);
                return Ok(());
            }
            "--install" => return platform::install::install(),
            "--uninstall" => return platform::install::uninstall(),
            "--update" => return update::run(),
            "--no-update" => o.no_update = true,
            "--render" => o.render = Some(PathBuf::from(val("--render")?)),
            "--pretty" => o.pretty = true,
            "--size" => {
                let v = val("--size")?;
                let (w, h) = v
                    .split_once('x')
                    .ok_or_else(|| usage("--size expects WxH"))?;
                o.size = (
                    w.parse().map_err(|_| usage("bad width"))?,
                    h.parse().map_err(|_| usage("bad height"))?,
                );
            }
            "--scale" => o.scale = val("--scale")?.parse().map_err(|_| usage("bad --scale"))?,
            "--scroll" => {
                o.scroll = val("--scroll")?
                    .parse()
                    .map_err(|_| usage("bad --scroll"))?
            }
            "--dark" => o.dark = true,
            "--mem-report" => o.mem_report = true,
            other if !other.starts_with('-') => o.file = Some(PathBuf::from(other)),
            other => return Err(usage(format!("unknown flag {other}\n{USAGE}"))),
        }
    }

    let mut app = App::new(o.file.clone())?;
    // A file argument opens by construction (`App::new`), not through
    // `open_path`, so it would otherwise never reach the recent list.
    if let Some(f) = &o.file {
        app.note_recent(f);
    }
    let session = settings::load();
    // --dark wins, then the user's saved manual pick, then the OS theme.
    if o.dark {
        app.set_dark_manual(true);
    } else if session.manual {
        app.set_dark_manual(session.dark);
    } else {
        app.set_dark(platform::system_prefers_dark());
    }
    // Restore the last session's tabs and view unless a file was given.
    if o.file.is_none() && !session.files.is_empty() {
        let mode = match session.mode {
            1 => Mode::Edit,
            2 => Mode::Split,
            _ => Mode::Preview,
        };
        app.restore(&session.files, session.active, mode);
    }
    // The recent list crosses sessions whether or not tabs were restored.
    app.restore_recent(&session.recent);
    // No file argument and no restored tabs: offer the recent list as the way
    // back in — the most recent still-existing file becomes the session's tab.
    if o.file.is_none() && session.files.is_empty() {
        if let Some(path) = session.recent.iter().find(|p| p.is_file()) {
            let mut host = crate::platform::NullHost::default();
            app.open_path(path.clone(), &mut host);
        }
    }
    // Reopen where the window was left — with or without a file argument, and
    // maximized if that is how it was closed. The platform reads this back
    // while creating the window.
    app.restore_window(session.window, session.maximized);
    // The recent-files list outlives the tabs it came from; a file given on
    // the command line is as recent as one picked in a dialog.
    app.restore_recent(&session.recent);
    if let Some(f) = &o.file {
        app.note_recent(f);
    }
    if o.pretty {
        return pretty_print(o.file.as_deref());
    }
    #[cfg(any(test, feature = "mem-census"))]
    if o.mem_report {
        if let Some(path) = o.file.as_deref() {
            let mut a = App::new(Some(path.to_path_buf()))?;
            let _ = a.render_headless(o.size.0, o.size.1, o.scale);
            crate::platform::print_line("-- after tab 1 --");
            crate::platform::print_line(&crate::mem::report());
            // Open the same file a second time (2-tab cost, like the user's
            // report), repaint, and print again.
            a.open_path(
                path.to_path_buf(),
                &mut crate::platform::NullHost::default(),
            );
            let _ = a.render_headless(o.size.0, o.size.1, o.scale);
            crate::platform::print_line("-- after 2nd tab (same file) --");
            crate::platform::print_line(&crate::mem::report());
            return Ok(());
        }
    }
    if let Some(out) = o.render {
        let mut c = app.render_headless(o.size.0, o.size.1, o.scale);
        if o.scroll > 0.0 {
            app.set_scroll(o.scroll);
            c = crate::gfx::canvas::Canvas::new(o.size.0, o.size.1);
            crate::platform::Handler::paint(&mut app, &mut c);
        }
        std::fs::write(&out, c.to_ppm())?;
        platform::print_line(&format!(
            "wrote {} ({}x{})",
            out.display(),
            o.size.0,
            o.size.1
        ));
        return Ok(());
    }
    // Hand a second launch's file to the already-running instance and exit
    // before any window appears: "open with Editor" on a file while an
    // editor is already open must land as a new tab in *that* window, not
    // paint a second one. No file → same handoff (focuses the open window).
    if platform::forward_to_running(o.file.as_deref())? {
        return Ok(());
    }
    // Silent update check, detached: the check lives its own life (its
    // thread may end in an installer spawn + process exit) while the window
    // opens immediately. Off by the About box toggle (persisted), by
    // --no-update, or EDITOR_NO_UPDATE_CHECK; see src/update.rs for the
    // whole story (consent dialog + SHA-256 gate sit inside it).
    if !o.no_update && session.auto_update && update::due_for_check() {
        update::background_check();
    }
    platform::run(app)
}

/// Print FILE canonicalized: `.json` pretty-prints one document, `.jsonl`
/// pretty-prints one value per line (bad lines fail with their position).
/// Anything else is echoed unchanged.
fn pretty_print(file: Option<&std::path::Path>) -> Result<()> {
    let path = file.ok_or_else(|| usage("--pretty needs a FILE"))?;
    let src = std::fs::read_to_string(path)?;
    match FileKind::from_path(path) {
        FileKind::Json => {
            let p = json::pretty::pretty(&json::parser::parse(&src)?);
            platform::print_line(&p);
        }
        FileKind::JsonLines => {
            for rec in jsonl::parse(&src) {
                match rec.value {
                    Ok(v) => {
                        let p = json::pretty::pretty(&v);
                        platform::print_line(&p);
                    }
                    // The parser sees one line, so re-anchor its position to
                    // the physical file line for actionable errors.
                    Err(Error::Parse { col, msg, .. }) => {
                        return Err(Error::Parse {
                            line: rec.line,
                            col,
                            msg,
                        });
                    }
                    Err(other) => return Err(other),
                }
            }
        }
        FileKind::Markdown
        | FileKind::PlainText
        | FileKind::Csv
        | FileKind::Yaml
        | FileKind::Toml
        | FileKind::Ini
        | FileKind::Log
        | FileKind::Code => platform::print_text(&src),
    }
    Ok(())
}
