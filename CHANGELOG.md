# Changelog

All notable changes to Parabloom are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). See `RELEASING.md` for how a
release is cut.

## [Unreleased]

### Fixed

- A one-line text block that sits flush with the page's left margin, or with its body-text margin, no longer counts as an indented paragraph start just because that margin lies past `--indent-x`. PDFs that put every line in its own block with a margin past 80 pt (such as browser print-to-PDF pages) used to get one highlight per line instead of one per paragraph. Where the page's blocks show its normal line spacing, a larger gap still starts a new paragraph, so a heading set at the body margin stays separate.
- Body text that runs below `--footnote-y` is no longer cut off as a footnote. A line below the cut-off is kept when it continues the lines above it: same column, same left edge, same line height and the same line spacing, set at the page's body size. Notes in smaller type, or after a larger gap, are still dropped. Before, a paragraph whose last line fell below the cut-off lost that line, and could then look unfinished and bridge onto the next page.
- A one-line block that starts at the same left edge as the line before it, while that line's sentence is still running, no longer counts as an indented paragraph start. Indented quotes and columns set one line per block now expand as whole paragraphs instead of one highlight per line.
- A line whose box is stretched by a superscript or a taller glyph (a footnote reference such as "[5]") no longer splits its paragraph by pushing the measured line gap past `--gap-threshold`. While the sentence is still running, a height difference of up to a quarter of a line is discounted from the gap; a heading after a finished sentence, or in much larger type, still starts its own paragraph.
- Running heads and footers are recognised when the same text (ignoring page numbers) sits at the same height on a page up to two pages away. A running head opening a page blocks a page bridge, as a heading does, so a bridge can no longer highlight it (such as the title and URL a browser prints at the top of each page). A running footer, such as a browser's "7 of 16" and date, is no longer taken as the page's last paragraph.

## [0.2.0] - 2026-11-15

This release changes which text a highlight grows into across page breaks, so some highlights come out differently than in 0.1.0.

### Changed

- Page bridges now start from a page's last body paragraph. Footnotes, page numbers and blank lines at the bottom of a page are skipped, so a highlight no longer bridges onto them, and a paragraph that really does continue onto the next page now bridges even when a page number sits below it.
- A highlight no longer bridges onto a heading at the top of the next page: a short lone line such as "Appendix 3:", or a title set larger than the body text.
- Blank paragraphs, and a page number swept up by a long selection, are left out of the expanded highlight. A highlight on a page number alone still keeps it. Page numbers are recognised by position as well as content, so a year or a word such as "civil" inside body text is kept.
- CI and release workflows use `actions/checkout@v5`, which runs on Node.js 24 instead of the deprecated Node.js 20.

## [0.1.0] - 2026-11-15

### Added

- `parabloom` command that expands partial PDF highlights to cover each whole paragraph, based on the `expand_highlights.py` reference script.
- In-place updates when the input and output paths match, using an incremental save with a staged, permission-preserving full-rewrite fallback.
- Preservation of annotation notes, authors, dates and reply threads when highlights are expanded or merged.
- Column-aware paragraph grouping, matching of every selected quad, and rotation-safe text extraction.
- Prebuilt binaries for Linux and macOS (x86_64 and arm64), a vendored source archive and SHA-256 checksums on each GitHub release, and publishing to crates.io.

[Unreleased]: https://github.com/jag3773/parabloom/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jag3773/parabloom/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/jag3773/parabloom/releases/tag/v0.1.0
