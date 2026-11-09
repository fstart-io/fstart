"""Stage size merging and the PR size report."""

import importlib.util
import json
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "stage_sizes", Path(__file__).with_name("stage_sizes.py")
)
assert SPEC is not None and SPEC.loader is not None
stage_sizes = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(stage_sizes)


def sizes(flat, text, rodata=0, data=0, bss=0):
    return {"flat": flat, "text": text, "rodata": rodata, "data": data, "bss": bss}


class StageSizes(unittest.TestCase):
    def test_merge_keys_boards_and_records_the_pr(self):
        lines = [
            json.dumps({"board": "a", "stages": {"ramstage": sizes(10, 8)}}),
            "",
            json.dumps({"board": "b", "stages": {"stage": sizes(4, 4)}}),
        ]
        merged = stage_sizes.merge(lines, "abc", source_date_epoch=0, pr=7)
        self.assertEqual(merged["commit"], "abc")
        self.assertEqual(merged["source_date_epoch"], 0)
        self.assertEqual(merged["pr"], 7)
        self.assertEqual(sorted(merged["boards"]), ["a", "b"])

    def test_report_lists_changed_new_and_gone_stages(self):
        base = {"commit": "b" * 40, "source_date_epoch": 0, "boards": {
            "x61": {"bootblock": sizes(1000, 800), "ramstage": sizes(5000, 4000)},
            "old": {"stage": sizes(10, 10)},
        }}
        head = {"commit": "c" * 40, "source_date_epoch": 0, "boards": {
            "x61": {"bootblock": sizes(1000, 800), "ramstage": sizes(5128, 4096, rodata=32)},
            "new": {"stage": sizes(20, 20)},
        }}
        text = stage_sizes.report(base, head)
        self.assertTrue(text.startswith(stage_sizes.MARKER))
        self.assertIn("1 stage(s) changed, images +128 bytes in total; 1 new; 1 gone.", text)
        self.assertIn("| x61 | ramstage | 5,128 | +128 (+2.56%) | +96 (+2.40%) | +32 | 0 | 0 |", text)
        self.assertNotIn("| x61 | bootblock | 1,000 | 0", text)
        self.assertIn("New: new/stage", text)
        self.assertIn("Gone: old/stage", text)

    def test_report_without_changes(self):
        doc = {"commit": "c" * 40, "source_date_epoch": 0,
               "boards": {"q35": {"stage": sizes(1, 1)}}}
        for epoch in (0, 1735689600):
            doc["source_date_epoch"] = epoch
            with self.subTest(epoch=epoch):
                self.assertIn("No stage size changed.", stage_sizes.report(doc, doc))

    def test_report_requires_matching_recorded_epochs(self):
        good = {"commit": "c" * 40, "source_date_epoch": 0,
                "boards": {"q35": {"stage": sizes(1, 1)}}}
        for epoch in (None, 1, -1, "0", False, 0.0):
            bad = {**good, "source_date_epoch": epoch,
                   "boards": {"q35": {"stage": sizes(2, 2)}}}
            for base, head in ((good, bad), (bad, good), (bad, bad)):
                with self.subTest(base=base, head=head):
                    # Matching nonnegative integer epochs are comparable.
                    if type(epoch) is int and epoch >= 0 and base == head:
                        continue
                    text = stage_sizes.report(base, head)
                    self.assertTrue(text.startswith(stage_sizes.MARKER))
                    self.assertIn("not comparable", text)
                    self.assertNotIn("| Board |", text)
                    self.assertNotIn("No stage size changed.", text)
        legacy = {k: v for k, v in good.items() if k != "source_date_epoch"}
        for base, head in ((legacy, good), (good, legacy), (legacy, legacy)):
            self.assertIn("not comparable", stage_sizes.report(base, head))

    def test_merge_rejects_invalid_epochs(self):
        for epoch in (-1, None, "0", False, 0.0):
            with self.subTest(epoch=epoch), self.assertRaises(ValueError):
                stage_sizes.merge([], "abc", source_date_epoch=epoch)

    def test_names_and_commits_from_the_pr_are_validated(self):
        good = {"commit": "c" * 40, "source_date_epoch": 0,
                "boards": {"q35": {"stage": sizes(1, 1)}}}
        for bad in (
            {**good, "boards": {"x | <img>": {"stage": sizes(1, 1)}}},
            {**good, "boards": {"q35\n": {"stage": sizes(1, 1)}}},
            {**good, "commit": ["`x`", "[link](https://example.com)"]},
            {**good, "commit": "c" * 39 + "`"},
        ):
            with self.assertRaises(ValueError):
                stage_sizes.report(good, bad)


if __name__ == "__main__":
    unittest.main()
