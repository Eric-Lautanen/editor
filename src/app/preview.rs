//! The preview orchestration (R4): relayout, the bands, the patch paths,
//! and the preview jobs — the whole cluster P1–P4 edits. Extracted from
//! app.rs with no logic changed; every method is `pub(super)` so the rest
//! of `app` (and the tests) keep working as before.

use super::*;

impl App {
    /// What the preview pane paints and hit-tests at: its own share of the
    /// shared progress. Identity outside Split.
    pub(super) fn preview_scroll(&self) -> f32 {
        if self.mode == Mode::Split {
            self.scroll_ratio() * self.preview_max()
        } else {
            self.scroll.offset
        }
    }

    pub(super) fn relayout(&mut self) {
        let w = match self.mode {
            // The preview list must wrap at the pane it is painted into, not
            // the whole window, or Split clips lines and mis-centers them.
            Mode::Split => self.split_panes().0.w,
            _ => self.size.0 as f32,
        };
        let view_h = self.viewport().h;
        let edit_w = self.editor_area().w;
        let pending = self.preview_pending();
        let streaming = self.tabs[self.active].streamer.is_some();
        // Keep the reader's place when width/zoom changes reflow the document.
        let ratio = {
            let tab = &self.tabs[self.active];
            if tab.scroll.content_height > 0.0 {
                tab.scroll.offset / tab.scroll.content_height
            } else {
                0.0
            }
        };
        // A planned pane whose text changed by an edit is patched, not
        // rebuilt: the buffer's edit log names the lines that moved, so only
        // those are measured again and the pane never waits on a
        // whole-document pass. A line pane and a JSONL's record index were
        // patched already, at the edit (see `patch_plan_for_edit`); this
        // catches Markdown, whose block index is re-derived from the
        // re-parse, and anything the edit-time pass could not finish. A
        // resize, an overflowed log or a streaming load falls through to the
        // full path below.
        if !streaming && !pending && self.mode != Mode::Edit {
            if let Some(height) = self.current_plan_height(w) {
                // Already patched at the edit: the paint only needs the editor
                // half and the pane's height, never a re-measure of the plan.
                self.install_preview(
                    layout::display_list::DisplayList::new(Vec::new(), height, Vec::new()),
                    edit_w,
                    view_h,
                    ratio,
                );
                return;
            }
            if self.plan_needs_patch(w) && self.patch_plan(w, edit_w, view_h, ratio) {
                return;
            }
        }
        // Keep the reader's place when width/zoom changes reflow the document.
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        // T1 · The editor band's covered row range, read while `self` is
        // still immutably borrowable — the Edit branch below holds `tab`.
        // Edit covers the whole viewport; Split's right half is installed
        // through `install_preview`, which recomputes for its own pane. The
        // range is over the pane's own paint offset (`editor_scroll()`), not
        // the shared raw offset: they differ in Split.
        let scroll_y = self.editor_scroll().max(0.0);
        let edit_band = (self.mode == Mode::Edit)
            .then(|| self.editor_band_range(edit_w, self.viewport(), scroll_y));
        let tab = &mut self.tabs[self.active];
        // Everything on screen persists; a big document's own scale (this
        // runs on every resize, zoom and pane switch, so whole-document work
        // at 1M+ rows is what turns those into freezes).
        //
        // A tab whose streamed load is still in flight previews as FAST
        // text — one wrapped plain pass over only the visible band, height
        // counted arithmetically — because the kind's fancy layout (CSV
        // sheet, JSON tree, markdown spans) is whole-document work and
        // would stall the pump on a big file. The same band covers the
        // window between a big tab's layout going to the worker and its
        // result landing (see `start_preview_job`).
        if tab.streamer.is_some() || pending {
            self.relayout_band(w, view_h, edit_w, ratio, pending);
            return;
        }

        if self.mode == Mode::Edit {
            // One pass over the buffer's lines and the scroll range is the
            // whole job. status_extra is untouched here: absent a reader
            // pane there is no suffix to rewrite.
            tab.status_extra.clear();
            // No reader plan is on screen in Edit, so no one will replay these
            // edits: drop them now rather than let a session's typing fill the
            // log and force the switch back to Split to rebuild from scratch.
            tab.buffer.forget_edits();
            // T1 · The editor pane is a band now: screenful-sized ops at
            // their absolute rows, full-document height. The covered range
            // rides on the tab (and the painter clips; every other consumer
            // reads height, scrollbar and hit-test geometry, which the band
            // keeps whole).
            let (bf, bn) = edit_band.expect("the Edit branch has a band range");
            tab.editor_pane.list = layout::text::editor_band(&tab.buffer, edit_w, &ctx, bf, bn);
            tab.editor_band = Some((bf, bn, edit_w, self.zoom, tab.buffer.line_count()));
            let rail = gutter::rail_w(&self.fonts, &ctx, tab.buffer.line_count());
            // h_max needs the widest display row. The old loop measured
            // every laid-out op — for a million-line pane that is a second
            // full walk *and* a full glyph-measure per line (13 s on the
            // 34 MB log), all to answer "how wide is the longest line".
            // R1's rule; T1·h_max: the whole-document scan runs when a
            // parameter it answers to changed (width, zoom, scale). A
            // keystroke's width growth lands in `tab.widest.0` through the
            // patch path's touched-row measure instead, so typing a line
            // that is not near the widest costs zero walk.
            let extent = if tab.widest.1 != edit_w
                || tab.widest.2 != self.zoom
                || tab.widest.3 != self.scale
            {
                let e = layout::text::widest_width(&tab.buffer, edit_w, &ctx, false);
                tab.widest = (e, edit_w, self.zoom, self.scale);
                e
            } else {
                tab.widest.0
            };
            // Content is painted inset by the rail, so it scrolls that much
            // farther before hitting the right edge.
            tab.h_max = (extent + rail + 32.0 * self.scale - edit_w).max(0.0);
            tab.h_scroll = tab.h_scroll.min(tab.h_max);
            tab.h_target = tab.h_target.min(tab.h_max);
            let content = tab.editor_pane.list.height;
            self.install_scroll_geometry(content, view_h, ratio);
            // This branch used to `return` without clearing the flag, so Edit
            // mode re-laid-out the whole buffer on every frame — 200 ms a
            // paint on a 754k-line file. The reader half below clears it at
            // the end; this early exit has to do the same, and has to rescan
            // the find bar for itself now that it no longer runs by accident.
            self.rescan_find();
            self.layout_dirty = false;
            return;
        }
        // C1 · Width work on an untouched line plan is never a re-lex: the
        // text the plan indexed and checkpointed is this revision's, so a
        // paint at a new width — a resize, the Split pane's narrower column,
        // the print-scale a load-time plan misses — recounts the rows and
        // carries the tint checkpoints over (see `replan_pane`). The full
        // build below would re-measure whole-document style with it: for a
        // big code file that is the entire lex again.
        if matches!(&tab.pane_plan, Some(layout::PanePlan::Lines(_)))
            && line_kind(tab.kind).is_some()
            && tab.pane_rev == tab.buffer.revision()
        {
            self.replan_pane(w);
            let tab = &self.tabs[self.active];
            let height = tab.pane_plan.as_ref().map_or(0.0, |p| p.height());
            self.install_preview(
                layout::display_list::DisplayList::new(Vec::new(), height, Vec::new()),
                edit_w,
                view_h,
                ratio,
            );
            return;
        }
        // A big document's real pane (JSON tree, Markdown spans) is seconds
        // of whole-document work on the UI thread: hand it to a worker and
        // keep the band up until it lands (`poll_preview_job` installs it).
        // A line-based pane is not in that class: counting its rows is one
        // cheap pass, and its plan paints styled text from the first frame,
        // so it is planned inline rather than waiting for a worker.
        let units = self.virtual_units;
        // C1 · A code pane's plan can either count rows inline (milliseconds,
        // tint states lazily filled at the first far band — one whole-lexed
        // pass over the document) or build on the worker with the checkpoints
        // filled as it goes. A big code file takes the worker: editing at the
        // top then jumping to the end must not re-lex the whole document on
        // the UI thread.
        let planned_inline = line_kind(tab.kind).is_some()
            && tab.buffer.line_count() >= units
            && !(line_kind(tab.kind) == Some(layout::highlight::Kind::Code)
                && self.async_layout
                && tab.buffer.line_count() > self.async_lines);
        if !planned_inline && self.async_layout && tab.buffer.line_count() > self.async_lines {
            self.relayout_band(w, view_h, edit_w, ratio, true);
            self.start_preview_job(w);
            return;
        }
        // A parse from this exact buffer is reused; one from an earlier
        // revision is not: a reload, an edit or a streamed seal all mint a new
        // revision, and the pane has to show the text it is showing now.
        let revision = tab.buffer.revision();
        let records = if tab.records_rev == revision {
            std::mem::take(&mut tab.records)
        } else {
            tab.records.clear();
            Vec::new()
        };
        let doc = tab
            .json_doc
            .take()
            .filter(|(rev, _)| *rev == revision)
            .map(|(_, d)| d);
        let ext = extension_of(tab.path.as_deref());
        let outcome = Self::preview_layout(
            PaneInput {
                kind: tab.kind,
                buffer: &tab.buffer,
                ext: &ext,
                records,
                doc,
                collapsed: &tab.collapsed,
                // C1 · Without a worker, a big code plan fills its tint
                // checkpoints now, at plan time: the fill is the same lex the
                // first far band would otherwise pay on the UI thread, and
                // here there is no off-thread pass to move it to — no user
                // frame to protect, only the load itself.
                states_eager: !self.async_layout,
            },
            w,
            &ctx,
            units,
        );
        let list = match outcome {
            // The inline call leaves `rows` empty: it was built on the UI
            // thread here, so it is built below with the list in hand.
            PreviewLayout::List { list, status, .. } => {
                tab.status_extra = status;
                tab.pane_plan = None;
                tab.markdown_doc = None;
                tab.csv_sheet = None;
                list
            }
            PreviewLayout::Jsonl {
                records,
                plan,
                status,
            } => {
                tab.status_extra = status;
                tab.pane_plan = None;
                tab.markdown_doc = None;
                tab.csv_sheet = None;
                tab.records = records;
                tab.records_errors = record_errors(&tab.records);
                tab.records_rev = revision;
                tab.plan_width = w;
                tab.plan_collapsed_gen = tab.collapsed_gen;
                let height = plan.height;
                tab.plan = plan;
                // Painted per-frame by paint_panes' band path: no ops, so no
                // index to build either.
                layout::display_list::DisplayList::new(Vec::new(), height, Vec::new())
            }
            PreviewLayout::Planned {
                plan,
                doc,
                sheet,
                status,
            } => {
                tab.status_extra = status;
                let height = plan.height();
                tab.pane_plan = Some(plan);
                tab.pane_rev = revision;
                tab.plan_width = w;
                tab.markdown_doc = doc.map(|d| (revision, d));
                tab.csv_sheet = sheet.map(|s| (revision, s));
                // Paint the band, not a list: no ops to hold or index.
                layout::display_list::DisplayList::new(Vec::new(), height, Vec::new())
            }
            PreviewLayout::Json { doc, status } => {
                tab.status_extra = status;
                tab.pane_plan = None;
                tab.markdown_doc = None;
                tab.csv_sheet = None;
                tab.json_doc = Some((revision, doc));
                tab.plan_width = w;
                tab.plan_collapsed_gen = tab.collapsed_gen;
                let height = tab.json_doc.as_ref().map_or(0.0, |(_, d)| d.plan.height);
                // Paint the band, not a list: no ops to hold or index.
                layout::display_list::DisplayList::new(Vec::new(), height, Vec::new())
            }
        };
        self.install_preview(list, edit_w, view_h, ratio);
    }

    /// R3 · The one scroll install. `relayout`'s Edit branch,
    /// `install_preview`, `relayout_band` and `poll_preview_job` wrote this
    /// identical four-line block; whatever changes scroll-on-(re)install —
    /// the editor band, the streaming pump's probe, any band work — changes
    /// it here, once.
    ///
    /// A re-install of the *same* document at the *same* place — the editor
    /// band crossing the rows it covers during a flick, a paint that rebuilds
    /// a pane mid-ride — is not an input, so it must not end the ride: the
    /// coast and a glide's target carry over. This used to go through
    /// `scroll_by(0.0)`, which is `ScrollState`'s way of saying "the pointer
    /// took over", and it killed both: since T1 made the editor pane a band,
    /// every coast long enough to matter leaves the band within about a
    /// screen, so a touch flick stopped dead in Edit (and stuttered in Split,
    /// where the editor half is a band too). Preview kept its whole list for
    /// a small file, never rebuilt, and was the one pane that felt right.
    /// Only a document whose height (or the view's place in it) changed under
    /// the ride ends it, because its pixel offsets no longer mean those rows.
    pub(super) fn install_scroll_geometry(&mut self, content: f32, view_h: f32, ratio: f32) {
        let tab = &mut self.tabs[self.active];
        let same_document = (tab.scroll.content_height - content).abs() < 0.5;
        tab.scroll.content_height = content;
        tab.scroll.viewport_height = view_h;
        let offset = (ratio * content).clamp(0.0, tab.scroll.max());
        let same_place = (offset - tab.scroll.offset).abs() < 0.5;
        tab.scroll.offset = offset;
        if same_document && same_place {
            // Both rides are measured in this document's pixels, so they
            // carry over; the destination only has to stay in range.
            tab.scroll.target = tab.scroll.target.clamp(0.0, tab.scroll.max());
        } else {
            tab.scroll.coast = 0.0;
            tab.scroll.target = offset;
        }
    }

    /// Install a done layout: the editor half if this mode has one, the pane's
    /// scroll range, and the preview the painter reads. Shared by the full
    /// relayout and the plan-patch path, so the two cannot size the pane
    /// differently.
    pub(super) fn install_preview(
        &mut self,
        list: DisplayList,
        edit_w: f32,
        view_h: f32,
        ratio: f32,
    ) {
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        // T1 · The editor band's covered range, read while `self` is still
        // immutably borrowable. Edit installs the whole-viewport pane, Split
        // its right half — both are `editor_area()` — and the rows must
        // cover the pane's own paint offset (`editor_scroll()`), not the
        // shared raw offset: they differ in Split.
        let edit_band = if self.mode != Mode::Preview {
            Some(self.editor_band_range(edit_w, self.editor_area(), self.editor_scroll().max(0.0)))
        } else {
            None
        };
        let tab = &mut self.tabs[self.active];
        if self.mode != Mode::Preview {
            // T1 · The editor pane is a band now (Edit and Split both read
            // this path); the covered range, above, is the pane's own.
            let (bf, bn) = edit_band.expect("a non-Preview mode has an editor band");
            tab.editor_pane.list = layout::text::editor_band(&tab.buffer, edit_w, &ctx, bf, bn);
            tab.editor_band = Some((bf, bn, edit_w, self.zoom, tab.buffer.line_count()));
            let rail = gutter::rail_w(&self.fonts, &ctx, tab.buffer.line_count());
            // The widest display row, at the width the whole pane needs. Same
            // cached rule as the Edit branch (`tab.widest`): the
            // whole-document scan runs when a width/zoom/scale change
            // re-asked the question; a keystroke grows the cache through
            // `grow_widest_after_edit`'s touched-rows measure, so typing in
            // Split prices its own rows, not the document. R1's arithmetic is
            // unchanged (ASCII is `len × advance`, at most one non-ASCII
            // line measured whole) — the Edit branch and the band follow the
            // same rule, so their scrolled extents agree.
            let extent = if tab.widest.1 != edit_w
                || tab.widest.2 != self.zoom
                || tab.widest.3 != self.scale
            {
                let e = layout::text::widest_width(&tab.buffer, edit_w, &ctx, false);
                tab.widest = (e, edit_w, self.zoom, self.scale);
                e
            } else {
                tab.widest.0
            };
            // Content is painted inset by the rail, so it scrolls that much
            // farther before hitting the right edge.
            tab.h_max = (extent + rail + 32.0 * self.scale - edit_w).max(0.0);
            tab.h_scroll = tab.h_scroll.min(tab.h_max);
            tab.h_target = tab.h_target.min(tab.h_max);
        }
        let content = match self.mode {
            Mode::Preview => list.height,
            Mode::Edit => tab.editor_pane.list.height,
            Mode::Split => list.height.max(tab.editor_pane.list.height),
        };
        self.install_scroll_geometry(content, view_h, ratio);
        self.preview.list = list;
        self.preview_rows = self.preview.rows(&self.fonts);
        // The word's span was measured against the old layout; after a
        // reflow it would highlight the wrong text.
        self.preview_word = None;
        self.rescan_find();
        self.layout_dirty = false;
        // Whatever this layout was made from is now what the pane shows, so
        // every logged edit is accounted for: a later patch starts from here.
        // (A streamed load's whole-document span is exactly the case: its
        // plan is built from the sealed text, and replaying that span onto it
        // would be nonsense.)
        self.tabs[self.active].buffer.forget_edits();
    }

    /// T1 · The lines the editor band must cover for a pane of `area`
    /// showing content from `scroll_y` down, on the no-wrap row grid. Two
    /// viewports of slack on each side on purpose: ordinary scrolling and
    /// the per-tick pump never rebuild; only leaving the covered range
    /// does. Reads only from `self`, so every install site calls it before
    /// the tab is borrowed mutably.
    pub(super) fn editor_band_range(
        &self,
        edit_w: f32,
        area: Rect,
        scroll_y: f32,
    ) -> (usize, usize) {
        let ctx = self.ctx();
        let p = layout::text::plain_params(edit_w, &ctx, false);
        let n = self.buffer.line_count();
        // The band's rows sit at pad + i * line_h; the visible content
        // window is [scroll_y, scroll_y + area.h).
        let first_row = (scroll_y / p.line_h).floor().max(0.0) as usize;
        let visible = ((area.h / p.line_h).ceil() as usize).max(1);
        let slack = (visible / 2).max(4);
        (
            first_row.saturating_sub(slack),
            (first_row + visible + slack + 1).min(n),
        )
    }

    /// T1 · Whether the editor pane's band still answers this frame: stale
    /// when absent, built at a different width or zoom, built from a
    /// different line count, or when the visible rows now fall outside the
    /// covered range (a scroll or a caret that followed past it).
    /// `paint_panes` checks it and falls into `relayout`, which rebuilds
    /// the band instead of painting blank rows — the reader pane's
    /// stream_top/Bottom check, for the editor half.
    ///
    /// The band answers when it *contains* the rows this view needs — that
    /// is `bf <= first && next <= bn` — so the answer here is its negation.
    /// Getting that backwards is invisible while the view sits inside the
    /// band (it rebuilds every frame, which looks right) and blank the pane
    /// the moment the view jumps outside it: the rows then missing are
    /// never rebuilt, because the question "is this stale?" was answered
    /// with the test for "is this fresh?".
    pub(super) fn editor_band_stale(&self) -> bool {
        if self.mode == Mode::Preview {
            return false;
        }
        let Some((bf, bn, w, zoom, n)) = self.tabs[self.active].editor_band else {
            return true;
        };
        let area = self.editor_area();
        if w != area.w || zoom != self.zoom || n != self.buffer.line_count() {
            return true;
        }
        // The window the pane paints, not the slack-widened one
        // `editor_band_range` builds from: asking for the slack back as well
        // marks the band stale on every single downward scroll, which is a
        // rebuild per frame rather than the "only leaving the covered range
        // rebuilds" the band exists for.
        let p = layout::text::plain_params(area.w, &self.ctx(), false);
        let first = (self.editor_scroll().max(0.0) / p.line_h).floor().max(0.0) as usize;
        // One extra row for the partial one at the bottom of the pane; the
        // window stops at the document's end, where the band does, so a view
        // of the last lines is not asked for rows that do not exist.
        let rows = (area.h / p.line_h).ceil() as usize + 1;
        first < bf || (first + rows).min(n) > bn
    }

    /// The fast band: a wrapped plain pass over only the visible rows, with
    /// the height counted arithmetically from the per-line row cache. Used
    /// while a stream is filling the buffer (its kind layout is
    /// whole-document work) and while a big tab's real layout is still on the
    /// worker — either way the window keeps painting at frame speed.
    pub(super) fn relayout_band(
        &mut self,
        w: f32,
        view_h: f32,
        edit_w: f32,
        ratio: f32,
        prefix: bool,
    ) {
        if prefix {
            self.build_preview_prefix(w);
        }
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        // T1 · The editor band's covered range, read while `self` is still
        // immutably borrowable; the pump's editor half below installs it.
        let edit_band = if self.mode != Mode::Preview {
            Some(self.editor_band_range(edit_w, self.editor_area(), self.editor_scroll().max(0.0)))
        } else {
            None
        };
        let tab = &mut self.tabs[self.active];
        let top = ratio * tab.scroll.content_height;
        // The band reaches a viewport either side of the scroll position, so
        // a small scroll between relayouts still lands on painted rows.
        let (band_top, band_bottom) = ((top - view_h).max(0.0), top + 2.0 * view_h);
        // The row-count cache rides on the tab and grows one pump's tail
        // per stagger; a width change recounts from the top (rare).
        let existing = !tab.stream_rows.is_empty() && tab.stream_width == w;
        let cached_take = if existing {
            tab.stream_rows.clone()
        } else {
            Vec::new()
        };
        let (band, rows) = layout::text::layout_plain_band(
            &tab.buffer,
            w,
            &ctx,
            band_top,
            band_bottom,
            cached_take,
            existing,
        );
        let list = match (&tab.stream_prefix, prefix) {
            (Some((_, prefix_list, cut)), true) => {
                let mut ops = prefix_list.ops.clone();
                ops.extend(
                    band.ops
                        .iter()
                        .filter(|op| match op {
                            DrawOp::Text { y, .. } => *y > *cut,
                            _ => true,
                        })
                        .cloned(),
                );
                layout::display_list::DisplayList::new(
                    ops,
                    band.height,
                    // The prefix is the pane's own layout over those lines, so
                    // its toggles and links are live too.
                    prefix_list.hits.clone(),
                )
            }
            _ => band,
        };
        // Store what this band counted back for the next one.
        tab.stream_rows = rows;
        tab.stream_width = w;
        tab.stream_top = band_top;
        tab.stream_bottom = band_bottom;
        if self.mode != Mode::Preview {
            // T1 · The band's editor half, like the others': screenful-sized
            // ops at absolute rows, full-document height. The pump's band
            // remounts this every stagger, so the editor pane waits on a
            // viewport-sized pass, not `layout_plain_edit`'s whole document.
            let (bf, bn) = edit_band.expect("a non-Preview mode has an editor band");
            tab.editor_pane.list = layout::text::editor_band(&tab.buffer, edit_w, &ctx, bf, bn);
            tab.editor_band = Some((bf, bn, edit_w, self.zoom, tab.buffer.line_count()));
            let rail = gutter::rail_w(&self.fonts, &ctx, tab.buffer.line_count());
            // Widest-row rule via `tab.widest`, as in the Edit branch: the
            // band pump is staggered, and a full-document scan every stroke
            // would re-introduce the 13 s h_max the R1 rule removed.
            let extent = if tab.widest.1 != edit_w
                || tab.widest.2 != self.zoom
                || tab.widest.3 != self.scale
            {
                let e = layout::text::widest_width(&tab.buffer, edit_w, &ctx, false);
                tab.widest = (e, edit_w, self.zoom, self.scale);
                e
            } else {
                tab.widest.0
            };
            // Content is painted inset by the rail, so it scrolls that much
            // farther before hitting the right edge.
            tab.h_max = (extent + rail + 32.0 * self.scale - edit_w).max(0.0);
            tab.h_scroll = tab.h_scroll.min(tab.h_max);
            tab.h_target = tab.h_target.min(tab.h_max);
        }
        let content = if self.mode == Mode::Preview {
            list.height
        } else {
            list.height.max(tab.editor_pane.list.height)
        };
        self.install_scroll_geometry(content, view_h, ratio);
        self.preview.list = list;
        self.preview_rows = self.preview.rows(&self.fonts);
        self.preview_word = None;
        self.layout_dirty = false;
    }

    /// Lay out the active tab's first [`PREVIEW_PREFIX_LINES`] lines for real
    /// and cache them for [`App::relayout_band`] to paint over the band. Only
    /// the line-based kinds: their layout is exact over a prefix of the file,
    /// and those are the panes where the wait for colours is measured in
    /// seconds. Rebuilt when the width changes (wrap differs).
    pub(super) fn build_preview_prefix(&mut self, w: f32) {
        let units = self.virtual_units;
        let prefix = {
            let tab = &self.tabs[self.active];
            if !matches!(
                tab.kind,
                FileKind::PlainText
                    | FileKind::Code
                    | FileKind::Log
                    | FileKind::Yaml
                    | FileKind::Toml
                    | FileKind::Ini
                    | FileKind::Csv
                    | FileKind::JsonLines
                    | FileKind::Json
            ) {
                return;
            }
            if matches!(&tab.stream_prefix, Some((built, _, _)) if *built == w) {
                return;
            }
            let lines = tab.buffer.line_count().min(PREVIEW_PREFIX_LINES);
            let mut text = String::new();
            for i in 0..lines {
                if i > 0 {
                    text.push('\n');
                }
                text.push_str(tab.buffer.line(i));
            }
            let ctx = Ctx {
                fonts: &self.fonts,
                theme: &self.theme,
                scale: self.scale,
                zoom: self.zoom,
            };
            let buffer = Buffer::from_text(&text);
            let ext = extension_of(tab.path.as_deref());
            // JSONL lays its prefix out as headed records (the same rows the
            // big file's plan gives the same lines); the rest go through the
            // ordinary kind layout, which is exact over a prefix of them.
            if tab.kind == FileKind::Json {
                // A tree needs the whole document parsed, so the first frame
                // shows the JSON as tinted text and the tree replaces it: the
                // pane carries its colours from the seal either way.
                let list = layout::highlight::layout(
                    &buffer,
                    w,
                    &ctx,
                    layout::highlight::Kind::Code,
                    "json",
                );
                let cut = list
                    .ops
                    .iter()
                    .filter_map(|op| match op {
                        DrawOp::Text { y, .. } => Some(*y),
                        _ => None,
                    })
                    .fold(0.0f32, f32::max);
                self.tabs[self.active].stream_prefix = Some((w, list, cut));
                return;
            }
            if tab.kind == FileKind::JsonLines {
                let mut status = String::new();
                let list = Self::layout_jsonl(&buffer, &tab.collapsed, &mut status, w, &ctx);
                let cut = list
                    .ops
                    .iter()
                    .filter_map(|op| match op {
                        DrawOp::Text { y, .. } => Some(*y),
                        _ => None,
                    })
                    .fold(0.0f32, f32::max);
                self.tabs[self.active].stream_prefix = Some((w, list, cut));
                return;
            }
            // The prefix is a state of the document, not its tree: a JSON
            // document gets tinted text here, so no parsed tree is handed in.
            match Self::preview_layout(
                PaneInput {
                    kind: tab.kind,
                    buffer: &buffer,
                    ext: &ext,
                    records: Vec::new(),
                    doc: None,
                    collapsed: &tab.collapsed,
                    // The streaming prefix never carries states; the real
                    // build that lands later does.
                    states_eager: false,
                },
                w,
                &ctx,
                units,
            ) {
                PreviewLayout::List { list, .. } => {
                    // Where the prefix stops: below its last baseline the
                    // band's own (plain) rows paint.
                    let cut = list
                        .ops
                        .iter()
                        .filter_map(|op| match op {
                            DrawOp::Text { y, .. } => Some(*y),
                            _ => None,
                        })
                        .fold(0.0f32, f32::max);
                    Some((list, cut))
                }
                // No planned pane lays out a prefix: JSONL and JSON have
                // their own above, and a planned line or block pane paints
                // styled text itself from the first frame.
                PreviewLayout::Jsonl { .. }
                | PreviewLayout::Json { .. }
                | PreviewLayout::Planned { .. } => None,
            }
        };
        self.tabs[self.active].stream_prefix = prefix.map(|(list, cut)| (w, list, cut));
    }

    /// Whether the active tab's big layout is currently on the worker.
    pub(super) fn preview_pending(&self) -> bool {
        self.preview_job
            .as_ref()
            .is_some_and(|j| j.tab == self.active)
    }

    /// Start the active tab's reader layout on a worker thread. The pane keeps
    /// the fast band until [`App::poll_preview_job`] installs the result, so a
    /// multi-second whole-document layout never runs inside a paint.
    pub(super) fn start_preview_job(&mut self, width: f32) {
        let idx = self.active;
        let tab = &self.tabs[idx];
        // An unedited buffer *is* its saved text, so hand that over instead of
        // cloning the buffer: one memcpy rather than one allocation per line.
        // An edited tab has to hand over the buffer itself.
        let source = if tab.buffer.dirty {
            JobSource::Owned(tab.buffer.clone())
        } else {
            JobSource::Saved(tab.saved_text.clone())
        };
        let collapsed = tab.collapsed.clone();
        let kind = tab.kind;
        let ext = extension_of(tab.path.as_deref());
        let collapsed_gen = tab.collapsed_gen;
        let revision = tab.buffer.revision();
        let (scale, zoom) = (self.scale, self.zoom);
        let units = self.virtual_units;
        let mode = self.mode;
        let theme = self.theme.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("preview-layout".into())
            .spawn(move || {
                let buffer = match source {
                    JobSource::Saved(text) => Buffer::from_text(&text),
                    JobSource::Owned(buffer) => buffer,
                };
                // Font metrics are interior-mutable (glyph caches), so the
                // worker parses its own copy of the same embedded faces. The
                // metrics — and therefore every wrap and baseline — are
                // identical, which is what makes both paths agree.
                let fonts = FontSystem::load_embedded();
                let ctx = Ctx {
                    fonts: &fonts,
                    theme: &theme,
                    scale,
                    zoom,
                };
                // The worker parses its own copy: the tree it produces travels
                // back to the pane with it, so there is nothing to hand in.
                let out = Self::preview_layout(
                    PaneInput {
                        kind,
                        buffer: &buffer,
                        ext: &ext,
                        records: Vec::new(),
                        doc: None,
                        collapsed: &collapsed,
                        // C1 · The worker build fills the code plan's tint
                        // checkpoints as it runs, so a band far into the
                        // file never re-lexes the document to learn what
                        // colour its lines are in.
                        states_eager: true,
                    },
                    width,
                    &ctx,
                    units,
                );
                // The row index is a second whole-document walk; it belongs
                // to the same off-thread pass as the list it indexes.
                let out = match out {
                    PreviewLayout::List { list, status, .. } => {
                        let view = PreviewView { list };
                        let rows = view.rows(&fonts);
                        PreviewLayout::List {
                            list: view.list,
                            rows,
                            status,
                        }
                    }
                    other => other,
                };
                let _ = tx.send(out);
            });
        if spawned.is_err() {
            // No worker available: let the next relayout run it inline.
            self.preview_job = None;
            self.layout_dirty = true;
            return;
        }
        self.preview_job = Some(PreviewJob {
            rx,
            tab: idx,
            revision,
            kind,
            width,
            mode,
            scale,
            zoom,
            collapsed_gen,
        });
    }

    /// Install a finished worker layout, or drop it when the tab has moved on
    /// (another buffer revision, width, zoom, mode or collapse set).
    pub(super) fn poll_preview_job(&mut self, host: &mut dyn Host) {
        let Some(job) = self.preview_job.as_ref() else {
            return;
        };
        let outcome = match job.rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                // The worker died without sending a result (it should never
                // happen — the sender is moved into the closure — but a
                // panicked closure is exactly this) — the band on screen is
                // then stale, and nothing else would ever revive it: no
                // future event marks the layout dirty. Start over now.
                self.preview_job = None;
                self.layout_dirty = true;
                host.request_redraw();
                return;
            }
        };
        let job = self.preview_job.take().expect("checked above");
        let view_h = self.viewport().h;
        let width = match self.mode {
            Mode::Split => self.split_panes().0.w,
            _ => self.size.0 as f32,
        };
        let tab = &self.tabs[job.tab];
        let fresh = job.tab == self.active
            && job.revision == tab.buffer.revision()
            && job.kind == tab.kind
            && job.mode == self.mode
            && job.width == width
            && job.scale == self.scale
            && job.zoom == self.zoom
            && job.collapsed_gen == tab.collapsed_gen;
        if !fresh {
            // The view changed while it ran: the band is still up, and the
            // next relayout starts a job that matches the new state.
            self.layout_dirty = true;
            host.request_redraw();
            return;
        }
        let ratio = if tab.scroll.content_height > 0.0 {
            tab.scroll.offset / tab.scroll.content_height
        } else {
            0.0
        };
        let tab = &mut self.tabs[job.tab];
        let mut rows = Vec::new();
        let height = match outcome {
            PreviewLayout::List {
                list,
                rows: built,
                status,
            } => {
                tab.status_extra = status;
                tab.pane_plan = None;
                tab.markdown_doc = None;
                tab.csv_sheet = None;
                let h = list.height;
                self.preview.list = list;
                rows = built;
                h
            }
            PreviewLayout::Jsonl {
                records,
                plan,
                status,
            } => {
                tab.status_extra = status;
                tab.pane_plan = None;
                tab.markdown_doc = None;
                tab.csv_sheet = None;
                tab.records_errors = record_errors(&tab.records);
                tab.records = records;
                tab.records_rev = job.revision;
                tab.plan_width = job.width;
                tab.plan_collapsed_gen = job.collapsed_gen;
                let h = plan.height;
                tab.plan = plan;
                self.preview.list =
                    layout::display_list::DisplayList::new(Vec::new(), h, Vec::new());
                h
            }
            PreviewLayout::Planned {
                plan,
                doc,
                sheet,
                status,
            } => {
                tab.status_extra = status;
                let h = plan.height();
                tab.pane_plan = Some(plan);
                tab.pane_rev = job.revision;
                tab.plan_width = job.width;
                tab.markdown_doc = doc.map(|d| (job.revision, d));
                tab.csv_sheet = sheet.map(|s| (job.revision, s));
                self.preview.list =
                    layout::display_list::DisplayList::new(Vec::new(), h, Vec::new());
                h
            }
            PreviewLayout::Json { doc, status } => {
                tab.status_extra = status;
                tab.pane_plan = None;
                tab.markdown_doc = None;
                tab.csv_sheet = None;
                // The job's own freshness check pins `job.revision` to the
                // buffer this tree came from.
                let h = doc.plan.height;
                tab.json_doc = Some((job.revision, doc));
                tab.plan_width = job.width;
                tab.plan_collapsed_gen = job.collapsed_gen;
                self.preview.list =
                    layout::display_list::DisplayList::new(Vec::new(), h, Vec::new());
                h
            }
        };
        // The job's layout is what the pane now shows; any edit up to this
        // point is part of the text it was built from.
        tab.buffer.forget_edits();
        let content = match self.mode {
            Mode::Preview => height,
            Mode::Edit => tab.editor_pane.list.height,
            Mode::Split => height.max(tab.editor_pane.list.height),
        };
        self.install_scroll_geometry(content, view_h, ratio);
        let tab = &mut self.tabs[self.active];
        // The band's row cache described the same text; the real pane's own
        // layout is what scrolls now, so drop it.
        tab.stream_rows.clear();
        tab.stream_width = 0.0;
        tab.stream_prefix = None;
        // The row index was built with the list on the worker (empty for a
        // virtualized JSONL pane, which paints bands and indexes nothing).
        self.preview_rows = rows;
        self.preview_word = None;
        self.rescan_find();
        self.layout_dirty = false;
        host.request_redraw();
    }

    /// One drain step for the active tab's streamed load, called from the
    /// tick heartbeat. The chunk is bounded, so each tick stays a few
    /// milliseconds; sealing (the tail is read through) hands the tab the
    /// saved-text snapshot a synchronous load would have taken.
    pub(super) fn pump_stream(&mut self, host: &mut dyn Host) {
        if self.streamer.is_none() {
            return;
        }
        let Some(mut loader) = self.tabs[self.active].streamer.take() else {
            return;
        };
        // `drain` answers "finished", not "still loading": true means the
        // tail has been read and the buffer was rebuilt as one revision.
        let done = loader.drain(&mut self.tabs[self.active].buffer);
        self.tabs[self.active].log_tally = None;

        if done {
            // The loader hands the snapshot over: copying it here was another
            // whole-file allocation on the UI thread at the seal.
            let saved = loader.into_saved();
            self.tabs[self.active].mark_saved(&saved);
        } else {
            self.tabs[self.active].streamer = Some(loader);
        }
        // New content must reach the screen: the layout caches key on the
        // buffer revision, so without this flag the drain grows the buffer
        // while the view keeps showing the head the open painted.
        //
        // The relayout of a *sealed* load is whole-document work for the
        // kind's pane, so it runs once, at the seal. While the stream is live
        // the relayout takes the fast band path (see `relayout`); still
        // O(viewport) per stroke, it too is staggered over
        // [STREAM_RELAYOUT_TICKS] ticks, and the cheap repaints between
        // staggers reuse the last list.
        // The seal must always reach the screen: it is the one relayout that
        // swaps the fast band for the kind's real pane, and a repaint between
        // staggers would otherwise clear the flag before it landed.
        if done || self.ticks.is_multiple_of(STREAM_RELAYOUT_TICKS) {
            self.layout_dirty = true;
        }
        // The spinner in the preview pane must rotate smoother than the 40 ms
        // tick: ask the host for its fast frame clock while the load runs —
        // the same one a glide uses, retired at the seal (on_frame drops it
        // when nothing else needs frames).
        if !done {
            self.stream_animating = true;
            self.start_glide_clock(host);
            host.set_animating(true);
        }
        // A redraw request alone is not enough for the heartbeat: the tick
        // handler's own branches above may go on to nothing (no caret, no
        // glide), so make the repaint explicit.
        host.request_redraw();
    }

    /// The reader pane for `kind`: either the finished display list or the
    /// document a virtualized pane paints bands of. Whole-document work — the
    /// part that makes a big tab expensive — so the same call runs inline for a
    /// small tab and on a worker thread for a big one, and both produce
    /// identical panes (no `self`).
    pub(super) fn preview_layout(
        input: PaneInput<'_>,
        width: f32,
        ctx: &Ctx,
        units: usize,
    ) -> PreviewLayout {
        let PaneInput {
            kind,
            buffer,
            ext,
            records,
            doc,
            collapsed,
            states_eager,
        } = input;
        let mut status = String::new();
        let list = match kind {
            FileKind::Json => {
                // One parse serves both paths. A tree big enough that laying it
                // all out is the cost being avoided goes virtual (the caller
                // keeps the tree); a smaller one lays out whole, as it always
                // has. A tree handed back in is re-measured from itself — the
                // edit that made a relayout necessary may not have been
                // localizable, but a resize or a toggle still must not re-parse
                // a minified document of a million nodes.
                // A tree handed back in stays the pane's tree whatever its
                // size: an edit that broke most of it must not demote the pane
                // to a whole-document layout (which would drop the tree and
                // re-parse on the next keystroke). Only a fresh parse decides
                // whether a document is worth keeping at all.
                let held = doc.is_some();
                let parsed = match doc {
                    Some(mut doc) => {
                        doc.replan(collapsed, width, ctx);
                        Ok(doc)
                    }
                    None => layout::json_tree::Doc::parse(&buffer.text(), collapsed, width, ctx),
                };
                match parsed {
                    Ok(doc) => {
                        if held || doc.plan.is_virtual() {
                            // A tree kept across a broken edit still reports it:
                            // a resize or a fold does not make the document
                            // parse again.
                            status = json_status_of(&doc);
                            return PreviewLayout::Json { doc, status };
                        }
                        layout::json_tree::layout_value(&doc.value, collapsed, width, ctx)
                    }
                    Err(crate::error::Error::Parse { line, col, msg }) => {
                        status = format!("error {line}:{col}");
                        let snippet = buffer.line(
                            line.saturating_sub(1)
                                .min(buffer.line_count().saturating_sub(1)),
                        );
                        layout::json_tree::layout_error(line, col, &msg, Some(snippet), width, ctx)
                    }
                    Err(other) => {
                        status = "error".into();
                        layout::json_tree::layout_error(1, 1, &other.to_string(), None, width, ctx)
                    }
                }
            }
            FileKind::JsonLines if buffer.line_count() <= VIRTUAL_JSONL_LINES => {
                Self::layout_jsonl(buffer, collapsed, &mut status, width, ctx)
            }
            FileKind::JsonLines => {
                // Virtualized: the parse is the cost, so reuse records the
                // caller already holds; a cold call parses here instead.
                let mut records = records;
                if records.is_empty() {
                    records = jsonl::parse_parallel(&buffer.text());
                }
                status = record_count_status(&records);
                let plan = layout::json_tree::build_plan(&records, collapsed, width, ctx, true);
                return PreviewLayout::Jsonl {
                    records,
                    plan,
                    status,
                };
            }
            FileKind::Markdown => {
                let doc = markdown::parse_buffer(buffer);
                // A block's height is whatever its spans wrap to, so planning
                // Markdown is a full measure pass. Above the threshold the
                // plan still pays for itself: the pane then paints a band per
                // frame and keeps one `f32` per block instead of every op.
                if doc.blocks.len() >= units {
                    let plan = layout::markdown::plan(&doc, width, ctx);
                    return PreviewLayout::Planned {
                        plan: layout::PanePlan::Markdown(plan),
                        doc: Some(doc),
                        sheet: None,
                        status,
                    };
                }
                layout::markdown::layout(&doc, width, ctx)
            }
            FileKind::Csv => {
                let sheet = crate::parse::csv::parse_with_ext(&buffer.text(), None);
                status = if sheet.cols == 0 {
                    String::new()
                } else if sheet.capped {
                    format!(
                        "{} rows × {} cols (showing {})",
                        sheet.total_rows,
                        sheet.cols,
                        sheet.rows.len()
                    )
                } else {
                    format!("{} rows × {} cols", sheet.total_rows, sheet.cols)
                };
                // S1 · The sheet paints in bands from a plan, not from a
                // whole-document display list rebuilt every frame: the plan
                // holds the width scan the layout already ran and the sheet
                // itself rides on the tab beside its revision.
                if sheet.cols > 0 {
                    return PreviewLayout::Planned {
                        plan: layout::PanePlan::Csv(layout::csv::plan(&sheet, width, ctx)),
                        doc: None,
                        sheet: Some(sheet),
                        status,
                    };
                }
                layout::csv::layout(&sheet, width, ctx)
            }
            // The line-based panes: plain text, logs, configs and code share
            // one grid, one plan and one band painter. Above the threshold
            // they return the plan; below it each keeps its full list.
            _ => {
                if let Some(hk) = line_kind(kind) {
                    if buffer.line_count() >= units {
                        let plan = if hk == layout::highlight::Kind::Plain {
                            layout::PanePlan::Text(layout::text::plan_lines(buffer, width, ctx))
                        } else {
                            layout::PanePlan::Lines(layout::highlight::build_plan_states(
                                buffer,
                                width,
                                ctx,
                                hk,
                                ext,
                                states_eager,
                            ))
                        };
                        if kind == FileKind::Log {
                            status = log_counts(buffer);
                        }
                        return PreviewLayout::Planned {
                            plan,
                            doc: None,
                            sheet: None,
                            status,
                        };
                    }
                }
                match kind {
                    FileKind::Yaml => layout::highlight::layout(
                        buffer,
                        width,
                        ctx,
                        layout::highlight::Kind::Yaml,
                        "",
                    ),
                    FileKind::Toml => layout::highlight::layout(
                        buffer,
                        width,
                        ctx,
                        layout::highlight::Kind::Toml,
                        "",
                    ),
                    FileKind::Ini => layout::highlight::layout(
                        buffer,
                        width,
                        ctx,
                        layout::highlight::Kind::Ini,
                        "",
                    ),
                    FileKind::Log => {
                        status = log_counts(buffer);
                        layout::highlight::layout(
                            buffer,
                            width,
                            ctx,
                            layout::highlight::Kind::Log,
                            "",
                        )
                    }
                    FileKind::Code => layout::highlight::layout(
                        buffer,
                        width,
                        ctx,
                        layout::highlight::Kind::Code,
                        ext,
                    ),
                    _ => layout::text::layout_plain(buffer, width, ctx, true),
                }
            }
        };
        PreviewLayout::List {
            list,
            rows: Vec::new(),
            status,
        }
    }

    /// Test/CLI helper: is the active tab's big layout still on the worker?
    #[cfg(test)]
    pub fn preview_job_pending(&self) -> bool {
        self.preview_pending()
    }

    /// Bring the planned pane up to date with the edits since it was built.
    ///
    /// The buffer logs which line ranges each edit replaced, so a line pane
    /// recounts only those lines and shifts the index below them; a Markdown
    /// pane re-parses (block structure is the parser's output, not a line
    /// index) and measures only the block run that changed. Returns whether
    /// the plan now describes the buffer: `false` means the caller should lay
    /// the document out whole, exactly as before.
    pub(super) fn patch_plan(&mut self, w: f32, edit_w: f32, view_h: f32, ratio: f32) -> bool {
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        let tab = &mut self.tabs[self.active];
        let revision = tab.buffer.revision();
        let kind = tab.kind;
        let ext = extension_of(tab.path.as_deref());
        if tab.pane_plan.is_none() {
            // A virtual JSONL keeps its records and index on the tab rather
            // than in a `PanePlan`, and a virtual JSON tree keeps its nodes,
            // ranges and plan there: the same patch, a different home, and it
            // takes the edit log itself.
            return match tab.kind {
                FileKind::Json => self.patch_json_plan(w, edit_w, view_h, ratio),
                _ => self.patch_jsonl_plan(w, edit_w, view_h, ratio),
            };
        }
        // The log is consumed either way: a Markdown patch does not use line
        // spans, and no consumer may see these once this attempt is done.
        let edits = tab.buffer.take_edits();
        // S1/T6 · The CSV keystroke reproduces the sheet: one re-parse of the
        // capped rows and one fresh plan from it, both inline. The frame
        // afterwards is a band either way; what a row patch (T6's other
        // option) would spare is the parse.
        let mut csv_replanned = None;
        let plan = tab.pane_plan.as_mut().expect("checked above");
        let patched = match plan {
            layout::PanePlan::Text(rows) => match &edits {
                Some(edits) => layout::text::patch_lines(rows, &tab.buffer, w, &ctx, edits),
                None => false,
            },
            layout::PanePlan::Lines(p) => match &edits {
                Some(edits) => {
                    let hk = line_kind(kind).unwrap_or(layout::highlight::Kind::Plain);
                    p.patch(&tab.buffer, w, &ctx, hk, &ext, edits)
                }
                None => false,
            },
            layout::PanePlan::Markdown(p) => {
                // A huge Markdown document is planned on the worker: re-parsing
                // it here would block the frame for as long as the worker did,
                // so the edit takes the existing off-thread path instead.
                if self.async_layout && tab.buffer.line_count() > self.async_lines {
                    return false;
                }
                let Some((_, old)) = tab.markdown_doc.take() else {
                    return false;
                };
                // T2-slice · A keystroke re-parses only the block run it
                // landed in (blank-line-bounded, ref-def-safe); a whole
                // re-parse stays exactly the fallback an unlocalizable edit
                // takes.
                let (doc, splice) = match edits.as_deref().unwrap_or(&[]) {
                    [] => (old, None),
                    edits => crate::parse::markdown::block::patch_document(old, &tab.buffer, edits),
                };
                match splice {
                    Some(run) => {
                        let ok = p.patch_run(&doc, w, &ctx, run);
                        tab.markdown_doc = Some((revision, doc));
                        ok
                    }
                    None => {
                        tab.markdown_doc = Some((revision, doc));
                        false
                    }
                }
            }
            layout::PanePlan::Csv(_) => {
                if self.async_layout && tab.buffer.line_count() > self.async_lines {
                    return false;
                }
                let sheet = crate::parse::csv::parse_with_ext(&tab.buffer.text(), None);
                csv_replanned = Some(layout::csv::plan(&sheet, w, &ctx));
                tab.csv_sheet = Some((revision, sheet));
                true
            }
        };
        if let Some(fresh) = csv_replanned {
            tab.pane_plan = Some(layout::PanePlan::Csv(fresh));
            tab.plan_width = w;
        }
        if !patched {
            return false;
        }
        tab.pane_rev = revision;
        // The counts in the status line are a scan of the text, so they are
        // as stale as the plan was.
        if kind == FileKind::Log {
            super::log_status_for(tab, edits.as_deref());
        }
        let height = tab.pane_plan.as_ref().map_or(0.0, |p| p.height());
        self.finish_paid_patch(height, edit_w, view_h, ratio);
        true
    }

    /// R2 · The one paid-patch tail. `patch_plan`, `patch_json_plan` and
    /// `patch_jsonl_plan` end the same way: the pane shows its *plan* (an
    /// empty display list of the plan's height — the band painter does the
    /// per-frame work), the patch counter ticks in the tests, and the patch
    /// reports landed. Each patch refreshes its own status fact before
    /// calling — the three facts differ in shape, so they stay at the call
    /// sites — but the install and the counter are one block now, which is
    /// exactly where T2/T3/T4's counters and band work will plug in.
    pub(super) fn finish_paid_patch(&mut self, height: f32, edit_w: f32, view_h: f32, ratio: f32) {
        self.install_preview(
            layout::display_list::DisplayList::new(Vec::new(), height, Vec::new()),
            edit_w,
            view_h,
            ratio,
        );
        #[cfg(test)]
        {
            self.plan_patches += 1;
        }
    }

    /// Bring a virtual JSON pane up to date with the edits since it was built:
    /// the node each span landed in is parsed again, spliced into the tree with
    /// its neighbours untouched, and only its rows are measured. Everything
    /// above, beside and below it keeps the measurement it had, so a keystroke
    /// costs the value being typed into rather than the document. Returns
    /// whether the tree, its ranges and its plan now describe the buffer.
    pub(super) fn patch_json_plan(&mut self, w: f32, edit_w: f32, view_h: f32, ratio: f32) -> bool {
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        let tab = &mut self.tabs[self.active];
        if !Self::json_doc_active(tab, w) {
            return false;
        }
        let revision = tab.buffer.revision();
        let Some(edits) = tab.buffer.take_edits() else {
            return false;
        };
        let patched = match tab.json_doc.as_mut() {
            Some((rev, doc)) => {
                // The revision is stale by definition — the buffer moved past
                // the tree — so the patch is what brings them back together.
                let ok = doc.patch(&tab.buffer, &edits, &tab.collapsed, w, &ctx);
                if ok {
                    *rev = revision;
                }
                ok
            }
            None => false,
        };
        let (height, status) = match &tab.json_doc {
            Some((_, doc)) if patched => (doc.plan.height, json_status(&tab.json_doc)),
            _ => return false,
        };
        tab.status_extra = status;
        self.finish_paid_patch(height, edit_w, view_h, ratio);
        true
    }

    /// Bring a virtual JSONL pane up to date with the edits since it was
    /// built: the lines the spans replaced are parsed again and measured, and
    /// the record index below them shifts. The other records keep their
    /// measurement, so a keystroke into a 33 MB JSONL costs one record rather
    /// than the whole parse it used to redo. Returns whether the pane's
    /// records and index now describe the buffer.
    pub(super) fn patch_jsonl_plan(
        &mut self,
        w: f32,
        edit_w: f32,
        view_h: f32,
        ratio: f32,
    ) -> bool {
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        let tab = &mut self.tabs[self.active];
        if !Self::jsonl_plan_active(tab, w) {
            return false;
        }
        let revision = tab.buffer.revision();
        let Some(edits) = tab.buffer.take_edits() else {
            return false;
        };
        // T4 · the errored-record count is running: the patch reports its
        // exact delta, and the status formats the two numbers.
        let (patched, error_delta) = layout::json_tree::Plan::patch(
            &mut tab.plan,
            &mut tab.records,
            &tab.buffer,
            &tab.collapsed,
            w,
            &ctx,
            &edits,
        );
        if !patched {
            return false;
        }
        tab.records_errors = (tab.records_errors as isize + error_delta) as usize;
        tab.records_rev = revision;
        tab.status_extra = record_count_status_counted(tab.records.len(), tab.records_errors);
        let height = tab.plan.height;
        self.finish_paid_patch(height, edit_w, view_h, ratio);
        true
    }

    /// Rebuild the active tab's planned pane at `w`. A resize, zoom or switch
    /// into Split changes the wrap, and every height in a plan is the wrap's.
    /// Line plans count rows again; a Markdown plan re-measures from the
    /// parsed document it keeps beside its revision, never a re-parse.
    pub(super) fn replan_pane(&mut self, w: f32) {
        let revision = self.tabs[self.active].buffer.revision();
        let fresh = {
            let ctx = self.ctx();
            let tab = &self.tabs[self.active];
            match &tab.pane_plan {
                Some(layout::PanePlan::Text(_)) => Some(layout::PanePlan::Text(
                    layout::text::plan_lines(&tab.buffer, w, &ctx),
                )),
                Some(layout::PanePlan::Lines(_)) => line_kind(tab.kind).map(|hk| {
                    // C1 · The replan is width work only: the fresh plan
                    // adopts the old plan's known tint checkpoints (a text
                    // property, not a width one), so the eager fill a worker
                    // build paid for survives a mode switch or resize.
                    let mut fresh = layout::highlight::build_plan(&tab.buffer, w, &ctx, hk);
                    if let Some(layout::PanePlan::Lines(old)) = &tab.pane_plan {
                        fresh.adopt_states(old, tab.buffer.line_count());
                    }
                    layout::PanePlan::Lines(fresh)
                }),
                Some(layout::PanePlan::Markdown(_)) => match &tab.markdown_doc {
                    Some((rev, doc)) if *rev == revision => Some(layout::PanePlan::Markdown(
                        layout::markdown::plan(doc, w, &ctx),
                    )),
                    _ => None,
                },
                Some(layout::PanePlan::Csv(_)) => match &tab.csv_sheet {
                    Some((rev, sheet)) if *rev == revision => {
                        Some(layout::PanePlan::Csv(layout::csv::plan(sheet, w, &ctx)))
                    }
                    _ => None,
                },
                None => None,
            }
        };
        let tab = &mut self.tabs[self.active];
        if let Some(plan) = fresh {
            tab.pane_plan = Some(plan);
            tab.pane_rev = revision;
        }
        tab.plan_width = w;
    }

    /// C1 · The headless open. `--render` and the perf gate drain a streamed
    /// load inline — there is no worker (`async_layout` off) — and their
    /// first paint used to build the pane's plan there, which for a big code
    /// file is the whole-document checkpoint fill billed to the frame the
    /// gate budgets. Plan the pane at construction: the fill belongs to
    /// load, where this harness family already invoices its parsing, and the
    /// first paint takes the plan through `relayout`'s width hand-off (row
    /// recount + [`Self::replan_pane`]'s state adoption, milliseconds).
    pub(super) fn preplan_pane(&mut self) {
        if self.async_layout {
            // The interactive open hands the same fill to the worker and
            // keeps the band up while it lands.
            return;
        }
        let tab = &self.tabs[self.active];
        if tab.path.is_none()
            || tab.streamer.is_some()
            || line_kind(tab.kind) != Some(layout::highlight::Kind::Code)
            || tab.buffer.line_count() < self.virtual_units
        {
            return;
        }
        let ext = extension_of(tab.path.as_deref());
        let ctx = Ctx {
            fonts: &self.fonts,
            theme: &self.theme,
            scale: self.scale,
            zoom: self.zoom,
        };
        let revision = tab.buffer.revision();
        let width = crate::platform::DEFAULT_SIZE.0 as f32;
        let plan = layout::highlight::build_plan_states(
            &tab.buffer,
            width,
            &ctx,
            layout::highlight::Kind::Code,
            &ext,
            true,
        );
        let tab = &mut self.tabs[self.active];
        tab.pane_plan = Some(layout::PanePlan::Lines(plan));
        tab.pane_rev = revision;
        tab.plan_width = width;
    }

    /// S2 · The reader band for this frame, with the Markdown cache in
    /// front of the build. A markdown pane that scrolls inside the units
    /// its last band covered reuses those ops; anything else (a different
    /// kind, a wider scroll, a changed revision/width/zoom/scale) builds a
    /// fresh band and becomes the next cached one.
    /// S2 · The reader band for this frame, with the Markdown cache in
    /// front of the build. A markdown pane that scrolls inside the units
    /// its last band covered reuses those ops; anything else (a different
    /// kind, a wider scroll, a changed revision/width/zoom/scale) builds a
    /// fresh band and becomes the next cached one.
    pub(super) fn band_cached(&mut self, pane_w: f32, top: f32, bottom: f32) -> DisplayList {
        if let Some(list) = self.cached_md_band(pane_w, top, bottom) {
            return list;
        }
        let band = self.band_for(pane_w, &self.ctx(), top, bottom);
        self.store_md_band(pane_w, self.zoom, self.scale, top, bottom, &band);
        band
    }

    /// The cached Markdown band's ops when they still describe the request:
    /// the pane is a planned markdown pane, the cache's revision/width/zoom
    /// and scale are the topic's, and the units the request touches are a
    /// subset of the units the cached band covers — each of which it rents
    /// whole, so the viewport's clip does the edges.
    fn cached_md_band(&self, pane_w: f32, top: f32, bottom: f32) -> Option<DisplayList> {
        let tab = &self.tabs[self.active];
        let Some(layout::PanePlan::Markdown(p)) = &tab.pane_plan else {
            return None;
        };
        let c = tab.md_band.as_ref()?;
        let rev = tab.buffer.revision();
        if c.rev != rev
            || !matches!(&tab.markdown_doc, Some((r, _)) if *r == rev)
            || c.width != pane_w
            || c.zoom != self.zoom
            || c.scale != self.scale
        {
            return None;
        }
        let units = p.rows.band(top, bottom);
        if units.start >= c.units.start && units.end <= c.units.end {
            return Some(c.list.clone());
        }
        None
    }

    /// Keep the band a miss just built as the next cache entry — only for a
    /// planned markdown pane; every other kind builds fresh every frame and
    /// stays that way.
    fn store_md_band(
        &mut self,
        pane_w: f32,
        zoom: f32,
        scale: f32,
        top: f32,
        bottom: f32,
        list: &DisplayList,
    ) {
        let tab = &mut self.tabs[self.active];
        let (units, rev) = match (&tab.pane_plan, &tab.markdown_doc) {
            (Some(layout::PanePlan::Markdown(p)), Some((rev, _))) => {
                (p.rows.band(top, bottom), *rev)
            }
            _ => return,
        };
        tab.md_band = Some(MdBand {
            rev,
            width: pane_w,
            zoom,
            scale,
            units,
            list: list.clone(),
        });
        #[cfg(test)]
        {
            self.md_band_built += 1;
        }
    }

    /// R5 · The one band dispatch. `paint_panes` used to build every planned
    /// kind's band in its own match arm; the arms are one method now, so
    /// S1's CSV band, T1's editor band and S4's measurement each add an arm
    /// here rather than a copy of the whole assembly.
    pub(super) fn band_for(&self, pane_w: f32, ctx: &Ctx, top: f32, bottom: f32) -> DisplayList {
        if let Some(plan) = &self.pane_plan {
            let ext = extension_of(self.path.as_deref());
            match plan {
                // Every planned kind paints the same way: its own
                // painter over the units the plan says the viewport
                // covers, at document y.
                layout::PanePlan::Text(rows) => {
                    layout::text::layout_lines_band(&self.buffer, rows, pane_w, ctx, top, bottom)
                }
                layout::PanePlan::Lines(p) => {
                    let hk = line_kind(self.kind).unwrap_or(layout::highlight::Kind::Plain);
                    p.layout_band(&self.buffer, pane_w, ctx, hk, &ext, top..bottom)
                }
                layout::PanePlan::Markdown(p) => match &self.markdown_doc {
                    Some((rev, doc)) if *rev == self.buffer.revision() => {
                        layout::markdown::layout_band(doc, p, pane_w, ctx, top, bottom)
                    }
                    _ => layout::display_list::DisplayList::default(),
                },
                layout::PanePlan::Csv(p) => match &self.csv_sheet {
                    Some((rev, sheet)) if *rev == self.buffer.revision() => {
                        layout::csv::layout_band(sheet, p, pane_w, ctx, top, bottom)
                    }
                    _ => layout::display_list::DisplayList::default(),
                },
            }
        } else {
            match self.kind {
                FileKind::JsonLines => layout::json_tree::layout_band(layout::json_tree::Band {
                    plan: &self.plan,
                    records: &self.records,
                    buf: &self.buffer,
                    collapsed: &self.collapsed,
                    width: pane_w,
                    ctx,
                    top,
                    bottom,
                }),
                // `banded` above is exactly this branch's condition: a
                // JSON pane with a tree and a plan big enough to band.
                _ => match &self.json_doc {
                    Some((_, doc)) => {
                        layout::json_tree::layout_value_band(layout::json_tree::ValueBand {
                            value: &doc.value,
                            plan: &doc.plan,
                            collapsed: &self.collapsed,
                            width: pane_w,
                            ctx,
                            top,
                            bottom,
                        })
                    }
                    None => layout::display_list::DisplayList::default(),
                },
            }
        }
    }
}
