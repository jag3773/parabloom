#!/usr/bin/env python3
"""Compare two block dumps (PyMuPDF vs Parabloom) and summarise differences.

Usage: python3 tools/compare_blocks.py <python.json> <rust.json> [--tol 0.01] [--show N]
"""
import argparse
import json
import math


def coord_diff(a, b):
    """serde_json writes non-finite floats as null; Python writes Infinity.
    Treat null as matching any non-finite value (an empty MuPDF rect)."""
    if a is None or b is None:
        other = b if a is None else a
        return 0.0 if other is None or math.isinf(other) else math.inf
    if math.isinf(a) or math.isinf(b):
        return 0.0 if a == b else math.inf
    return abs(a - b)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("py")
    ap.add_argument("rs")
    ap.add_argument("--tol", type=float, default=0.01)
    ap.add_argument("--show", type=int, default=5)
    a = ap.parse_args()

    py = json.load(open(a.py))
    rs = json.load(open(a.rs))
    stats = dict(pages=0, pages_same=0, block_count_diff=0, line_count_diff=0,
                 bbox_diff=0, text_diff=0, lines=0)
    shown = 0
    if len(py) != len(rs):
        print(f"PAGE COUNT DIFFERS: py={len(py)} rs={len(rs)}")
    for p, (pb, rb) in enumerate(zip(py, rs)):
        stats["pages"] += 1
        same = True
        if len(pb) != len(rb):
            stats["block_count_diff"] += 1
            same = False
            if shown < a.show:
                print(f"p{p+1}: blocks py={len(pb)} rs={len(rb)}")
                shown += 1
        for bi, (b1, b2) in enumerate(zip(pb, rb)):
            if len(b1["lines"]) != len(b2["lines"]):
                stats["line_count_diff"] += 1
                same = False
                if shown < a.show:
                    print(f"p{p+1} b{bi}: lines py={len(b1['lines'])} rs={len(b2['lines'])}")
                    shown += 1
                continue
            for l1, l2 in zip(b1["lines"], b2["lines"]):
                stats["lines"] += 1
                d = max(coord_diff(l1[k], l2[k]) for k in ("x0", "y0", "x1", "y1"))
                if d > a.tol:
                    stats["bbox_diff"] += 1
                    same = False
                    if shown < a.show:
                        print(f"p{p+1} b{bi}: bbox diff {d:.4f}\n  py={[round(l1[k],3) for k in ('x0','y0','x1','y1')]} {l1['text'][:50]!r}\n  rs={[round(l2[k],3) for k in ('x0','y0','x1','y1')]} {l2['text'][:50]!r}")
                        shown += 1
                if l1["text"] != l2["text"]:
                    stats["text_diff"] += 1
                    same = False
                    if shown < a.show:
                        print(f"p{p+1} b{bi}: text\n  py={l1['text']!r}\n  rs={l2['text']!r}")
                        shown += 1
        if same:
            stats["pages_same"] += 1
    print(json.dumps(stats))


if __name__ == "__main__":
    main()
