#!/usr/bin/env python3
"""Compare OCaml Pulse debug traces with Rust infer-rs traces.

Usage:
  # 1. Run OCaml with --debug on a C file:
  infer --pulse-only --debug -j 1 -- clang -c file.c

  # 2. Generate textual and run Rust with tracing:
  infer capture --dump-textual -j 1 -- clang -c file.c
  infer-rs --debug-level-analysis 1 file.sil 2> rust_trace.log

  # 3. Compare:
  python3 scripts/compare_traces.py \\
      --ocaml-dir infer-out/captured/<hash>/nodes/ \\
      --rust-log rust_trace.log \\
      --proc malloc_then_free_ok
"""

import argparse
import glob
import os
import re
import sys
from dataclasses import dataclass, field
from html.parser import HTMLParser
from pathlib import Path


# --- OCaml HTML trace parser ---

@dataclass
class OcamlInstrTrace:
    instr: str
    disjuncts_before: int | None = None
    disjuncts_after: int | None = None
    details: list[str] = field(default_factory=list)


@dataclass
class OcamlNodeTrace:
    node_id: str
    instrs: list[OcamlInstrTrace] = field(default_factory=list)


class OcamlHTMLParser(HTMLParser):
    """Extract per-instruction traces from OCaml's --debug HTML output."""

    def __init__(self):
        super().__init__()
        self.text_parts = []
        self.in_code = False
        self.skip = False

    def handle_starttag(self, tag, attrs):
        if tag in ('style', 'script'):
            self.skip = True
        if tag == 'code':
            self.in_code = True

    def handle_endtag(self, tag):
        if tag in ('style', 'script'):
            self.skip = False
        if tag == 'code':
            self.in_code = False

    def handle_data(self, data):
        if not self.skip:
            self.text_parts.append(data)


def parse_ocaml_node(html_path: str) -> OcamlNodeTrace | None:
    """Parse an OCaml HTML debug file for one CFG node."""
    parser = OcamlHTMLParser()
    with open(html_path) as f:
        parser.feed(f.read())

    text = ''.join(parser.text_parts)

    # Extract node name from filename
    basename = os.path.basename(html_path)
    m = re.search(r'_node(\d+)\.html$', basename)
    node_id = m.group(1) if m else basename

    trace = OcamlNodeTrace(node_id=node_id)

    # Find instruction executions and disjunct counts
    # Pattern: "exec_instr <SIL_INSTR>" followed by state info
    lines = text.split('\n')
    current_instr = None

    for line in lines:
        line = line.strip()
        if not line:
            continue

        # Detect "exec_instr ..." sections
        m = re.match(r'(?:Result of )?exec_instr (.+)', line)
        if m:
            instr_text = m.group(1).strip().rstrip(';')
            if line.startswith('Result of'):
                # This is the result — look for disjunct count
                pass
            else:
                current_instr = OcamlInstrTrace(instr=instr_text)
                trace.instrs.append(current_instr)
            continue

        # Detect disjunct counts in STATE lines
        m = re.match(r'(?:PRE )?STATE:\s*(\d+) disjuncts?', line)
        if m and current_instr:
            count = int(m.group(1))
            if current_instr.disjuncts_after is None:
                current_instr.disjuncts_after = count

        m = re.match(r'PRE STATE:\s*(\d+) disjuncts?', line)
        if m:
            # This is the pre-state before instructions
            pass

        # Detect per-disjunct execution
        if current_instr and ('Got ' in line and 'disjunct' in line):
            current_instr.details.append(line)

        # Detect prune/model info
        if current_instr and any(kw in line for kw in [
            'Found ocaml model', 'skipping unknown', 'Applying pre/post',
            'Materializing PRE', 'is_bop_equal', 'not a comparison'
        ]):
            current_instr.details.append(line)

    return trace


def parse_ocaml_traces(nodes_dir: str, proc_name: str) -> list[OcamlNodeTrace]:
    """Parse all node HTML files for a given procedure."""
    pattern = os.path.join(nodes_dir, f'{proc_name}*_node*.html')
    files = sorted(glob.glob(pattern))
    if not files:
        # Try broader match
        pattern = os.path.join(nodes_dir, f'*{proc_name}*_node*.html')
        files = sorted(glob.glob(pattern))
    traces = []
    for f in files:
        trace = parse_ocaml_node(f)
        if trace and trace.instrs:
            traces.append(trace)
    return traces


# --- Rust log trace parser ---

@dataclass
class RustInstrTrace:
    node: int
    instr_idx: int
    instr: str
    disjuncts_before: int
    continue_before: int
    disjuncts_after: int
    continue_after: int
    details: list[str] = field(default_factory=list)


def parse_rust_traces(log_path: str, proc_name: str) -> list[RustInstrTrace]:
    """Parse Rust --debug-level-analysis log for a specific procedure."""
    traces = []
    in_proc = False
    current = None

    with open(log_path) as f:
        for line in f:
            line = line.strip()

            # Only process lines for our target procedure
            if f'[{proc_name}]' not in line:
                continue

            # Parse [proc] exec line
            m = re.search(
                r'\[' + re.escape(proc_name) + r'\] exec node=(\d+) instr=(\d+) disjuncts=(\d+) \(continue=(\d+)\) (.+)',
                line
            )
            if m:
                if current:
                    traces.append(current)
                current = RustInstrTrace(
                    node=int(m.group(1)),
                    instr_idx=int(m.group(2)),
                    instr=m.group(5).strip(),
                    disjuncts_before=int(m.group(3)),
                    continue_before=int(m.group(4)),
                    disjuncts_after=0,
                    continue_after=0,
                )
                continue

            # Parse [proc] result line
            m = re.search(r'\[' + re.escape(proc_name) + r'\] result disjuncts=(\d+) \(continue=(\d+)\)', line)
            if m and current:
                current.disjuncts_after = int(m.group(1))
                current.continue_after = int(m.group(2))
                continue

            # Collect detail lines
            if current and any(kw in line for kw in [
                'disjunct #', '[call]', '[prune]'
            ]):
                # Strip timestamp prefix
                detail = re.sub(r'^\[.*?\]\s*', '', line)
                current.details.append(detail)

    if current:
        traces.append(current)

    return traces


# --- Comparison ---

def compare(
    ocaml_traces: list[OcamlNodeTrace],
    rust_traces: list[RustInstrTrace],
    proc_name: str,
):
    """Compare OCaml and Rust traces, printing divergences."""
    # Flatten OCaml traces to per-instruction
    ocaml_instrs = []
    for node in ocaml_traces:
        for instr in node.instrs:
            ocaml_instrs.append((node.node_id, instr))

    print(f"\n{'='*60}")
    print(f"Comparison for: {proc_name}")
    print(f"OCaml: {len(ocaml_instrs)} instructions across {len(ocaml_traces)} nodes")
    print(f"Rust:  {len(rust_traces)} instructions")
    print(f"{'='*60}\n")

    # Print side-by-side
    max_instrs = max(len(ocaml_instrs), len(rust_traces))

    divergences = 0
    for i in range(max_instrs):
        ocaml_line = ""
        rust_line = ""
        marker = " "

        if i < len(ocaml_instrs):
            nid, oi = ocaml_instrs[i]
            after = f"→{oi.disjuncts_after}d" if oi.disjuncts_after is not None else ""
            instr_short = oi.instr[:50]
            ocaml_line = f"n{nid}: {instr_short} {after}"

        if i < len(rust_traces):
            ri = rust_traces[i]
            instr_short = ri.instr[:50]
            rust_line = f"n{ri.node}: {instr_short} {ri.continue_before}c→{ri.continue_after}c"

        # Check for divergence in disjunct counts
        if i < len(ocaml_instrs) and i < len(rust_traces):
            oi = ocaml_instrs[i][1]
            ri = rust_traces[i]
            if oi.disjuncts_after is not None and oi.disjuncts_after != ri.disjuncts_after:
                marker = "!"
                divergences += 1

        print(f"{marker} OCaml: {ocaml_line:<65} | Rust: {rust_line}")

        # Print details for divergent instructions
        if marker == "!":
            if i < len(ocaml_instrs):
                for d in ocaml_instrs[i][1].details:
                    print(f"    OCaml: {d[:100]}")
            if i < len(rust_traces):
                for d in rust_traces[i].details:
                    print(f"    Rust:  {d[:100]}")

    print(f"\n{'='*60}")
    print(f"Total divergences: {divergences}")
    if divergences == 0:
        print("Traces match!")
    print(f"{'='*60}\n")

    # Also print Rust details for any prune/call events
    prune_events = [r for r in rust_traces if any('[prune]' in d for d in r.details)]
    if prune_events:
        print("Prune events (Rust):")
        for r in prune_events:
            for d in r.details:
                if '[prune]' in d:
                    status = "KILLED" if r.continue_after < r.continue_before else "kept"
                    print(f"  node={r.node} {r.continue_before}c→{r.continue_after}c {status}: {d}")
        print()

    call_events = [r for r in rust_traces if any('[call]' in d for d in r.details)]
    if call_events:
        print("Call dispatch (Rust):")
        for r in call_events:
            for d in r.details:
                if '[call]' in d:
                    print(f"  node={r.node}: {d}")
        print()


def main():
    parser = argparse.ArgumentParser(description='Compare OCaml and Rust Pulse traces')
    parser.add_argument('--ocaml-dir', required=True,
                        help='Path to OCaml infer-out/captured/<hash>/nodes/ directory')
    parser.add_argument('--rust-log', required=True,
                        help='Path to Rust trace log (stderr from --debug-level-analysis 1)')
    parser.add_argument('--proc', required=True,
                        help='Procedure name to compare')
    args = parser.parse_args()

    ocaml_traces = parse_ocaml_traces(args.ocaml_dir, args.proc)
    if not ocaml_traces:
        print(f"No OCaml traces found for '{args.proc}' in {args.ocaml_dir}")
        print(f"Available files: {glob.glob(os.path.join(args.ocaml_dir, '*.html'))[:5]}")
        sys.exit(1)

    rust_traces = parse_rust_traces(args.rust_log, args.proc)
    if not rust_traces:
        print(f"No Rust traces found for '{args.proc}' in {args.rust_log}")
        sys.exit(1)

    compare(ocaml_traces, rust_traces, args.proc)


if __name__ == '__main__':
    main()
