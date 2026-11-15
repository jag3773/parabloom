use mupdf::pdf::{PdfAnnotationType, PdfDocument};
use mupdf::{Point, Quad};
use parabloom::expand::{Options, expand_highlights};
use parabloom::layout::{detect_page_continuations, find_paragraph_index};
use parabloom::model::Line;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "parabloom-regression-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn input(&self, name: &str, raw: &[u8], quads: Vec<Quad>, note: bool) -> PathBuf {
        let path = self.path(&format!("{name}.pdf"));
        let doc = PdfDocument::from_bytes(raw).unwrap();
        {
            let mut page = doc.load_pdf_page(0).unwrap();
            let mut h = page.add_highlight_annotation(quads).unwrap();
            if note {
                h.set_contents("Important research note").unwrap();
                h.set_author("Reader").unwrap();
                h.object()
                    .dict_put("CreationDate", doc.new_string("D:20260101000000Z").unwrap())
                    .unwrap();
            }
            h.update().unwrap();
        }
        doc.save(path.to_str().unwrap()).unwrap();
        path
    }
    fn expand(&self, src: &Path) -> PdfDocument {
        let out = self.path("out.pdf");
        expand_highlights(src, &out, &Options::default()).unwrap();
        PdfDocument::open(out.to_str().unwrap()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn q(x: f32, y: f32) -> Quad {
    Quad::new(
        Point { x, y },
        Point { x: x + 25.0, y },
        Point { x, y: y + 13.0 },
        Point {
            x: x + 25.0,
            y: y + 13.0,
        },
    )
}
const BODY: &str = "BT /F1 11 Tf 1 0 0 1 90 700 Tm (The first paragraph begins here) Tj 1 0 0 1 72 686 Tm (and ends on its second line.) Tj ET";
fn pdf(objects: Vec<String>) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{}\nendobj\n", i + 1, obj).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for n in offsets.iter().skip(1) {
        out.extend(format!("{n:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    out
}

fn one_page(content: &str, resources: &str, extra: Vec<String>) -> Vec<u8> {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> {resources} >> /Contents 4 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    objects.extend(extra);
    pdf(objects)
}

#[test]
fn blank_page_highlight() {
    let fixture = Fixture::new();
    let src = fixture.input(
        "blank",
        &one_page("", "", vec![]),
        vec![q(100.0, 81.0)],
        false,
    );
    let out = fixture.expand(&src);
    assert_eq!(out.load_pdf_page(0).unwrap().annotations().count(), 1);
}

#[test]
fn highlight_metadata() {
    let fixture = Fixture::new();
    let src = fixture.input(
        "notes",
        &one_page(BODY, "", vec![]),
        vec![q(100.0, 81.0)],
        true,
    );
    let out = fixture.expand(&src);
    let page = out.load_pdf_page(0).unwrap();
    let h = page.annotations().next().unwrap();
    assert_eq!(h.contents().unwrap(), Some("Important research note"));
    assert_eq!(h.author().unwrap(), Some("Reader"));
    assert_eq!(
        h.object()
            .get_dict("CreationDate")
            .unwrap()
            .unwrap()
            .as_string()
            .unwrap(),
        "D:20260101000000Z"
    );
}

#[test]
fn columns() {
    let fixture = Fixture::new();
    let body = "BT /F1 11 Tf 1 0 0 1 72 700 Tm (Left column first line) Tj 1 0 0 1 72 686 Tm (Left column ends.) Tj 1 0 0 1 330 700 Tm (Right column first line) Tj 1 0 0 1 330 686 Tm (Right column ends.) Tj ET";
    let src = fixture.input(
        "columns",
        &one_page(body, "", vec![]),
        vec![q(350.0, 81.0)],
        false,
    );
    let out = fixture.expand(&src);
    let page = out.load_pdf_page(0).unwrap();
    let h = page.annotations().next().unwrap();
    assert!(h.quad_points().unwrap().iter().all(|q| q.ul.x >= 330.0));
    let paras = vec![
        vec![Line {
            x0: 72.0,
            y0: 80.0,
            x1: 250.0,
            y1: 110.0,
            text: "left".into(),
        }],
        vec![Line {
            x0: 330.0,
            y0: 80.0,
            x1: 500.0,
            y1: 110.0,
            text: "right".into(),
        }],
    ];
    let r = parabloom::model::Rect {
        x0: 350.0,
        y0: 81.0,
        x1: 375.0,
        y1: 94.0,
    };
    assert_eq!(find_paragraph_index(&paras, &r), Some(1));
}

#[test]
fn multiple_paragraphs_one_highlight() {
    let fixture = Fixture::new();
    let body = format!(
        "{BODY} BT /F1 11 Tf 1 0 0 1 90 640 Tm (The second paragraph starts) Tj 1 0 0 1 72 626 Tm (and finishes here.) Tj ET"
    );
    let src = fixture.input(
        "multi",
        &one_page(&body, "", vec![]),
        vec![q(100.0, 81.0), q(100.0, 141.0)],
        false,
    );
    let out = fixture.expand(&src);
    let page = out.load_pdf_page(0).unwrap();
    let hs: Vec<_> = page
        .annotations()
        .filter(|a| a.r#type().unwrap() == PdfAnnotationType::Highlight)
        .collect();
    assert_eq!(hs.len(), 2);
    assert!(hs.iter().all(|h| h.quad_points().unwrap().len() == 2));
}

#[test]
fn repeated_form_image_scan() {
    let fixture = Fixture::new();
    let extra = vec![
        "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << >> /Length 0 >>\nstream\n\nendstream".into(),
        "<< /Type /XObject /Subtype /Form /BBox [0 0 1 1] /Resources << /XObject << /Image 8 0 R >> >> /Length 0 >>\nstream\n\nendstream".into(),
        "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\nx\nendstream".into(),
    ];
    let src = fixture.input(
        "forms",
        &one_page(BODY, "/XObject << /A 6 0 R /B 6 0 R /C 7 0 R >>", extra),
        vec![q(100.0, 81.0)],
        false,
    );
    let out = fixture.expand(&src);
    let page = out.load_pdf_page(0).unwrap();
    assert_eq!(
        page.annotations().next().unwrap().color().unwrap(),
        Some(mupdf::color::AnnotationColor::Rgb {
            red: 0.2,
            green: 0.4,
            blue: 0.8
        })
    );
}

#[test]
fn short_completed_page_ending() {
    let line = |x1, text: &str| Line {
        x0: 72.0,
        y0: 100.0,
        x1,
        y1: 112.0,
        text: text.into(),
    };
    let pages = vec![
        vec![
            vec![line(540.0, "Earlier body paragraph.")],
            vec![line(160.0, "The end.")],
        ],
        vec![vec![line(540.0, "Introduction to a new section.")]],
    ];
    assert_eq!(detect_page_continuations(&pages), vec![false, false]);
    let single_line_pages = vec![vec![vec![line(160.0, "The end.")]], pages[1].clone()];
    assert_eq!(
        detect_page_continuations(&single_line_pages),
        vec![false, false]
    );
}

#[test]
fn fallback_temp_collision() {
    let fixture = Fixture::new();
    let raw = String::from_utf8(one_page(BODY, "", vec![])).unwrap();
    let broken =
        raw.split("xref\n").next().unwrap().to_owned() + "trailer << /Root 1 0 R >>\n%%EOF\n";
    let src = fixture.path("repaired.pdf");
    let tmp = fixture.path("repaired.pdf.tmp");
    std::fs::write(&src, broken).unwrap();
    std::fs::write(&tmp, "UNRELATED IMPORTANT FILE").unwrap();
    let r = expand_highlights(&src, &src, &Options::default());
    r.unwrap();
    assert_eq!(
        std::fs::read_to_string(&tmp).unwrap(),
        "UNRELATED IMPORTANT FILE"
    );
    assert!(PdfDocument::open(src.to_str().unwrap()).is_ok());
}

#[cfg(unix)]
#[test]
fn fallback_permissions() {
    let fixture = Fixture::new();
    use std::os::unix::fs::PermissionsExt;
    let raw = String::from_utf8(one_page(BODY, "", vec![])).unwrap();
    let broken =
        raw.split("xref\n").next().unwrap().to_owned() + "trailer << /Root 1 0 R >>\n%%EOF\n";
    let src = fixture.path("private.pdf");
    std::fs::write(&src, broken).unwrap();
    std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600)).unwrap();
    let r = expand_highlights(&src, &src, &Options::default());
    r.unwrap();
    assert_eq!(
        std::fs::metadata(&src).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn nested_replies() {
    let fixture = Fixture::new();
    let path = fixture.path("replies.pdf");
    let doc = PdfDocument::from_bytes(&one_page(BODY, "", vec![])).unwrap();
    {
        let mut page = doc.load_pdf_page(0).unwrap();
        let mut h = page.add_highlight_annotation(q(100.0, 81.0)).unwrap();
        h.update().unwrap();
        let r = mupdf::Rect {
            x0: 300.0,
            y0: 300.0,
            x1: 320.0,
            y1: 320.0,
        };
        let mut a = page.add_text_annotation(r, "first reply").unwrap();
        a.object().dict_put("IRT", h.object()).unwrap();
        a.update().unwrap();
        let mut b = page.add_text_annotation(r, "reply to reply").unwrap();
        b.object().dict_put("IRT", a.object()).unwrap();
        b.update().unwrap();
    }
    doc.save(path.to_str().unwrap()).unwrap();
    let out = fixture.expand(&path);
    let page = out.load_pdf_page(0).unwrap();
    let annots: Vec<_> = page.annotations().collect();
    assert_eq!(
        annots.len(),
        3,
        "keep the highlight and both levels of replies"
    );
    let xrefs: Vec<_> = annots.iter().map(|a| a.xref().unwrap()).collect();
    for a in &annots {
        if let Some(irt) = a.object().get_dict("IRT").unwrap() {
            assert!(xrefs.contains(&irt.as_indirect().unwrap()));
        }
    }
}

#[test]
fn merged_notes_keep_authors_dates_and_nested_replies() {
    let fixture = Fixture::new();
    let doc = PdfDocument::from_bytes(&one_page(BODY, "", vec![])).unwrap();
    {
        let mut page = doc.load_pdf_page(0).unwrap();
        for (x, contents, author) in [
            (100.0, "First note", "Alice"),
            (150.0, "Second note", "Bob"),
        ] {
            let mut h = page.add_highlight_annotation(q(x, 81.0)).unwrap();
            h.set_contents(contents).unwrap();
            h.set_author(author).unwrap();
            h.object()
                .dict_put("CreationDate", doc.new_string("D:20260101000000Z").unwrap())
                .unwrap();
            h.object()
                .dict_put("M", doc.new_string("D:20260102000000Z").unwrap())
                .unwrap();
            h.update().unwrap();
            if author == "Bob" {
                let r = mupdf::Rect {
                    x0: 300.0,
                    y0: 300.0,
                    x1: 320.0,
                    y1: 320.0,
                };
                let mut reply = page.add_text_annotation(r, "Direct reply").unwrap();
                reply.object().dict_put("IRT", h.object()).unwrap();
                reply.update().unwrap();
                let mut nested = page.add_text_annotation(r, "Nested reply").unwrap();
                nested.object().dict_put("IRT", reply.object()).unwrap();
                nested.update().unwrap();
            }
        }
    }
    let source = fixture.path("merged.pdf");
    doc.save(source.to_str().unwrap()).unwrap();
    for input in [source, fixture.path("out.pdf")] {
        let out = fixture.expand(&input);
        let page = out.load_pdf_page(0).unwrap();
        let annots: Vec<_> = page.annotations().collect();
        assert_eq!(annots.len(), 4);
        assert_eq!(
            annots
                .iter()
                .filter(|a| a.r#type().unwrap() == PdfAnnotationType::Highlight)
                .count(),
            1
        );
        let bob = annots
            .iter()
            .find(|a| a.contents().unwrap() == Some("Second note"))
            .unwrap();
        assert_eq!(bob.author().unwrap(), Some("Bob"));
        for (key, expected) in [
            ("CreationDate", "D:20260101000000Z"),
            ("M", "D:20260102000000Z"),
        ] {
            assert_eq!(
                bob.object()
                    .get_dict(key)
                    .unwrap()
                    .unwrap()
                    .as_string()
                    .unwrap(),
                expected
            );
        }
        let xrefs: Vec<_> = annots.iter().map(|a| a.xref().unwrap()).collect();
        for a in &annots {
            if let Some(irt) = a.object().get_dict("IRT").unwrap() {
                assert!(xrefs.contains(&irt.as_indirect().unwrap()));
            }
        }
    }
}

#[test]
fn merging_highlight_replies_retains_the_thread_root() {
    let fixture = Fixture::new();
    let doc = PdfDocument::from_bytes(&one_page(BODY, "", vec![])).unwrap();
    {
        let mut page = doc.load_pdf_page(0).unwrap();
        let mut child = page.add_highlight_annotation(q(100.0, 81.0)).unwrap();
        child.set_contents("Child note").unwrap();
        let mut root = page.add_highlight_annotation(q(150.0, 81.0)).unwrap();
        root.set_contents("Root note").unwrap();
        child.object().dict_put("IRT", root.object()).unwrap();
        child.update().unwrap();
        root.update().unwrap();
    }
    let source = fixture.path("highlight-replies.pdf");
    doc.save(source.to_str().unwrap()).unwrap();
    let out = fixture.expand(&source);
    let page = out.load_pdf_page(0).unwrap();
    let annots: Vec<_> = page.annotations().collect();
    assert_eq!(annots.len(), 2);
    let root = annots
        .iter()
        .find(|a| a.r#type().unwrap() == PdfAnnotationType::Highlight)
        .unwrap();
    assert_eq!(root.contents().unwrap(), Some("Root note"));
    assert!(root.object().get_dict("IRT").unwrap().is_none());
    let child = annots
        .iter()
        .find(|a| a.contents().unwrap() == Some("Child note"))
        .unwrap();
    assert_eq!(
        child
            .object()
            .get_dict("IRT")
            .unwrap()
            .unwrap()
            .as_indirect()
            .unwrap(),
        root.xref().unwrap()
    );
}

#[test]
fn partially_unmatched_selection_is_kept_whole() {
    let fixture = Fixture::new();
    let quads = vec![q(100.0, 81.0), q(100.0, 740.0)];
    let src = fixture.input("partial", &one_page(BODY, "", vec![]), quads.clone(), true);
    let out = fixture.path("out.pdf");
    let stats = expand_highlights(&src, &out, &Options::default()).unwrap();
    assert_eq!(stats.pages_modified, 0);
    let doc = PdfDocument::open(out.to_str().unwrap()).unwrap();
    let page = doc.load_pdf_page(0).unwrap();
    let h = page.annotations().next().unwrap();
    assert_eq!(h.quad_points().unwrap(), quads);
    assert_eq!(h.contents().unwrap(), Some("Important research note"));
}

#[test]
fn rotated_highlight_expands_in_unrotated_page_space() {
    let fixture = Fixture::new();
    for rotation in [90, 180, 270] {
        let doc = PdfDocument::from_bytes(&one_page(BODY, "", vec![])).unwrap();
        {
            let mut page = doc.load_pdf_page(0).unwrap();
            page.add_highlight_annotation(q(100.0, 81.0))
                .unwrap()
                .update()
                .unwrap();
            page.set_rotation(rotation).unwrap();
        }
        let input = fixture.path("rotated.pdf");
        doc.save(input.to_str().unwrap()).unwrap();
        let out = fixture.expand(&input);
        let mut page = out.load_pdf_page(0).unwrap();
        assert_eq!(page.rotation().unwrap(), rotation);
        page.set_rotation(0).unwrap();
        let quads = page.annotations().next().unwrap().quad_points().unwrap();
        assert_eq!(quads.len(), 2);
        assert!((quads[0].ul.x - 90.0).abs() < 0.01);
        assert!((quads[0].ul.y - 80.175).abs() < 0.01);
    }
}

#[test]
fn cross_page_segments_copy_metadata() {
    let fixture = Fixture::new();
    let contents = [
        "BT /F1 11 Tf 72 200 Td (A sentence that continues and) Tj ET",
        "BT /F1 11 Tf 72 700 Td (finishes on the next page.) Tj ET",
    ];
    let bytes = pdf(vec![
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".into(),
        format!("<< /Length {} >> stream\n{}\nendstream", contents[0].len(), contents[0]),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 7 0 R >>".into(),
        format!("<< /Length {} >> stream\n{}\nendstream", contents[1].len(), contents[1]),
    ]);
    let src = fixture.input("cross-page", &bytes, vec![q(100.0, 581.0)], true);
    let out = fixture.expand(&src);
    for p in 0..2 {
        let page = out.load_pdf_page(p).unwrap();
        let h = page.annotations().next().unwrap();
        assert_eq!(h.r#type().unwrap(), PdfAnnotationType::Highlight);
        assert_eq!(h.contents().unwrap(), Some("Important research note"));
        assert_eq!(h.author().unwrap(), Some("Reader"));
    }
}
