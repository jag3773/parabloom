#!/usr/bin/env python3
"""End-to-end parity: run the Python reference and Parabloom on copies of the
same PDFs and compare the highlight annotations they write.

Per highlight it compares /QuadPoints, /Rect, /C, /CA, /BM and the decoded
appearance stream. Inputs are only read; all writes go to a temp directory.

Usage:
  python3 tools/parity.py --ref <expand_highlights.py> --bin <parabloom> \
      [--tol 0.001] [--json out.json] [--args "--indent-x 45"] PDF...
"""
import argparse
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time

import fitz


def highlights(path):
    doc = fitz.open(path)
    pages = []
    for page in doc:
        hs = []
        for annot in page.annots() or []:
            if annot.type[0] != 8:
                continue
            x = annot.xref

            def nums(key):
                t, v = doc.xref_get_key(x, key)
                if t != "array":
                    return None
                return [float(n) for n in v.strip("[]").split()]

            def name(key):
                t, v = doc.xref_get_key(x, key)
                return v if t != "null" else None

            ca = doc.xref_get_key(x, "CA")
            ap_t, ap_v = doc.xref_get_key(x, "AP/N")
            ap = doc.xref_stream(int(ap_v.split()[0])) if ap_t == "xref" else None
            hs.append({
                "quads": nums("QuadPoints"),
                "rect": nums("Rect"),
                "color": nums("C"),
                "ca": float(ca[1]) if ca[0] in ("float", "int") else None,
                "bm": name("BM"),
                "ap": ap,
            })
        pages.append(hs)
    doc.close()
    return pages


def close(a, b, tol):
    if a is None or b is None:
        return a == b
    return len(a) == len(b) and all(abs(x - y) <= tol for x, y in zip(a, b))


def compare(py, rs, tol):
    diffs = []
    if len(py) != len(rs):
        return [f"page count {len(py)} vs {len(rs)}"]
    for p, (hp, hr) in enumerate(zip(py, rs)):
        if len(hp) != len(hr):
            diffs.append(f"p{p+1}: {len(hp)} highlights vs {len(hr)}")
            continue
        # Order is creation order in both tools; match by sorted quads to be safe.
        key = lambda h: (h["quads"] or [])[:2]
        for a, b in zip(sorted(hp, key=key), sorted(hr, key=key)):
            for k in ("quads", "rect", "color"):
                if not close(a[k], b[k], tol):
                    diffs.append(f"p{p+1}: {k} differs")
            if a["ca"] is None or b["ca"] is None or abs(a["ca"] - b["ca"]) > tol:
                diffs.append(f"p{p+1}: CA {a['ca']} vs {b['ca']}")
            if a["bm"] != b["bm"]:
                diffs.append(f"p{p+1}: BM {a['bm']} vs {b['bm']}")
            if a["ap"] != b["ap"] and not getattr(compare, "ignore_ap", False):
                diffs.append(f"p{p+1}: appearance stream differs")
    return diffs


def run(cmd):
    t = time.monotonic()
    r = subprocess.run(cmd, capture_output=True, text=True)
    return r, time.monotonic() - t


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ref", required=True)
    ap.add_argument("--bin", required=True)
    ap.add_argument("--tol", type=float, default=0.001)
    ap.add_argument("--json")
    ap.add_argument("--args", default="")
    ap.add_argument("--ignore-ap", action="store_true",
                    help="skip appearance-stream comparison")
    ap.add_argument("pdfs", nargs="+")
    a = ap.parse_args()
    extra = shlex.split(a.args)
    compare.ignore_ap = a.ignore_ap

    results = []
    tmp = tempfile.mkdtemp(prefix="parabloom-parity-")
    try:
        for pdf in a.pdfs:
            name = os.path.basename(pdf)
            src_py = os.path.join(tmp, "py-in.pdf")
            src_rs = os.path.join(tmp, "rs-in.pdf")
            out_py = os.path.join(tmp, "py-out.pdf")
            out_rs = os.path.join(tmp, "rs-out.pdf")
            shutil.copyfile(pdf, src_py)
            shutil.copyfile(pdf, src_rs)
            rp, tp = run([sys.executable, a.ref, src_py, out_py] + extra)
            rr, tr = run([a.bin, src_rs, out_rs] + extra)
            row = {"pdf": name, "py_s": round(tp, 2), "rs_s": round(tr, 2),
                   "py_out": rp.stdout.strip().splitlines()[:1],
                   "rs_out": rr.stdout.strip().splitlines()[:1]}
            if rp.returncode != 0 or rr.returncode != 0:
                row["error"] = {"py": rp.returncode, "rs": rr.returncode,
                                "py_err": rp.stderr[-400:], "rs_err": rr.stderr[-400:]}
                row["diffs"] = ["run failed"]
            else:
                hp, hr = highlights(out_py), highlights(out_rs)
                row["highlights"] = sum(len(x) for x in hp)
                row["diffs"] = compare(hp, hr, a.tol)
            results.append(row)
            status = "OK  " if not row["diffs"] else "DIFF"
            print(f"{status} {name:52s} hl={row.get('highlights','-'):>4} "
                  f"py={row['py_s']:6.2f}s rs={row['rs_s']:6.2f}s "
                  f"{'; '.join(row['diffs'][:3])}", flush=True)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    ok = sum(1 for r in results if not r["diffs"])
    py_t = sum(r["py_s"] for r in results)
    rs_t = sum(r["rs_s"] for r in results)
    print(f"\n{ok}/{len(results)} identical. Time: python {py_t:.1f}s, parabloom {rs_t:.1f}s")
    if a.json:
        for r in results:
            for k in list(r):
                if isinstance(r[k], bytes):
                    del r[k]
        json.dump(results, open(a.json, "w"), indent=1)


if __name__ == "__main__":
    main()
