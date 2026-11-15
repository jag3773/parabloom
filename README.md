# Parabloom

Parabloom expands partial PDF highlights so each one covers the whole paragraph it sits in. Highlight a few words while you read, run Parabloom, and every marked paragraph comes back fully highlighted, ready for annotation extraction.

It is a single binary with no runtime dependencies, a Rust port of the `expand_highlights.py` script (PyMuPDF). It produces the same highlights, apart from the deliberate differences listed below.

## Usage

```bash
parabloom input.pdf output.pdf
```

Pass the same path twice to overwrite in place. Parabloom then saves incrementally, appending to the file, and falls back to a full rewrite when the PDF can't take an incremental save.

| Option | Default | Effect |
|---|---|---|
| `--indent-x N` | 80 | A single-line block whose left edge passes this X starts a new paragraph. Lower it for small-page books (45 suits 432×648 pt ebooks). |
| `--gap-threshold N` | 12 | A vertical gap larger than this (points) between lines starts a new paragraph. |
| `--footnote-y N` | 720 | Lines starting below this Y count as footnotes and are ignored. |
| `--no-blue-on-image` | off | Keep original colours on pages with images. By default those highlights turn blue (0.2, 0.4, 0.8). |
| `--debug` | off | Print page continuations and each expanded paragraph. |

Each expanded highlight keeps the original's colour and uses 0.4 opacity with Multiply blending. A highlight that spans several paragraphs grows to cover all of them, still as one highlight. A paragraph already covered by an earlier highlight is not covered twice.

## How it works

PDFs store positioned characters, not paragraphs. Parabloom rebuilds paragraphs from layout:

1. **Extract.** MuPDF segments each page into text blocks and lines. Parabloom rebuilds each line box from its character boxes, as PyMuPDF does, so the numbers match.
2. **Group.** Multi-line blocks stay as MuPDF segmented them. Single-line blocks merge by indent and vertical gap. A block that looks like a split of the open paragraph (normal line gap, flush left or centred with it, sentence still running) merges into it.
3. **Bridge pages.** A paragraph continues onto the next page when that page starts flush left and the previous page ended mid-sentence, at the right margin, or the next line starts lowercase. A lone centred or all-caps last line counts as an attribution and blocks the bridge.
4. **Replace.** Each line of an original highlight is matched to the paragraph that contains it, preferring a paragraph in the same column. The original is deleted with any reply annotations and replaced by one highlight per page, covering every paragraph it touched. Blank paragraphs, and a page number swept up by a long selection, are left out.

## Parity with the Python script

Parabloom ports the 2026-08-03 patched version of `expand_highlights.py`, which adds centred pull-quote joining and the attribution guard. `tools/` holds the harness used to check it:

- `dump_blocks.py` / `examples/dump_blocks.rs`: text blocks from each engine; `compare_blocks.py` diffs them.
- `golden.py layout` / `examples/layout_from_blocks.rs`: run both paragraph implementations on identical block input.
- `parity.py`: run both tools end to end on copies of real PDFs and compare `/QuadPoints`, `/Rect`, `/C`, `/CA`, `/BM` and appearance streams.

Results on a private library of 381 highlighted PDFs (books, articles, slide decks; 20,571 highlights after expansion):

| Check | Result |
|---|---|
| Text extraction, 30 PDFs / 4,670 pages | Identical boxes and text on 4,648 pages (99.5%) |
| Paragraph logic on identical input, 30 PDFs | Identical: all ~38,000 paragraphs and page continuations |
| End to end, 381 PDFs | 348 identical; 33 differ on 91 pages in total |
| Time, 381 PDFs | Python 594 s, Parabloom 128 s |

On each of the 91 differing pages, MuPDF 1.27 extracted different blocks than 1.26 on that page or a neighbour. No difference traced to the port itself.

### Known differences

- **MuPDF version.** The `mupdf` crate bundles MuPDF 1.27.2; PyMuPDF 1.26.5 ships 1.26.10. On a small share of pages 1.27 groups lines into blocks differently, which can change how far a highlight reaches.
- **Appearance stream bytes.** Both draw the same paths in the same colour and graphics state. PyMuPDF wraps its stream in an extra `q … Q` and repeats `/H gs`.
- **`/NM` names** read `parabloom-A0` instead of `fitz-A0`.
- **Highlights across paragraphs.** The Python script matches each highlight to the single paragraph at its vertical centre, so a highlight dragged over several paragraphs shrinks to one of them. Parabloom matches line by line and keeps every paragraph the highlight touched. This is a deliberate change: on the test library it changes about 3,600 pages, nearly all by restoring paragraphs Python dropped.
- **Rotated pages** follow PyMuPDF's approach (work with `/Rotate` set to 0, then restore it) but no rotated page in the test corpus carried a highlight, so this path is untested against real files.

## Building

```bash
cargo build --release
```

The first build compiles MuPDF from vendored source and needs a C toolchain and `libclang` (Xcode Command Line Tools on macOS). The binary lands at `target/release/parabloom`.

```bash
cargo test
```

## License

Parabloom's source code is MIT licensed (see `LICENSE`).

The compiled binary statically links [MuPDF](https://mupdf.com) through the [`mupdf`](https://crates.io/crates/mupdf) crate. Both are AGPL-3.0, so a distributed Parabloom binary is subject to the AGPL: whoever distributes it must also offer the corresponding source. Artifex sells commercial MuPDF licences for cases where that doesn't fit.
