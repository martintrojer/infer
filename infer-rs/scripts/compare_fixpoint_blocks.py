#!/usr/bin/env python3
"""Compare retained fixpoint PRE/POST blocks dumped by --debug-fixpoint-nodes.

Example:
  python3 scripts/compare_fixpoint_blocks.py \
      --old-log /tmp/wpblock-prepost-dump.log \
      --new-log /tmp/wpblock-prepost-dump-after-vlb.log \
      --proc whirlpool_block \
      --block 29:PRE \
      --block 31:PRE \
      --block 38:POST

The comparison is intentionally coarse. It extracts the selected Rust
`[pulse-fixpoint] ... retained PRE/POST` blocks, computes a small structural
signature, then shows a first normalized line diff. This makes it easy to
separate real retained-state shape changes from fresh-id or map-order noise.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class BlockSelector:
    node: int
    kind: str

    @classmethod
    def parse(cls, raw: str) -> "BlockSelector":
        try:
            node_raw, kind_raw = raw.split(":", 1)
        except ValueError as exc:
            raise argparse.ArgumentTypeError(
                f"invalid block selector {raw!r}, expected NODE:PRE or NODE:POST"
            ) from exc
        kind = kind_raw.upper()
        if kind not in {"PRE", "POST"}:
            raise argparse.ArgumentTypeError(
                f"invalid block selector {raw!r}, expected kind PRE or POST"
            )
        try:
            node = int(node_raw)
        except ValueError as exc:
            raise argparse.ArgumentTypeError(
                f"invalid node in block selector {raw!r}"
            ) from exc
        return cls(node=node, kind=kind)

    def render(self) -> str:
        return f"{self.node}:{self.kind}"


@dataclass
class BlockStats:
    line_count: int
    abstract_values: int
    invalid_attrs: int
    initialized_attrs: int
    uninitialized_attrs: int
    must_be_valid_entries: int
    var_names: tuple[str, ...]

    def render(self) -> str:
        return (
            f"lines={self.line_count} avs={self.abstract_values} "
            f"invalid={self.invalid_attrs} initialized={self.initialized_attrs} "
            f"uninitialized={self.uninitialized_attrs} must={self.must_be_valid_entries} "
            f"vars={len(self.var_names)} first={list(self.var_names[:12])}"
        )


def build_start_regex(proc: str) -> re.Pattern[str]:
    return re.compile(
        rf"\[pulse-fixpoint\] proc={re.escape(proc)} node=(\d+) retained (PRE|POST) = DisjunctiveDomain \{{"
    )


def build_stop_regex(proc: str) -> re.Pattern[str]:
    return re.compile(
        rf"\[pulse-fixpoint\] proc={re.escape(proc)} node=\d+ "
        rf"(retained (PRE|POST) = DisjunctiveDomain \{{|loc=Location )"
        rf"|\[pulse-progress\]|\[ondemand\]"
    )


def extract_block_lines(path: Path, proc: str, selector: BlockSelector) -> list[str]:
    start_re = build_start_regex(proc)
    stop_re = build_stop_regex(proc)
    active = False
    lines: list[str] = []
    with path.open(errors="replace") as handle:
        for line in handle:
            if active and stop_re.search(line):
                break
            match = start_re.search(line)
            if match:
                key = BlockSelector(node=int(match.group(1)), kind=match.group(2))
                if active:
                    break
                if key == selector:
                    active = True
                    lines.append(line.rstrip("\n"))
                continue
            if active:
                lines.append(line.rstrip("\n"))
    if not lines:
        raise FileNotFoundError(
            f"did not find block {selector.render()} for proc {proc} in {path}"
        )
    return lines


def block_stats(lines: list[str]) -> BlockStats:
    var_names: set[str] = set()
    for line in lines:
        var_names.update(re.findall(r'plain: "([^"]+)"', line))
    return BlockStats(
        line_count=len(lines),
        abstract_values=sum(line.count("AbstractValue(") for line in lines),
        invalid_attrs=sum(line.count("Invalid(") for line in lines),
        initialized_attrs=sum(line.count("Initialized") for line in lines),
        uninitialized_attrs=sum(line.count("Uninitialized") for line in lines),
        must_be_valid_entries=sum(line.count("must_be_valid") for line in lines),
        var_names=tuple(sorted(var_names)),
    )


def normalize_line(line: str) -> str:
    line = re.sub(r"\[20\d\d-[^\]]+\]\s+", "", line)
    line = re.sub(r"AbstractValue\(\s*\d+\s*\)", "AbstractValue(N)", line)
    line = re.sub(
        r"visit_count=\d+ pre_disjuncts=\d+ post_disjuncts=\d+",
        "visit_count=K pre_disjuncts=D post_disjuncts=D",
        line,
    )
    return line


def normalized_hash(lines: list[str]) -> str:
    payload = "\n".join(normalize_line(line) for line in lines)
    return hashlib.md5(payload.encode()).hexdigest()


def same_line_positions(old: list[str], new: list[str]) -> int:
    return sum(
        normalize_line(old_line) == normalize_line(new_line)
        for old_line, new_line in zip(old, new)
    )


def first_diff(old: list[str], new: list[str]) -> tuple[int | None, str | None, str | None]:
    for index, (old_line, new_line) in enumerate(zip(old, new), start=1):
        old_norm = normalize_line(old_line)
        new_norm = normalize_line(new_line)
        if old_norm != new_norm:
            return index, old_norm, new_norm
    if len(old) != len(new):
        return min(len(old), len(new)) + 1, "<length mismatch>", "<length mismatch>"
    return None, None, None


def compare_one(
    old_log: Path,
    new_log: Path,
    proc: str,
    selector: BlockSelector,
) -> None:
    old_lines = extract_block_lines(old_log, proc, selector)
    new_lines = extract_block_lines(new_log, proc, selector)
    old_stats = block_stats(old_lines)
    new_stats = block_stats(new_lines)
    same_positions = same_line_positions(old_lines, new_lines)
    diff_line, old_diff, new_diff = first_diff(old_lines, new_lines)

    print(f"== {selector.render()} ==")
    print(f"old: {old_stats.render()}")
    print(f"new: {new_stats.render()}")
    print(f"signature_equal={old_stats == new_stats}")
    print(
        "normalized_hashes="
        f"{normalized_hash(old_lines)} {normalized_hash(new_lines)}"
    )
    print(
        f"same_normalized_positions={same_positions}/{min(len(old_lines), len(new_lines))}"
    )
    if diff_line is None:
        print("first_diff=none")
    else:
        print(f"first_diff=line {diff_line}")
        print(f"  old: {old_diff}")
        print(f"  new: {new_diff}")
    print()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old-log", type=Path, required=True)
    parser.add_argument("--new-log", type=Path, required=True)
    parser.add_argument("--proc", default="whirlpool_block")
    parser.add_argument(
        "--block",
        dest="blocks",
        metavar="NODE:PRE|POST",
        type=BlockSelector.parse,
        action="append",
        required=True,
        help="selected retained block to compare; can be passed multiple times",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    for selector in args.blocks:
        compare_one(args.old_log, args.new_log, args.proc, selector)
    return 0


if __name__ == "__main__":
    sys.exit(main())
