#!/usr/bin/env python3
"""Golden output from the Python reference, for parity testing.

Two modes:

  layout  <blocks.json> <ref.py>  [--indent-x N ...]
      Feed a block dump (tools/dump_blocks.py) through the reference script's
      paragraph logic and print paragraphs + page continuations as JSON. This
      tests the Rust layout port independently of the PDF engine.

  annots  <input.pdf> <ref.py> [--indent-x N ...]
      Run the reference script end to end on a copy of the PDF and print the
      resulting highlight annotations per page as JSON.

<ref.py> is the reference implementation, normally the 2026-08-03 patched
script at archive/expand_highlights/expand_highlights_patched.py. It is
imported, never modified.
"""
import argparse
import importlib.util
import json
import os
import shutil
import sys
import tempfile


def load_ref(path):
    spec = importlib.util.spec_from_file_location("expand_ref", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class FakePage:
    """Stands in for a PyMuPDF page: get_text("dict") returns the dump."""

    def __init__(self, blocks):
        self._blocks = blocks

    def get_text(self, mode):
        assert mode == "dict"
        out = []
        for b in self._blocks:
            out.append({
                "type": 0,
                "bbox": tuple(b["bbox"]),
                "lines": [
                    {"bbox": (l["x0"], l["y0"], l["x1"], l["y1"]),
                     "spans": [{"text": l["text"]}]}
                    for l in b["lines"]
                ],
            })
        return {"blocks": out}


def as_lines(para):
    return [{"x0": l[0], "y0": l[1], "x1": l[2], "y1": l[3], "text": l[4]} for l in para]


def cmd_layout(a, ref):
    pages = json.load(open(a.blocks))
    page_paras = [
        ref.group_lines_into_paragraphs(
            FakePage(blocks), indent_x=a.indent_x,
            gap_threshold=a.gap_threshold, footnote_y=a.footnote_y)
        for blocks in pages
    ]
    cont = ref.detect_page_continuations(page_paras)
    json.dump({
        "paragraphs": [[as_lines(p) for p in paras] for paras in page_paras],
        "continues": cont,
    }, sys.stdout)


def cmd_annots(a, ref):
    import fitz

    tmp = tempfile.mkdtemp(prefix="parabloom-golden-")
    try:
        src = os.path.join(tmp, "in.pdf")
        out = os.path.join(tmp, "out.pdf")
        shutil.copyfile(a.pdf, src)
        devnull = open(os.devnull, "w")
        old = sys.stdout
        sys.stdout = devnull
        try:
            stats = ref.expand_highlights(
                src, out, indent_x=a.indent_x, gap_threshold=a.gap_threshold,
                footnote_y=a.footnote_y, blue_on_image=not a.no_blue_on_image)
        finally:
            sys.stdout = old
        json.dump({"stats": stats, "pages": read_highlights(fitz, out)}, sys.stdout)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def read_highlights(fitz, path):
    """Highlights per page: quad points (page space), stroke colour, opacity."""
    doc = fitz.open(path)
    pages = []
    for page in doc:
        hs = []
        for annot in page.annots() or []:
            if annot.type[0] != 8:
                continue
            hs.append({
                "quads": [[q.ul.x, q.ul.y, q.lr.x, q.lr.y] for q in annot.vertices_quads()]
                if hasattr(annot, "vertices_quads") else quads_of(fitz, annot),
                "color": list(annot.colors.get("stroke") or []),
                "opacity": annot.opacity,
            })
        pages.append(hs)
    doc.close()
    return pages


def quads_of(fitz, annot):
    v = annot.vertices or []
    out = []
    for i in range(0, len(v) - 3, 4):
        ul, ur, ll, lr = (fitz.Point(p) for p in v[i:i + 4])
        out.append([ul.x, ul.y, lr.x, lr.y])
    return out


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("layout", "annots"):
        p = sub.add_parser(name)
        p.add_argument("blocks" if name == "layout" else "pdf")
        p.add_argument("ref")
        p.add_argument("--indent-x", type=float, default=80)
        p.add_argument("--gap-threshold", type=float, default=12)
        p.add_argument("--footnote-y", type=float, default=720)
        p.add_argument("--no-blue-on-image", action="store_true")
    a = ap.parse_args()
    ref = load_ref(a.ref)
    {"layout": cmd_layout, "annots": cmd_annots}[a.cmd](a, ref)


if __name__ == "__main__":
    main()
