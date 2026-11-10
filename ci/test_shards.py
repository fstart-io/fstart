"""The firmware job matrix CI derives from the board list."""

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "shards", Path(__file__).with_name("shards.py")
)
assert SPEC is not None and SPEC.loader is not None
shards = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(shards)


def board(name, platform, dependency="fstart-platform-qemu", variants=()):
    return {
        "board": name,
        "platform": platform,
        "build_profile": {"dependency": dependency, "name": "rust"},
        "variants": list(variants),
    }


class Shards(unittest.TestCase):
    def test_one_shard_per_platform_with_its_boots(self):
        matrix = shards.matrix(
            [
                board("qemu-riscv64", "riscv64"),
                board("qemu-q35", "x86_64"),
                board("unmatched", "riscv64", "fstart-platform-sifive"),
            ],
            {"qemu-q35", "qemu-riscv64"},
        )
        self.assertEqual(
            matrix["include"],
            [
                {
                    "shard": "riscv64",
                    "boards": "qemu-riscv64 unmatched",
                    "boots": "qemu-riscv64",
                    "qemu": "qemu-system-misc ipxe-qemu",
                    "assets": "payloads",
                },
                {
                    "shard": "x86_64",
                    "boards": "qemu-q35",
                    "boots": "qemu-q35",
                    "qemu": "qemu-system-x86",
                    "assets": "x86_64",
                },
            ],
        )

    def test_shard_without_boots_installs_no_qemu(self):
        (shard,) = shards.matrix([board("pc2", "aarch64")], set())["include"]
        self.assertEqual((shard["boots"], shard["qemu"], shard["assets"]), ("", "", ""))

    def test_large_platform_splits_by_platform_crate_with_variants_kept(self):
        boards = [board(f"intel-{i}", "x86_64", "fstart-platform-intel") for i in range(6)]
        boards.append(board("amd", "x86_64", "fstart-platform-amd", ["amd-b", "amd-c"]))
        include = shards.matrix(boards, set())["include"]
        self.assertEqual([s["shard"] for s in include], ["x86_64-1", "x86_64-2"])
        # The AMD board sorts first; its variants count against the limit
        # and are never separated from it.
        self.assertEqual(
            include[0]["boards"], "amd amd-b amd-c intel-0 intel-1 intel-2 intel-3"
        )
        self.assertEqual(include[1]["boards"], "intel-4 intel-5")

    def test_every_build_is_in_exactly_one_shard(self):
        boards = [
            board(f"b{i}", platform, variants=[f"b{i}-v"] if i % 3 == 0 else [])
            for i, platform in enumerate(["x86_64"] * 11 + ["x86"] * 4 + ["armv7"] * 2)
        ]
        built = [
            build
            for shard in shards.matrix(boards, set())["include"]
            for build in shard["boards"].split()
        ]
        expected = [build for b in boards for build in [b["board"], *b["variants"]]]
        self.assertCountEqual(built, expected)

    def test_boards_without_metadata_are_still_built(self):
        bare = board("bare", None)
        bare["build_profile"] = None
        include = shards.matrix(
            [bare, board("novel", "ppc64"), board("qemu-q35", "x86_64")], set()
        )["include"]
        self.assertEqual(
            [(s["shard"], s["boards"]) for s in include],
            [("other", "bare"), ("ppc64", "novel"), ("x86_64", "qemu-q35")],
        )

    def test_boot_on_a_platform_without_qemu_packages_is_an_error(self):
        with self.assertRaisesRegex(ValueError, "ppc64, which novel boot"):
            shards.matrix([board("novel", "ppc64")], {"novel"})

    def test_boot_test_of_an_unknown_board_is_an_error(self):
        with self.assertRaisesRegex(ValueError, "gone"):
            shards.matrix([board("qemu-q35", "x86_64")], {"gone"})


if __name__ == "__main__":
    unittest.main()
