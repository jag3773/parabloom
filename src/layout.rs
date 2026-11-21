//! Paragraph reconstruction from text blocks. No PDF library calls.
//!
//! Based on the 2026-08-03 patched Python reference, retaining its float
//! and string semantics while using column-aware grouping and conservative
//! geometric matching to avoid changing unrelated selections.

// Negated float comparisons (`!(a > b)`) are deliberate: they keep the
// reference's `if a > b: continue` behaviour for values that don't compare.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

use crate::model::{Block, Line, Paragraph, Rect};

/// Tuning knobs, with the reference script's defaults.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// X threshold above which a single-line block starts a new paragraph.
    pub indent_x: f64,
    /// Vertical gap above which a single-line block starts a new paragraph.
    pub gap_threshold: f64,
    /// Lines (and blocks) starting below this Y are footnotes and ignored.
    pub footnote_y: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            indent_x: 80.0,
            gap_threshold: 12.0,
            footnote_y: 720.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Python semantics helpers
// ---------------------------------------------------------------------------

/// `str.isspace()` for one character: Unicode White_Space plus the
/// information separators U+001C..U+001F, which Python also treats as space.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_rstrip(s: &str) -> &str {
    s.trim_end_matches(py_isspace)
}

fn py_lstrip(s: &str) -> &str {
    s.trim_start_matches(py_isspace)
}

/// `str.isalpha()` for one character. Approximated by Rust's Alphabetic
/// property, which also admits a few combining marks and circled letters
/// that Python rejects; such characters do not start or end body text.
fn py_isalpha(c: char) -> bool {
    c.is_alphabetic()
}

/// `round(x, 1)`: Python rounds the exact binary value to one decimal,
/// ties to even. Rust's `{:.1}` formatting does the same.
pub(crate) fn py_round1(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{x:.1}").parse().unwrap_or(x)
}

/// Python's `min()` over floats: the first of the smallest values.
fn py_min(it: impl Iterator<Item = f64>) -> Option<f64> {
    it.fold(None, |acc, v| match acc {
        None => Some(v),
        Some(m) => Some(if v < m { v } else { m }),
    })
}

/// Python's `max()` over floats: the first of the largest values.
fn py_max(it: impl Iterator<Item = f64>) -> Option<f64> {
    it.fold(None, |acc, v| match acc {
        None => Some(v),
        Some(m) => Some(if v > m { v } else { m }),
    })
}

// ---------------------------------------------------------------------------
// Text signals
// ---------------------------------------------------------------------------

const SENTENCE_END_PUNCT: [char; 4] = ['.', '!', '?', '…'];
const TRAILING_CLOSERS: [char; 7] = ['"', '\u{201d}', '\u{2019}', '\u{bb}', ')', ']', '}'];

/// True if the line's last meaningful character is sentence-final
/// punctuation, after stripping trailing closing quotes and brackets.
pub fn line_ends_with_sentence_punct(text: &str) -> bool {
    let mut s = py_rstrip(text);
    while let Some(last) = s.chars().next_back() {
        if TRAILING_CLOSERS.contains(&last) {
            s = py_rstrip(&s[..s.len() - last.len_utf8()]);
        } else {
            break;
        }
    }
    s.chars()
        .next_back()
        .is_some_and(|c| SENTENCE_END_PUNCT.contains(&c))
}

/// True if the line's first non-whitespace character is a lowercase letter.
pub fn line_starts_with_lowercase(text: &str) -> bool {
    py_lstrip(text)
        .chars()
        .next()
        .is_some_and(|c| py_isalpha(c) && c.is_lowercase())
}

// ---------------------------------------------------------------------------
// Paragraph grouping
// ---------------------------------------------------------------------------

/// Which lines of each block survive the footnote cut-off.
///
/// Lines starting below `footnote_y` (or in a block starting below it) are
/// dropped as footnotes, unless they carry on the body text above them: a
/// line is kept when the nearest kept line above it in the same column has
/// the same left edge and line height, and the gap between them matches
/// that line's own gap to the line above it, and it is set at the page's
/// body size. Notes set in smaller type, or after a larger gap, are still
/// dropped.
fn footnote_zone_keep(blocks: &[Block], footnote_y: f64) -> Vec<Vec<bool>> {
    let mut keep: Vec<Vec<bool>> = blocks
        .iter()
        .map(|b| {
            b.lines
                .iter()
                .map(|l| !(b.bbox[1] > footnote_y) && !(l.y0 > footnote_y))
                .collect()
        })
        .collect();
    let blank = |l: &Line| l.text.chars().all(py_isspace);
    let mut pool: Vec<&Line> = Vec::new();
    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for (bi, b) in blocks.iter().enumerate() {
        for (li, l) in b.lines.iter().enumerate() {
            if blank(l) {
                continue;
            }
            if keep[bi][li] {
                pool.push(l);
            } else {
                candidates.push((bi, li));
            }
        }
    }
    if candidates.is_empty() {
        return keep;
    }
    // Only body-sized lines are rescued; notes in smaller type stay cut.
    // Body size is the line height that carries half the page's text, so
    // a few tall title lines or a block of notes don't set it.
    let mut sized: Vec<(f64, usize)> = pool
        .iter()
        .map(|l| {
            (
                l.y1 - l.y0,
                l.text.chars().filter(|c| !py_isspace(*c)).count(),
            )
        })
        .collect();
    sized.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let total: usize = sized.iter().map(|(_, n)| n).sum();
    let mut seen = 0;
    let Some(body_h) = sized.iter().find_map(|&(h, n)| {
        seen += n;
        (2 * seen >= total).then_some(h)
    }) else {
        return keep;
    };
    candidates.sort_by(|a, b| {
        let (la, lb) = (&blocks[a.0].lines[a.1], &blocks[b.0].lines[b.1]);
        la.y0
            .partial_cmp(&lb.y0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Nearest pool line above `l` in the same column.
    fn above<'a>(pool: &[&'a Line], l: &Line) -> Option<&'a Line> {
        pool.iter()
            .copied()
            .filter(|p| p.y1 <= l.y0 + 1.0 && l.x0 < p.x1 + 3.0 && p.x0 < l.x1 + 3.0)
            .max_by(|a, b| a.y1.partial_cmp(&b.y1).unwrap_or(std::cmp::Ordering::Equal))
    }
    for (bi, li) in candidates {
        let c = &blocks[bi].lines[li];
        let Some(prev) = above(&pool, c) else {
            continue;
        };
        let Some(prev2) = above(&pool, prev) else {
            continue;
        };
        let (h, ph, ph2) = (c.y1 - c.y0, prev.y1 - prev.y0, prev2.y1 - prev2.y0);
        let (gap, pgap) = (c.y0 - prev.y1, prev.y0 - prev2.y1);
        let same_height =
            (h - ph).abs() <= 0.08 * ph && (ph - ph2).abs() <= 0.08 * ph && h >= 0.9 * body_h;
        let same_rhythm = gap >= -1.0 && (gap - pgap).abs() <= 1.5;
        let aligned = (c.x0 - prev.x0).abs() <= 3.0;
        if same_height && same_rhythm && aligned {
            keep[bi][li] = true;
            pool.push(c);
        }
    }
    keep
}

/// Group a page's text blocks into paragraphs (`group_lines_into_paragraphs`).
///
/// Multi-line blocks are taken as paragraphs as MuPDF segmented them;
/// single-line blocks are merged by indent and gap. A block also merges into
/// the open paragraph when it looks like a split of it (normal line gap, not
/// indented or both lines centred, and a sentence still in progress).
pub fn group_lines_into_paragraphs(blocks: &[Block], params: &Params) -> Vec<Paragraph> {
    let Params {
        indent_x,
        gap_threshold,
        footnote_y,
    } = *params;

    // Text blocks with their lines, footnote zone removed.
    let keep = footnote_zone_keep(blocks, footnote_y);
    let mut records: Vec<(f64, Vec<Line>)> = Vec::new();
    for (block, keep) in blocks.iter().zip(&keep) {
        let by0 = block.bbox[1];
        let lines: Vec<Line> = block
            .lines
            .iter()
            .zip(keep)
            .filter(|(_, k)| **k)
            .map(|(l, _)| l.clone())
            .collect();
        if lines.is_empty() {
            continue;
        }
        // MuPDF can place two columns in a single block. Partition its
        // lines by horizontal overlap before treating a block as a paragraph.
        let mut columns: Vec<Vec<Line>> = Vec::new();
        for line in lines {
            let column = columns.iter().position(|column| {
                column
                    .iter()
                    .any(|other| line.x0 < other.x1 + 3.0 && other.x0 < line.x1 + 3.0)
            });
            if let Some(i) = column {
                columns[i].push(line);
            } else {
                columns.push(vec![line]);
            }
        }
        let split = columns.len() > 1;
        for lines in columns {
            let top = if split {
                py_min(lines.iter().map(|line| line.y0)).unwrap()
            } else {
                by0
            };
            records.push((top, lines));
        }
    }
    // Stable sort by block top, as Python's list.sort.
    records.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let all_lines = || records.iter().flat_map(|(_, ls)| ls.iter());

    // Typical intra-block line gap (median of non-negative gaps).
    let mut intra_gaps: Vec<f64> = Vec::new();
    for (_, ls) in &records {
        for w in ls.windows(2) {
            let g = w[1].y0 - w[0].y1;
            if g >= 0.0 {
                intra_gaps.push(g);
            }
        }
    }
    intra_gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let typical_line_gap = intra_gaps.get(intra_gaps.len() / 2).copied();
    let left_margin = py_min(all_lines().map(|l| l.x0)).unwrap_or(0.0);
    let right_margin = py_max(all_lines().map(|l| l.x1)).unwrap_or(0.0);

    // Centred-text continuation: the modal left edge marks body text.
    let mut counts: Vec<(f64, usize)> = Vec::new();
    let mut n_x0s = 0usize;
    for l in all_lines() {
        let k = py_round1(l.x0);
        n_x0s += 1;
        match counts.iter_mut().find(|(v, _)| *v == k) {
            Some((_, c)) => *c += 1,
            None => counts.push((k, 1)),
        }
    }
    // Counter.most_common(1): highest count, first inserted on ties.
    let modal = counts
        .iter()
        .fold(None::<(f64, usize)>, |best, &(v, c)| match best {
            Some((_, bc)) if bc >= c => best,
            _ => Some((v, c)),
        });
    let (modal_x0, modal_n) = modal.map(|(v, c)| (Some(v), c)).unwrap_or((None, 0));
    let page_has_body_margin = n_x0s > 0 && (modal_n as f64 / n_x0s as f64) >= 0.5;
    let page_center = (left_margin + right_margin) / 2.0;

    let centered = |line: &Line| -> bool {
        if ((line.x0 + line.x1) / 2.0 - page_center).abs() > 12.0 {
            return false;
        }
        if page_has_body_margin && modal_x0.is_some_and(|m| (line.x0 - m).abs() <= 3.0) {
            return false;
        }
        true
    };

    let continues_paragraph = |prev: &Line, first: &Line| -> bool {
        let Some(typical) = typical_line_gap else {
            return false;
        };
        let gap = first.y0 - prev.y1;
        let gap_similar = 0.0 <= gap && gap <= (typical + 2.5).min(gap_threshold);
        let mut not_indented = first.x0 <= left_margin + 3.0;
        if !not_indented && centered(prev) && centered(first) {
            not_indented = true;
        }
        let text_continues = !line_ends_with_sentence_punct(&prev.text)
            || line_starts_with_lowercase(&first.text)
            || prev.x1 >= right_margin - 5.0;
        let same_column = first.x0 < prev.x1 + 3.0 && prev.x0 < first.x1 + 3.0;
        gap_similar && same_column && not_indented && text_continues
    };

    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut current: Vec<Line> = Vec::new();

    for (_, lines) in &records {
        let first = &lines[0];

        if current
            .last()
            .is_some_and(|prev| continues_paragraph(prev, first))
        {
            current.extend(lines.iter().cloned());
            continue;
        }

        if lines.len() > 1 {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
            current = lines.clone();
            continue;
        }

        let is_new_para = match current.last() {
            None => true,
            Some(prev) => {
                let gap = first.y0 - prev.y1;
                // A line flush with the page's left margin or its body-text
                // margin is not an indent, even when that margin sits past
                // indent_x.
                let flush = first.x0 <= left_margin + 3.0
                    || (page_has_body_margin
                        && modal_x0.is_some_and(|m| (first.x0 - m).abs() <= 3.0));
                // Nor is a line that starts where the previous one did while
                // its sentence is still running: the next line of an
                // indented quote or a column set one line per block.
                let wraps =
                    (first.x0 - prev.x0).abs() <= 1.0 && !line_ends_with_sentence_punct(&prev.text);
                // A superscript or a taller glyph stretches its line's box
                // and eats into the measured gap; while the sentence is still
                // running, discount a height difference of up to a quarter
                // of a line. A heading follows a finished sentence.
                let (h_prev, h_first) = (prev.y1 - prev.y0, first.y1 - first.y0);
                let bump = (h_first - h_prev).abs();
                let line_gap = if bump <= 0.25 * h_prev.min(h_first)
                    && !line_ends_with_sentence_punct(&prev.text)
                {
                    gap - bump
                } else {
                    gap
                };
                // The flush and wrap exceptions only hold at the open
                // paragraph's own line spacing: a gap clearly larger than
                // its lines show is a paragraph break even below
                // gap_threshold.
                let line_spaced = match current.as_slice() {
                    [.., a, b] => line_gap <= (b.y0 - a.y1) + 2.5,
                    _ => true,
                };
                let indented = first.x0 > indent_x && !((flush || wraps) && line_spaced);
                indented
                    || line_gap > gap_threshold
                    || gap < -3.0
                    || first.x0 >= prev.x1 + 3.0
                    || prev.x0 >= first.x1 + 3.0
                    || (prev.x1 < 400.0 && gap > 3.0)
            }
        };
        if is_new_para && !current.is_empty() {
            paragraphs.push(std::mem::take(&mut current));
        }
        current.push(first.clone());
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }
    paragraphs
}

/// Paragraphs whose actual text-line boxes intersect the selected region.
/// A small near-line tolerance covers rounding differences; distant regions
/// (such as excluded footnotes and image-only areas) are left unmatched.
pub fn find_paragraph_indices(paragraphs: &[Paragraph], rect: &Rect) -> Vec<usize> {
    fn overlaps(line: &Line, rect: &Rect, tolerance: f64) -> bool {
        line.x0 < rect.x1
            && rect.x0 < line.x1
            && line.y0 < rect.y1 + tolerance
            && rect.y0 < line.y1 + tolerance
            && line.x0 < line.x1
            && line.y0 < line.y1
    }
    if ![rect.x0, rect.y0, rect.x1, rect.y1]
        .iter()
        .all(|v| v.is_finite())
        || rect.x0 >= rect.x1
        || rect.y0 >= rect.y1
    {
        return Vec::new();
    }
    let matches: Vec<_> = paragraphs
        .iter()
        .enumerate()
        .filter(|(_, para)| para.iter().any(|line| overlaps(line, rect, 0.0)))
        .map(|(i, _)| i)
        .collect();
    if !matches.is_empty() {
        return matches;
    }
    paragraphs
        .iter()
        .enumerate()
        .filter(|(_, para)| para.iter().any(|line| overlaps(line, rect, 3.0)))
        .map(|(i, _)| i)
        .collect()
}

/// Best paragraph by intersection area, considering both axes. Returns None
/// when no nearby text overlaps horizontally instead of guessing a paragraph.
pub fn find_paragraph_index(paragraphs: &[Paragraph], rect: &Rect) -> Option<usize> {
    let matches = find_paragraph_indices(paragraphs, rect);
    let mut best = None;
    let mut best_area = -1.0;
    for idx in matches {
        let area: f64 = paragraphs[idx]
            .iter()
            .map(|line| {
                (line.x1.min(rect.x1) - line.x0.max(rect.x0)).max(0.0)
                    * (line.y1.min(rect.y1) - line.y0.max(rect.y0)).max(0.0)
            })
            .sum();
        if area > best_area {
            best = Some(idx);
            best_area = area;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Page furniture: page numbers, footnotes and blank lines
// ---------------------------------------------------------------------------

const FOOTNOTE_SYMBOLS: [&str; 4] = ["*", "\u{2020}", "\u{2021}", "\u{a7}"];

fn para_text(para: &Paragraph) -> String {
    para.iter().map(|l| l.text.as_str()).collect()
}

fn is_blank(para: &Paragraph) -> bool {
    para_text(para).chars().all(py_isspace)
}

/// True for a lowercase roman numeral in canonical form (`i` to `mmmcmxcix`),
/// so words made of the same letters ("civil", "mild") don't qualify.
fn is_roman(t: &str) -> bool {
    let value = |c: char| match c {
        'i' => Some(1),
        'v' => Some(5),
        'x' => Some(10),
        'l' => Some(50),
        'c' => Some(100),
        'd' => Some(500),
        'm' => Some(1000),
        _ => None,
    };
    let Some(digits) = t.chars().map(value).collect::<Option<Vec<u32>>>() else {
        return false;
    };
    if digits.is_empty() {
        return false;
    }
    let mut n = 0u32;
    for (i, &d) in digits.iter().enumerate() {
        if digits.get(i + 1).is_some_and(|&next| next > d) {
            n = n.wrapping_sub(d);
        } else {
            n = n.wrapping_add(d);
        }
    }
    if n == 0 || n > 3999 {
        return false;
    }
    let mut canonical = String::new();
    let mut rest = n;
    for (v, sym) in [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ] {
        while rest >= v {
            canonical.push_str(sym);
            rest -= v;
        }
    }
    canonical == t
}

/// True for text that could be a page number: up to four digits, or a
/// canonical lowercase roman numeral.
fn looks_like_page_number(text: &str) -> bool {
    let t = py_rstrip(py_lstrip(text));
    (!t.is_empty() && t.len() <= 4 && t.chars().all(|c| c.is_ascii_digit())) || is_roman(t)
}

/// True for a page number (folio): a lone line of page-number text with no
/// other text below it or none above it. A year or numeral inside the body
/// has text on both sides and stays body text.
fn is_folio(paras: &[Paragraph], i: usize) -> bool {
    let para = &paras[i];
    if para.len() != 1 || !looks_like_page_number(&para[0].text) {
        return false;
    }
    let f = &para[0];
    let others = || {
        paras
            .iter()
            .enumerate()
            .filter(move |(j, _)| *j != i)
            .flat_map(|(_, p)| p.iter())
            .filter(|l| !l.text.chars().all(py_isspace))
            .map(|l| (l.y0 + l.y1) / 2.0)
    };
    let nothing_below = others().all(|c| c <= f.y1);
    let nothing_above = others().all(|c| c >= f.y0);
    nothing_below || nothing_above
}

fn is_marker_text(text: &str) -> bool {
    let t = py_rstrip(py_lstrip(text));
    (!t.is_empty() && t.len() <= 3 && t.chars().all(|c| c.is_ascii_digit()))
        || FOOTNOTE_SYMBOLS.contains(&t)
}

/// Body line height for the page: the 75th-percentile height of its
/// non-blank lines, so a page dense with footnotes still reflects its body
/// type rather than the notes'.
fn typical_height(paras: &[Paragraph]) -> Option<f64> {
    let mut heights: Vec<f64> = paras
        .iter()
        .flatten()
        .filter(|l| !l.text.chars().all(py_isspace))
        .map(|l| l.y1 - l.y0)
        .collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    heights.get(heights.len() * 3 / 4).copied()
}

/// True when `marker` is a footnote marker for `text`: level with the text
/// line and to its left (hanging-number layouts leave up to a few ems). The
/// marker is either a superscript, set noticeably shorter than the text, or
/// a full-size number beside text that is itself set smaller than the
/// page's typical line. MuPDF reports the marker as a line of its own, and
/// column splitting can move it into a paragraph of its own.
fn marks(marker: &Line, text: &Line, typical: Option<f64>) -> bool {
    let (mh, th) = (marker.y1 - marker.y0, text.y1 - text.y0);
    let superscript = mh < 0.8 * th;
    let small_note = typical.is_some_and(|t| mh < 0.85 * t && th < 0.85 * t);
    is_marker_text(&marker.text)
        && (superscript || small_note)
        && marker.y0 < text.y1
        && text.y0 < marker.y1
        && text.x0 >= marker.x1 - 1.0
        && text.x0 - marker.x1 <= 48.0
}

/// True for a paragraph that looks like a footnote: a superscript marker
/// starts it, whether the marker stayed in the paragraph (one of its first
/// two lines, at its left edge) or grouping split it into a neighbouring
/// paragraph. A paragraph holding only the split-off marker counts too.
fn is_footnote(paras: &[Paragraph], i: usize) -> bool {
    let para = &paras[i];
    if para.len() >= 2 {
        let left = py_min(para.iter().map(|l| l.x0)).unwrap_or(0.0);
        let tallest = para.iter().map(|l| l.y1 - l.y0).fold(0.0, f64::max);
        if para.iter().take(2).any(|l| {
            is_marker_text(&l.text) && (l.x0 - left).abs() <= 1.0 && (l.y1 - l.y0) < 0.8 * tallest
        }) {
            return true;
        }
    }
    let first = &para[0];
    let typical = typical_height(paras);
    if paras.iter().enumerate().any(|(j, other)| {
        j != i && (marks(&other[0], first, typical) || marks(first, &other[0], typical))
    }) {
        return true;
    }
    // A paragraph holding only a marker set shorter than the page's typical
    // line is a split-off footnote marker even when its text sits too far
    // away to pair with.
    if para.len() == 1 && is_marker_text(&first.text) {
        return typical.is_some_and(|t| (first.y1 - first.y0) < 0.8 * t);
    }
    false
}

/// True for body text: not blank, not a page number, not a footnote.
fn is_body(paras: &[Paragraph], i: usize) -> bool {
    !is_blank(&paras[i]) && !is_folio(paras, i) && !is_footnote(paras, i)
}

/// Drop page furniture a long selection catches by accident: blank
/// paragraphs, and page numbers when the highlight also reaches real text.
/// Leaves `matches` alone if nothing else would remain.
pub fn drop_page_furniture(paragraphs: &[Paragraph], matches: &mut Vec<usize>) {
    let mut kept: Vec<usize> = matches
        .iter()
        .copied()
        .filter(|&i| !is_blank(&paragraphs[i]))
        .collect();
    if kept.iter().any(|&i| !is_folio(paragraphs, i)) {
        kept.retain(|&i| !is_folio(paragraphs, i));
    }
    if !kept.is_empty() {
        *matches = kept;
    }
}

/// Index of the page's last body paragraph: the one a page-bottom sentence
/// would continue from. Footnotes above the `footnote_y` cut-off, a page
/// number and blank lines would otherwise take that place.
pub fn last_body_index(paras: &[Paragraph]) -> Option<usize> {
    (0..paras.len()).rev().find(|&i| is_body(paras, i))
}

/// Lines of the page's body paragraphs, skipping `skip` (an index or none).
fn body_lines(paras: &[Paragraph], skip: Option<usize>) -> impl Iterator<Item = &Line> {
    (0..paras.len())
        .filter(move |&i| Some(i) != skip && is_body(paras, i))
        .flat_map(move |i| paras[i].iter())
}

/// Text of a line with digits, whitespace and case removed, so a running
/// head or footer compares equal across pages whatever its page number.
fn furniture_key(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_numeric() && !py_isspace(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// Which paragraphs of page `p` are page furniture: a running head or
/// footer whose every non-blank line repeats, at the same height, on a page
/// up to two pages either side. Blank paragraphs are not furniture here.
fn page_furniture(page_paragraphs: &[Vec<Paragraph>], p: usize) -> Vec<bool> {
    let lo = p.saturating_sub(2);
    let hi = (p + 2).min(page_paragraphs.len() - 1);
    let others: Vec<(f64, String)> = (lo..=hi)
        .filter(|&q| q != p)
        .flat_map(|q| page_paragraphs[q].iter().flatten())
        .map(|l| (l.y0, furniture_key(&l.text)))
        .filter(|(_, k)| !k.is_empty())
        .collect();
    page_paragraphs[p]
        .iter()
        .map(|para| {
            let keys: Vec<(f64, String)> = para
                .iter()
                .map(|l| (l.y0, furniture_key(&l.text)))
                .filter(|(_, k)| !k.is_empty())
                .collect();
            !keys.is_empty()
                && keys.iter().all(|(y0, key)| {
                    others
                        .iter()
                        .any(|(oy, k)| (y0 - oy).abs() <= 2.0 && k == key)
                })
        })
        .collect()
}

/// Index of page `p`'s last body paragraph, also skipping a running footer
/// that repeats on the neighbouring pages (`last_body_index` sees one page
/// only).
fn last_body_on(page_paragraphs: &[Vec<Paragraph>], p: usize) -> Option<usize> {
    let paras = &page_paragraphs[p];
    let furniture = page_furniture(page_paragraphs, p);
    (0..paras.len())
        .rev()
        .find(|&i| !furniture[i] && is_body(paras, i))
}

/// The paragraph of page `p` that would continue the previous page's last
/// body paragraph: its first paragraph, unless that is a running head that
/// repeats on the neighbouring pages, which blocks the bridge as a heading
/// does.
fn continuation_target(page_paragraphs: &[Vec<Paragraph>], p: usize) -> Option<usize> {
    if page_paragraphs[p].is_empty() || page_furniture(page_paragraphs, p)[0] {
        return None;
    }
    Some(0)
}

/// For each page, whether its first paragraph continues the last paragraph
/// of the previous page (`detect_page_continuations`, patched). A running
/// head opening the page blocks the bridge; a running footer closing the
/// previous page is not its last paragraph.
pub fn detect_page_continuations(page_paragraphs: &[Vec<Paragraph>]) -> Vec<bool> {
    const MARGIN_TOLERANCE: f64 = 5.0;
    let n = page_paragraphs.len();
    let mut out = vec![false; n];

    for p in 1..n {
        let prev_paras = &page_paragraphs[p - 1];
        let curr_paras = &page_paragraphs[p];
        if prev_paras.is_empty() || curr_paras.is_empty() {
            continue;
        }
        // Footnotes at the bottom of the previous page don't continue onto
        // this one; the last body paragraph does.
        let Some(prev_idx) = last_body_on(page_paragraphs, p - 1) else {
            continue;
        };
        let prev_last = &prev_paras[prev_idx];
        let Some(first_idx) = continuation_target(page_paragraphs, p) else {
            continue;
        };
        let curr_first = &curr_paras[first_idx];
        // Page geometry comes from the page's own text, so a running head or
        // footer spanning the page doesn't set it.
        let furniture = page_furniture(page_paragraphs, p);
        let sized = || {
            curr_paras
                .iter()
                .zip(&furniture)
                .filter(|(_, f)| !**f)
                .flat_map(|(para, _)| para.iter())
        };

        // Guard 1: an indented first line is a fresh paragraph.
        let left_margin = py_min(sized().map(|l| l.x0)).unwrap();
        if !(curr_first[0].x0 <= left_margin + MARGIN_TOLERANCE) {
            continue;
        }

        // Guard 3: a heading opens the page. That is a first paragraph that
        // starts like a sentence but does not end like one, and is either a
        // short lone line ("Appendix 3:", "Lab Results") or set in larger
        // type than the page's body lines (a section title, even one that
        // wraps).
        let first_line = &curr_first[0];
        let last_line = &curr_first[curr_first.len() - 1];
        if !line_starts_with_lowercase(&first_line.text)
            && !line_ends_with_sentence_punct(&last_line.text)
        {
            let curr_right = py_max(sized().map(|l| l.x1)).unwrap();
            let is_short_line = curr_first.len() == 1
                && (first_line.x1 - first_line.x0) < 0.6 * (curr_right - left_margin);
            // Body line height comes from the rest of the page, so a tall
            // title can't set its own baseline; a page with nothing else on
            // it borrows the previous page's body text.
            let heights_of = |lines: &mut dyn Iterator<Item = &Line>| -> Vec<f64> {
                let mut h: Vec<f64> = lines.map(|l| l.y1 - l.y0).collect();
                h.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                h
            };
            let mut heights = heights_of(&mut body_lines(curr_paras, Some(first_idx)));
            if heights.is_empty() {
                heights = heights_of(&mut body_lines(prev_paras, None));
            }
            let is_large = heights.get(heights.len() / 2).is_some_and(|&body_height| {
                curr_first.iter().all(|l| l.y1 - l.y0 > 1.15 * body_height)
            });
            if is_short_line || is_large {
                continue;
            }
        }

        // Guard 2: a page ending on a lone centred line (narrow or all caps)
        // ends on an epigraph citation or attribution, not a wrapped sentence.
        if prev_last.len() == 1 {
            let pl = &prev_last[0];
            let prev_left = py_min(prev_paras.iter().flatten().map(|l| l.x0)).unwrap();
            let prev_right = py_max(prev_paras.iter().flatten().map(|l| l.x1)).unwrap();
            let prev_center = (prev_left + prev_right) / 2.0;
            let is_centered = ((pl.x0 + pl.x1) / 2.0 - prev_center).abs() <= 12.0;
            let is_narrow = (pl.x1 - pl.x0) < 0.85 * (prev_right - prev_left);
            let letters: Vec<char> = pl.text.chars().filter(|&c| py_isalpha(c)).collect();
            let is_allcaps = !letters.is_empty()
                && (letters.iter().filter(|c| c.is_uppercase()).count() as f64
                    / letters.len() as f64)
                    > 0.85;
            if is_centered && (is_narrow || is_allcaps) {
                continue;
            }
        }

        // Margin evidence comes from body text only: a page number or a
        // footnote says nothing about where body lines end.
        let right_margin = py_max(body_lines(prev_paras, None).map(|l| l.x1)).unwrap();
        let last = &prev_last[prev_last.len() - 1];
        // A page containing only one line provides no independent evidence
        // of its body margin; rely on the text signals in that case.
        let has_margin_evidence = body_lines(prev_paras, None).count() > 1;
        let prev_reaches_margin = has_margin_evidence && last.x1 >= right_margin - MARGIN_TOLERANCE;
        let prev_lacks_terminal_punct = !line_ends_with_sentence_punct(&last.text);
        let curr_starts_lowercase = line_starts_with_lowercase(&curr_first[0].text);

        if prev_reaches_margin || prev_lacks_terminal_punct || curr_starts_lowercase {
            out[p] = true;
        }
    }
    out
}

/// The paragraph at `(page, idx)` plus any cross-page continuations to either
/// side, in the reference order: itself, then backwards, then forwards.
pub fn walk_segments(
    page_paragraphs: &[Vec<Paragraph>],
    continues_from_prev: &[bool],
    page: usize,
    idx: usize,
) -> Vec<(usize, usize)> {
    let n = page_paragraphs.len();
    let mut out = vec![(page, idx)];

    let (mut cp, mut ci) = (page, idx);
    while cp > 0 && continues_from_prev[cp] && continuation_target(page_paragraphs, cp) == Some(ci)
    {
        let pp = cp - 1;
        let Some(pi) = last_body_on(page_paragraphs, pp) else {
            break;
        };
        out.push((pp, pi));
        cp = pp;
        ci = pi;
    }

    let (mut cp, mut ci) = (page, idx);
    while last_body_on(page_paragraphs, cp) == Some(ci) && cp + 1 < n && continues_from_prev[cp + 1]
    {
        let np = cp + 1;
        let Some(ni) = continuation_target(page_paragraphs, np) else {
            break;
        };
        out.push((np, ni));
        cp = np;
        ci = ni;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x0: f64, y0: f64, x1: f64, y1: f64, text: &str) -> Line {
        Line {
            x0,
            y0,
            x1,
            y1,
            text: text.to_owned(),
        }
    }

    #[test]
    fn round1_matches_python() {
        // Values checked against CPython 3.9 round(x, 1).
        assert_eq!(py_round1(0.25), 0.2);
        assert_eq!(py_round1(0.35), 0.3);
        assert_eq!(py_round1(0.45), 0.5);
        assert_eq!(py_round1(36.95), 37.0);
        assert_eq!(py_round1(54.94999), 54.9);
        assert_eq!(py_round1(-1.25), -1.2);
        assert!(py_round1(f64::INFINITY).is_infinite());
    }

    #[test]
    fn sentence_punct() {
        assert!(line_ends_with_sentence_punct("He went home."));
        assert!(line_ends_with_sentence_punct("Really?\u{201d}  "));
        assert!(line_ends_with_sentence_punct("(See above.)"));
        assert!(line_ends_with_sentence_punct("and so\u{2026}"));
        assert!(!line_ends_with_sentence_punct("and the"));
        assert!(!line_ends_with_sentence_punct("EPHESIANS 4:17-24"));
        assert!(!line_ends_with_sentence_punct(""));
        assert!(!line_ends_with_sentence_punct("\u{201d}"));
        assert!(line_ends_with_sentence_punct("end.\u{a0}"));
    }

    #[test]
    fn lowercase_start() {
        assert!(line_starts_with_lowercase("  and then"));
        assert!(line_starts_with_lowercase("\u{e9}tait"));
        assert!(!line_starts_with_lowercase("And then"));
        assert!(!line_starts_with_lowercase("\u{201c}and"));
        assert!(!line_starts_with_lowercase(""));
    }

    #[test]
    fn multiline_blocks_stay_separate() {
        let b1 = Block {
            bbox: [72.0, 100.0, 540.0, 126.0],
            lines: vec![
                line(72.0, 100.0, 540.0, 112.0, "First para line one"),
                line(72.0, 114.0, 300.0, 126.0, "ends here."),
            ],
        };
        let b2 = Block {
            bbox: [90.0, 128.0, 540.0, 154.0],
            lines: vec![
                line(90.0, 128.0, 540.0, 140.0, "Second para"),
                line(72.0, 142.0, 400.0, 154.0, "ends."),
            ],
        };
        let paras = group_lines_into_paragraphs(&[b1, b2], &Params::default());
        assert_eq!(paras.len(), 2);
    }

    #[test]
    fn split_block_merges_when_sentence_continues() {
        // Same line gap, not indented, previous line has no terminal punct.
        let b1 = Block {
            bbox: [72.0, 100.0, 540.0, 126.0],
            lines: vec![
                line(72.0, 100.0, 540.0, 112.0, "A sentence that"),
                line(72.0, 114.0, 540.0, 126.0, "keeps going and"),
            ],
        };
        let b2 = Block {
            bbox: [72.0, 128.0, 540.0, 154.0],
            lines: vec![
                line(72.0, 128.0, 540.0, 140.0, "keeps going"),
                line(72.0, 142.0, 300.0, 154.0, "until it ends."),
            ],
        };
        let paras = group_lines_into_paragraphs(&[b1, b2], &Params::default());
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].len(), 4);
    }

    #[test]
    fn one_line_blocks_flush_with_margin_past_indent_x_merge() {
        // Browser print-to-PDF: every line is its own block and the body
        // margin (84) sits past the default indent_x (80). Lines flush with
        // the page margin are not indents; the larger gap splits paragraphs.
        let rows = [
            (
                41.0,
                503.2,
                "I propose that there is one problem chief among them, an",
            ),
            (
                67.4,
                509.6,
                "impetus for bad software from which almost all others take",
            ),
            (93.8, 270.2, "root: imaginary problems."),
            (
                127.7,
                510.5,
                "Most complicated or broken software is not designed to be",
            ),
            (
                154.1,
                487.3,
                "overly complex or dysfunctional. It\u{2019}s just designed to do",
            ),
            (180.5, 396.8, "something other than its intended purpose."),
        ];
        let blocks: Vec<Block> = rows
            .iter()
            .map(|&(y0, x1, text)| Block {
                bbox: [84.0, y0, x1, y0 + 16.5],
                lines: vec![line(84.0, y0, x1, y0 + 16.5, text)],
            })
            .collect();
        let paras = group_lines_into_paragraphs(&blocks, &Params::default());
        assert_eq!(paras.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3]);

        // A tightly set two-line block elsewhere on the page (a header, a
        // caption) doesn't make the body's wider line spacing look like
        // paragraph breaks.
        let mut with_header = vec![Block {
            bbox: [84.0, 0.0, 400.0, 21.0],
            lines: vec![
                line(84.0, 0.0, 400.0, 10.0, "Imaginary Problems Are the Root"),
                line(84.0, 11.0, 300.0, 21.0, "of Bad Software"),
            ],
        }];
        with_header.extend(blocks.iter().cloned());
        let paras = group_lines_into_paragraphs(&with_header, &Params::default());
        assert_eq!(paras.iter().map(Vec::len).collect::<Vec<_>>(), [2, 3, 3]);

        // A one-line block indented from the page margin still starts a
        // new paragraph, even with a normal line gap.
        let mut indented = blocks[..3].to_vec();
        indented[1].bbox[0] = 104.0;
        indented[1].lines[0].x0 = 104.0;
        let paras = group_lines_into_paragraphs(&indented, &Params::default());
        assert_eq!(paras.len(), 2);

        // Outdented bullets lower the page's minimum left edge; body lines
        // at the modal margin are still flush.
        let mut with_bullet = blocks.clone();
        let y0 = 230.0;
        with_bullet.push(Block {
            bbox: [68.1, y0, 484.8, y0 + 16.5],
            lines: vec![line(68.1, y0, 484.8, y0 + 16.5, "\u{2022} Fast load time")],
        });
        let paras = group_lines_into_paragraphs(&with_bullet, &Params::default());
        assert_eq!(paras.iter().map(Vec::len).collect::<Vec<_>>(), [3, 3, 1]);
    }

    #[test]
    fn indented_quote_in_one_line_blocks_stays_whole() {
        // Body at 109, a quote indented to 122, every line its own block.
        let one = |x0: f64, y0: f64, text: &str| Block {
            bbox: [x0, y0, 490.0, y0 + 15.3],
            lines: vec![line(x0, y0, 490.0, y0 + 15.3, text)],
        };
        let blocks = vec![
            one(109.1, 438.6, "During the past 30 years, managers have been"),
            one(109.1, 459.6, "bombarded with two competing approaches."),
            one(109.1, 501.6, "Douglas McGregor drew a distinction between"),
            one(
                109.1,
                522.6,
                "the assumptions which underlie these approaches:",
            ),
            one(
                122.3,
                579.6,
                "Theory X assumes that people dislike work and must be coerced,",
            ),
            one(
                122.3,
                600.6,
                "controlled, and directed toward organizational goals.",
            ),
            one(122.3, 621.6, "Most people prefer to be treated this way."),
        ];
        let paras = group_lines_into_paragraphs(&blocks, &Params::default());
        // The quote's second line continues its sentence; its third starts
        // a new sentence at the quote's indent, as before.
        assert_eq!(paras.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 2, 1]);
    }

    #[test]
    fn heading_at_body_margin_after_tight_lines_stays_separate() {
        // Body lines 0.6 pt apart in one block, margin at 90 (past
        // indent_x); a same-size heading follows after a 7.3 pt gap, below
        // gap_threshold but far above the page's line spacing.
        let block = Block {
            bbox: [90.0, 409.3, 521.0, 450.2],
            lines: vec![
                line(
                    90.0,
                    409.3,
                    520.9,
                    422.6,
                    "variety of combinations can be found,",
                ),
                line(
                    90.0,
                    423.2,
                    515.6,
                    436.5,
                    "significantly, that a Church which has",
                ),
                line(
                    90.0,
                    437.1,
                    455.9,
                    450.2,
                    "ceased to be a mission is not a Church.",
                ),
            ],
        };
        let heading = Block {
            bbox: [90.0, 457.5, 311.4, 470.8],
            lines: vec![line(
                90.0,
                457.5,
                311.4,
                470.8,
                "Donald Durnbaugh, The Believers",
            )],
        };
        let paras = group_lines_into_paragraphs(&[block, heading], &Params::default());
        assert_eq!(paras.iter().map(Vec::len).collect::<Vec<_>>(), [3, 1]);
    }

    #[test]
    fn superscript_line_does_not_split_its_paragraph() {
        // 18 pt lines, 10.8 pt apart; a footnote reference makes one line's
        // box 21.8 pt tall, so its measured gap (12.6) passes gap_threshold.
        let one = |y0: f64, y1: f64, text: &str| Block {
            bbox: [84.0, y0, 500.0, y1],
            lines: vec![line(84.0, y0, 500.0, y1, text)],
        };
        let blocks = vec![
            one(
                250.4,
                268.4,
                "Only you can make the connections between two",
            ),
            one(279.2, 297.2, "Zetteln, and so writing texts about them."),
            one(
                401.9,
                419.9,
                "The time it takes to integrate a Zettel into the",
            ),
            one(
                432.5,
                454.3,
                "Zettelkasten grows with the size of the arcive.[5] This is",
            ),
            one(
                465.1,
                483.1,
                "the only task whose time consumption is dependent on",
            ),
        ];
        let paras = group_lines_into_paragraphs(&blocks, &Params::default());
        assert_eq!(paras.iter().map(Vec::len).collect::<Vec<_>>(), [2, 3]);

        // A heading set only a little larger, after a finished sentence,
        // keeps its own paragraph.
        let mut numbered = blocks.clone();
        numbered[2] = one(310.9, 331.6, "1. Teaching is the most impactful aspect.");
        let paras = group_lines_into_paragraphs(&numbered, &Params::default());
        assert_eq!(paras.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1, 2]);

        // A heading in much larger type keeps its own paragraph.
        let mut titled = blocks.clone();
        titled[2] = one(330.0, 360.0, "A Heading Set Large");
        let paras = group_lines_into_paragraphs(&titled, &Params::default());
        assert_eq!(paras.len(), 3);
    }

    #[test]
    fn running_header_does_not_take_a_page_bridge() {
        let body = |y0: f64, text: &str| line(84.0, y0, 520.0, y0 + 15.0, text);
        // A browser prints the same title at the top of every page and a
        // page count at the bottom.
        let header = vec![line(
            0.0,
            0.1,
            612.0,
            11.2,
            "How to Partner with Churches Well",
        )];
        let prev = vec![
            header.clone(),
            vec![body(40.0, "First paragraph ends here.")],
            vec![
                body(678.0, "our church planters coming in from East Africa. In"),
                body(
                    702.0,
                    "two of the families are actually coming from the same",
                ),
                body(726.0, "church. We have raised them up, and we will be"),
            ],
            vec![line(0.0, 780.6, 28.3, 791.7, "6 of 16")],
        ];
        let curr = vec![
            header,
            vec![
                body(40.0, "supporting them as they go out to plant churches,"),
                body(64.0, "and that is the plan for the next few years."),
            ],
            vec![body(100.0, "Another paragraph.")],
            vec![line(0.0, 780.6, 28.3, 791.7, "7 of 16")],
        ];
        // The running head opening the page blocks the bridge, as a heading
        // would, so it is never highlighted as the paragraph's other half.
        let pages = vec![prev.clone(), curr.clone()];
        let cont = detect_page_continuations(&pages);
        assert_eq!(cont, vec![false, false]);
        assert_eq!(walk_segments(&pages, &cont, 0, 2), vec![(0, 2)]);
        assert_eq!(walk_segments(&pages, &cont, 1, 0), vec![(1, 0)]);

        // Without the running head the paragraph bridges, and the repeated
        // page count below it is not taken as the previous page's last
        // paragraph.
        let headless: Vec<Paragraph> = curr[1..].to_vec();
        let pages = vec![prev.clone(), headless];
        let cont = detect_page_continuations(&pages);
        assert_eq!(cont, vec![false, true]);
        assert_eq!(walk_segments(&pages, &cont, 0, 2), vec![(0, 2), (1, 0)]);
        assert_eq!(walk_segments(&pages, &cont, 1, 0), vec![(1, 0), (0, 2)]);

        // A section heading that appears only on this page still blocks the
        // bridge, even when it is set smaller than the body text.
        let mut headed = curr;
        headed[0] = vec![line(84.0, 0.1, 240.0, 11.2, "The Library of Congress")];
        assert_eq!(
            detect_page_continuations(&[prev, headed]),
            vec![false, false]
        );
    }

    #[test]
    fn body_text_running_past_footnote_y_is_kept() {
        let one = |x0: f64, y0: f64, h: f64, text: &str| Block {
            bbox: [x0, y0, 500.0, y0 + h],
            lines: vec![line(x0, y0, 500.0, y0 + h, text)],
        };
        // Body set in 16.5 pt lines at a 26.4 pt pitch; the last line starts
        // at 723.5, just past the default footnote_y of 720.
        let mut blocks = vec![
            one(84.0, 670.7, 16.5, "in the case of some particularly nasty"),
            one(
                84.0,
                697.1,
                16.5,
                "Goldman Sachs, getting a few brown envelopes",
            ),
            one(
                84.0,
                723.5,
                16.5,
                "sent to a few officers and a strange suicide.",
            ),
        ];
        let paras = group_lines_into_paragraphs(&blocks, &Params::default());
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].len(), 3);

        // A note in smaller type that keeps its own rhythm below the cut,
        // in the same block as the note above it, stays cut.
        let note = Block {
            bbox: [108.0, 688.5, 542.0, 734.1],
            lines: vec![
                line(
                    108.0,
                    688.5,
                    542.0,
                    697.3,
                    "In addition, his description fails",
                ),
                line(
                    108.0,
                    697.7,
                    542.0,
                    706.5,
                    "what they did with the food laws.",
                ),
                line(
                    108.0,
                    706.9,
                    542.0,
                    715.7,
                    "categories of liberal and conservative",
                ),
                line(
                    108.0,
                    716.1,
                    349.3,
                    724.9,
                    "of history which was being inaugurated.",
                ),
                line(
                    108.0,
                    725.3,
                    385.7,
                    734.1,
                    "See D H Kelsey, The Uses of Scripture",
                ),
            ],
        };
        let mut with_note = blocks.clone();
        with_note.clear();
        for (i, y0) in [494.4, 520.8, 547.2, 573.6, 600.0, 626.4, 652.8]
            .iter()
            .enumerate()
        {
            let text = format!("Body line {i} of the page, set in the body type, ends.");
            with_note.push(one(84.0, *y0, 16.5, &text));
        }
        with_note.push(note);
        let paras = group_lines_into_paragraphs(&with_note, &Params::default());
        assert_eq!(paras.iter().map(Vec::len).sum::<usize>(), 11);

        // A smaller-type note, a line after a larger gap, and an indented
        // line below the cut are still dropped.
        for extra in [
            one(84.0, 749.9, 12.0, "1. A footnote in smaller type."),
            one(84.0, 760.0, 16.5, "Running footer"),
            one(120.0, 749.9, 16.5, "Indented line"),
        ] {
            blocks.push(extra);
            let paras = group_lines_into_paragraphs(&blocks, &Params::default());
            assert_eq!(paras.iter().map(Vec::len).sum::<usize>(), 3);
            blocks.pop();
        }
    }

    #[test]
    fn find_paragraph_by_overlap_without_distant_guesses() {
        let paras = vec![
            vec![
                line(72.0, 100.0, 540.0, 112.0, "a"),
                line(72.0, 114.0, 540.0, 126.0, "b"),
            ],
            vec![line(72.0, 200.0, 540.0, 212.0, "c")],
        ];
        let r = |y0, y1| Rect {
            x0: 80.0,
            y0,
            x1: 120.0,
            y1,
        };
        assert_eq!(find_paragraph_index(&paras, &r(102.0, 110.0)), Some(0));
        assert_eq!(find_paragraph_index(&paras, &r(201.0, 211.0)), Some(1));
        assert_eq!(find_paragraph_index(&paras, &r(170.0, 180.0)), None);
        assert_eq!(find_paragraph_index(&[], &r(1.0, 2.0)), None);
    }

    #[test]
    fn continuation_and_attribution_guard() {
        let p0 = vec![vec![
            line(72.0, 600.0, 540.0, 612.0, "a wrapped sentence that"),
            line(72.0, 614.0, 540.0, 626.0, "runs to the margin and"),
        ]];
        let p1 = vec![vec![
            line(72.0, 72.0, 540.0, 84.0, "carries on here."),
            line(72.0, 86.0, 300.0, 98.0, "done."),
        ]];
        assert_eq!(
            detect_page_continuations(&[p0, p1.clone()]),
            vec![false, true]
        );

        // Previous page ends on a lone centred all-caps attribution.
        let p0 = vec![
            vec![line(72.0, 100.0, 540.0, 112.0, "Body text.")],
            vec![line(250.0, 600.0, 362.0, 612.0, "EPHESIANS 4:17-24")],
        ];
        assert_eq!(
            detect_page_continuations(&[p0, p1.clone()]),
            vec![false, false]
        );

        // Next page opens on a heading-like lone line.
        let p0 = vec![vec![
            line(72.0, 600.0, 540.0, 612.0, "Elder, leaders of house"),
            line(
                72.0,
                614.0,
                300.0,
                626.0,
                "City Elder, appointed for a city",
            ),
        ]];
        for heading in ["Appendix 3:", "Lab Results"] {
            let h = vec![
                vec![line(72.0, 72.0, 200.0, 84.0, heading)],
                vec![line(72.0, 100.0, 540.0, 112.0, "Body text.")],
            ];
            assert_eq!(
                detect_page_continuations(&[p0.clone(), h]),
                vec![false, false]
            );
        }
        // A wide or wrapped title set larger than the body is a heading too.
        let titled = vec![
            vec![
                line(
                    72.0,
                    88.0,
                    347.0,
                    116.0,
                    "Images: Communicating Powerfully to",
                ),
                line(72.0, 108.0, 123.0, 136.0, "People"),
            ],
            vec![
                line(
                    72.0,
                    138.0,
                    540.0,
                    160.0,
                    "Biblical counselors want to understand",
                ),
                line(
                    72.0,
                    153.0,
                    540.0,
                    175.0,
                    "people according to concepts that are",
                ),
            ],
            vec![line(
                72.0,
                247.0,
                540.0,
                269.0,
                "In this brief section I want to deal.",
            )],
        ];
        assert_eq!(
            detect_page_continuations(&[p0.clone(), titled]),
            vec![false, false]
        );
        // A full-width lone line is body text, not a heading.
        let wide = vec![
            vec![line(
                72.0,
                72.0,
                540.0,
                84.0,
                "COVID the world had more jobs at higher wages",
            )],
            vec![line(72.0, 100.0, 540.0, 112.0, "Body text.")],
        ];
        assert_eq!(
            detect_page_continuations(&[p0.clone(), wide]),
            vec![false, true]
        );
        // A short one-line paragraph that ends a sentence doesn't reach the
        // column's right margin, so a new page after it starts fresh.
        let ends = vec![
            vec![line(
                72.0,
                580.0,
                540.0,
                592.0,
                "Body text that runs right across the",
            )],
            vec![line(72.0, 600.0, 200.0, 612.0, "Discovery Center.")],
        ];
        let next = vec![vec![
            line(
                72.0,
                72.0,
                540.0,
                84.0,
                "Historically the organization has funded",
            ),
            line(72.0, 86.0, 540.0, 98.0, "translations around the world."),
        ]];
        assert_eq!(detect_page_continuations(&[ends, next]), vec![false, false]);
        // A lone line that finishes the sentence still continues it.
        let end = vec![vec![line(72.0, 72.0, 200.0, 84.0, "and so it ends.")]];
        assert_eq!(
            detect_page_continuations(&[p0.clone(), end]),
            vec![false, true]
        );
    }

    #[test]
    fn page_furniture_dropped_from_long_selections() {
        let page = vec![
            vec![line(280.0, 40.0, 300.0, 52.0, "xii")],
            vec![line(72.0, 100.0, 540.0, 112.0, "body text")],
            vec![line(72.0, 120.0, 540.0, 132.0, " ")],
            vec![line(300.0, 700.0, 312.0, 712.0, "26")],
        ];
        // Top and bottom folios and a blank line all go.
        let mut m = vec![0, 1, 2, 3];
        drop_page_furniture(&page, &mut m);
        assert_eq!(m, vec![1]);
        // A highlight on the page number alone keeps it.
        let mut m = vec![3];
        drop_page_furniture(&page, &mut m);
        assert_eq!(m, vec![3]);
        // Only blank lines: leave the match alone rather than lose it.
        let mut m = vec![2];
        drop_page_furniture(&page, &mut m);
        assert_eq!(m, vec![2]);
    }

    #[test]
    fn numeric_body_text_is_not_a_folio() {
        // A year and a roman-letter word on lines of their own inside the
        // body, selected along with the paragraph above: both are kept.
        for word in ["2026", "civil", "mild"] {
            let page = vec![
                vec![line(72.0, 100.0, 540.0, 112.0, "The plan was adopted in")],
                vec![line(72.0, 134.0, 110.0, 146.0, word)],
                vec![line(
                    72.0,
                    170.0,
                    540.0,
                    182.0,
                    "More body text follows here.",
                )],
                vec![line(300.0, 700.0, 312.0, 712.0, "9")],
            ];
            let mut m = vec![0, 1];
            drop_page_furniture(&page, &mut m);
            assert_eq!(m, vec![0, 1], "{word}");
            assert_eq!(last_body_index(&page), Some(2), "{word}");
        }
        assert!(is_roman("xii") && is_roman("iv") && is_roman("mcmxc"));
        assert!(!is_roman("civil") && !is_roman("mild") && !is_roman("iiii") && !is_roman(""));
    }

    #[test]
    fn folio_is_not_margin_evidence() {
        // One finished full-width body line, then a fresh capitalised
        // paragraph: no bridge, with or without a centred folio below.
        let body = vec![line(72.0, 600.0, 540.0, 612.0, "This section is complete.")];
        let next = vec![vec![
            line(
                72.0,
                72.0,
                540.0,
                84.0,
                "A new paragraph opens the next page",
            ),
            line(72.0, 86.0, 540.0, 98.0, "and carries on."),
        ]];
        let alone = vec![body.clone()];
        assert_eq!(
            detect_page_continuations(&[alone, next.clone()]),
            vec![false, false]
        );
        let with_folio = vec![body, vec![line(300.0, 745.0, 306.0, 757.0, "9")]];
        assert_eq!(
            detect_page_continuations(&[with_folio, next]),
            vec![false, false]
        );
    }

    #[test]
    fn split_footnote_marker_through_the_pipeline() {
        // MuPDF returns the footnote as one block, but its superscript
        // marker (x 72-77) and its text (x 84-400) don't overlap, so column
        // splitting turns them into separate paragraphs.
        let block = |lines: Vec<Line>| {
            let x0 = lines.iter().map(|l| l.x0).fold(f64::INFINITY, f64::min);
            let y0 = lines.iter().map(|l| l.y0).fold(f64::INFINITY, f64::min);
            let x1 = lines.iter().map(|l| l.x1).fold(f64::NEG_INFINITY, f64::max);
            let y1 = lines.iter().map(|l| l.y1).fold(f64::NEG_INFINITY, f64::max);
            Block {
                bbox: [x0, y0, x1, y1],
                lines,
            }
        };
        let page0 = vec![
            block(vec![
                line(
                    72.0,
                    600.0,
                    540.0,
                    612.0,
                    "a body sentence that runs to the",
                ),
                line(72.0, 614.0, 540.0, 626.0, "margin and keeps going into the"),
            ]),
            block(vec![
                line(
                    84.0,
                    688.0,
                    400.0,
                    700.0,
                    "See the appendix for the full survey.",
                ),
                line(72.0, 689.0, 77.0, 695.0, "1"),
            ]),
        ];
        let page1 = vec![block(vec![
            line(72.0, 72.0, 540.0, 84.0, "next page, where it finally ends."),
            line(72.0, 86.0, 300.0, 98.0, "Done."),
        ])];
        let params = Params::default();
        let pages = vec![
            group_lines_into_paragraphs(&page0, &params),
            group_lines_into_paragraphs(&page1, &params),
        ];
        assert_eq!(pages[0].len(), 3, "body, footnote text, marker");
        // A hanging marker further left still pairs with its text.
        let hanging = vec![
            vec![line(
                72.0,
                600.0,
                540.0,
                612.0,
                "body text that runs on to the",
            )],
            vec![line(
                125.0,
                688.0,
                540.0,
                700.0,
                "Guthrie observes that these",
            )],
            vec![line(90.0, 689.0, 97.0, 695.0, "17")],
        ];
        assert_eq!(last_body_index(&hanging), Some(0));
        // Full-size footnote numbers: no superscript to pair, but the notes
        // are set smaller than the body, so they still don't end the page.
        let small_notes = vec![
            vec![
                line(
                    60.0,
                    500.0,
                    540.0,
                    512.0,
                    "Over time the original patterns were",
                ),
                line(
                    60.0,
                    516.0,
                    540.0,
                    528.0,
                    "forgotten, and the tendency toward",
                ),
                line(
                    60.0,
                    532.0,
                    540.0,
                    544.0,
                    "eisegesis grew until study Bibles",
                ),
            ],
            vec![line(60.0, 622.0, 71.0, 631.0, " 16 ")],
            vec![
                line(
                    90.0,
                    621.0,
                    541.0,
                    630.0,
                    "Strauch, Biblical Eldership, 37.",
                ),
                line(90.0, 631.0, 543.0, 640.0, "Philip Schaff noted in 1910."),
            ],
            vec![line(60.0, 656.0, 71.0, 665.0, " 17 ")],
            vec![line(
                90.0,
                656.0,
                537.0,
                665.0,
                "Guthrie observes: these three",
            )],
            vec![line(304.0, 745.0, 308.0, 755.0, "7")],
        ];
        assert_eq!(last_body_index(&small_notes), Some(0));
        let body = pages[0]
            .iter()
            .position(|p| p[0].text.starts_with("a body"))
            .unwrap();
        let note = pages[0]
            .iter()
            .position(|p| p[0].text.starts_with("See the"))
            .unwrap();
        assert_eq!(last_body_index(&pages[0]), Some(body));
        let cont = detect_page_continuations(&pages);
        assert_eq!(cont, vec![false, true]);
        let segs = walk_segments(&pages, &cont, 1, 0);
        assert!(segs.contains(&(0, body)), "{segs:?}");
        assert!(!segs.contains(&(0, note)), "{segs:?}");
    }

    #[test]
    fn sparse_page_wrapped_title_is_a_heading() {
        // A two-line title set at 28pt over a single 12pt body line: the
        // title must not set the body height it is compared against.
        let prev = vec![vec![
            line(
                72.0,
                600.0,
                540.0,
                612.0,
                "a sentence that does not finish on",
            ),
            line(
                72.0,
                614.0,
                540.0,
                626.0,
                "this page because it keeps on going",
            ),
        ]];
        let curr = vec![
            vec![
                line(72.0, 72.0, 400.0, 100.0, "Communicating Powerfully to"),
                line(72.0, 102.0, 200.0, 130.0, "People"),
            ],
            vec![line(
                72.0,
                150.0,
                540.0,
                162.0,
                "Body text follows the title here.",
            )],
        ];
        assert_eq!(
            detect_page_continuations(&[prev.clone(), curr]),
            vec![false, false]
        );
        // With nothing else on the page, the previous page's body sets it.
        let only_title = vec![vec![
            line(72.0, 72.0, 400.0, 100.0, "Communicating Powerfully to"),
            line(72.0, 102.0, 200.0, 130.0, "People"),
        ]];
        assert_eq!(
            detect_page_continuations(&[prev, only_title]),
            vec![false, false]
        );
    }

    #[test]
    fn footnotes_do_not_end_the_page() {
        // Body paragraph, then a footnote whose superscript marker MuPDF
        // reports as its own short line at the left edge.
        let body = vec![
            line(72.0, 600.0, 540.0, 612.0, "a sentence that runs to the"),
            line(72.0, 614.0, 540.0, 626.0, "margin and keeps going"),
        ];
        let note = vec![
            line(80.0, 697.0, 527.0, 710.0, "Most people talk at a rate of"),
            line(72.0, 698.0, 84.0, 706.5, "22"),
            line(72.0, 710.6, 277.0, 720.0, "Robert. The Art of Helping."),
        ];
        let folio = vec![line(300.0, 745.0, 312.0, 757.0, "9")];
        assert_eq!(
            last_body_index(&[body.clone(), note.clone(), folio]),
            Some(0)
        );
        let p0 = vec![body.clone(), note.clone()];
        assert_eq!(last_body_index(&p0), Some(0));
        let p1 = vec![vec![line(72.0, 72.0, 540.0, 84.0, "onto the next page.")]];
        let pages = [p0, p1];
        let cont = detect_page_continuations(&pages);
        assert_eq!(cont, vec![false, true]);
        // The bridge links the body paragraph, never the footnote.
        assert_eq!(walk_segments(&pages, &cont, 1, 0), vec![(1, 0), (0, 0)]);
        assert_eq!(walk_segments(&pages, &cont, 0, 1), vec![(0, 1)]);
        assert_eq!(walk_segments(&pages, &cont, 0, 0), vec![(0, 0), (1, 0)]);
        // A superscript inside body text is not a footnote marker.
        let inline = vec![
            line(72.0, 600.0, 540.0, 612.0, "Listen - what a person says"),
            line(113.0, 601.0, 125.0, 609.0, "22"),
        ];
        assert_eq!(last_body_index(&[inline]), Some(0));
    }

    #[test]
    fn walk_segments_follows_continuations() {
        let a = vec![
            vec![line(0.0, 0.0, 1.0, 1.0, "x")],
            vec![line(0.0, 2.0, 1.0, 3.0, "y")],
        ];
        let b = vec![vec![line(0.0, 0.0, 1.0, 1.0, "z")]];
        let c = vec![vec![line(0.0, 0.0, 1.0, 1.0, "w")]];
        let pages = vec![a, b, c];
        let cont = vec![false, true, true];
        assert_eq!(
            walk_segments(&pages, &cont, 1, 0),
            vec![(1, 0), (0, 1), (2, 0)]
        );
        assert_eq!(walk_segments(&pages, &cont, 0, 0), vec![(0, 0)]);
    }
}
