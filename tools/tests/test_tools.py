import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


TOOLS = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


parity = load("parity")


class ComparisonToolsTest(unittest.TestCase):
    def compare_blocks(self, py, rs):
        with tempfile.TemporaryDirectory() as directory:
            paths = [Path(directory) / name for name in ("py.json", "rs.json")]
            for path, value in zip(paths, (py, rs)):
                path.write_text(json.dumps(value))
            result = subprocess.run(
                [sys.executable, str(TOOLS / "compare_blocks.py"), *map(str, paths)],
                capture_output=True, text=True, check=False,
            )
            return result.returncode, json.loads(result.stdout.splitlines()[-1])

    def block(self):
        return {"bbox": [72, 100, 540, 112], "lines": [
            {"x0": 72, "y0": 100, "x1": 540, "y1": 112, "text": "body"}
        ]}

    def test_identical_blocks_succeed(self):
        status, stats = self.compare_blocks([[self.block()]], [[self.block()]])
        self.assertEqual(status, 0)
        self.assertEqual(stats["pages_same"], 1)

    def test_block_bounds_are_compared(self):
        changed = self.block()
        changed["bbox"][1] = 800
        status, stats = self.compare_blocks([[self.block()]], [[changed]])
        self.assertEqual(status, 1)
        self.assertEqual(stats["block_bbox_diff"], 1)
        self.assertEqual(stats["pages_same"], 0)

    def test_missing_pages_are_counted(self):
        status, stats = self.compare_blocks([[self.block()], []], [[self.block()]])
        self.assertEqual(status, 1)
        self.assertEqual(stats["pages"], 2)
        self.assertEqual(stats["page_count_diff"], 1)

    def test_line_difference_fails(self):
        changed = self.block()
        changed["lines"][0]["text"] = "changed"
        status, stats = self.compare_blocks([[self.block()]], [[changed]])
        self.assertEqual(status, 1)
        self.assertEqual(stats["text_diff"], 1)

    def run_parity(self, process_status=0, outputs=None):
        if outputs is None:
            outputs = [[[]], [[]]]
        with tempfile.TemporaryDirectory() as directory:
            pdf = Path(directory) / "input.pdf"
            pdf.write_bytes(b"fixture")
            args = ["parity.py", "--ref", "ref.py", "--bin", "parabloom", str(pdf)]
            completed = subprocess.CompletedProcess([], process_status, "", "failure")
            with patch.object(sys, "argv", args), \
                    patch.object(parity, "run", return_value=(completed, 0)), \
                    patch.object(parity, "highlights", side_effect=outputs), \
                    contextlib.redirect_stdout(io.StringIO()):
                return parity.main()

    def test_parity_child_failure_fails(self):
        self.assertEqual(self.run_parity(process_status=1), 1)

    def test_parity_cli_returns_failure_status(self):
        with tempfile.TemporaryDirectory() as directory:
            reference = Path(directory) / "reference.py"
            reference.write_text("raise SystemExit(1)\n")
            pdf = Path(directory) / "input.pdf"
            pdf.write_text("raise SystemExit(1)\n")
            result = subprocess.run(
                [sys.executable, str(TOOLS / "parity.py"), "--ref", str(reference),
                 "--bin", sys.executable, str(pdf)],
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("run failed", result.stdout)

    def test_parity_difference_fails(self):
        self.assertEqual(self.run_parity(outputs=[[[]], [[], []]]), 1)

    def test_identical_parity_succeeds(self):
        self.assertEqual(self.run_parity(), 0)

    def test_missing_opacity_matches_itself(self):
        highlight = {"quads": [], "rect": [], "color": [], "ca": None,
                     "bm": None, "ap": None}
        self.assertEqual(parity.compare([[highlight]], [[copy.deepcopy(highlight)]], 0.001), [])


if __name__ == "__main__":
    unittest.main()
