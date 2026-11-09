#!/usr/bin/env python3
"""Stage size records and the per-PR size report.

`fbuild build --size-report FILE` appends one JSON line per board. CI merges
them into `stage-sizes.json` (uploaded as the `stage-sizes` artifact of every
run) and the size-report workflow compares a pull request's sizes with its
base commit's:

    stage_sizes.py merge LINES.jsonl OUT.json --commit SHA --source-date-epoch SECONDS [--pr N]
    stage_sizes.py report BASE.json HEAD.json > report.md
"""

import argparse
import json
import re
import sys

FIELDS = ("text", "rodata", "data", "bss")
# Names come from a pull request's own build: keep them out of the markdown.
NAME = re.compile(r"[A-Za-z0-9._-]{1,64}")
SHA = re.compile(r"[0-9a-f]{40}")
MARKER = "<!-- fstart-stage-sizes -->"


def merge(lines, commit, source_date_epoch, pr=None):
    if type(source_date_epoch) is not int or source_date_epoch < 0:
        raise ValueError("SOURCE_DATE_EPOCH must be a nonnegative integer")
    boards = {}
    for line in lines:
        if not line.strip():
            continue
        record = json.loads(line)
        boards[record["board"]] = record["stages"]
    merged = {"commit": commit, "source_date_epoch": source_date_epoch, "boards": boards}
    if pr is not None:
        merged["pr"] = pr
    return merged


def _sizes(document):
    """(board, stage) -> {field: int}, validated."""
    out = {}
    for board, stages in document.get("boards", {}).items():
        for stage, sizes in stages.items():
            if not NAME.fullmatch(board) or not NAME.fullmatch(stage):
                raise ValueError(f"unexpected board/stage name {board!r}/{stage!r}")
            out[(board, stage)] = {k: int(sizes[k]) for k in FIELDS + ("flat",)}
    return out


def _delta(new, old):
    change = new - old
    if change == 0:
        return "0"
    percent = f" ({change / old * 100:+.2f}%)" if old else ""
    return f"{change:+,}{percent}"


def _commit(document):
    commit = document.get("commit")
    if not isinstance(commit, str) or not SHA.fullmatch(commit):
        raise ValueError(f"unexpected commit {commit!r}")
    return commit


def report(base, head):
    """Markdown comparing `head`'s stage sizes with `base`'s."""
    old, new = _sizes(base), _sizes(head)
    head_commit, base_commit = _commit(head), _commit(base)
    lines = [MARKER, "### Stage sizes", ""]
    lines.append(
        f"`{head_commit[:12]}` against base `{base_commit[:12]}`. "
        "*Image* is the loaded stage (`stage.bin`); the other columns are section bytes "
        "(bss without the heap and stack reservations)."
    )
    lines.append("")
    old_epoch, new_epoch = base.get("source_date_epoch"), head.get("source_date_epoch")
    if (type(old_epoch) is not int or old_epoch < 0
            or type(new_epoch) is not int or old_epoch != new_epoch):
        lines += [
            "Stage sizes are not comparable: the builds do not record the same "
            "`SOURCE_DATE_EPOCH`. A baseline built with the same fixed epoch is needed.",
            "",
        ]
        return "\n".join(lines)
    changed = [k for k in sorted(new) if k in old and new[k] != old[k]]
    added = sorted(k for k in new if k not in old)
    removed = sorted(k for k in old if k not in new)
    total = sum(new[k]["flat"] - old[k]["flat"] for k in changed)
    if not (changed or added or removed):
        lines.append("No stage size changed.")
    else:
        lines.append(
            f"{len(changed)} stage(s) changed, images {total:+,} bytes in total"
            + (f"; {len(added)} new" if added else "")
            + (f"; {len(removed)} gone" if removed else "")
            + "."
        )
    if changed:
        lines += ["", "| Board | Stage | Image | Δ image | Δ text | Δ rodata | Δ data | Δ bss |",
                  "|---|---|---:|---:|---:|---:|---:|---:|"]
        for board, stage in changed:
            n, o = new[(board, stage)], old[(board, stage)]
            lines.append(
                f"| {board} | {stage} | {n['flat']:,} | {_delta(n['flat'], o['flat'])} | "
                + " | ".join(_delta(n[f], o[f]) for f in FIELDS)
                + " |"
            )
    for title, keys in (("New", added), ("Gone", removed)):
        if keys:
            lines += ["", f"{title}: " + ", ".join(f"{b}/{s}" for b, s in keys)]
    lines += ["", "<details><summary>All stages</summary>", "",
              "| Board | Stage | Image | text | rodata | data | bss |",
              "|---|---|---:|---:|---:|---:|---:|"]
    for board, stage in sorted(new):
        n = new[(board, stage)]
        lines.append(
            f"| {board} | {stage} | {n['flat']:,} | "
            + " | ".join(f"{n[f]:,}" for f in FIELDS)
            + " |"
        )
    lines += ["", "</details>", ""]
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    m = commands.add_parser("merge")
    m.add_argument("lines")
    m.add_argument("out")
    m.add_argument("--commit", required=True)
    m.add_argument("--source-date-epoch", type=int, required=True)
    m.add_argument("--pr", type=int)
    r = commands.add_parser("report")
    r.add_argument("base")
    r.add_argument("head")
    args = parser.parse_args(argv)
    if args.command == "merge":
        with open(args.lines) as lines:
            merged = merge(lines, args.commit, args.source_date_epoch, args.pr)
        with open(args.out, "w") as out:
            json.dump(merged, out, indent=1, sort_keys=True)
    else:
        with open(args.base) as base, open(args.head) as head:
            sys.stdout.write(report(json.load(base), json.load(head)))


if __name__ == "__main__":
    main()
