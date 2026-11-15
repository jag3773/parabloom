//! The end-to-end pipeline: extract paragraphs, replace each highlight with
//! one covering its whole paragraph, and save.
//!
//! Mirrors `expand_highlights()` in the reference script, including the
//! PyMuPDF geometry and appearance conventions, with conservative matching,
//! preservation of notes and reply threads, and safe fallback saves.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Write;
use std::path::Path;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use mupdf::pdf::{
    Encryption, PdfAnnotation, PdfAnnotationType, PdfDocument, PdfObject, PdfPage, PdfWriteOptions,
};
use mupdf::{Buffer, Point, Quad, color::AnnotationColor};

use crate::extract::pdf_page_text;
use crate::layout::{
    Params, detect_page_continuations, find_paragraph_indices, group_lines_into_paragraphs,
    walk_segments,
};
use crate::model::{Paragraph, Rect};
use crate::save::save_replacement;

/// Highlight colour used on pages that contain images.
pub const BLUE: [f32; 3] = [0.2, 0.4, 0.8];
/// Opacity of every expanded highlight.
pub const OPACITY: f32 = 0.4;

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub params: Params,
    pub blue_on_image: bool,
    pub debug: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            params: Params::default(),
            blue_on_image: true,
            debug: false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub pages_modified: usize,
    pub highlights_expanded: usize,
    pub highlights_blued: usize,
}

#[derive(Debug)]
pub enum Error {
    Pdf(mupdf::Error),
    Io(std::io::Error),
    NotPdf(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Pdf(e) => write!(f, "{e}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::NotPdf(p) => write!(f, "{p} is not a PDF"),
        }
    }
}

impl std::error::Error for Error {}

impl From<mupdf::Error> for Error {
    fn from(e: mupdf::Error) -> Self {
        Error::Pdf(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// An original highlight's colour, as PyMuPDF reports `colors["stroke"]`:
/// the `/C` components, empty when the key is missing.
#[derive(Debug, Clone, PartialEq)]
enum Color {
    Components(Vec<f32>),
}

impl Color {
    fn from_annot(annot: &PdfAnnotation) -> Color {
        let comps = match annot.color() {
            Ok(Some(AnnotationColor::Gray(g))) => vec![g],
            Ok(Some(AnnotationColor::Rgb { red, green, blue })) => vec![red, green, blue],
            Ok(Some(AnnotationColor::Cmyk {
                cyan,
                magenta,
                yellow,
                key,
            })) => vec![cyan, magenta, yellow, key],
            _ => vec![],
        };
        Color::Components(comps)
    }

    fn components(&self) -> &[f32] {
        let Color::Components(c) = self;
        c
    }
}

/// Clamp an annotation coordinate to MuPDF's infinite-rect limits
/// (`FZ_MIN_INF_RECT`..`FZ_MAX_INF_RECT`). MuPDF 1.26, which PyMuPDF uses,
/// saturates there; 1.27 does not. It only matters for corrupt highlights
/// whose `/Rect` holds integer-overflow sentinels, where the clamped box's
/// centre decides which paragraph the reference highlights.
fn saturate(v: f32) -> f64 {
    v.clamp(i32::MIN as f32, 0x7fffff80 as f32) as f64
}

/// Run `f` with the page's rotation temporarily set to 0, as PyMuPDF does
/// for text extraction and highlight creation. Restores the rotation even
/// when `f` fails.
fn unrotated<T>(
    page: &mut PdfPage,
    f: impl FnOnce(&mut PdfPage) -> Result<T, Error>,
) -> Result<T, Error> {
    let rotation = page.rotation()?;
    if rotation == 0 {
        return f(page);
    }
    page.set_rotation(0)?;
    let result = f(page);
    page.set_rotation(rotation)?;
    result
}

/// True if any Image XObject is reachable from the page's resources,
/// recursing into Form XObjects (PyMuPDF `page.get_images(full=True)`).
fn page_has_images(page: &PdfPage) -> Result<bool, Error> {
    let Some(resources) = page.object().get_dict_inheritable("Resources")? else {
        return Ok(false);
    };
    let mut seen = HashSet::new();
    resources_have_images(&resources, &mut seen, 0)
}

fn resources_have_images(
    resources: &PdfObject,
    seen: &mut HashSet<i32>,
    depth: usize,
) -> Result<bool, Error> {
    if depth > 32 {
        return Ok(false);
    }
    let Some(xobjects) = resources.get_dict("XObject")? else {
        return Ok(false);
    };
    if !xobjects.is_dict()? {
        return Ok(false);
    }
    let n = xobjects.dict_len()?;
    for i in 0..n as i32 {
        let Some(obj) = xobjects.get_dict_val(i)? else {
            continue;
        };
        if !obj.is_dict()? {
            continue;
        }
        match obj.get_dict("Subtype")? {
            Some(subtype) if subtype.is_name()? && subtype.as_name()? == b"Image" => {
                return Ok(true);
            }
            _ => {}
        }
    }
    for i in 0..n as i32 {
        let Some(obj) = xobjects.get_dict_val(i)? else {
            continue;
        };
        if !obj.is_dict()? {
            continue;
        }
        let xref = if obj.is_indirect()? {
            obj.as_indirect()?
        } else {
            0
        };
        if let Some(sub) = obj.get_dict("Resources")? {
            // Reused indirect forms and cycles only end this branch. Direct
            // objects have no xref identity; the depth limit bounds those.
            if xref != 0 && !seen.insert(xref) {
                continue;
            }
            if resources_have_images(&sub, seen, depth + 1)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// A unique `/NM` for a new highlight on this page, in the style of
/// PyMuPDF's `fitz-A<n>`.
fn unique_name(page: &PdfPage, counter: &mut usize) -> String {
    let existing: HashSet<String> = page
        .annotations()
        .filter_map(|a| a.object().get_dict("NM").ok().flatten())
        .filter_map(|nm| nm.as_string().ok())
        .collect();
    loop {
        let name = format!("parabloom-A{counter}");
        *counter += 1;
        if !existing.contains(&name) {
            return name;
        }
    }
}

fn quads_for(para: &Paragraph) -> Vec<Quad> {
    para.iter()
        .map(|l| {
            let (x0, y0, x1, y1) = (l.x0 as f32, l.y0 as f32, l.x1 as f32, l.y1 as f32);
            Quad::new(
                Point { x: x0, y: y0 },
                Point { x: x1, y: y0 },
                Point { x: x0, y: y1 },
                Point { x: x1, y: y1 },
            )
        })
        .collect()
}

fn set_color(annot: &mut PdfAnnotation, comps: &[f32]) -> Result<(), Error> {
    let color = match *comps {
        [g] => AnnotationColor::Gray(g),
        [red, green, blue] => AnnotationColor::Rgb { red, green, blue },
        [cyan, magenta, yellow, key] => AnnotationColor::Cmyk {
            cyan,
            magenta,
            yellow,
            key,
        },
        _ => {
            // PyMuPDF writes `/C []` for an original highlight without a colour.
            let doc = annot
                .object()
                .document()
                .ok_or_else(|| Error::NotPdf("annotation".into()))?;
            annot.object().dict_put("C", doc.new_array()?)?;
            return Ok(());
        }
    };
    annot.set_color(color)?;
    Ok(())
}

/// Deflate the annotation's normal appearance stream. MuPDF writes it
/// uncompressed; PyMuPDF compresses it, and an uncompressed stream per
/// highlight grows each processed book by roughly 1.6x as much.
fn compress_appearance(annot: &PdfAnnotation) -> Result<(), Error> {
    let Some(ap) = annot.object().get_dict("AP")? else {
        return Ok(());
    };
    let Some(mut n) = ap.get_dict("N")? else {
        return Ok(());
    };
    if !n.is_stream()? || n.get_dict("Filter")?.is_some() {
        return Ok(());
    }
    let raw = n.read_stream()?;
    let mut enc = ZlibEncoder::new(Vec::with_capacity(raw.len() / 3), Compression::default());
    enc.write_all(&raw)?;
    let packed = enc.finish()?;
    n.write_raw_stream_buffer(&Buffer::from_bytes(&packed)?)?;
    let doc = annot
        .object()
        .document()
        .ok_or_else(|| Error::NotPdf("annotation".into()))?;
    n.dict_put("Filter", doc.new_name("FlateDecode")?)?;
    Ok(())
}

/// A source highlight that has a complete, valid expansion plan.
struct Original {
    page: usize,
    xref: i32,
    parent: Option<i32>,
    rect: mupdf::Rect,
    color: Color,
    metadata: Vec<(String, PdfObject)>,
}

impl Original {
    fn snapshot(page: usize, annot: &PdfAnnotation) -> Result<Self, Error> {
        let object = annot.object();
        let mut metadata = Vec::new();
        for i in 0..object.dict_len()? as i32 {
            let key = object.get_dict_key(i)?.unwrap().as_name()?;
            let key = String::from_utf8_lossy(&key).into_owned();
            // Geometry, appearance, identity and relationship keys belong to
            // the destination annotation. All other metadata stays intact.
            if !matches!(
                key.as_str(),
                "Type"
                    | "Subtype"
                    | "Rect"
                    | "QuadPoints"
                    | "P"
                    | "AP"
                    | "AS"
                    | "C"
                    | "CA"
                    | "BM"
                    | "NM"
                    | "Popup"
                    | "IRT"
                    | "RT"
                    | "StructParent"
            ) {
                metadata.push((key, object.get_dict_val(i)?.unwrap()));
            }
        }
        Ok(Self {
            page,
            xref: annot.xref()?,
            parent: object
                .get_dict("IRT")?
                .map(|parent| parent.as_indirect())
                .transpose()?,
            rect: annot.bounds()?,
            color: Color::from_annot(annot),
            metadata,
        })
    }

    fn copy_metadata(&self, annot: &mut PdfAnnotation) -> Result<(), Error> {
        for (key, value) in &self.metadata {
            annot.object().dict_put(key.as_str(), value.clone())?;
        }
        Ok(())
    }

    /// Whether the highlight carries authored note text. Author, subject and
    /// date fields alone are not notes: most viewers write them on every
    /// highlight, so treating them as notes would turn each merged highlight
    /// into an empty sticky note. They are still copied when a note exists.
    fn has_note(&self) -> bool {
        self.metadata.iter().any(|(key, value)| {
            matches!(key.as_str(), "Contents" | "RC")
                && (value.is_stream().unwrap_or(false)
                    || value.as_string().is_ok_and(|s| !s.trim().is_empty()))
        })
    }
}

struct Job {
    para: usize,
    sources: Vec<usize>,
}

fn quad_rect(q: Quad) -> Rect {
    let xs = [q.ul.x, q.ur.x, q.ll.x, q.lr.x];
    let ys = [q.ul.y, q.ur.y, q.ll.y, q.lr.y];
    Rect {
        x0: saturate(xs.into_iter().fold(f32::INFINITY, f32::min)),
        y0: saturate(ys.into_iter().fold(f32::INFINITY, f32::min)),
        x1: saturate(xs.into_iter().fold(f32::NEG_INFINITY, f32::max)),
        y1: saturate(ys.into_iter().fold(f32::NEG_INFINITY, f32::max)),
    }
}

/// Expand every highlight in `input` to its full paragraph and write the
/// result to `output` (which may be the same file).
pub fn expand_highlights(input: &Path, output: &Path, opts: &Options) -> Result<Stats, Error> {
    let in_str = input
        .to_str()
        .ok_or_else(|| Error::NotPdf(input.display().to_string()))?;
    let out_str = output
        .to_str()
        .ok_or_else(|| Error::NotPdf(output.display().to_string()))?;
    let doc = PdfDocument::open(in_str)?;
    let n_pages = doc.page_count()? as usize;
    let mut stats = Stats::default();

    // Phase 1: paragraphs for every page.
    let mut page_paragraphs: Vec<Vec<Paragraph>> = Vec::with_capacity(n_pages);
    for p in 0..n_pages {
        let mut page = doc.load_pdf_page(p as i32)?;
        let blocks = pdf_page_text(&mut page)?.blocks;
        page_paragraphs.push(group_lines_into_paragraphs(&blocks, &opts.params));
    }

    // Phase 2: cross-page continuations.
    let continues_from_prev = detect_page_continuations(&page_paragraphs);
    if opts.debug {
        let pages: Vec<usize> = continues_from_prev
            .iter()
            .enumerate()
            .filter(|(_, c)| **c)
            .map(|(p, _)| p + 1)
            .collect();
        println!("Pages whose first paragraph continues from the previous page: {pages:?}");
    }

    // Plan every quad before modifying anything. If any selected region
    // cannot be matched, preserve the whole original (including its replies).
    let mut originals = Vec::new();
    let mut work: Vec<Vec<Job>> = (0..n_pages).map(|_| Vec::new()).collect();
    for pno in 0..n_pages {
        let mut page = doc.load_pdf_page(pno as i32)?;
        unrotated(&mut page, |pg| {
            for annot in pg.annotations() {
                if annot.r#type()? != PdfAnnotationType::Highlight {
                    continue;
                }
                let mut regions: Vec<_> = annot.quad_points()?.into_iter().map(quad_rect).collect();
                if regions.is_empty() {
                    let r = annot.bounds()?;
                    regions.push(Rect {
                        x0: saturate(r.x0),
                        y0: saturate(r.y0),
                        x1: saturate(r.x1),
                        y1: saturate(r.y1),
                    });
                }
                let mut segments = Vec::new();
                let mut complete = true;
                for region in &regions {
                    let matches = find_paragraph_indices(&page_paragraphs[pno], region);
                    if matches.is_empty() {
                        complete = false;
                        break;
                    }
                    for idx in matches {
                        segments.extend(walk_segments(
                            &page_paragraphs,
                            &continues_from_prev,
                            pno,
                            idx,
                        ));
                    }
                }
                if !complete {
                    if opts.debug {
                        println!("  Kept unmatched highlight on page {}", pno + 1);
                    }
                    continue;
                }
                let source = originals.len();
                originals.push(Original::snapshot(pno, &annot)?);
                for (sp, si) in segments {
                    if let Some(job) = work[sp].iter_mut().find(|job| job.para == si) {
                        if !job.sources.contains(&source) {
                            job.sources.push(source);
                        }
                    } else {
                        work[sp].push(Job {
                            para: si,
                            sources: vec![source],
                        });
                    }
                }
            }
            Ok(())
        })?;
    }

    let mut reused = HashSet::new();
    let mut replacements: Vec<Option<PdfObject>> = (0..originals.len()).map(|_| None).collect();
    for (pno, jobs) in work.iter().enumerate() {
        if jobs.is_empty() {
            continue;
        }
        let mut page = doc.load_pdf_page(pno as i32)?;
        let blue = opts.blue_on_image && page_has_images(&page)?;
        let mut name_counter = 0;
        for job in jobs {
            let para = &page_paragraphs[pno][job.para];
            let local = job
                .sources
                .iter()
                .copied()
                .filter(|&id| originals[id].page == pno && !reused.contains(&id))
                // Keep the root when highlights themselves form a reply
                // thread; retaining a child instead could create a cycle.
                .min_by_key(|&id| {
                    originals[id].parent.is_some_and(|parent| {
                        job.sources
                            .iter()
                            .any(|&source| originals[source].xref == parent)
                    })
                });
            let source = local.unwrap_or(job.sources[0]);
            let original = &originals[source];
            let object = unrotated(&mut page, |pg| {
                let mut annot = if local.is_some() {
                    let annot = pg
                        .annotations()
                        .find(|a| a.xref().ok() == Some(original.xref))
                        .ok_or_else(|| Error::NotPdf("missing source annotation".into()))?;
                    reused.insert(source);
                    annot
                } else {
                    let mut annot = pg.add_highlight_annotation(quads_for(para))?;
                    original.copy_metadata(&mut annot)?;
                    annot
                };
                annot.set_quad_points(quads_for(para))?;
                set_color(
                    &mut annot,
                    if blue {
                        &BLUE
                    } else {
                        original.color.components()
                    },
                )?;
                annot.set_opacity(OPACITY)?;
                if annot.object().get_dict("NM")?.is_none() {
                    let name = unique_name(pg, &mut name_counter);
                    annot.object().dict_put("NM", doc.new_string(&name)?)?;
                }
                annot.object().dict_put("BM", doc.new_name("Multiply")?)?;
                annot.update()?;
                compress_appearance(&annot)?;
                Ok(annot.object())
            })?;
            for &source in &job.sources {
                if originals[source].page == pno && replacements[source].is_none() {
                    replacements[source] = Some(object.clone());
                }
            }
            stats.highlights_expanded += 1;
            if blue {
                stats.highlights_blued += 1;
            }
            if opts.debug {
                println!(
                    "  Expanded page {} paragraph {} ({} lines)",
                    pno + 1,
                    job.para + 1,
                    para.len()
                );
            }
        }
        stats.pages_modified += 1;
    }

    // A merged-away highlight with authored metadata becomes a text reply.
    // Preserve each note separately instead of flattening multiple authors
    // and dates into a single highlight's metadata fields.
    let mut redirects = HashMap::new();
    for (id, original) in originals.iter().enumerate() {
        if reused.contains(&id) {
            continue;
        }
        let mut destination = replacements[id].as_ref().unwrap().clone();
        if original.has_note() {
            let mut page = doc.load_pdf_page(original.page as i32)?;
            destination = unrotated(&mut page, |pg| {
                let mut note = pg.add_text_annotation(original.rect, "")?;
                original.copy_metadata(&mut note)?;
                note.object().dict_put("IRT", destination.clone())?;
                note.object().dict_put("RT", doc.new_name("R")?)?;
                let name = unique_name(pg, &mut 0);
                note.object().dict_put("NM", doc.new_string(&name)?)?;
                note.update()?;
                Ok(note.object())
            })?;
        }
        redirects.insert(original.xref, destination);
    }
    // Only roots whose identities changed need retargeting. Descendants keep
    // their existing parent and all levels of the thread remain attached.
    for pno in 0..n_pages {
        let mut page = doc.load_pdf_page(pno as i32)?;
        for annot in page.annotations() {
            let destination = annot
                .object()
                .get_dict("IRT")?
                .map(|irt| irt.as_indirect())
                .transpose()?
                .and_then(|xref| redirects.get(&xref));
            if let Some(destination) = destination {
                annot.object().dict_put("IRT", destination.clone())?;
            }
        }
        let removed: Vec<_> = page
            .annotations()
            .filter(|a| a.xref().is_ok_and(|xref| redirects.contains_key(&xref)))
            .collect();
        for annot in removed {
            page.delete_annotation(annot)?;
        }
    }

    save(&doc, input, output, in_str, out_str)?;
    Ok(stats)
}

/// Save like the reference: incremental when overwriting the input (keeping
/// encryption), falling back to a full save via a temporary file; otherwise a
/// full save with PyMuPDF's defaults (no encryption).
fn save(
    doc: &PdfDocument,
    input: &Path,
    output: &Path,
    in_str: &str,
    out_str: &str,
) -> Result<(), Error> {
    let same_file = in_str == out_str
        || match (input.canonicalize(), output.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        };
    let mut full = PdfWriteOptions::default();
    full.set_encryption(Encryption::None);

    if !same_file {
        doc.save_with_options(out_str, full)?;
        return Ok(());
    }
    if doc.can_be_saved_incrementally() {
        let mut inc = PdfWriteOptions::default();
        inc.set_incremental(true).set_encryption(Encryption::Keep);
        if doc.save_with_options(out_str, inc).is_ok() {
            return Ok(());
        }
    }
    // Resolve aliases so a fallback save updates the same target as an
    // incremental save, without replacing a symlink itself.
    let target = output.canonicalize()?;
    save_replacement(doc, &target, full)?;
    Ok(())
}
