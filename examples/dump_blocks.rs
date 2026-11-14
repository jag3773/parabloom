//! Dump each page's text blocks as JSON, for comparison with PyMuPDF.
//!
//! Usage: cargo run --release --example dump_blocks -- <input.pdf>

use mupdf::pdf::PdfDocument;
use parabloom::extract::page_text;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: dump_blocks <input.pdf>")?;
    let doc = PdfDocument::open(&path)?;
    let n = doc.page_count()?;
    let mut pages = Vec::with_capacity(n as usize);
    for p in 0..n {
        let page = doc.load_page(p)?;
        pages.push(page_text(&page)?.blocks);
    }
    println!("{}", serde_json::to_string(&pages)?);
    Ok(())
}
