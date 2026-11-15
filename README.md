# Parabloom

Parabloom expands partial PDF highlights so each one covers the whole paragraph it sits in. Highlight a few words while you read, run Parabloom, and every marked paragraph comes back fully highlighted, ready for annotation extraction.

It is a single binary with no runtime dependencies, based on the `expand_highlights.py` script (PyMuPDF), with additional safeguards for annotations and file writes.

## Usage

```bash
parabloom input.pdf output.pdf
```

Pass the same path twice to overwrite in place. Parabloom then saves incrementally, appending to the file, and falls back to a full rewrite when the PDF can't take an incremental save. The fallback stages the PDF in an exclusively created private directory, preserves the input's file permissions, and replaces the original only after writing successfully.

| Option | Default | Effect |
|---|---|---|
| `--indent-x N` | 80 | A single-line block whose left edge passes this X starts a new paragraph. Lower it for small-page books (45 suits 432×648 pt ebooks). |
| `--gap-threshold N` | 12 | A vertical gap larger than this (points) between lines starts a new paragraph. |
| `--footnote-y N` | 720 | Lines starting below this Y count as footnotes and are ignored. |
| `--no-blue-on-image` | off | Keep original colours on pages with images. By default those highlights turn blue (0.2, 0.4, 0.8). |
| `--debug` | off | Print page continuations and each expanded paragraph. |

Each expanded highlight keeps the original's colour (unless image-page recoloring applies) and uses 0.4 opacity with Multiply blending. Highlights covering the same paragraph collapse to one. Original notes, authors, dates and reply threads are preserved; note text from merged highlights becomes a separate text reply with its author and dates, while merged highlights without note text add no reply. Highlights with any selected region that cannot be matched to text remain unchanged, including highlights on scanned pages and excluded footnotes.

## How it works

PDFs store positioned characters, not paragraphs. Parabloom rebuilds paragraphs from layout:

1. **Extract.** MuPDF segments each page into text blocks and lines. Parabloom rebuilds each line box from its character boxes, as PyMuPDF does, so the numbers match.
2. **Group.** Blocks containing disjoint columns are split before paragraph grouping. Single-line blocks merge by indent and vertical gap. A block that looks like a split of the open paragraph (normal line gap, flush left or centred with it, sentence still running) merges into it.
3. **Bridge pages.** A paragraph continues onto the next page when that page starts flush left and the previous page's last body paragraph ended mid-sentence, at the page's body-text right margin, or the next line starts lowercase. Footnotes, page numbers and blank lines at the bottom of a page don't count as its last paragraph. A lone centred or all-caps last line counts as an attribution, and a heading at the top of the next page (a short lone line such as "Appendix 3:", or a title set larger than the body text) blocks the bridge too.
4. **Replace.** Every selected quad is matched against text-line boxes in both axes. All intersected paragraphs are expanded, with one highlight per paragraph segment. Existing annotations are reused where possible, and merged annotations retain their notes and reply relationships. Blank paragraphs, and a page number swept up by a long selection, are left out.

## Parity with the Python script

Parabloom ports the 2026-08-03 patched version of `expand_highlights.py`, which adds centred pull-quote joining and the attribution guard. `tools/` holds the harness used to check it:

- `dump_blocks.py` / `examples/dump_blocks.rs`: text blocks from each engine; `compare_blocks.py` diffs them.
- `golden.py layout` / `examples/layout_from_blocks.rs`: run both paragraph implementations on identical block input.
- `parity.py`: run both tools end to end on copies of real PDFs and compare `/QuadPoints`, `/Rect`, `/C`, `/CA`, `/BM` and appearance streams.

Historical results for the initial port, before the safety and layout corrections below, on a private library of 381 highlighted PDFs (books, articles, slide decks; 20,571 highlights after expansion):

| Check | Result |
|---|---|
| Text extraction, 30 PDFs / 4,670 pages | Identical boxes and text on 4,648 pages (99.5%) |
| Paragraph logic on identical input, 30 PDFs | Identical: all ~38,000 paragraphs and page continuations |
| End to end, 381 PDFs | 348 identical; 33 differ on 91 pages in total |
| Time, 381 PDFs | Python 594 s, Parabloom 128 s |

On each of the 91 differing pages, MuPDF 1.27 extracted different blocks than 1.26 on that page or a neighbour. At that time, no difference traced to the port itself. These measurements have not been rerun for the current implementation. The private corpus is not distributed; the reference script is archived in [`archive/expand_highlights/`](archive/expand_highlights/).

### Known differences

- **Safety and matching.** Unmatched selections and notes are retained, selection matching considers columns and every quad, and page continuations use the page body margin. These intentional corrections can differ from the Python reference. Both comparison tools exit nonzero on differences or failures; block comparison includes block bounds and missing pages.
- **MuPDF version.** The `mupdf` crate bundles MuPDF 1.27.2; PyMuPDF 1.26.5 ships 1.26.10. On a small share of pages 1.27 groups lines into blocks differently, which can change how far a highlight reaches.
- **Appearance stream bytes.** Both draw the same paths in the same colour and graphics state. PyMuPDF wraps its stream in an extra `q … Q` and repeats `/H gs`.
- **`/NM` names.** Existing annotation names are retained. New annotations use names such as `parabloom-A0`.
- **Page bridges.** The Python script treats whatever sits last on a page as the paragraph that may continue: often a page number or a footnote. Highlights then bridge onto page numbers, footnotes and the next page's headings, while real continuations in books with page numbers at the bottom never bridge. Parabloom skips footnotes, page numbers and blank lines when finding a page's last paragraph, and refuses to bridge onto a heading.
- **Rotated pages** follow PyMuPDF's approach (work with `/Rotate` set to 0, then restore it) and the extraction example uses the same path. Generated regression fixtures cover rotation; the historical corpus did not contain highlighted rotated pages.

## Installing

```bash
cargo install parabloom --locked
```

This builds from source, so it needs the same toolchain as [Building](#building). Each [GitHub release](https://github.com/jag3773/parabloom/releases) also has prebuilt binaries for Linux and macOS (x86_64 and arm64), a source archive with all dependencies vendored, and SHA-256 checksums. `RELEASING.md` describes what each release contains and how one is made. To build it yourself, see below.

## Building

```bash
cargo build --release
```

The first build compiles MuPDF from vendored source and needs a C toolchain and `libclang` (Xcode Command Line Tools on macOS). The binary lands at `target/release/parabloom`.

```bash
cargo test --all-targets --locked
python3 -m unittest discover -s tools/tests -v
```

## License

Parabloom's source code is MIT licensed (see `LICENSE`).

The compiled binary statically links [MuPDF](https://mupdf.com) through the [`mupdf`](https://crates.io/crates/mupdf) crate. Both are AGPL-3.0, so a distributed Parabloom binary is subject to the AGPL: whoever distributes it must also offer the corresponding source. Artifex sells commercial MuPDF licences for cases where that doesn't fit.
