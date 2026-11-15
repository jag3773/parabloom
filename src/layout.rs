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
    let mut records: Vec<(f64, Vec<Line>)> = Vec::new();
    for block in blocks {
        let by0 = block.bbox[1];
        if by0 > footnote_y {
            continue;
        }
        let lines: Vec<Line> = block
            .lines
            .iter()
            .filter(|l| !(l.y0 > footnote_y))
            .cloned()
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
                first.x0 > indent_x
                    || gap > gap_threshold
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

/// For each page, whether its first paragraph continues the last paragraph
/// of the previous page (`detect_page_continuations`, patched).
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
        let Some(prev_idx) = last_body_index(prev_paras) else {
            continue;
        };
        let prev_last = &prev_paras[prev_idx];
        let curr_first = &curr_paras[0];

        // Guard 1: an indented first line is a fresh paragraph.
        let left_margin = py_min(curr_paras.iter().flatten().map(|l| l.x0)).unwrap();
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
            let curr_right = py_max(curr_paras.iter().flatten().map(|l| l.x1)).unwrap();
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
            let mut heights = heights_of(&mut body_lines(curr_paras, Some(0)));
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
    while ci == 0 && cp > 0 && continues_from_prev[cp] {
        let pp = cp - 1;
        let Some(pi) = last_body_index(&page_paragraphs[pp]) else {
            break;
        };
        out.push((pp, pi));
        cp = pp;
        ci = pi;
    }

    let (mut cp, mut ci) = (page, idx);
    while last_body_index(&page_paragraphs[cp]) == Some(ci)
        && cp + 1 < n
        && continues_from_prev[cp + 1]
    {
        let np = cp + 1;
        if page_paragraphs[np].is_empty() {
            break;
        }
        out.push((np, 0));
        cp = np;
        ci = 0;
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
