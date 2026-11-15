# expand_highlights.py (historical)

The Python scripts Parabloom was ported from, kept as a historical reference. They are not maintained, the build doesn't use them, and they are left out of the published crate. To expand highlights, use `parabloom`.

| File | Version |
|---|---|
| `expand_highlights.py` | 2026-07-11, the script in use before the port |
| `expand_highlights_patched.py` | 2026-08-03 patch adding centred pull-quote joining and the attribution guard. Parabloom's initial port matched this version. |

Both are byte-for-byte copies of the originals. Running them needs PyMuPDF (`pip install pymupdf`), which is AGPL-3.0.

The parity tools take the patched script as their reference:

```bash
python3 tools/parity.py --ref archive/expand_highlights/expand_highlights_patched.py --bin target/release/parabloom file.pdf
python3 tools/golden.py layout blocks.json archive/expand_highlights/expand_highlights_patched.py
```

Parabloom now deliberately differs from these scripts: it keeps every paragraph a highlight touches, preserves notes, and keeps page bridges off footnotes, page numbers and headings. The main README's "Known differences" section has the full list.
