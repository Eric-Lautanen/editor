//! Streaming big-file loads: the window opens at once, the rest settles
//! behind it.
//!
//! `Tab::from_path` read and parsed the whole file synchronously, so a
//! 42 MB table froze the window for the better part of two seconds — long
//! enough to read as "the app hung". Nothing the user *sees* in that
//! window needs 400k rows to exist: one screen is a few hundred lines, and
//! the scrollbar height can come from the line count long before the rows
//! do.
//!
//! So a load over [`STREAM_HEAD_BYTES`] is split: the first
//! [`STREAM_HEAD_BYTES`] (≳ the first screen of any real file) are shown
//! synchronously, and the remainder is drained by [`Loader::drain`] — one
//! bounded chunk per `Event::Tick`, each a few milliseconds — until the
//! file ends. The finish ("seal") swaps the tab's buffer to the whole-file
//! text in one rebuilt revision and snapshots the saved text, exactly
//! what a synchronous load produced — so the dirty compare, the reload
//! watch and every revision-keyed cache see what they always saw.
//!
//! One invariant describes the whole module: **the file's text is
//! [`Loader::head_text`] + [`Loader::decoded`]**, and the buffer during
//! the drain shows a *prefix* of it that ends after a '\n' (complete
//! lines only; a chunk cut mid-line is held back by [`Loader::shown`]
//! until the next chunk completes the line — delayed, never truncated or
//! lost). The seal rebuilds the buffer from that invariant in one step:
//! nothing is re-read, nothing is lost.

use crate::core::buffer::Buffer;
use std::io::Read;
use std::path::Path;

/// How much of a big file is shown before the window opens: ~10k rows at
/// the ~100-byte lines these documents have. The user's first screen lives
/// here with room to spare.
pub const STREAM_HEAD_BYTES: usize = 1_000_000;

/// One drain per tick, bounded: each tick is one sequential read, a UTF-8
/// decode and the line split for that much text — all on the UI thread at
/// the heartbeat, so it is sized to stay a frame's work (1 MB measures
/// ~15-20 ms) rather than to finish the tail fast. 4 MB ticks measured
/// 90-180 ms, which read as a stutter every heartbeat.
pub const STREAM_CHUNK_BYTES: usize = 1_000_000;

/// The result of [`load`].
pub enum Loaded {
    /// The whole file is in the buffer; nothing else will arrive. The old
    /// synchronous-shaped contract.
    Whole { buffer: Buffer, saved_text: String },
    /// The head is shown (`buffer`); the tail streams in on ticks until
    /// [`Loader::drain`] reports done and the caller seals the tab.
    Streaming { buffer: Buffer, loader: Box<Loader> },
}

/// An in-flight streaming load. Owns the file handle; `head_text +
/// decoded` is the whole file (the invariant above).
pub struct Loader {
    file: std::fs::File,
    /// Bytes one drain reads: [`STREAM_CHUNK_BYTES`] in production,
    /// injected small in tests so the same logic turns over on a fixture
    /// of a few dozen bytes.
    chunk: usize,
    /// The complete head lines (ends with '\n' or empty): what the buffer
    /// was built from at open, and the first half of the file text.
    head_text: String,
    /// Every byte the file produced after the head cut, in order. Starts
    /// with the head read's trailing partial line, if any — bytes that
    /// were consumed from the handle but are not shown until a '\n'
    /// completes them. Grows by one chunk per drain; the seal adds no
    /// bytes to it.
    decoded: String,
    /// How many bytes of `decoded` the buffer already shows. Only ever
    /// advances to a '\n' boundary, so the buffer never holds a half
    /// line.
    shown: usize,
    /// Bytes of the last read that ended inside a multi-byte UTF-8 char:
    /// re-fed to the next read so no char is ever decoded twice or
    /// mangled by a lossy chunk boundary.
    scratch: Vec<u8>,
    saved: String,
    done: bool,
}

impl Loader {
    /// Read up to [`Loader::chunk`] more bytes, extend the buffer with
    /// every now-complete line, and report whether the load is finished —
    /// when it is, the caller takes [`Loader::saved_text`]. Bounded by
    /// the chunk: the cost of one tick, never the cost of the rest of
    /// the file; that is the point.
    pub fn drain(&mut self, buffer: &mut Buffer) -> bool {
        if self.done {
            return true;
        }
        let mut chunk = vec![0u8; self.chunk];
        let (n, _) = read_upto(&mut self.file, &mut chunk, &mut self.scratch);
        if n > 0 {
            self.decoded.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
        let more = self.more_file(n);
        // The showable frontier: the last '\n' in decoded while file
        // continues; everything on the last chunk.
        let frontier = if more {
            self.decoded.rfind('\n').map_or(0, |i| i + 1)
        } else {
            self.decoded.len()
        };
        let from = self.shown;
        if frontier > from {
            // Straight out of `decoded`: copying the fresh span into a String
            // first was a second full copy of every chunk, per tick.
            for line in self.decoded[from..frontier].split_inclusive('\n') {
                buffer.append_line(line.strip_suffix('\n').unwrap_or(line));
            }
            self.shown = frontier;
        }
        if more {
            false
        } else {
            self.seal(buffer)
        }
    }

    /// Whether bytes follow what has been read so far. The file's size is
    /// the truth; a metadata miss falls back on "the chunk was filled",
    /// which the next drain (an empty read) settles in one call.
    fn more_file(&self, just_read: usize) -> bool {
        match self.file.metadata() {
            Ok(m) => {
                let cursor = self.head_text.len() as u64
                    + self.decoded.len() as u64
                    + self.scratch.len() as u64;
                cursor < m.len() || just_read == self.chunk
            }
            Err(_) => just_read == self.chunk,
        }
    }

    /// The load is finished: snapshot the file's text and close the buffer.
    ///
    /// The buffer does *not* get rebuilt here. [`Buffer::append_line`] was
    /// made to produce exactly the lines `Buffer::from_text` would (CRLF
    /// trimming, the head's trailing slot), so the only difference left is
    /// the empty last line a newline-terminated text gains, which one append
    /// adds. Rebuilding instead cost a per-line allocation for the whole
    /// file — hundreds of milliseconds on a large one, on the UI thread, at
    /// the end of a load that had stayed smooth until then.
    fn seal(&mut self, buffer: &mut Buffer) -> bool {
        if !self.done {
            let mut full = std::mem::take(&mut self.head_text);
            full.push_str(&self.decoded);
            if full.ends_with('\n') {
                buffer.append_line("");
            }
            buffer.sealed();
            self.saved = full;
            self.done = true;
        }
        true
    }

    /// The saved-text snapshot a synchronous load would have taken. Only
    /// meaningful once [`Loader::drain`] has reported done.
    pub fn saved_text(&self) -> &str {
        &self.saved
    }

    /// The same snapshot, moved out: the caller's tab wants to own it, and
    /// copying the whole file's text there was one more full-size copy on the
    /// UI thread at the seal.
    pub fn into_saved(self) -> String {
        self.saved
    }
}

/// Read up to `buf.len()` bytes into `buf` starting with `scratch`'s
/// contents (held-back bytes from the previous read), and return
/// `(count, leftover)`: `count` covers whole UTF-8 characters only — a
/// chunk ending inside a multi-byte char would decode lossily into a
/// replacement, mangling bytes the next read completes — and `leftover`
/// holds the trim's tail for the next call. The file cursor ends after
/// both.
fn read_upto(file: &mut std::fs::File, buf: &mut [u8], scratch: &mut Vec<u8>) -> (usize, Vec<u8>) {
    let cap = buf.len();
    let mut n = 0usize;
    let want = cap.min(scratch.len());
    if want > 0 && n < cap {
        buf[n..n + want].copy_from_slice(&scratch[..want]);
        n += want;
    }
    if n < cap {
        n += read_upto_plain(file, &mut buf[n..]);
    }
    // Trim to the last complete UTF-8 char inside `n`: a chunk cut inside
    // a multi-byte char would decode lossily into a replacement char,
    // mangling bytes the next read completes. The held-back bytes replace
    // the scratch for the next read.
    let mut end = n;
    if let Err(e) = std::str::from_utf8(&buf[..end]) {
        if e.error_len().is_none() {
            // Trailing incomplete char: it completes next time.
            let valid = e.valid_up_to();
            let held = buf[valid..end].to_vec();
            end = valid;
            *scratch = held;
        }
    }
    (end, Vec::new())
}

/// Plain saturating read of up to `buf.len()` bytes; returns the count.
fn read_upto_plain(file: &mut std::fs::File, buf: &mut [u8]) -> usize {
    let mut n = 0usize;
    while n < buf.len() {
        match file.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(m) => n += m,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    n
}

/// Load `path`: synchronously when small ([`Loaded::Whole`], the exact old
/// contract) and split when large ([`Loaded::Streaming`]: head shown, the
/// rest settled on ticks, one seal at the end).
pub fn load(path: &Path) -> std::io::Result<Loaded> {
    load_with(path, STREAM_HEAD_BYTES, STREAM_CHUNK_BYTES)
}

/// [`load`], with the pacing constants injectable so the tests exercise
/// the same logic on fixtures of a few dozen bytes.
pub fn load_with(path: &Path, head_bytes: usize, chunk_bytes: usize) -> std::io::Result<Loaded> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size as usize <= head_bytes {
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let saved_text = text.clone();
        return Ok(Loaded::Whole {
            buffer: Buffer::from_text(&text),
            saved_text,
        });
    }
    let mut head = vec![0u8; head_bytes];
    let mut scratch = Vec::new();
    let (n, _) = read_upto(&mut file, &mut head, &mut scratch);
    let head_all = String::from_utf8_lossy(&head[..n]);
    // Cut the shown head on a line end; the trailing partial line (the
    // head read consumed it, but a half line must not be shown) begins
    // the drain's accumulator.
    let cut = head_all.rfind('\n').map_or(0, |i| i + 1);
    let buffer = Buffer::from_text(&head_all[..cut]);
    Ok(Loaded::Streaming {
        buffer,
        loader: Box::new(Loader {
            file,
            chunk: chunk_bytes,
            head_text: head_all[..cut].to_string(),
            decoded: head_all[cut..].to_string(),
            shown: 0,
            scratch,
            saved: String::new(),
            done: false,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A fixture whose boundaries are all interesting: the head cut lands
    /// mid-line, the tail chunks cut inside lines, and the file ends in
    /// both shapes.
    fn fixture(head: usize, rows: usize, trailing_newline: bool) -> String {
        let mut s = String::new();
        s.push_str(&"x".repeat(head - 20));
        s.push_str("HEADCUT continues past the head edge\n");
        s.push_str(&"y".repeat(head * 3));
        s.push('\n');
        for i in 0..rows {
            s.push_str(&format!("row-{i} padding\n"));
        }
        s.push_str("final line");
        if trailing_newline {
            s.push('\n');
        }
        s
    }

    fn written(name: &str, body: &str) -> PathBuf {
        let dir = crate::testing::temp_dir().join("editor_load_tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    /// Stream the fixture with tiny pacing and hand back buffer + snapshot.
    fn stream_all(path: &Path) -> (Buffer, String, usize) {
        match load_with(path, 200, 24).unwrap() {
            Loaded::Whole { .. } => panic!("the fixture must stream"),
            Loaded::Streaming {
                mut loader,
                mut buffer,
            } => {
                let mut pumps = 0;
                while !loader.drain(&mut buffer) {
                    pumps += 1;
                    assert!(pumps < 10_000, "the drain never finished");
                }
                let saved = loader.saved_text().to_string();
                (buffer, saved, pumps)
            }
        }
    }

    #[test]
    fn a_streamed_load_ends_up_identical_to_a_synchronous_one() {
        for trailing in [true, false] {
            let body = fixture(200, 40, trailing);
            let p = written("stream_fixture.txt", &body);
            let (buffer, saved, _) = stream_all(&p);
            let sync = Buffer::from_text(&body);
            assert_eq!(
                buffer.text(),
                sync.text(),
                "trailing newline = {trailing}: streamed text diverges"
            );
            assert_eq!(saved, body, "the snapshot must be the file's text");
        }
    }

    #[test]
    fn the_buffer_shows_a_growing_prefix_of_the_file_while_draining() {
        let body = fixture(200, 200, true);
        let p = written("stream_prefix.txt", &body);
        match load_with(&p, 200, 24).unwrap() {
            Loaded::Streaming {
                mut loader,
                mut buffer,
            } => {
                let mut pumps = 0;
                let mut last_len = buffer.text().len();
                while !loader.drain(&mut buffer) {
                    pumps += 1;
                    let now = buffer.text().len();
                    assert!(now >= last_len, "shown content shrank mid-drain");
                    last_len = now;
                    // The shown text is always a prefix of the whole file
                    // ending on a line boundary. (`Buffer::text()` joins
                    // lines with '\n', so a buffer whose last line is
                    // empty ends with '\n'; a real mid-drain buffer whose
                    // last line is *partial* is impossible by the showable
                    // frontier rule — the assert checks the file prefix.)
                    let shown = buffer.text();
                    assert!(
                        body.starts_with(&shown),
                        "drain {pumps}: shown text is not a file prefix: {shown:?}"
                    );
                }
                assert!(pumps > 3, "the sweep should take several ticks");
            }
            Loaded::Whole { .. } => panic!("must stream"),
        }
    }

    #[test]
    fn each_drain_is_bounded_by_the_chunk_not_the_file() {
        let body = fixture(200, 400, true);
        let p = written("stream_bounds.txt", &body);
        let (_, _, pumps) = stream_all(&p);
        // The sweep is in file-size/chunk territory, not line count: the
        // drain reads one chunk per call, whatever the chunk contains.
        assert!(
            pumps <= body.len() / 24 + 8,
            "a {} byte file took {pumps} pumps at chunk 24",
            body.len()
        );
    }

    #[test]
    fn a_small_file_loads_whole_with_no_loader() {
        let p = written("small.txt", "a\nb\n");
        match load_with(&p, 1 << 20, 1 << 20).unwrap() {
            Loaded::Whole { buffer, saved_text } => {
                assert_eq!(buffer.text(), "a\nb\n");
                assert_eq!(saved_text, "a\nb\n");
            }
            Loaded::Streaming { .. } => panic!("a small file must not stream"),
        }
    }

    #[test]
    fn a_utf8_char_cut_by_a_chunk_edge_is_not_lost() {
        // λ is 2 bytes; a 3-byte head lands between two of them. The seal
        // decodes each chunk independently, and a lossy decode of a
        // mid-char cut would corrupt that char — which is why the decode
        // is per-chunk lossy but the *seal* never re-decodes: it builds
        // from decoded text. The end state must equal a sync load.
        let body = "λ".repeat(64);
        let p = written("utf8.txt", &body);
        match load_with(&p, 3, 5).unwrap() {
            Loaded::Streaming {
                mut loader,
                mut buffer,
            } => {
                while !loader.drain(&mut buffer) {}
                let sync = Buffer::from_text(&body);
                assert_eq!(buffer.text(), sync.text());
                assert_eq!(loader.saved_text(), body);
            }
            Loaded::Whole { .. } => panic!("must stream"),
        }
    }

    #[test]
    fn a_file_ending_without_a_newline_still_completes() {
        let mut body = fixture(200, 4, false);
        body.push_str("no trailing newline");
        let p = written("no_newline.txt", &body);
        let (buffer, saved, _) = stream_all(&p);
        assert_eq!(buffer.text(), body);
        assert_eq!(saved, body);
    }
}
