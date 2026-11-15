//! Dump each page's text blocks as JSON, for comparison with PyMuPDF.
//!
//! Usage: cargo run --release --example dump_blocks -- <input.pdf>

use mupdf::pdf::PdfDocument;
use parabloom::extract::pdf_page_text;
use parabloom::model::Block;

fn dump(doc: &PdfDocument) -> Result<Vec<Vec<Block>>, mupdf::Error> {
    let n = doc.page_count()?;
    let mut pages = Vec::with_capacity(n as usize);
    for p in 0..n {
        let mut page = doc.load_pdf_page(p)?;
        pages.push(pdf_page_text(&mut page)?.blocks);
    }
    Ok(pages)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: dump_blocks <input.pdf>")?;
    let doc = PdfDocument::open(&path)?;
    let pages = dump(&doc)?;
    println!("{}", serde_json::to_string(&pages)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotated_dump_uses_unrotated_coordinates_and_restores_rotation() {
        let contents = "BT /F1 12 Tf 72 700 Td (A rotated page) Tj ET";
        let bytes = format!(
            "%PDF-1.7\n\
            1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
            2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
            3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792]\
            /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >> endobj\n\
            4 0 obj << /Length {} >> stream\n{}\nendstream endobj\n\
            5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n\
            trailer << /Root 1 0 R >>\n%%EOF",
            contents.len(),
            contents
        );
        let doc = PdfDocument::from_bytes(bytes.as_bytes()).unwrap();
        let expected = dump(&doc).unwrap();
        for rotation in [90, 180, 270] {
            doc.load_pdf_page(0)
                .unwrap()
                .set_rotation(rotation)
                .unwrap();
            assert_eq!(dump(&doc).unwrap(), expected);
            assert_eq!(doc.load_pdf_page(0).unwrap().rotation().unwrap(), rotation);
        }
    }
}
