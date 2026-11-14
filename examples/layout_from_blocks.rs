//! Run the layout port on a block dump and print paragraphs + continuations
//! as JSON, in the same shape as `tools/golden.py layout`.
//!
//! Usage: layout_from_blocks <blocks.json> [indent_x gap_threshold footnote_y]

use parabloom::layout::{Params, detect_page_continuations, group_lines_into_paragraphs};
use parabloom::model::Block;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: layout_from_blocks <blocks.json>")?;
    let mut params = Params::default();
    if let Some(v) = args.next() {
        params.indent_x = v.parse()?;
    }
    if let Some(v) = args.next() {
        params.gap_threshold = v.parse()?;
    }
    if let Some(v) = args.next() {
        params.footnote_y = v.parse()?;
    }
    let pages: Vec<Vec<Block>> = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let paras: Vec<_> = pages
        .iter()
        .map(|b| group_lines_into_paragraphs(b, &params))
        .collect();
    let cont = detect_page_continuations(&paras);
    println!(
        "{}",
        serde_json::json!({ "paragraphs": paras, "continues": cont })
    );
    Ok(())
}
