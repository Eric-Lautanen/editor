# Readitor sample

A tour of everything the renderer understands. The point of this file is that
it is *long*: open it and scroll — the overlay scrollbar on the right is the
thing you are aiming at — then press `Ctrl+E` for Edit mode to see the
line-number gutter, the no-wrap editor, and its own horizontal scrollbar.

A paragraph with **bold**, *italic*, ***bold italic***, `inline code`,
~~struck text~~, and a [link](https://example.com). A hard break ends this
line,  
and the text carries on underneath.

Autolinks work too: <https://example.org/docs>. So do images, which render as
a measured placeholder:

![a placeholder image](assets/logo.png)

## Headings and rules

Text directly under an H2 renders at full size. Headings scale with their
level, and the top two levels draw a rule beneath them.

### H3 — a deeper heading

Body text again, this time under an H3.

#### H4 and below

Deeper levels still render, set apart by weight rather than size.

---

## Lists

Unordered lists nest arbitrarily:

- one
- two
  - nested
    - deeper still
- three

Ordered lists keep their own counters, and nested ordered lists restart them:

1. first
2. second
    1. nested one
    2. nested two
3. third

Task lists carry a box per item, checked or not:

- [x] done task
- [ ] todo task
- [x] another done task

A list item holding a whole paragraph still hangs its continuation lines under
the bullet, so the marker column stays clear.

## Quotes

> A blockquote.
>
> It can hold several paragraphs, **formatting**, and lists:
>
> - quoted bullet
> - second bullet
>
> Nested quotes:
>
> > an inner quote

## Code

Fenced code with an info string:

```rust
fn main() {
    println!("hi");
}
```

Fenced code without one:

```text
plain fenced block
line two
line three
```

One deliberately very long line, so Edit mode's no-wrap editor has something to
scroll sideways through:

```text
0123456789 abcdefghij klmnopqrst uvwxyz 0123456789 abcdefghij klmnopqrst uvwxyz 0123456789 abcdefghij klmnopqrst uvwxyz 0123456789 abcdefghij klmnopqrst uvwxyz the end
```

## Tables

| Name | Value | Notes |
|------|------:|:------|
| a    | 1     | short |
| b    | 2     | a much longer cell that has to be measured |
| c    | 3     | pipe \| escaped |
| d    | 4     | *emphasis* in a cell |

Ragged rows are padded out to the column count:

| left | right |
|:-----|------:|
| 1    |
| 2    | 2 |

## Unicode and fallback

café – “curly quotes” • bullets … → arrows ≤ ≥ © ® ™ € £ ½ ñ Ü 中文

An inline `code span with 中文` falls back to a face that has the glyph.

## Why so long?

The reader follows the scrollbar, so the document has to be taller than the
window — otherwise the bar never appears and there is nothing to drag. Below
is a run of ordinary prose, section by section, to make that true on a
maximized 1920×1200 window as well as a small one.

### Section one

The quick brown fox jumps over the lazy dog. The quick brown fox jumps over
the lazy dog. The quick brown fox jumps over the lazy dog. Wrapping happens at
spaces, and a long unbroken word — antidisestablishmentarianism — simply runs
past the column rather than being cut in half.

Reading is mostly scrolling, so the bar wants to be wide enough to hit and
dark enough to see, but never so heavy that it fights the text underneath it.

### Section two

Proportional faces are picked per glyph, so bold, italic, and code spans all
measure differently inside the same wrapped line. The wrap is computed from
those real advances, not from a fixed character width.

- A first bullet with a paragraph's worth of text, long enough to wrap onto
  several lines and still hang correctly under its marker.
- A second bullet, shorter.
- A third bullet with `code`, a [link](https://example.org), and **bold**.

### Section three

> Long quotes keep their bar on the left and their text inset from it, line
> after line, so the two stay visually separate even when the quote wraps
> several times.

After the quote, body text resumes at the full column width. Nothing about the
previous block leaks into this one.

### Section four

Numbers: 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25
26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50.

Punctuation: ( ) [ ] { } < > / \ | ~ ! ? , . : ; ' " ` ^ & * % $ # @ + = - _.

### Section five

A paragraph that mentions every inline style at once: **bold**, *italic*,
***both***, `code`, ~~strike~~, a [link](https://example.com/long/path), an
autolink <https://example.org>, and a trailing hard break.  
Second line of the same paragraph after the break.

### Section six

| Step | Action | Result |
|-----:|:-------|:-------|
| 1    | open   | a tab appears |
| 2    | scroll | the bar follows |
| 3    | drag   | the document moves |
| 4    | close  | the tab is gone |

### Section seven

Markdown that is not understood degrades to plain paragraphs rather than
disappearing: the parser is total, so unknown or malformed input still shows
up as text you can read and select.

### Section eight

Collapsing and expanding works in the JSON tree, not here — this file is
Markdown, so its structure is headings and blocks. Switch tabs (`Ctrl+T` for a
new one) to see the tree view against `samples/sample.json`.

### Section nine

One more screen of prose, so the scrollbar thumb has real travel to move
through rather than a token sliver:

Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod
tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam,
quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo
consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse
cillum dolore eu fugiat nulla pariatur.

Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia
deserunt mollit anim id est laborum. Sed ut perspiciatis unde omnis iste natus
error sit voluptatem accusantium doloremque laudantium, totam rem aperiam,
eaque ipsa quae ab illo inventore veritatis et quasi architecto beatae vitae
dicta sunt explicabo.

Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit, sed
quia consequuntur magni dolores eos qui ratione voluptatem sequi nesciunt.
Neque porro quisquam est, qui dolorem ipsum quia dolor sit amet, consectetur,
adipisci velit, sed quia non numquam eius modi tempora incidunt ut labore et
dolore magnam aliquam quaerat voluptatem.

---

End of the tour. Everything above — headings, lists, quotes, code, tables,
links, images, rules — is drawn by the same software canvas that paints this
last rule.
