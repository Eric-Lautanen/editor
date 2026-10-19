//! The per-document tab and everything it owns: the buffer, the saved text,
//! the revision/dirty/load plumbing, and the support types the preview
//! engine moves through (R4: extracted from the app.rs haystack; no logic
//! changed, only the door between modules — `pub(super)`).

//! NOTE: `use super::*` brings the whole `app` scope in; everything
//! `tab.rs` declares is `pub(super)` so the rest of `app` sees it.

use super::*;

/// S2 · The reader pane's cached Markdown band: the ops the last band
/// laid out, the units they cover, and the things the band answers to (the
/// buffer revision, the pane width, the zoom and the DPI scale). A scroll
/// that stays inside the covered units and changes none of them reuses the
/// ops instead of re-rendering every visible block per frame.
#[derive(Clone)]
pub(super) struct MdBand {
    pub(super) rev: u64,
    pub(super) width: f32,
    pub(super) zoom: f32,
    pub(super) scale: f32,
    pub(super) units: std::ops::Range<usize>,
    pub(super) list: DisplayList,
}

// Files above this are refused rather than opened (see [`size_error`]):
/// everything Editor does — buffer, layout, render — is whole-document, so
/// a huge file is a memory and latency trap, and windowing it is not in this
/// milestone.
pub(super) const MAX_OPEN_BYTES: u64 = 64 * 1024 * 1024;

/// The refusal message for an oversized file, or `None` when it fits.
/// Split out so the rule is testable without writing a 64 MB fixture.
pub(super) fn size_error(len: u64) -> Option<String> {
    (len > MAX_OPEN_BYTES).then(|| {
        format!(
            "file is {} MB — larger than the {} MB open limit",
            len >> 20,
            MAX_OPEN_BYTES >> 20
        )
    })
}

impl Tab {
    pub(super) fn untitled() -> Self {
        Self {
            path: None,
            kind: FileKind::PlainText,
            buffer: Buffer::from_text(""),
            editor: EditorView::default(),
            editor_pane: PreviewView::default(),
            saved_text: String::new(),
            saved_lines: 1, // an empty text is one line
            edited_from: usize::MAX,
            log_tally: None,
            records_errors: 0,
            collapsed: HashSet::new(),
            status_extra: String::new(),
            scroll: ScrollState::default(),
            h_scroll: 0.0,
            h_target: 0.0,
            h_max: 0.0,
            mtime: None,
            plan: layout::json_tree::Plan::default(),
            records: Vec::new(),
            records_rev: 0,
            plan_width: 0.0,
            plan_collapsed_gen: 0,
            collapsed_gen: 0,
            json_doc: None,
            streamer: None,
            stream_rows: Vec::new(),
            editor_band: None,
            widest: (0.0, 0.0, 0.0, 0.0),
            stream_width: 0.0,
            stream_top: 0.0,
            stream_bottom: 0.0,
            stream_prefix: None,
            pane_plan: None,
            pane_rev: 0,
            markdown_doc: None,
            csv_sheet: None,
            md_band: None,
        }
    }

    pub(super) fn from_path_streaming(p: PathBuf, headless: bool) -> Result<Self> {
        // Refuse oversized files before reading: the text is held in
        // memory and laid out at once, so a huge file is a memory and
        // latency trap — a clear refusal beats a frozen window.
        if let Some(msg) = size_error(std::fs::metadata(&p)?.len()) {
            return Err(crate::error::Error::Platform(msg));
        }
        let kind = FileKind::from_path(&p);
        let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        match crate::load::load(&p)? {
            crate::load::Loaded::Whole { buffer, saved_text } => {
                let saved_lines = saved_text.split("\n").count();
                Ok(Self {
                    kind,
                    buffer,
                    saved_text,
                    saved_lines,
                    edited_from: usize::MAX,
                    mtime,
                    path: Some(p),
                    streamer: None,
                    ..Self::untitled()
                })
            }
            crate::load::Loaded::Streaming { mut loader, buffer } => {
                let mut tab = Self {
                    kind,
                    mtime,
                    path: Some(p),
                    streamer: None,
                    ..Self::untitled()
                };
                tab.buffer = buffer;
                if headless {
                    while !loader.drain(&mut tab.buffer) {}
                    tab.saved_text = loader.saved_text().to_string();
                    tab.saved_lines = tab.saved_text.split("\n").count();
                    tab.edited_from = usize::MAX;
                } else {
                    tab.streamer = Some(*loader);
                }
                Ok(tab)
            }
        }
    }
    /// T7 · The text became the saved one again (a load, a save, a reload):
    /// cache its line count and clear the edit watermark, so the dirty
    /// compare carries only the guarantee this capture makes — that the
    /// whole buffer is the snapshot.
    pub fn mark_saved(&mut self, text: &str) {
        self.saved_text = text.to_string();
        self.saved_lines = text.split("\n").count();
        self.edited_from = usize::MAX;
    }
    /// Short name for the title bar and window title.
    pub fn name(&self) -> &str {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("Untitled")
    }
}

/// What a reader-pane layout job produced: either the finished display list
/// or, for a virtualized pane, the parsed document and its plan — those panes
/// paint a band of it themselves, per frame, and index nothing.
pub(super) enum PreviewLayout {
    List {
        list: DisplayList,
        /// The pane's row index (drag-selection, hover, copy). Built beside
        /// the list on the worker: for a huge pane it is another
        /// whole-document walk, and it must not land on the UI thread either.
        rows: Vec<preview_view::Row>,
        status: String,
    },
    Jsonl {
        records: Vec<jsonl::Record>,
        plan: layout::json_tree::Plan,
        status: String,
    },
    Json {
        /// The tree with its node ranges and plan: an edit patches the node it
        /// landed in rather than re-parsing the document.
        doc: layout::json_tree::Doc,
        status: String,
    },
    /// A planned line or block pane: the plan is what the pane paints from,
    /// so no display list exists for the whole document.
    Planned {
        plan: layout::PanePlan,
        /// Markdown's parsed document, kept for re-planning at a new width.
        doc: Option<crate::parse::markdown::ast::Document>,
        /// S1 · The CSV sheet kept beside its revision, the same seat
        /// markdown's document takes.
        sheet: Option<crate::parse::csv::Sheet>,
        status: String,
    },
}

/// What a JSON tree says about itself: nothing while every node is a value the
/// parser produced, or the first node it could not describe. The pane keeps the
/// tree either way (that is the point of [`layout::json_tree::Doc::patch`]), so
/// this is how a reader learns the text under it does not parse.
pub(super) fn json_status_of(doc: &layout::json_tree::Doc) -> String {
    match &doc.broken {
        Some(msg) => format!("error: {msg}"),
        None => String::new(),
    }
}

/// [`json_status_of`] for a tab's tree, absent when there is none.
pub(super) fn json_status(doc: &Option<(u64, layout::json_tree::Doc)>) -> String {
    doc.as_ref()
        .map_or(String::new(), |(_, d)| json_status_of(d))
}

/// The document a worker lays out, handed over without rebuilding anything on
/// the UI thread: the tab's own text when it is unedited (one flat copy), or
/// its buffer when it has been.
pub(super) enum JobSource {
    Saved(String),
    Owned(Buffer),
}

/// What a reader pane is laid out from: the document, and whatever was already
/// parsed out of it. A relayout hands the caller's own parses back in (a resize
/// must not re-parse a 33 MB JSONL, or a minified JSON that is one line and a
/// million nodes); a worker passes them empty and parses its own copy.
pub(super) struct PaneInput<'a> {
    pub(super) kind: FileKind,
    pub(super) buffer: &'a Buffer,
    pub(super) ext: &'a str,
    pub(super) records: Vec<jsonl::Record>,
    pub(super) doc: Option<layout::json_tree::Doc>,
    pub(super) collapsed: &'a HashSet<usize>,
    /// C1 · Fill the code line plan's tint checkpoints during the build
    /// instead of lazily at the first far band. True only for worker builds:
    /// the scan is whole-document work, which is why a big code pane is
    /// planned off-thread at all.
    pub(super) states_eager: bool,
}

/// A big tab's reader layout building on a worker thread (see
/// [`App::start_preview_job`]). The result only installs while every field
/// here still describes the tab it was built for; anything else means the
/// document moved under it and the layout must run again.
pub(super) struct PreviewJob {
    pub(super) rx: std::sync::mpsc::Receiver<PreviewLayout>,
    pub(super) tab: usize,
    pub(super) revision: u64,
    pub(super) kind: FileKind,
    pub(super) width: f32,
    pub(super) mode: Mode,
    pub(super) scale: f32,
    pub(super) zoom: f32,
    pub(super) collapsed_gen: u32,
}

/// One open file: everything that is per-document lives here.
pub struct Tab {
    pub path: Option<PathBuf>,
    pub kind: FileKind,
    pub buffer: Buffer,
    pub editor: EditorView,
    /// Plain-text pane backing Edit mode and the bottom half of Split.
    pub editor_pane: PreviewView,
    /// Last saved (or loaded) text, normalized exactly as the buffer holds
    /// it. `dirty` is recomputed against this after every edit, so undoing
    /// back to the saved content clears the tab's save button again.
    pub saved_text: String,
    /// T7 · The saved text's line count, cached beside it. The dirty compare
    /// needs the count only to refuse early, and counting it by walking the
    /// whole saved snapshot for it priced an O(document) pass per keystroke.
    pub saved_lines: usize,
    /// T7 · The smallest line any edit since the load or save touched. The
    /// dirty compare starts there: a mutation only changes lines at and
    /// below its first edited line (`EditSpan::start`), so everything above
    /// the watermark is what the snapshot had. `usize::MAX` is the fresh or
    /// just-saved state — nothing differs, nothing to walk. [`Tab::mark_saved`]
    /// resets it; the edit log is capped and `install_preview` eats it, so a
    /// compare-time read would under-count: this field carries the min.
    pub edited_from: usize,
    /// T4 · The running log tally: severity per line and the window
    /// counts, one edit behind the buffer. `None` says the text was
    /// replaced wholesale since the last tally (a load, a reload, a
    /// stream drain) and the next edit re-seeds it by full scan.
    pub log_tally: Option<super::LogTally>,
    /// T4 · The errored-record count `records` summarize: seeded whenever
    /// a rebuild lands a whole record list, and the record patch moves
    /// it by its exact per-edit delta, so the status line never
    /// recounts the list per keystroke.
    pub records_errors: usize,
    /// Collapsed JSON tree nodes (pre-order ids, stable across toggles).
    pub collapsed: HashSet<usize>,
    /// Right-side status suffix set by layout (`error 2:5`, `3 records`, …).
    pub status_extra: String,
    pub scroll: ScrollState,
    /// Horizontal scroll of the editor pane (no-wrap lines), its glide
    /// destination, and its max. Mirrors the vertical scroller: coarse
    /// wheel steps glide `h_scroll` toward `h_target` over frames, while
    /// pointer-driven input (thumb drag, finger, caret) sets both at once.
    pub h_scroll: f32,
    pub h_target: f32,
    pub h_max: f32,
    /// File modification time as last seen or written (reload detection).
    pub mtime: Option<std::time::SystemTime>,
    /// Virtual-layout plan for a big JSONL (record tops, heights, id bases).
    /// Empty for every other kind — small JSONLs take the plain layout.
    pub plan: layout::json_tree::Plan,
    /// The parsed JSONL records, for the virtualized path: the plan and the
    /// band layouts index into this. Relayouts (scroll, resize, collapse) reuse
    /// it instead of re-parsing a 33 MB document per frame — but only while
    /// `records_rev` still names this buffer, or a reload's pane would keep
    /// showing the file as it was before (a reload reuses the tab).
    pub records: Vec<jsonl::Record>,
    /// The buffer revision `records` was parsed from.
    pub records_rev: u64,
    /// Width the current `records`/`plan` were built at, so a resize rebuilds
    /// them and a repaint at the same width does not.
    pub plan_width: f32,
    /// Collapsed-set generation at which `plan` was built; a toggle changes
    /// record heights, so the plan is rebuilt against the new set.
    pub plan_collapsed_gen: u32,
    /// Bumped on every collapse toggle so the plan knows to rebuild.
    pub collapsed_gen: u32,
    /// The parsed tree of a virtualized JSON document — the tree, every node's
    /// source range, and its plan — paired with the buffer revision it was
    /// parsed from. A one-line 5 MB document is a single line of source and a
    /// million nodes, so it can never be re-parsed per relayout: a resize,
    /// zoom, toggle or scroll reuses this. The revision is what keeps a *stale*
    /// tree from outliving an edit: a keystroke patches the one node it landed
    /// in (`Doc::patch`), and only an edit that cannot be localized drops it and
    /// re-parses.
    pub json_doc: Option<(u64, layout::json_tree::Doc)>,
    /// An in-flight streamed load (a file over the head threshold): pumped
    /// once per tick until it seals; `None` for a synchronously loaded one.
    pub streamer: Option<crate::load::Loader>,
    /// During streaming: per-line wrapped-row counts from the last stagger,
    /// aligned with the buffer's lines at that stagger. Lines are
    /// append-only during a stream, so the next stagger wraps only the
    /// added tail instead of the whole document — the O(new lines) that
    /// keeps the pump off the 450 ms full-rescan deadline.
    pub stream_rows: Vec<u32>,
    /// T1 · What the editor pane's list currently *covers*: its first and
    /// next-exclusive line, the `edit_w` and zoom it was built at, and the
    /// line count then. The pane's list is a band now (screenful-sized ops,
    /// full-document height); this is the guard that says when the band no
    /// longer answers — the scroll left the covered range, a width/zoom
    /// change, a line-count change from an edit or pump stagger, or any edit
    /// touching a visible line (see `App::editor_band_stale`, which a scan
    /// of one edit's touched rows covers through `relayout`).
    pub editor_band: Option<(usize, usize, f32, f32, usize)>,
    /// T1 · The widest editor row's rendered extent, and the (edit width,
    /// zoom, scale) it was computed at. Edits grow it in place via the
    /// touched rows (`text::touched_extent`); only a width/zoom/scale
    /// change runs the whole-document scan again. A deletion that removes
    /// the widest row leaves the stored extent larger than the text needs
    /// until the next parameter change — a scrollbar a few px long, kept
    /// only until then — traded for typing that never walks the document
    /// (T1's headline win).
    pub widest: (f32, f32, f32, f32),
    /// Width the streaming height was counted at (resizes recount).
    pub stream_width: f32,
    /// The y range the last fast band covered: a scroll that leaves it
    /// rebuilds the band instead of painting blank space while a big tab's
    /// real layout is still on the worker.
    pub stream_top: f32,
    pub stream_bottom: f32,
    /// A highlighted prefix of a big pane, painted over the fast band while
    /// the worker lays out the whole document: the top of the file shows its
    /// syntax colours at once instead of waiting seconds for the rest, and
    /// the prefix is exact because those kinds lay out line by line. Holds
    /// the width it was built at and the y where it stops.
    pub(super) stream_prefix: Option<(f32, DisplayList, f32)>,
    /// The banded reader plan for this tab: a big line-based pane's rows, or a
    /// big Markdown document's blocks. `None` means the pane is a
    /// whole-document display list (the small-file path, unchanged).
    pub pane_plan: Option<layout::PanePlan>,
    /// The buffer revision `pane_plan` was built from. Plans index the text,
    /// so a plan from an earlier revision must never paint a later one.
    pub pane_rev: u64,
    /// Parsed Markdown kept beside its revision: a resize or zoom re-plans
    /// from this instead of re-parsing a big document.
    pub markdown_doc: Option<(u64, crate::parse::markdown::ast::Document)>,
    /// S1 · The parsed CSV sheet beside the buffer revision it is the parse
    /// of: the pane's plan paints units from it, and re-planning (a resize,
    /// or T6's documented keystroke re-parse) builds from this without
    /// holding the whole display list again.
    pub csv_sheet: Option<(u64, crate::parse::csv::Sheet)>,
    /// S2 · The cached Markdown band of the reader pane, `None` until the
    /// first banded markdown frame built one.
    pub(super) md_band: Option<MdBand>,
}
