//! Paragraph reconstruction from text blocks. No PDF library calls.
//!
//! Each function mirrors one in the reference script, the 2026-08-03 patched
//! `expand_highlights.py`, including its handling of empty (infinite) boxes
//! and Python's float and string semantics. Changing behaviour here changes
//! highlights on books that were tuned against the Python version, so keep
//! these functions in step with the reference.

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
        records.push((by0, lines));
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
        if page_has_body_margin {
            if let Some(m) = modal_x0 {
                if (line.x0 - m).abs() <= 3.0 {
                    return false;
                }
            }
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
        gap_similar && not_indented && text_continues
    };

    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut current: Vec<Line> = Vec::new();

    for (_, lines) in &records {
        let first = &lines[0];

        if let Some(prev) = current.last() {
            if continues_paragraph(prev, first) {
                current.extend(lines.iter().cloned());
                continue;
            }
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
                first.x0 > indent_x || gap > gap_threshold || (prev.x1 < 400.0 && gap > 3.0)
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

/// Index of the paragraph that contains the highlight's vertical centre, or
/// failing that the paragraph whose vertical midpoint is nearest.
pub fn find_paragraph_index(paragraphs: &[Paragraph], rect: &Rect) -> Option<usize> {
    let hy = (rect.y0 + rect.y1) / 2.0;
    for (idx, para) in paragraphs.iter().enumerate() {
        let py0 = para[0].y0;
        let py1 = para[para.len() - 1].y1;
        if py0 - 3.0 <= hy && hy <= py1 + 3.0 {
            return Some(idx);
        }
    }
    let mut best = None;
    let mut best_dist = f64::INFINITY;
    for (idx, para) in paragraphs.iter().enumerate() {
        let mid = (para[0].y0 + para[para.len() - 1].y1) / 2.0;
        let dist = (hy - mid).abs();
        if dist < best_dist {
            best_dist = dist;
            best = Some(idx);
        }
    }
    best
}

/// True for text that is only a page number: up to six digits or lowercase roman
/// numerals, ignoring surrounding whitespace.
fn is_page_number(text: &str) -> bool {
    let t = py_rstrip(py_lstrip(text));
    !t.is_empty()
        && t.chars().count() <= 6
        && (t.chars().all(|c| c.is_ascii_digit()) || t.chars().all(|c| "ivxlcdm".contains(c)))
}

/// Every paragraph a highlight touches, matched line by line.
///
/// `lines` holds the bounding box of each of the highlight's quads (one per
/// highlighted line). Each line picks the paragraph that contains its
/// vertical centre, preferring an exact hit over one within the 3pt
/// tolerance, and among those a paragraph that overlaps the line
/// horizontally, so a line in one column never lands in the paragraph beside
/// it. A highlight dragged across several paragraphs therefore covers all of
/// them. Indices come back in quad order without repeats. When no line lands
/// in a paragraph (no quads, or every centre falls in a gap), this falls back
/// to [`find_paragraph_index`] on the whole rectangle, the reference
/// behaviour.
pub fn find_paragraph_indices(paragraphs: &[Paragraph], rect: &Rect, lines: &[Rect]) -> Vec<usize> {
    let pick = |q: &Rect, tol: f64| -> Option<usize> {
        let hy = (q.y0 + q.y1) / 2.0;
        let mut first = None;
        for (i, p) in paragraphs.iter().enumerate() {
            let (y0, y1) = (p[0].y0, p[p.len() - 1].y1);
            if !(y0 - tol <= hy && hy <= y1 + tol) {
                continue;
            }
            let px0 = py_min(p.iter().map(|l| l.x0)).unwrap_or(f64::INFINITY);
            let px1 = py_max(p.iter().map(|l| l.x1)).unwrap_or(f64::NEG_INFINITY);
            if q.x0 < px1 && px0 < q.x1 {
                return Some(i);
            }
            first.get_or_insert(i);
        }
        first
    };
    let mut out: Vec<usize> = Vec::new();
    for q in lines {
        if let Some(i) = pick(q, 0.0).or_else(|| pick(q, 3.0)) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    // A selection dragged down a page can catch blank lines and the page
    // number. Drop blank paragraphs, and number-only ones when the highlight
    // also reaches real text.
    let text = |i: usize| -> String { paragraphs[i].iter().map(|l| l.text.as_str()).collect() };
    out.retain(|&i| !text(i).chars().all(py_isspace));
    if out.iter().any(|&i| !is_page_number(&text(i))) {
        out.retain(|&i| !is_page_number(&text(i)));
    }
    if out.is_empty() {
        out.extend(find_paragraph_index(paragraphs, rect));
    }
    out
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

        let right_margin = py_max(prev_last.iter().map(|l| l.x1)).unwrap();
        let last = &prev_last[prev_last.len() - 1];
        let prev_reaches_margin = last.x1 >= right_margin - MARGIN_TOLERANCE;
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
    fn find_paragraph_by_centre_then_nearest() {
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
        assert_eq!(find_paragraph_index(&paras, &r(170.0, 180.0)), Some(1));
        assert_eq!(find_paragraph_index(&[], &r(1.0, 2.0)), None);
    }

    #[test]
    fn multi_paragraph_highlight_covers_each_paragraph() {
        // Highlighted lines as boxes across the column, centred on `y`.
        let at = |ys: &[f64]| -> Vec<Rect> {
            ys.iter()
                .map(|&y| Rect {
                    x0: 72.0,
                    y0: y - 5.0,
                    x1: 300.0,
                    y1: y + 5.0,
                })
                .collect()
        };
        let paras = vec![
            vec![
                line(72.0, 100.0, 540.0, 112.0, "a text"),
                line(72.0, 114.0, 540.0, 126.0, "b text"),
            ],
            vec![line(72.0, 140.0, 540.0, 152.0, "c text")],
            vec![line(72.0, 200.0, 540.0, 212.0, "d text")],
        ];
        // Whole-page highlight: its rectangle's centre (156) sits in a gap.
        let big = Rect {
            x0: 72.0,
            y0: 100.0,
            x1: 540.0,
            y1: 212.0,
        };
        assert_eq!(
            find_paragraph_indices(&paras, &big, &at(&[106.0, 120.0, 146.0, 206.0])),
            vec![0, 1, 2]
        );
        // Several lines of one paragraph give that paragraph once.
        assert_eq!(
            find_paragraph_indices(&paras, &big, &at(&[106.0, 120.0])),
            vec![0]
        );
        // An exact hit beats a neighbour within the 3pt tolerance.
        let tight = vec![
            vec![line(72.0, 100.0, 540.0, 126.0, "a text")],
            vec![line(72.0, 127.0, 540.0, 139.0, "b text")],
        ];
        assert_eq!(find_paragraph_indices(&tight, &big, &at(&[128.0])), vec![1]);
        // Within tolerance of a paragraph but in no paragraph: still matched.
        assert_eq!(find_paragraph_indices(&paras, &big, &at(&[128.5])), vec![0]);
        // No quads, or only gaps: fall back to the rectangle's centre.
        let small = Rect {
            x0: 80.0,
            y0: 201.0,
            x1: 120.0,
            y1: 211.0,
        };
        assert_eq!(find_paragraph_indices(&paras, &small, &[]), vec![2]);
        assert_eq!(
            find_paragraph_indices(&paras, &small, &at(&[175.0])),
            vec![2]
        );
    }

    #[test]
    fn highlighted_line_matches_its_own_column() {
        // A sidebar (x 40-160) beside a main-column paragraph (x 200-540),
        // both spanning the same heights.
        let paras = vec![
            vec![
                line(200.0, 100.0, 540.0, 112.0, "main one"),
                line(200.0, 114.0, 540.0, 126.0, "main two"),
            ],
            vec![
                line(40.0, 100.0, 160.0, 112.0, "side one"),
                line(40.0, 114.0, 160.0, 126.0, "side two"),
            ],
        ];
        let r = |x0, x1| Rect {
            x0,
            y0: 114.0,
            x1,
            y1: 126.0,
        };
        assert_eq!(
            find_paragraph_indices(&paras, &r(40.0, 120.0), &[r(40.0, 120.0)]),
            vec![1]
        );
        assert_eq!(
            find_paragraph_indices(&paras, &r(220.0, 400.0), &[r(220.0, 400.0)]),
            vec![0]
        );
        // Blank lines and a page number caught by a long selection are
        // dropped; a highlight on the page number alone still keeps it.
        let page = vec![
            vec![line(72.0, 100.0, 540.0, 112.0, "body text")],
            vec![line(72.0, 120.0, 540.0, 132.0, " ")],
            vec![line(300.0, 700.0, 312.0, 712.0, "26")],
        ];
        let at = |y: f64| Rect {
            x0: 72.0,
            y0: y - 5.0,
            x1: 540.0,
            y1: y + 5.0,
        };
        assert_eq!(
            find_paragraph_indices(&page, &at(400.0), &[at(106.0), at(126.0), at(706.0)]),
            vec![0]
        );
        assert_eq!(
            find_paragraph_indices(&page, &at(706.0), &[at(706.0)]),
            vec![2]
        );
        // With no horizontal overlap anywhere, the first vertical match wins.
        assert_eq!(
            find_paragraph_indices(&paras, &r(560.0, 600.0), &[r(560.0, 600.0)]),
            vec![0]
        );
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
