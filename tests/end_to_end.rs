//! End-to-end test on a generated PDF: a partial highlight on one word must
//! grow to cover its whole paragraph, keep its colour, and leave the next
//! paragraph alone.

use std::path::PathBuf;

use mupdf::pdf::{PdfAnnotationType, PdfDocument};
use mupdf::{Point, Quad, color::AnnotationColor};
use parabloom::expand::{Options, expand_highlights};

/// A one-page letter-size PDF with two Helvetica paragraphs. Paragraph one
/// has an indented first line and two more lines; a 24pt gap separates it
/// from paragraph two. Offsets in the xref are left for MuPDF to repair.
fn sample_pdf() -> Vec<u8> {
    let lines = [
        (
            90,
            700,
            "The first paragraph starts with an indented line that",
        ),
        (
            72,
            686,
            "wraps onto a second line at the left margin and then",
        ),
        (72, 672, "ends on this third line."),
        (
            90,
            648,
            "The second paragraph is separate and stays unhighlighted",
        ),
        (72, 634, "because nobody marked any of its words."),
    ];
    let mut content = String::from("BT /F1 11 Tf\n");
    for (x, y, t) in lines {
        content.push_str(&format!("1 0 0 1 {x} {y} Tm ({t}) Tj\n"));
    }
    content.push_str("ET\n");
    format!(
        "%PDF-1.7\n\
         1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
         2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
         3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >> endobj\n\
         4 0 obj << /Length {} >> stream\n{}endstream endobj\n\
         5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n\
         trailer << /Root 1 0 R >>\n%%EOF\n",
        content.len(),
        content
    )
    .into_bytes()
}

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("parabloom-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn partial_highlight_expands_to_paragraph() {
    // Highlight one word ("paragraph") on the first line, in orange.
    let input = temp_path("in.pdf");
    let output = temp_path("out.pdf");
    let doc = PdfDocument::from_bytes(&sample_pdf()).unwrap();
    {
        let mut page = doc.load_pdf_page(0).unwrap();
        // Page space is y-down: the first baseline (y=700 in PDF space) sits at 92.
        let q = Quad::new(
            Point { x: 140.0, y: 81.0 },
            Point { x: 190.0, y: 81.0 },
            Point { x: 140.0, y: 94.0 },
            Point { x: 190.0, y: 94.0 },
        );
        let mut annot = page.add_highlight_annotation(q).unwrap();
        annot
            .set_color(AnnotationColor::Rgb {
                red: 1.0,
                green: 0.5,
                blue: 0.0,
            })
            .unwrap();
        annot.update().unwrap();
    }
    doc.save(input.to_str().unwrap()).unwrap();

    let stats = expand_highlights(&input, &output, &Options::default()).unwrap();
    assert_eq!(stats.highlights_expanded, 1);
    assert_eq!(stats.pages_modified, 1);
    assert_eq!(stats.highlights_blued, 0);

    let out = PdfDocument::open(output.to_str().unwrap()).unwrap();
    let page = out.load_pdf_page(0).unwrap();
    let highlights: Vec<_> = page
        .annotations()
        .filter(|a| a.r#type().unwrap() == PdfAnnotationType::Highlight)
        .collect();
    assert_eq!(
        highlights.len(),
        1,
        "the original is replaced, not duplicated"
    );
    let h = &highlights[0];

    let quads = h.quad_points().unwrap();
    assert_eq!(quads.len(), 3, "one quad per line of paragraph one");
    // The quads span paragraph one (baselines 92, 106, 120 in page space)
    // and stop before paragraph two (baseline 144).
    let top = quads.iter().map(|q| q.ul.y).fold(f32::INFINITY, f32::min);
    let bottom = quads
        .iter()
        .map(|q| q.ll.y)
        .fold(f32::NEG_INFINITY, f32::max);
    assert!(top < 92.0 && top > 70.0, "top {top}");
    assert!(bottom > 120.0 && bottom < 135.0, "bottom {bottom}");

    match h.color().unwrap() {
        Some(AnnotationColor::Rgb { red, green, blue }) => {
            assert!((red - 1.0).abs() < 1e-4 && (green - 0.5).abs() < 1e-4 && blue.abs() < 1e-4);
        }
        other => panic!("unexpected colour {other:?}"),
    }
    assert!((h.opacity().unwrap() - 0.4).abs() < 1e-4);
    let bm = h.object().get_dict("BM").unwrap().unwrap();
    assert_eq!(bm.as_name().unwrap(), b"Multiply");
}

#[test]
fn overwrite_in_place_keeps_one_highlight_per_paragraph() {
    let path = temp_path("inplace.pdf");
    let doc = PdfDocument::from_bytes(&sample_pdf()).unwrap();
    {
        let mut page = doc.load_pdf_page(0).unwrap();
        // Two highlights in the same paragraph collapse to one.
        for x in [100.0f32, 200.0] {
            let q = Quad::new(
                Point { x, y: 95.0 },
                Point {
                    x: x + 30.0,
                    y: 95.0,
                },
                Point { x, y: 108.0 },
                Point {
                    x: x + 30.0,
                    y: 108.0,
                },
            );
            page.add_highlight_annotation(q).unwrap().update().unwrap();
        }
    }
    doc.save(path.to_str().unwrap()).unwrap();

    let stats = expand_highlights(&path, &path, &Options::default()).unwrap();
    assert_eq!(stats.highlights_expanded, 1);

    let out = PdfDocument::open(path.to_str().unwrap()).unwrap();
    let page = out.load_pdf_page(0).unwrap();
    let n = page
        .annotations()
        .filter(|a| a.r#type().unwrap() == PdfAnnotationType::Highlight)
        .count();
    assert_eq!(n, 1);
}
