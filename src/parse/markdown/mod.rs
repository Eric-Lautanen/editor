//! Markdown front end (M4): `block` + `inline` produce the `ast::Document` that
//! `layout::markdown` renders.

pub mod ast;
pub mod block;
pub mod inline;

use ast::Document;

/// Parse Markdown (CommonMark subset + GFM tables/task lists) into an AST.
/// Total: unrecognized input degrades to paragraphs, never errors.
pub fn parse(src: &str) -> Document {
    block::parse_blocks(src)
}
