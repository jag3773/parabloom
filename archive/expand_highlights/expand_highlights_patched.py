#!/usr/bin/env python3
"""
Expand partial PDF highlights to cover the full paragraph they belong to.

Usage:
    python expand_highlights.py <input.pdf> <output.pdf> [options]

Options:
    --indent-x N       X-coordinate threshold for detecting indented (new paragraph)
                       lines. Default: 80
    --gap-threshold N   Vertical gap (in points) between lines that signals a paragraph
                       break. Default: 12
    --footnote-y N      Y-coordinate below which lines are excluded (footnote zone).
                       Default: 720
    --blue-on-image     Change highlight color to blue on pages that contain images.
                       This is ON by default. Use --no-blue-on-image to disable.
    --no-blue-on-image  Keep original highlight colors even on pages with images.
    --debug             Print paragraph groupings for each page with highlights.

Requires: pymupdf (pip install pymupdf)
"""

import argparse
import os
import sys

try:
    import fitz  # pymupdf
except ImportError:
    print("Error: pymupdf is required. Install with: pip install pymupdf --break-system-packages")
    sys.exit(1)


# Default blue color for highlights on pages with images
BLUE = [0.2, 0.4, 0.8]


def page_has_images(page):
    """Check whether a PDF page contains any embedded images."""
    image_list = page.get_images(full=True)
    return len(image_list) > 0


def group_lines_into_paragraphs(page, indent_x=80, gap_threshold=12, footnote_y=720):
    """
    Build a list of paragraphs from a PDF page. Each paragraph is a list of
    per-line tuples: [ [(x0, y0, x1, y1, text), ...], ... ]

    We rely on PyMuPDF's get_text("dict") output:
      - A "block" in dict mode is usually already a coherent paragraph
        (PyMuPDF merges adjacent lines with similar styling/spacing).
      - Each block exposes its constituent line bboxes and text spans, which
        we use for per-line highlight quads and for cross-page continuation
        detection.

    Grouping rules:
      - A multi-line block is treated as a paragraph as-is (no merging
        with neighbors). This is critical: get_text("blocks") on some PDFs
        returns whole paragraphs as a single block, and consecutive paragraphs
        often sit only 1-2pt apart vertically — merging them by gap would
        produce a giant super-paragraph and bleed highlights across multiple
        real paragraphs.
      - Single-line blocks may be merged with adjacent single-line blocks
        using the indent / gap heuristics (this handles PDFs where blocks
        actually come back as individual lines).

    Lines below footnote_y are excluded to keep footnote text from being
    swept into body paragraphs.
    """
    data = page.get_text("dict")

    def line_record(line):
        lx0, ly0, lx1, ly1 = line["bbox"]
        text = "".join(s.get("text", "") for s in line.get("spans", []))
        return (lx0, ly0, lx1, ly1, text)

    # Pull out text blocks with their line records, skip footnote zone
    block_records = []
    for block in data.get("blocks", []):
        if block.get("type", 0) != 0:  # skip image blocks
            continue
        bx0, by0, bx1, by1 = block["bbox"]
        if by0 > footnote_y:
            continue
        line_recs = []
        for line in block.get("lines", []):
            ly0 = line["bbox"][1]
            if ly0 > footnote_y:
                continue
            line_recs.append(line_record(line))
        if not line_recs:
            continue
        block_records.append({"bbox": (bx0, by0, bx1, by1), "lines": line_recs})

    # Sort by vertical position
    block_records.sort(key=lambda b: b["bbox"][1])

    # Page-level layout stats used to detect paragraphs that PyMuPDF split
    # across several consecutive multi-line blocks (common in ebook / web-export
    # PDFs, e.g. Substack): the vertical gap between such sibling blocks matches
    # the intra-block line gap, while a real paragraph break is clearly larger.
    intra_gaps = []
    for blk in block_records:
        ls = blk["lines"]
        for i in range(len(ls) - 1):
            g = ls[i + 1][1] - ls[i][3]
            if g >= 0:
                intra_gaps.append(g)
    typical_line_gap = sorted(intra_gaps)[len(intra_gaps) // 2] if intra_gaps else None
    left_margin = min((ln[0] for blk in block_records for ln in blk["lines"]), default=0)
    right_margin = max((ln[2] for blk in block_records for ln in blk["lines"]), default=0)

    # --- Centered-text continuation (pull quotes, epigraph pages) ---
    # Centered lines never sit at a common left margin, so the not_indented
    # test in continues_paragraph() wrongly refuses to join them into one
    # paragraph — leaving a pull quote only partly highlighted. Two lines are
    # allowed to join on centering evidence when both are horizontally
    # centered on the text column AND neither sits at the page's body-text
    # left margin (the modal x0). On a page that has no dominant left margin
    # at all (a pure epigraph page) the margin test is skipped.
    from collections import Counter as _Counter
    _x0s = [round(ln[0], 1) for blk in block_records for ln in blk["lines"]]
    _modal_x0, _modal_n = _Counter(_x0s).most_common(1)[0] if _x0s else (None, 0)
    _page_has_body_margin = bool(_x0s) and (_modal_n / len(_x0s)) >= 0.5
    page_center = (left_margin + right_margin) / 2

    def _centered(line):
        if abs((line[0] + line[2]) / 2 - page_center) > 12:
            return False
        if _page_has_body_margin and abs(line[0] - _modal_x0) <= 3:
            return False  # sits at the body-text left margin: justified, not centered
        return True

    def continues_paragraph(prev_line, first_line):
        """Decide whether a block starting at first_line continues the paragraph
        that currently ends at prev_line (block-split detection). Requires ALL of:
        the inter-block gap looks like a normal line gap, the new block is not
        indented, and the text signals a sentence in progress (no terminal
        punctuation on the previous line, lowercase start, or the previous line
        reaches the right margin)."""
        if typical_line_gap is None:
            return False
        gap = first_line[1] - prev_line[3]
        gap_similar = 0 <= gap <= min(typical_line_gap + 2.5, gap_threshold)
        not_indented = first_line[0] <= left_margin + 3
        if not not_indented and _centered(prev_line) and _centered(first_line):
            not_indented = True
        prev_text = prev_line[4] if len(prev_line) > 4 else ""
        first_text = first_line[4] if len(first_line) > 4 else ""
        text_continues = (
            not _line_ends_with_sentence_punct(prev_text)
            or _line_starts_with_lowercase(first_text)
            or prev_line[2] >= right_margin - 5
        )
        return gap_similar and not_indented and text_continues

    paragraphs = []
    current_lines = []  # accumulating lines of the paragraph being built

    def flush():
        nonlocal current_lines
        if current_lines:
            paragraphs.append(current_lines)
            current_lines = []

    for blk in block_records:
        is_multiline = len(blk["lines"]) > 1
        first_line = blk["lines"][0]

        # Block-split continuation: merge this block into the current paragraph
        # regardless of whether it is single- or multi-line.
        if current_lines and continues_paragraph(current_lines[-1], first_line):
            current_lines.extend(blk["lines"])
            continue

        if is_multiline:
            # PyMuPDF already grouped this paragraph — start a new paragraph
            # from it (kept open so a following split block can still merge).
            flush()
            current_lines = list(blk["lines"])
            continue

        # Single-line block: decide whether it starts a new paragraph
        lx0, ly0, lx1, ly1, _ltext = first_line
        is_new_para = False

        if not current_lines:
            is_new_para = True
        else:
            prev_x0, prev_y0, prev_x1, prev_y1, _ = current_lines[-1]
            gap = ly0 - prev_y1
            if lx0 > indent_x:
                is_new_para = True
            elif gap > gap_threshold:
                is_new_para = True
            elif prev_x1 < 400 and gap > 3:
                is_new_para = True

        if is_new_para:
            flush()
        current_lines.append(first_line)

    flush()
    return paragraphs


def find_paragraph_index(paragraphs, highlight_rect):
    """Return the index of the paragraph containing the highlight, or None."""
    hy_center = (highlight_rect.y0 + highlight_rect.y1) / 2

    for idx, para in enumerate(paragraphs):
        para_y0 = para[0][1]
        para_y1 = para[-1][3]
        if para_y0 - 3 <= hy_center <= para_y1 + 3:
            return idx

    # Fallback: closest paragraph by vertical center distance
    best_idx = None
    best_dist = float("inf")
    for idx, para in enumerate(paragraphs):
        mid = (para[0][1] + para[-1][3]) / 2
        dist = abs(hy_center - mid)
        if dist < best_dist:
            best_dist = dist
            best_idx = idx

    return best_idx


# Characters that, when found at the end of a line (after stripping trailing
# closing quotes/brackets/whitespace), indicate the sentence — and almost always
# the paragraph — has ended on that line.
_SENTENCE_END_PUNCT = {".", "!", "?", "…"}
# Trailing characters that can follow sentence-final punctuation (closing
# quotes/brackets); we strip these before checking the final char.
_TRAILING_CLOSERS = set('"”’»)]}')  # " ” ’ » ) ] }


def _line_ends_with_sentence_punct(text):
    """True if the line's last non-trivial char is sentence-final punctuation."""
    if not text:
        return False
    stripped = text.rstrip()
    # Strip trailing closing quote / bracket chars
    while stripped and stripped[-1] in _TRAILING_CLOSERS:
        stripped = stripped[:-1].rstrip()
    if not stripped:
        return False
    return stripped[-1] in _SENTENCE_END_PUNCT


def _line_starts_with_lowercase(text):
    """True if the line's first non-whitespace char is a lowercase letter."""
    if not text:
        return False
    stripped = text.lstrip()
    if not stripped:
        return False
    first = stripped[0]
    return first.isalpha() and first.islower()


def detect_page_continuations(page_paragraphs, margin_tolerance=5):
    """
    For each page, decide whether its first paragraph is a continuation of the
    last paragraph on the previous page (i.e., the paragraph spans the page break).

    Returns a list of booleans: continues_from_prev[p] == True means page p's
    first paragraph is a continuation from page p-1.

    Detection requires the *safety guard* that the current page's first line
    is NOT indented (sits at the page's left margin). Without that, we never
    merge — an indented first line is a fresh paragraph by typesetting
    convention.

    When the safety guard holds, the paragraph is considered to continue if
    ANY of these signals fires:
      1. The previous page's last line reaches the right margin (paragraph
         clearly wrapped past the page break).
      2. The previous page's last line does NOT end with sentence-final
         punctuation (paragraph wasn't terminated).
      3. The current page's first line starts with a lowercase letter
         (continuation of a sentence in progress).

    On indented-paragraph books, this combination catches normal text-wrap
    page breaks (where the last line ends a few words short of the margin)
    without producing false positives — a genuine new paragraph would either
    be indented (blocked by the guard) or, if not indented, would still start
    with an uppercase letter and follow sentence-final punctuation.
    """
    n_pages = len(page_paragraphs)
    continues_from_prev = [False] * n_pages

    for p in range(1, n_pages):
        prev_paras = page_paragraphs[p - 1]
        curr_paras = page_paragraphs[p]
        if not prev_paras or not curr_paras:
            continue

        prev_last_para = prev_paras[-1]
        curr_first_para = curr_paras[0]

        # Layout: left margin proxy from all lines on the current page
        left_margin = min(line[0] for para in curr_paras for line in para)
        first_line_x0 = curr_first_para[0][0]
        curr_not_indented = first_line_x0 <= left_margin + margin_tolerance

        if not curr_not_indented:
            continue  # safety guard: indented first line = fresh paragraph

        # Safety guard 2: a page that ends on a lone centered line is ending on
        # an epigraph citation or attribution ("EPHESIANS 4:17-24", "SANGEET
        # PAUL CHOUDARY"), not on a wrapped sentence. Such a line has no
        # terminal punctuation, which would otherwise fake a continuation into
        # the next chapter's opening paragraph.
        if len(prev_last_para) == 1:
            pl = prev_last_para[0]
            prev_left = min(line[0] for para in prev_paras for line in para)
            prev_right = max(line[2] for para in prev_paras for line in para)
            prev_center = (prev_left + prev_right) / 2
            is_centered = abs((pl[0] + pl[2]) / 2 - prev_center) <= 12
            is_narrow = (pl[2] - pl[0]) < 0.85 * (prev_right - prev_left)
            # An all-caps lone line is an attribution by this book's typographic
            # convention, regardless of how wide the sparse page makes it look.
            ptext = pl[4] if len(pl) > 4 else ""
            letters = [c for c in ptext if c.isalpha()]
            is_allcaps = bool(letters) and sum(c.isupper() for c in letters) / len(letters) > 0.85
            if is_centered and (is_narrow or is_allcaps):
                continue

        # Layout: right margin proxy from prev page's last paragraph lines
        right_margin = max(line[2] for line in prev_last_para)
        last_line_x1 = prev_last_para[-1][2]
        prev_reaches_margin = last_line_x1 >= right_margin - margin_tolerance

        # Text signals
        prev_last_text = prev_last_para[-1][4] if len(prev_last_para[-1]) > 4 else ""
        curr_first_text = curr_first_para[0][4] if len(curr_first_para[0]) > 4 else ""
        prev_lacks_terminal_punct = not _line_ends_with_sentence_punct(prev_last_text)
        curr_starts_lowercase = _line_starts_with_lowercase(curr_first_text)

        if prev_reaches_margin or prev_lacks_terminal_punct or curr_starts_lowercase:
            continues_from_prev[p] = True

    return continues_from_prev


def _build_quads(para):
    """Convert a paragraph (list of per-line records) to PyMuPDF Quads.
    Each line record is (x0, y0, x1, y1[, text]); only the bbox is used here.
    """
    return [
        fitz.Quad(
            fitz.Point(line[0], line[1]),
            fitz.Point(line[2], line[1]),
            fitz.Point(line[0], line[3]),
            fitz.Point(line[2], line[3]),
        )
        for line in para
    ]


def expand_highlights(
    input_path,
    output_path,
    indent_x=80,
    gap_threshold=12,
    footnote_y=720,
    blue_on_image=True,
    debug=False,
):
    """
    Main entry point. Opens the PDF, expands every highlight annotation to its
    full paragraph, optionally recolors to blue on pages with images, and saves.

    Returns a dict with stats: {pages_modified, highlights_expanded, images_detected}.
    """
    doc = fitz.open(input_path)
    n_pages = len(doc)

    stats = {"pages_modified": 0, "highlights_expanded": 0, "highlights_blued": 0}

    # --- Phase 1: build paragraphs for every page ---
    page_paragraphs = [
        group_lines_into_paragraphs(
            doc[p], indent_x=indent_x, gap_threshold=gap_threshold, footnote_y=footnote_y
        )
        for p in range(n_pages)
    ]

    # --- Phase 2: detect cross-page paragraph continuations ---
    continues_from_prev = detect_page_continuations(page_paragraphs)

    if debug:
        cont_pages = [p + 1 for p, c in enumerate(continues_from_prev) if c]
        print(f"Pages whose first paragraph continues from the previous page: {cont_pages}")

    # --- Phase 3: collect work plan AND delete original annotations in one pass ---
    # work[pno] is a list of {"para": [...], "color": [r,g,b]}
    # Annot refs go stale across page boundaries in PyMuPDF, so we must delete on
    # the same page where we read them — within a single iteration of the loop.
    from collections import defaultdict
    work = defaultdict(list)

    def walk_segments(pno, pidx):
        """Yield (page, para_idx) tuples for the paragraph at (pno, pidx) plus
        any cross-page continuations to either side."""
        yield (pno, pidx)

        # Walk left: backward through cross-page continuations
        cur_p, cur_i = pno, pidx
        while cur_i == 0 and cur_p > 0 and continues_from_prev[cur_p]:
            prev_p = cur_p - 1
            if not page_paragraphs[prev_p]:
                break
            prev_i = len(page_paragraphs[prev_p]) - 1
            yield (prev_p, prev_i)
            cur_p, cur_i = prev_p, prev_i

        # Walk right: forward through cross-page continuations
        cur_p, cur_i = pno, pidx
        while (
            page_paragraphs[cur_p]
            and cur_i == len(page_paragraphs[cur_p]) - 1
            and cur_p + 1 < n_pages
            and continues_from_prev[cur_p + 1]
        ):
            next_p = cur_p + 1
            if not page_paragraphs[next_p]:
                break
            yield (next_p, 0)
            cur_p, cur_i = next_p, 0

    for pno in range(n_pages):
        page = doc[pno]

        # Snapshot the highlight info as plain Python values before deleting
        highlight_snapshots = []
        for annot in page.annots() or []:
            if annot.type[0] != 8:
                continue
            highlight_snapshots.append(
                {
                    "rect": fitz.Rect(annot.rect),  # copy, not ref
                    "color": list(annot.colors.get("stroke", [1.0, 1.0, 0.0])),
                }
            )

        # Delete the original highlights on this page now (refs are still valid here)
        to_delete = [a for a in (page.annots() or []) if a.type[0] == 8]
        for a in to_delete:
            page.delete_annot(a)

        paragraphs = page_paragraphs[pno]
        if not paragraphs:
            continue

        for h in highlight_snapshots:
            pidx = find_paragraph_index(paragraphs, h["rect"])
            if pidx is None:
                if debug:
                    print(f"  WARNING: No paragraph found for highlight on page {pno + 1} at {h['rect']}")
                continue
            for seg_p, seg_i in walk_segments(pno, pidx):
                work[seg_p].append(
                    {"para": page_paragraphs[seg_p][seg_i], "color": h["color"]}
                )

    # --- Phase 5: add expanded highlights (deduped per paragraph) ---
    for pno in range(n_pages):
        jobs = work.get(pno, [])
        if not jobs:
            continue
        page = doc[pno]
        has_images = page_has_images(page)
        seen = set()
        page_modified = False

        if debug:
            print(f"\n=== Page {pno + 1} ({len(page_paragraphs[pno])} paragraphs, "
                  f"images={'yes' if has_images else 'no'}, jobs={len(jobs)}) ===")

        for job in jobs:
            para = job["para"]
            # Hashable key for dedupe: first and last line bbox tuples
            key = (para[0], para[-1])
            if key in seen:
                continue
            seen.add(key)

            color = BLUE if (blue_on_image and has_images) else job["color"]
            quads = _build_quads(para)
            new_annot = page.add_highlight_annot(quads=quads)
            if new_annot:
                new_annot.set_colors(stroke=color)
                new_annot.set_opacity(0.4)
                new_annot.update()
                stats["highlights_expanded"] += 1
                if blue_on_image and has_images:
                    stats["highlights_blued"] += 1
                page_modified = True
                if debug:
                    print(f"  Expanded: {len(para)} lines covering y={para[0][1]:.1f}-{para[-1][3]:.1f} "
                          f"(color={'blue' if blue_on_image and has_images else 'original'})")

        if page_modified:
            stats["pages_modified"] += 1

    # --- Save ---
    # Prefer incremental save when overwriting in place (smaller diff, preserves
    # original structure). PyMuPDF can't do incremental writes on auto-repaired
    # PDFs, so fall back to a full save when that happens.
    if output_path == input_path:
        try:
            doc.save(output_path, incremental=True, encryption=fitz.PDF_ENCRYPT_KEEP)
        except Exception:
            doc.save(output_path + ".tmp")
            doc.close()
            os.replace(output_path + ".tmp", output_path)
            return stats
    else:
        doc.save(output_path)

    doc.close()
    return stats


def main():
    parser = argparse.ArgumentParser(
        description="Expand partial PDF highlights to cover full paragraphs."
    )
    parser.add_argument("input", help="Input PDF file path")
    parser.add_argument("output", help="Output PDF file path (can be same as input)")
    parser.add_argument("--indent-x", type=float, default=80,
                        help="X threshold for paragraph indentation detection (default: 80)")
    parser.add_argument("--gap-threshold", type=float, default=12,
                        help="Vertical gap threshold for paragraph breaks (default: 12)")
    parser.add_argument("--footnote-y", type=float, default=720,
                        help="Y threshold below which lines are excluded (default: 720)")
    parser.add_argument("--blue-on-image", action="store_true", default=True,
                        help="Change highlight color to blue on pages with images (default: on)")
    parser.add_argument("--no-blue-on-image", action="store_true",
                        help="Keep original highlight colors even on pages with images")
    parser.add_argument("--debug", action="store_true",
                        help="Print paragraph groupings for debugging")
    args = parser.parse_args()

    blue = args.blue_on_image and not args.no_blue_on_image

    stats = expand_highlights(
        input_path=args.input,
        output_path=args.output,
        indent_x=args.indent_x,
        gap_threshold=args.gap_threshold,
        footnote_y=args.footnote_y,
        blue_on_image=blue,
        debug=args.debug,
    )

    print(f"Done. Expanded {stats['highlights_expanded']} highlights "
          f"across {stats['pages_modified']} pages.")
    if stats["highlights_blued"] > 0:
        print(f"  {stats['highlights_blued']} highlights recolored to blue (image pages).")


if __name__ == "__main__":
    main()
