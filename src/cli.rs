use crate::{
    app::{App, Mode},
    core::file_kind::FileKind,
    crash,
    error::{Error, Result},
    parse::{json, jsonl},
    platform, settings, update,
};
use std::path::PathBuf;

const USAGE: &str = "usage: editor [FILE ...]\n\
  --version | -V       print version\n\
  --install | --uninstall   register/remove file associations (portable use)\n\
  --update             check GitHub and install the newest release\n\
  --no-update          skip the automatic update check for this launch\n\
  --pretty             print FILE (.json/.jsonl) canonicalized to stdout\n\
  dev: --render OUT.ppm [--size WxH] [--scale S] [--dark|--light] [--scroll PX]\n\
                       [--mode preview|edit|split]   headless snapshot of FILE
  dev: --mem-report FILE                                                allocation census after load+layout (mem-census builds)";

struct Opts {
    /// Files to open, in order: the first is the tab the app starts on, the
    /// rest are opened after it. Several are allowed because the window has
    /// tabs — the shell can hand over `editor a.md b.md` and get two tabs.
    files: Vec<PathBuf>,
    render: Option<PathBuf>,
    pretty: bool,
    size: (usize, usize),
    scale: f32,
    dark: bool,
    /// `--light`: pin the light theme for this launch. `--dark` wins if both
    /// are given; with neither, the saved session and the OS decide.
    light: bool,
    scroll: f32,
    /// View mode for a headless snapshot (screenshots); unset keeps whatever
    /// the session restored.
    mode: Option<Mode>,
    /// Print the allocation census after the file is loaded/laid out.
    mem_report: bool,
    /// Skip the background update check this launch (dev loop; the env
    /// equivalent, `EDITOR_NO_UPDATE_CHECK`, covers whole sessions).
    no_update: bool,
    /// The first "do this instead of opening a window" flag on the line.
    immediate: Option<Immediate>,
}

/// Flags that replace the whole launch: the first one wins, so
/// `editor --update --install` updates and does not install.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Immediate {
    Version,
    Help,
    Install,
    Uninstall,
    Update,
}

fn usage(msg: impl Into<String>) -> Error {
    Error::Usage(msg.into())
}

/// Parse the command line. Kept apart from [`run`] so its rules — `--` ends the
/// flags, several files are allowed, an unknown flag is refused — can be
/// checked without a window or a filesystem.
fn parse(args: &[String]) -> Result<Opts> {
    let mut o = Opts {
        files: Vec::new(),
        render: None,
        pretty: false,
        size: platform::DEFAULT_SIZE,
        scale: 1.0,
        dark: false,
        light: false,
        scroll: 0.0,
        mode: None,
        mem_report: false,
        no_update: false,
        immediate: None,
    };
    let mut it = args.iter();
    // `--` ends the flags: everything after it is a file, including a name
    // that starts with a dash. Without this there was no way at all to open
    // `-draft.md`, and a second bare argument was silently dropped.
    let mut flags_done = false;
    while let Some(a) = it.next() {
        if flags_done {
            o.files.push(PathBuf::from(a));
            continue;
        }
        let mut val = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| usage(format!("{name} needs a value")))
        };
        match a.as_str() {
            "--version" | "-V" => {
                o.immediate.get_or_insert(Immediate::Version);
            }
            "--help" | "-h" => {
                o.immediate.get_or_insert(Immediate::Help);
            }
            "--install" => {
                o.immediate.get_or_insert(Immediate::Install);
            }
            "--uninstall" => {
                o.immediate.get_or_insert(Immediate::Uninstall);
            }
            "--update" => {
                o.immediate.get_or_insert(Immediate::Update);
            }
            "--" => flags_done = true,
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
            "--light" => o.light = true,
            "--mode" => {
                o.mode = Some(match val("--mode")?.as_str() {
                    "preview" => Mode::Preview,
                    "edit" => Mode::Edit,
                    "split" => Mode::Split,
                    other => return Err(usage(format!("bad --mode {other}"))),
                })
            }
            "--mem-report" => o.mem_report = true,
            other if !other.starts_with('-') => o.files.push(PathBuf::from(other)),
            other => return Err(usage(format!("unknown flag {other}\n{USAGE}"))),
        }
    }
    Ok(o)
}

pub fn run(args: Vec<String>) -> Result<()> {
    let o = parse(&args)?;
    if let Some(act) = o.immediate {
        return match act {
            Immediate::Version => {
                platform::print_line(&format!("editor {}", env!("CARGO_PKG_VERSION")));
                Ok(())
            }
            Immediate::Help => {
                platform::print_line(USAGE);
                Ok(())
            }
            Immediate::Install => platform::install::install(),
            Immediate::Uninstall => platform::install::uninstall(),
            Immediate::Update => update::run(),
        };
    }
    // These shapes take one file and produce output or a snapshot instead of a
    // window; anything else would either render an Untitled buffer or ignore
    // the extra arguments.
    if o.pretty && o.files.len() != 1 {
        return Err(usage("--pretty takes exactly one FILE"));
    }
    if o.render.is_some() && o.files.len() != 1 {
        return Err(usage("--render takes exactly one FILE"));
    }
    let headless = o.render.is_some() || o.pretty || o.mem_report;
    // Hand a second launch's files to the already-running window and exit
    // before anything is loaded or restored. This used to happen *after* the
    // App was built: a big file was parsed only to be thrown away, and an
    // unreadable one failed the launch instead of landing as a tab in the
    // window that was already open — an "Open with Editor" that does nothing.
    if !headless && platform::forward_to_running(&o.files)? {
        return Ok(());
    }

    let mut app = App::new(o.files.first().cloned())?;
    // A file argument opens by construction (`App::new`), not through
    // `open_path`, so it would otherwise never reach the recent list.
    if let Some(f) = o.files.first() {
        app.note_recent(f);
    }
    let session = settings::load();
    // --dark / --light win, then the user's saved manual pick, then the OS
    // theme.
    if o.dark {
        app.set_dark_manual(true);
    } else if o.light {
        app.set_dark_manual(false);
    } else if session.manual {
        app.set_dark_manual(session.dark);
    } else {
        app.set_dark(platform::system_prefers_dark());
    }
    // The OS's reduce-motion switch, read once like the theme. The app's own
    // preference (there isn't one) would win here if it ever gains one.
    app.set_reduce_motion(platform::prefers_reduced_motion());
    // Restore the last session's tabs and view unless a file was given.
    if o.files.is_empty() && !session.files.is_empty() {
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
    if o.files.is_empty() && session.files.is_empty() {
        if let Some(path) = session.recent.iter().find(|p| p.is_file()) {
            let mut host = crate::platform::NullHost::default();
            app.open_path(path.clone(), &mut host);
        }
    }
    // --mode names the view instead of cycling to it with Ctrl+E. Applied
    // here, after the session merge, so it wins over the restored mode —
    // and before the snapshot path below, which never sees a keystroke.
    if let Some(mode) = o.mode {
        app.set_mode(mode);
    }
    open_extra(&mut app, &o.files);
    // Reopen where the window was left — with or without a file argument, and
    // maximized if that is how it was closed. The platform reads this back
    // while creating the window.
    app.restore_window(session.window, session.maximized);
    // The files after the first land as tabs, in the order they were given:
    // after the session merge, so `editor a.md b.md` ends on `b.md`.
    // A crash report left by an earlier run is worth naming: the file is the
    // only witness a stripped, console-less release build leaves behind (see
    // src/crash.rs). Set last, so nothing in the startup path overwrites it —
    // and it clears itself on the user's first action, like every other
    // status hint.
    if let Some(report) = crash::latest() {
        app.status_extra = format!("previous run crashed — report: {}", report.display());
    }
    if o.pretty {
        return pretty_print(o.files.first().map(|p| p.as_path()));
    }
    #[cfg(any(test, feature = "mem-census"))]
    if o.mem_report {
        if let Some(path) = o.files.first() {
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
    // Silent update check, detached: the check lives its own life (its
    // thread may end in an installer spawn + process exit) while the window
    // opens immediately. Off by the About box toggle (persisted), by
    // --no-update, or EDITOR_NO_UPDATE_CHECK; see src/update.rs for the
    // whole story (consent dialog + SHA-256 gate sit inside it).
    // Before anything of this launch can download: delete installers an
    // earlier version staged and could not remove (Windows Installer was
    // reading its own msi as it installed). Deliberately outside the
    // twelve-hour check window below — the launch *after* an update is the
    // one that has a stale payload to clean.
    update::sweep_stale_payloads();
    if !o.no_update && session.auto_update && update::due_for_check() {
        update::background_check();
    }
    platform::run(app)
}

/// Open the files after the first as extra tabs, in the order given. A launch
/// with no file at all is the common case (double-click, Start menu), and an
/// earlier `&files[1..]` panicked on exactly that — `1..0` is not an empty
/// slice, it is a panic. `len() < 2` is the guard, and
/// `no_files_opens_no_extra_tabs` is the test that would have caught it.
fn open_extra(app: &mut App, files: &[PathBuf]) {
    if files.len() < 2 {
        return;
    }
    let mut host = crate::platform::NullHost::default();
    for f in &files[1..] {
        app.open_path(f.clone(), &mut host);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn several_files_are_kept_in_order() {
        let o = parse(&args(&["a.md", "b.json", "c.log"])).unwrap();
        assert_eq!(
            o.files,
            ["a.md", "b.json", "c.log"].map(PathBuf::from).to_vec()
        );
    }

    /// `--` ends the flags, so a file whose name starts with a dash can be
    /// opened at all — and a later `--help` is a *file*, not a request.
    #[test]
    fn a_double_dash_makes_everything_after_it_a_file() {
        let o = parse(&args(&["--no-update", "--", "-draft.md", "--help"])).unwrap();
        assert!(o.no_update);
        assert_eq!(o.immediate, None);
        assert_eq!(o.files, ["-draft.md", "--help"].map(PathBuf::from).to_vec());
    }

    #[test]
    fn flags_that_replace_the_launch_keep_the_first_one() {
        assert_eq!(
            parse(&args(&["--update", "--install"])).unwrap().immediate,
            Some(Immediate::Update)
        );
        assert_eq!(
            parse(&args(&["--version"])).unwrap().immediate,
            Some(Immediate::Version)
        );
    }

    /// The regression that shipped for one minute: launching with no file
    /// argument dereferenced `files[1..]` on an empty vector and panicked
    /// before the window was created (`cargo run` → exit 101).
    #[test]
    fn no_files_opens_no_extra_tabs() {
        let o = parse(&args(&[])).unwrap();
        let mut app = App::new(None).unwrap();
        open_extra(&mut app, &o.files);
        assert_eq!(app.tabs.len(), 1, "the untitled tab is all there is");
    }

    #[test]
    fn unknown_flags_and_missing_values_are_refused() {
        assert!(parse(&args(&["--nope"])).is_err());
        assert!(parse(&args(&["--size"])).is_err());
        assert!(parse(&args(&["--mode", "diagonal"])).is_err());
    }
}
