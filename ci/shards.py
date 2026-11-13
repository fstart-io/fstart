#!/usr/bin/env python3
"""Split the boards into the firmware jobs CI runs.

Hosted runners are scarce and every job pays for its own toolchain, host
tools and empty Cargo target directory, so boards are built several to a
job: grouped by architecture, where they share core/alloc and most
dependencies, and split into shards of at most MAX_BUILDS builds. A shard
also boots its own boards in QEMU, reusing the target directory that just
built them.

    fbuild boards --json > BOARDS.json
    qemu-boot-tests.sh --list | shards.py BOARDS.json

prints the job matrix: {"include": [{"shard", "boards", "boots", "qemu",
"assets"}, ...]}. `boards` and `boots` are space separated board ids;
`qemu` is the apt packages emulating the shard's architecture and `assets`
the boot-assets artifact its boots read, both empty without boots.
"""

import argparse
import json
import sys

# Builds per job: a board and each of its variants count as one build each.
MAX_BUILDS = 7

# CI installs no recommended packages, but the network cards of the virt
# machines need the option ROMs of ipxe-qemu and sbsa-ref's display the
# VGA BIOS of seabios, which only qemu-system-x86 depends on. The names
# are those of the runner image the workflow pins (Ubuntu 26.04).
QEMU_PACKAGES = {
    "aarch64": "qemu-system-arm ipxe-qemu seabios",
    "armv7": "qemu-system-arm ipxe-qemu",
    "riscv64": "qemu-system-riscv ipxe-qemu",
    "x86": "qemu-system-x86",
    "x86_64": "qemu-system-x86",
}
X86 = ("x86", "x86_64")


def _builds(board):
    return [board["board"], *board["variants"]]


def _platform(board):
    """The platform shard of a board; boards that declare none share one."""
    return board["platform"] or "other"


def _dependency(board):
    """The platform crate of a board, empty when it declares no profile."""
    return (board["build_profile"] or {}).get("dependency", "")


def _split(boards):
    """Consecutive runs of boards with at most MAX_BUILDS builds each."""
    shards = [[]]
    count = 0
    for board in boards:
        builds = len(_builds(board))
        if shards[-1] and count + builds > MAX_BUILDS:
            shards.append([])
            count = 0
        shards[-1].append(board)
        count += builds
    return shards


def matrix(boards, boot_boards):
    known = {board["board"] for board in boards}
    unknown = sorted(set(boot_boards) - known)
    if unknown:
        raise ValueError(f"boot tests name unknown boards: {', '.join(unknown)}")

    platforms = {}
    for board in boards:
        platforms.setdefault(_platform(board), []).append(board)

    include = []
    for platform, group in sorted(platforms.items()):
        # Boards of one platform crate share the most code: keep them together.
        group.sort(key=lambda b: (_dependency(b), b["board"]))
        shards = _split(group)
        for index, shard in enumerate(shards, 1):
            boots = [b["board"] for b in shard if b["board"] in boot_boards]
            # Every board is built; only one that boots needs an emulator.
            if boots and platform not in QEMU_PACKAGES:
                raise ValueError(
                    f"no QEMU package known for platform {platform}, "
                    f"which {', '.join(boots)} boot on"
                )
            include.append(
                {
                    "shard": platform if len(shards) == 1 else f"{platform}-{index}",
                    "boards": " ".join(build for b in shard for build in _builds(b)),
                    "boots": " ".join(boots),
                    "qemu": QEMU_PACKAGES[platform] if boots else "",
                    "assets": ("x86_64" if platform in X86 else "payloads") if boots else "",
                }
            )
    return {"include": include}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("boards", help="output of `fbuild boards --json`")
    args = parser.parse_args(argv)
    with open(args.boards, encoding="utf-8") as handle:
        boards = json.load(handle)
    # `qemu-boot-tests.sh --list` lines: "board payload".
    boot_boards = {line.split()[0] for line in sys.stdin if line.strip()}
    json.dump(matrix(boards, boot_boards), sys.stdout, separators=(",", ":"))
    print()


if __name__ == "__main__":
    main()
