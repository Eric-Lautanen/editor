---
title: extras tour
tags: [frontmatter, refs, code]
---

# Extras tour

This file exercises the newer Markdown paths: frontmatter above (never
rendered), a [reference link][docs], a collapsed [docs][] link, a shortcut
[docs] link, an ![image ref][logo], and an indented code block below.

[docs]: https://example.com/docs "optional title"
[logo]: assets/logo.png

## Indented code

    fn indented() {
        // four spaces makes a code block
        let x = 42;
    }

## Fenced tint

```rust
fn main() {
    // comments, "strings", keywords and 3.14 numbers tint
    let s = "ok";
    if true {
        println!("{s}");
    }
}
```

## Table polish

| Name | Value |
|------|------:|
| a    | 1     |
| b    | 2     |

A [second ref][docs] proves definitions work from anywhere in the file.
