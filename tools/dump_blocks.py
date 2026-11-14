#!/usr/bin/env python3
"""Dump each page's text blocks from PyMuPDF's get_text("dict") as JSON.

Mirrors examples/dump_blocks.rs so the two engines can be compared.

Usage: python3 tools/dump_blocks.py <input.pdf>
"""
import json
import sys

import fitz  # pymupdf


def main():
    # PyMuPDF prints MuPDF repair messages to stdout, which would corrupt
    # the JSON for damaged files.
    fitz.TOOLS.mupdf_display_errors(False)
    fitz.TOOLS.mupdf_display_warnings(False)
    doc = fitz.open(sys.argv[1])
    pages = []
    for page in doc:
        blocks = []
        for block in page.get_text("dict").get("blocks", []):
            if block.get("type", 0) != 0:
                continue
            lines = []
            for line in block.get("lines", []):
                x0, y0, x1, y1 = line["bbox"]
                text = "".join(s.get("text", "") for s in line.get("spans", []))
                lines.append({"x0": x0, "y0": y0, "x1": x1, "y1": y1, "text": text})
            blocks.append({"bbox": list(block["bbox"]), "lines": lines})
        pages.append(blocks)
    json.dump(pages, sys.stdout)


if __name__ == "__main__":
    main()
