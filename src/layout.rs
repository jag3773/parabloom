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
        let prev_last = &prev_paras[prev_paras.len() - 1];
        let curr_first = &curr_paras[0];

        // Guard 1: an indented first line is a fresh paragraph.
        let left_margin = py_min(curr_paras.iter().flatten().map(|l| l.x0)).unwrap();
        if !(curr_first[0].x0 <= left_margin + MARGIN_TOLERANCE) {
            continue;
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

        let right_margin = py_max(prev_paras.iter().flatten().map(|l| l.x1)).unwrap();
        let last = &prev_last[prev_last.len() - 1];
        // A page containing only one line provides no independent evidence
        // of its body margin; rely on the text signals in that case.
        let has_margin_evidence = prev_paras.iter().map(Vec::len).sum::<usize>() > 1;
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
        if page_paragraphs[pp].is_empty() {
            break;
        }
        let pi = page_paragraphs[pp].len() - 1;
        out.push((pp, pi));
        cp = pp;
        ci = pi;
    }

    let (mut cp, mut ci) = (page, idx);
    while !page_paragraphs[cp].is_empty()
        && ci == page_paragraphs[cp].len() - 1
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
        assert_eq!(detect_page_continuations(&[p0, p1]), vec![false, false]);
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
