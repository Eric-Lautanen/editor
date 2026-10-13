//! Allocation census: a std-only [`GlobalAlloc`] wrapper that tracks live
//! and total allocated bytes, plus a small registry that answers "which
//! component holds the heap?" by tagging call sites at the few places the
//! app allocates en masse (Tab/DisplayList/Canvas/preview_rows).
//!
//! How the tagger works, procedurally:
//! ALLOC TAGS — HOW COMPONENT ATTRIBUTION WORKS
//! ---------------------------------------------------------------
//! GlobalAlloc::alloc is called INFINITELY OFTEN. To avoid asking it
//! "which component?" on every call (a lookup per allocation, which itself
//! allocates — recursive), we do the attribution OUTSIDE the hot path:
//!
//!   1. Instrumented constructors (Buffer::from_text, layout_plain,
//!      Canvas::new, Canvas::resize, rows(), Face::parse) call
//!      `mem::component_begin(Component::X)` before their bulk allocations
//!      and `component_end()` after.
//!   2. That sets a thread-local enum CURRENT (a plain 4-byte store, no
//!      allocation).
//!   3. The allocator, in `alloc`/`dealloc`, reads CURRENT (a load only)
//!      and adds/subtracts the request size to the per-component counter
//!      plus the global live/total counters. Simple arithmetic, no lookup,
//!      no lock, no recursion: the instrumentation does not itself call
//!      alloc.
//!   4. Sampling and printing live in debug/`cfg(test)`-gated helpers so
//!      release builds stay byte-identical in behavior (the counters still
//!      tick but nothing reads them — one add per alloc, ~2 ns each).
//!
//! This keeps attribution accurate enough for the "which structure is the
//! RAM?" question the operator asked (opened 2 files: 5 MB -> 12 MB) while
//! never touching a hot loop's performance characteristics beyond a
//! thread-local read + add.
//!
//! USE: `mem::report()` prints a per-component table (and the global) via
//! `platform::print_line`; call it from the CLI (`--mem-report`) or on a
//! chord after opening tabs.
//!
//! DECISION LOG
//!   - No atomics: the app is single-threaded (one window, one update
//!     pass). If a repaint thread ever appears, switch the counters to
//!     AtomicU64 and CURRENT to an AtomicU8; the call sites do not change.
//!   - No alloc inside alloc(): the wrapper only does integer math against
//!     `System`, never touches strings, maps, or formatting.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

const MAX_COMPONENTS: usize = 16;

/// Pointer -> component registry so `dealloc` attributes the free to the
/// component that made the allocation, regardless of `CURRENT` at drop
/// time (Vecs outlive their constructors: buffers, canvases, display
/// lists are all dropped on the same thread much later). Open-addressed
/// static slots: no heap inside the allocator, linear probe, `0` ptr =
/// empty. Overflow just marks the entry untagged (the table is sized for
/// ~the app's typical live pointer population; census precision, not
/// completeness, is the goal).
const TAG_SLOTS: usize = 1 << 14; // 16384
const TAG_MASK: usize = TAG_SLOTS - 1;

#[repr(C)]
struct TagSlot {
    ptr: usize,
    size: u32,
    comp: u8,
}

static mut TAGS: [TagSlot; TAG_SLOTS] = unsafe { std::mem::zeroed() };

// Variants land as components get instrumented; the discriminants are the
// stable wire names for the census, so new tags go on the end.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Component {
    None = 0,
    Buffer,
    DisplayList,
    Canvas,
    GlyphCache,
}

const NAMES: [&str; MAX_COMPONENTS] = [
    "untagged",
    "buffer",
    "display-list",
    "canvas",
    "glyph-cache",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
    "",
];

// Live and total per component.
static mut LIVE: [u64; MAX_COMPONENTS] = [0; MAX_COMPONENTS];
static mut TOTAL: [u64; MAX_COMPONENTS] = [0; MAX_COMPONENTS];
// Globally live and cumulative.
static mut GLOBAL_LIVE: u64 = 0;
static mut GLOBAL_TOTAL: u64 = 0;

thread_local! {
    /// The thread's currently attributed component. Not a lock, not atomic (see
    /// module docs): a plain enum store/load.
    ///
    /// Genuinely thread-local, which the counters need because they are
    /// process-wide: with one `CURRENT` shared by every thread, a parallel
    /// test's allocation while another thread sat inside an instrumented
    /// constructor was billed to that constructor's component. That is not
    /// theoretical — it is what made `component_covers_glyph_cache` fail
    /// intermittently, since its "live must go down after the drop" is only
    /// true if no other thread is adding to the same row.
    ///
    /// `const`-initialised so the value lives in the thread's TLS image: there
    /// is no lazy-init code and no destructor, so the allocator can read it
    /// without allocating, re-entering itself, or racing teardown.
    static CURRENT: Cell<Component> = const { Cell::new(Component::None) };
}

#[inline]
fn current() -> Component {
    CURRENT.with(|c| c.get())
}

#[inline]
fn set_current(c: Component) {
    CURRENT.with(|slot| slot.set(c));
}

pub struct Tracked;

/// Live bytes and allocations per log2 size class.
///
/// The component table answers "which structure is the heap?"; this answers
/// "and is that one huge buffer or fifty thousand small ones?" — the question
/// that told us a single 19 MB allocation was a whole font file being read for
/// two characters. Two adds per allocation, and (unlike a backtrace probe) no
/// allocation of its own, so the tag table is not disturbed by measuring.
pub static mut BUCKETS: [u64; 40] = [0; 40];
pub static mut BCOUNT: [u64; 40] = [0; 40];

#[inline]
fn bucket(size: usize) -> usize {
    (usize::BITS - size.leading_zeros()) as usize % 40
}

/// Live bytes and counts per size class, largest class first.
pub fn buckets() -> Vec<(usize, u64, u64)> {
    let mut v: Vec<(usize, u64, u64)> = Vec::new();
    unsafe {
        for i in 0..40 {
            if BCOUNT[i] > 0 {
                v.push((i, BUCKETS[i], BCOUNT[i]));
            }
        }
    }
    v.sort_by_key(|&(_, bytes, _)| std::cmp::Reverse(bytes));
    v
}

unsafe impl GlobalAlloc for Tracked {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let c = current();
        let idx = c as usize % MAX_COMPONENTS;
        let size = layout.size() as u64;
        let p = System.alloc(layout);
        if !p.is_null() {
            unsafe {
                LIVE[idx] += size;
                TOTAL[idx] += size;
                GLOBAL_LIVE += size;
                GLOBAL_TOTAL += size;
                let b = bucket(layout.size());
                BUCKETS[b] += size;
                BCOUNT[b] += 1;
                tag_set(p as usize, layout.size() as u32, idx as u8);
            }
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        // Attribute by pointer (the component that allocated it), falling
        // back to CURRENT for pre-tagging allocations.
        let cur = current() as usize % MAX_COMPONENTS;
        let idx = unsafe { tag_take(p as usize) }.unwrap_or(cur);
        unsafe {
            LIVE[idx] = LIVE[idx].saturating_sub(layout.size() as u64);
            GLOBAL_LIVE = GLOBAL_LIVE.saturating_sub(layout.size() as u64);
            let b = bucket(layout.size());
            BUCKETS[b] = BUCKETS[b].saturating_sub(layout.size() as u64);
            BCOUNT[b] = BCOUNT[b].saturating_sub(1);
        }
        System.dealloc(p, layout)
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let old_idx =
            unsafe { tag_take(p as usize) }.unwrap_or(current() as usize % MAX_COMPONENTS);
        let np = System.realloc(p, layout, new_size);
        if !np.is_null() {
            let new_idx = current() as usize % MAX_COMPONENTS;
            unsafe {
                let old = layout.size() as u64;
                let new = new_size as u64;
                LIVE[old_idx] = LIVE[old_idx].saturating_sub(old);
                GLOBAL_LIVE = GLOBAL_LIVE.saturating_sub(old);
                LIVE[new_idx] += new;
                GLOBAL_LIVE += new;
                GLOBAL_TOTAL += new;
                // TOTAL stays on the allocating component only (live is what
                // the report reads; total shows gross traffic).
                TOTAL[new_idx] += new;
                tag_set(np as usize, new_size as u32, new_idx as u8);
            }
        }
        np
    }
}

#[inline]
unsafe fn tag_set(ptr: usize, size: u32, comp: u8) {
    let mut i = ptr.wrapping_mul(0x9E37_79B9) & TAG_MASK;
    for _ in 0..8 {
        let s = &mut TAGS[i];
        if s.ptr == 0 || s.ptr == ptr {
            s.ptr = ptr;
            s.size = size;
            s.comp = comp;
            return;
        }
        i = (i + 1) & TAG_MASK;
    }
    // Full probe: leave untagged (dealloc will fall back to CURRENT).
}

#[inline]
unsafe fn tag_take(ptr: usize) -> Option<usize> {
    let mut i = ptr.wrapping_mul(0x9E37_79B9) & TAG_MASK;
    for _ in 0..8 {
        let s = &TAGS[i];
        if s.ptr == ptr {
            let comp = s.comp as usize;
            TAGS[i].ptr = 0;
            return Some(comp);
        }
        if s.ptr == 0 {
            return None;
        }
        i = (i + 1) & TAG_MASK;
    }
    None
}

// -- component attribution ------------------------------------------------

pub fn begin(c: Component) {
    set_current(c);
}

pub fn end() {
    set_current(Component::None);
}

/// One accounting sample: global + per-component.
pub struct Snapshot {
    pub global_live: u64,
    pub global_total: u64,
    /// (name, live, total) for components with any traffic, sorted by live
    /// descending.
    pub rows: Vec<(&'static str, u64, u64)>,
}

pub fn snapshot() -> Snapshot {
    let mut rows = Vec::new();
    unsafe {
        for i in 0..MAX_COMPONENTS {
            if NAMES[i].is_empty() || (LIVE[i] == 0 && TOTAL[i] == 0) {
                continue;
            }
            rows.push((NAMES[i], LIVE[i], TOTAL[i]));
        }
    }
    rows.sort_by_key(|&(_, live, total)| (std::cmp::Reverse(live), std::cmp::Reverse(total)));
    unsafe {
        Snapshot {
            global_live: GLOBAL_LIVE,
            global_total: GLOBAL_TOTAL,
            rows,
        }
    }
}

/// Formatted multi-line report of the current allocation state.
pub fn report() -> String {
    let s = snapshot();
    let mut out = format!(
        "alloc: live = {} B ({:.2} KB) | total = {} B ({:.2} MB)\n",
        s.global_live,
        s.global_live as f64 / 1024.0,
        s.global_total,
        s.global_total as f64 / 1024.0 / 1024.0,
    );
    for (name, live, total) in &s.rows {
        out.push_str(&format!(
            "  {nam:14} live {l:10} B | total {t:10} B ({tmb:.2} MB)\n",
            nam = name,
            l = live,
            t = total,
            tmb = *total as f64 / 1024.0 / 1024.0,
        ));
    }
    out.push_str("  -- live bytes by allocation size class --\n");
    for (b, bytes, count) in buckets() {
        let lo = 1usize << b.saturating_sub(1);
        out.push_str(&format!(
            "  up to {lo:>9} B live {bytes:10} B ({mb:.2} MB) count {count}\n",
            mb = bytes as f64 / 1048576.0,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: tests share the process allocator (and the process-global
    // counters) with each other and run in parallel, so every assertion
    // here must tolerate unrelated allocations/free between any two of our
    // statements. What IS deterministic: the component we tag absorbs its
    // own allocations' sizes in `total`, and its `live` never goes above
    // `total`.
    #[test]
    fn attributes_and_accounts() {
        let _before = snapshot().global_live;
        begin(Component::Buffer);
        {
            let v: Vec<u8> = vec![0u8; 4096];
            let mid = snapshot();
            let buf = mid
                .rows
                .iter()
                .find(|r| r.0 == "buffer")
                .expect("buffer row");
            assert!(buf.2 >= 4096, "buffer total must include the vec");
            assert!(buf.1 <= buf.2, "live <= total (sampled)");
            // Free inside the attribution window (see module docs): drop
            // while CURRENT still says Buffer.
            drop(v);
        }
        end();
        let after = snapshot();
        let buf2 = after.rows.iter().find(|r| r.0 == "buffer").expect("row");
        // With parallel tests other traffic can land under our tag between
        // samples, so the only deterministic invariants are the accounting
        // relations, not the trajectory.
        assert!(buf2.1 <= buf2.2, "live <= total");
        // report() must build without panicking.
        assert!(report().contains("alloc:"));
    }

    #[test]
    fn component_covers_glyph_cache() {
        let before;
        {
            begin(Component::GlyphCache);
            let m: Vec<u32> = vec![0; 1024];
            let s = snapshot();
            let row = s.rows.iter().find(|r| r.0 == "glyph-cache").expect("row");
            assert!(row.2 >= 4096);
            before = row.1;
            drop(m); // free inside the window
        }
        end();
        let s = snapshot();
        let row = s.rows.iter().find(|r| r.0 == "glyph-cache").expect("row");
        assert!(row.1 < before);
    }
}
