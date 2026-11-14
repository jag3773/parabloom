//! Text extraction that reproduces PyMuPDF's `page.get_text("dict")`.
//!
//! PyMuPDF does not report MuPDF's raw line boxes. It rebuilds each line box
//! as the union of its character boxes, after correcting character quads for
//! fonts with implausible ascender/descender metrics, and it clips characters,
//! lines and blocks to the page bounds. Paragraph grouping depends on those
//! boxes, so this module repeats the same steps.
//!
//! Reference: `JM_make_textpage_dict`, `JM_make_text_block`,
//! `JM_make_spanlist`, `JM_char_bbox` and `JM_char_quad` in PyMuPDF 1.26.

use mupdf::text_page::{TextBlockType, TextChar, TextCharFlags, TextLine, TextPageFlags};
use mupdf::{Device, Font, Matrix, Page, Point, Quad, WriteMode};

use crate::model::{Block, Line};

/// PyMuPDF's `TEXTFLAGS_DICT` (199): ligatures, whitespace, images,
/// media-box clip, CID for unknown unicode.
pub fn dict_flags() -> TextPageFlags {
    TextPageFlags::PRESERVE_LIGATURES
        | TextPageFlags::PRESERVE_WHITESPACE
        | TextPageFlags::PRESERVE_IMAGES
        | TextPageFlags::CLIP
        | TextPageFlags::USE_CID_FOR_UNKNOWN_UNICODE
}

/// The text blocks on one page, in MuPDF's order.
pub struct PageText {
    pub blocks: Vec<Block>,
}

/// MuPDF's rect, kept in `f32` while boxes are being assembled.
#[derive(Clone, Copy, Debug)]
struct R {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl R {
    /// `fz_empty_rect`: MuPDF's finite sentinel, `FZ_MAX_INF_RECT` and
    /// `FZ_MIN_INF_RECT`. PyMuPDF reports these values for a line whose
    /// characters were all clipped, so they flow into the layout arithmetic.
    const EMPTY: R = R {
        x0: 0x7fffff80 as f32,
        y0: 0x7fffff80 as f32,
        x1: i32::MIN as f32,
        y1: i32::MIN as f32,
    };

    fn from_mupdf(r: mupdf::Rect) -> R {
        R {
            x0: r.x0,
            y0: r.y0,
            x1: r.x1,
            y1: r.y1,
        }
    }

    /// `fz_is_empty_rect`
    fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }

    /// `fz_union_rect`: an empty operand is ignored.
    fn union(self, b: R) -> R {
        if self.is_empty() {
            return b;
        }
        if b.is_empty() {
            return self;
        }
        R {
            x0: self.x0.min(b.x0),
            y0: self.y0.min(b.y0),
            x1: self.x1.max(b.x1),
            y1: self.y1.max(b.y1),
        }
    }

    /// `fz_intersect_rect` followed by `fz_is_empty_rect`.
    fn intersection_is_empty(self, b: R) -> bool {
        if self.is_empty() || b.is_empty() {
            return true;
        }
        let x0 = self.x0.max(b.x0);
        let y0 = self.y0.max(b.y0);
        let x1 = self.x1.min(b.x1);
        let y1 = self.y1.min(b.y1);
        x0 >= x1 || y0 >= y1
    }

    /// `JM_rects_overlap`
    fn overlaps(self, b: R) -> bool {
        !(self.x0 >= b.x1 || self.y0 >= b.y1 || self.x1 <= b.x0 || self.y1 <= b.y0)
    }

    /// `fz_contains_rect`
    fn contains(self, b: R) -> bool {
        if self.is_empty() || b.is_empty() {
            return false;
        }
        self.x0 <= b.x0 && self.y0 <= b.y0 && self.x1 >= b.x1 && self.y1 >= b.y1
    }

    fn to_f64(self) -> [f64; 4] {
        [
            self.x0 as f64,
            self.y0 as f64,
            self.x1 as f64,
            self.y1 as f64,
        ]
    }
}

/// `fz_rect_from_quad`
fn rect_from_quad(q: &Quad) -> R {
    let xs = [q.ul.x, q.ur.x, q.ll.x, q.lr.x];
    let ys = [q.ul.y, q.ur.y, q.ll.y, q.lr.y];
    R {
        x0: xs.iter().copied().fold(f32::INFINITY, f32::min),
        y0: ys.iter().copied().fold(f32::INFINITY, f32::min),
        x1: xs.iter().copied().fold(f32::NEG_INFINITY, f32::max),
        y1: ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
    }
}

/// A 2x3 affine matrix applied the way `fz_transform_point` does.
#[derive(Clone, Copy)]
struct M {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl M {
    fn apply(self, p: Point) -> Point {
        Point {
            x: p.x * self.a + p.y * self.c + self.e,
            y: p.x * self.b + p.y * self.d + self.f,
        }
    }

    fn apply_quad(self, q: Quad) -> Quad {
        Quad {
            ul: self.apply(q.ul),
            ur: self.apply(q.ur),
            ll: self.apply(q.ll),
            lr: self.apply(q.lr),
        }
    }
}

/// Per-line context needed by the character-level corrections.
struct LineCtx {
    horizontal: bool,
    /// Writing direction. MuPDF stores this on the line, but the Rust crate
    /// does not expose it, so it is derived from the first character's quad
    /// baseline (ul -> ur), which MuPDF builds along the same direction.
    dir: Point,
    first_origin_y: Option<f32>,
}

fn line_ctx(line: &TextLine<'_>) -> LineCtx {
    let horizontal = matches!(line.wmode(), WriteMode::Horizontal);
    let mut dir = Point { x: 1.0, y: 0.0 };
    let mut first_origin_y = None;
    if let Some(ch) = line.chars().next() {
        first_origin_y = Some(ch.origin().y);
        let q = ch.quad();
        let dx = q.ur.x - q.ul.x;
        let dy = q.ur.y - q.ul.y;
        let len = (dx * dx + dy * dy).sqrt();
        if len > 0.0 {
            dir = Point {
                x: dx / len,
                y: dy / len,
            };
            // Snap the common axis-aligned cases exactly, as MuPDF's own
            // direction vector is exact for unrotated text.
            if dir.y.abs() < 1e-4 {
                dir = Point {
                    x: dir.x.signum(),
                    y: 0.0,
                };
            } else if dir.x.abs() < 1e-4 {
                dir = Point {
                    x: 0.0,
                    y: dir.y.signum(),
                };
            }
        }
    }
    LineCtx {
        horizontal,
        dir,
        first_origin_y,
    }
}

/// `JM_char_quad`: re-compute a character quad when the font's
/// ascender/descender values make no sense.
fn char_quad(ctx: &LineCtx, ch: &TextChar<'_>, font: Option<&Font>) -> Quad {
    let raw = ch.quad();
    if !ctx.horizontal {
        return raw;
    }
    let Some(font) = font else { return raw };
    let mut asc = font.ascender();
    let mut dsc = font.descender();
    let fsize = ch.size();
    let mut asc_dsc = asc - dsc + f32::EPSILON;
    if asc_dsc >= 1.0 {
        return raw;
    }

    if asc < 1e-3 {
        dsc = -0.1;
        asc = 0.9;
        asc_dsc = 1.0;
    }
    if asc_dsc < 1.0 {
        dsc /= asc_dsc;
        asc /= asc_dsc;
    }
    asc_dsc = asc - dsc;
    asc = asc * fsize / asc_dsc;
    dsc = dsc * fsize / asc_dsc;

    let c = ctx.dir.x;
    let s = ctx.dir.y;
    let mut trm1 = M {
        a: c,
        b: -s,
        c: s,
        d: c,
        e: 0.0,
        f: 0.0,
    };
    let mut trm2 = M {
        a: c,
        b: s,
        c: -s,
        d: c,
        e: 0.0,
        f: 0.0,
    };
    if c == -1.0 {
        trm1.d = 1.0;
        trm2.d = 1.0;
    }
    let o = ch.origin();
    let xlate1 = M {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: -o.x,
        f: -o.y,
    };
    let xlate2 = M {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: o.x,
        f: o.y,
    };

    let mut q = trm1.apply_quad(xlate1.apply_quad(raw));
    if c == 1.0 && q.ul.y > 0.0 {
        q.ul.y = asc;
        q.ur.y = asc;
        q.ll.y = dsc;
        q.lr.y = dsc;
    } else {
        q.ul.y = -asc;
        q.ur.y = -asc;
        q.ll.y = -dsc;
        q.lr.y = -dsc;
    }
    if q.ll.x < 0.0 {
        q.ll.x = 0.0;
        q.ul.x = 0.0;
    }
    let cwidth = q.lr.x - q.ll.x;
    if cwidth < f32::EPSILON {
        if let Some(cp) = ch.char() {
            if let Ok(glyph) = font.encode_character(cp as i32) {
                if glyph != 0 {
                    if let Ok(fwidth) = font.advance_glyph(glyph) {
                        q.lr.x = q.ll.x + fwidth * fsize;
                        q.ur.x = q.lr.x;
                    }
                }
            }
        }
    }
    xlate2.apply_quad(trm2.apply_quad(q))
}

/// `JM_char_bbox`
fn char_bbox(ctx: &LineCtx, ch: &TextChar<'_>, font: Option<&Font>) -> R {
    let mut r = rect_from_quad(&char_quad(ctx, ch, font));
    if !ctx.horizontal && r.y1 < r.y0 + ch.size() {
        r.y0 = r.y1 - ch.size();
    }
    r
}

/// The style fields PyMuPDF uses to start a new span. `bidi` is not exposed
/// by the crate; font flags are covered by the font name plus superscript.
#[derive(PartialEq)]
struct SpanStyle {
    size: f32,
    superscript: bool,
    char_flags: u16,
    argb: u32,
    font_name: String,
}

/// `JM_make_spanlist` reduced to what paragraph grouping needs: the line box
/// and the line text.
fn make_line(line: &TextLine<'_>, tp_rect: R) -> (R, String) {
    let ctx = line_ctx(line);
    let mut text = String::new();
    let mut span_text = String::new();
    let mut span_rect = R::EMPTY;
    let mut line_rect = R::EMPTY;
    let mut old_style: Option<SpanStyle> = None;

    for ch in line.chars() {
        let font = ch.font();
        let r = char_bbox(&ctx, &ch, font.as_ref());
        if !tp_rect.overlaps(r) {
            continue;
        }
        let superscript = ctx.horizontal
            && ctx.dir.x == 1.0
            && ctx.dir.y == 0.0
            && ctx
                .first_origin_y
                .is_some_and(|fy| ch.origin().y < fy - ch.size() * 0.1);
        let style = SpanStyle {
            size: ch.size(),
            superscript,
            char_flags: (ch.flags() - TextCharFlags::SYNTHETIC).bits(),
            argb: ch.argb(),
            font_name: font
                .as_ref()
                .map(|f| f.name().to_owned())
                .unwrap_or_default(),
        };
        if old_style.as_ref() != Some(&style) {
            if old_style.is_some() {
                // Flush the previous span unconditionally, as PyMuPDF does
                // for every span except the last.
                text.push_str(&span_text);
                line_rect = line_rect.union(span_rect);
            }
            span_text.clear();
            span_rect = r;
            old_style = Some(style);
        }
        span_rect = span_rect.union(r);
        if let Some(cp) = ch.char() {
            span_text.push(cp);
        }
    }
    // The last span is kept only when its box is non-empty.
    if old_style.is_some() && !span_rect.is_empty() {
        text.push_str(&span_text);
        line_rect = line_rect.union(span_rect);
    }
    (line_rect, text)
}

/// Extract text blocks from a page the way `page.get_text("dict")` does.
///
/// The caller must make sure the page is unrotated, as PyMuPDF temporarily
/// sets rotation to 0 before extracting.
pub fn page_text(page: &Page) -> Result<PageText, mupdf::Error> {
    // PyMuPDF runs `fz_run_page` (contents, then annotations, then widgets)
    // into one text device. The crate's `Page::to_text_page` runs only the
    // contents, so annotation and form-field text is appended here with a
    // second device on the same text page. (Going through a display list
    // instead loses lines that hold only a non-breaking space.)
    let tp = page.to_text_page(dict_flags())?;
    {
        let dev = Device::from_text_page(&tp, dict_flags())?;
        page.run_annotations(&dev, &Matrix::IDENTITY)?;
        page.run_widgets(&dev, &Matrix::IDENTITY)?;
        // Dropping `dev` closes it, which finalises the text page.
    }
    let tp_rect = R::from_mupdf(page.bounds()?);
    let mut blocks = Vec::new();

    for block in tp.blocks() {
        let bbox = R::from_mupdf(block.bounds());
        let kind = block.r#type();
        if kind == TextBlockType::Image && !tp_rect.contains(bbox) {
            continue;
        }
        if tp_rect.intersection_is_empty(bbox) {
            continue;
        }
        if kind != TextBlockType::Text {
            continue;
        }
        let mut lines = Vec::new();
        let mut block_rect = R::EMPTY;
        for line in block.lines() {
            if tp_rect.intersection_is_empty(R::from_mupdf(line.bounds())) {
                continue;
            }
            let (r, text) = make_line(&line, tp_rect);
            block_rect = block_rect.union(r);
            let [x0, y0, x1, y1] = r.to_f64();
            lines.push(Line {
                x0,
                y0,
                x1,
                y1,
                text,
            });
        }
        blocks.push(Block {
            bbox: block_rect.to_f64(),
            lines,
        });
    }
    Ok(PageText { blocks })
}
