# infer-rs Development Rules

## Cross-reference with OCaml

**Every analysis change must be cross-referenced against the OCaml source.** Before implementing any Pulse, formula, or interproc change, read the corresponding OCaml code in `infer/src/pulse/` and verify the Rust implementation matches OCaml's approach. This is not optional — we are porting OCaml's analysis, not inventing our own.

Key OCaml files to cross-reference:
- `PulseInterproc.ml` — summary application, biabduction (materialize_pre, apply_post)
- `PulseFormula.ml` — constraint solver (prune_binop, and_equal_binop, term_eqs)
- `PulseModelsC.ml` — C models (malloc, free, fopen, call_c_function_ptr)
- `PulseSummary.ml` — summary creation (of_posts, exec_summary_of_post_common)
- `PulseAbductiveDomain.ml` — domain (pre/post, discard_unreachable, filter_for_summary)
- `Pulse.ml` — transfer functions (exec_instr, specialization)

## Correctness Over Numbers

**Always make the semantically correct edit first, even if compliance numbers get worse temporarily.**

- Confirm correctness first: match the OCaml source of truth, use traces/summaries when needed, and add focused tests for the behavior being changed.
- Only after the change is confirmed correct should you work on restoring or improving compliance numbers.
- Do not add workarounds, special cases, or heuristic hacks whose main purpose is to make the sweep numbers look better without fixing the underlying behavior.
- When a correct fix regresses totals, keep the correct fix and investigate the newly exposed gaps separately.

## LOG.md Purpose

`LOG.md` exists to preserve the minimum active debugging context needed to
resume quickly after chat compaction or interruption without reconstructing the
whole investigation from scratch.

## LOG.md Hygiene

`LOG.md` is for short-lived debugging context that must survive chat
compaction, not for storing the full project history.

- Keep it current with the active line of investigation: current hypothesis,
  OCaml cross-checks, repro commands, blockers, and latest validated
  checkpoint.
- Keep it clean of finished items. When work is done, move the durable result
  to the right long-lived place such as `README.md`, `docs/STATUS.md`,
  `TODO.md`, focused tests, or the commit history, then trim it out of
  `LOG.md`.
- Do not let `LOG.md` become a changelog or archive of closed work; stale
  finished sections should be removed or summarized elsewhere.

## Function Pointer Specialization Gotcha

When debugging `__call_c_function_ptr` / specialization parity, remember that
OCaml's specialization surface is dynamic-type driven, not Closure-attr
driven.

- Cross-reference `PulseAbductiveDomain.need_dynamic_type_specialization`,
  `PulseAbductiveDomain.Summary.heap_paths_that_need_dynamic_type_specialization`,
  `PulseArithmetic.and_dynamic_type_is_unsafe`,
  `PulseSpecialization.apply`, and `PulseCallOperations.ml`.
- Prefer storing and resolving known dynamic types in the abductive /
  path-condition state, and preserve them through equalities/substitution.
- Use `Closure(...)` only as a fallback for direct `Cfun` / closure values. Do
  not seed exported `Closure(...)` attrs onto specialized heap-path values just
  to make summaries match.
- Keep unknown-call behavior aligned too: bare pointer/function-value actuals
  may need pointee materialization before havoc, and unknown-call returns can
  carry `ReturnedFromUnknown(actuals)`.

## Direct-formal null parity gotcha

When debugging direct-formal null dereferences, separate real local branch proofs from generic
solver equalities.

- Preserve local branch conditions at depth 0. Model-side splits such as
  `free(NULL)` / `free(non-null)` should use `and_condition_direct(...)`, not just
  `and_equal_const(...)`, so summary classification can see the branch provenance.
- Record `Attribute::UsedAsBranchCond` on abstract values seen in `Prune`. Manifest-vs-latent
  classification depends on that signal together with the local zero condition.
- Do not treat "the formula contains `addr == 0`" as sufficient evidence that a direct-formal null
  dereference should be manifest. `latent.c:deref_then_free_then_deref_bad` is the counterexample.

## Imported arithmetic latent-summary gotcha

When debugging latent-vs-manifest parity for arithmetic guards imported through calls, preserve the
caller-visible arithmetic structure in recorded summary conditions.

- Cross-reference `PulseFormula.ml`, `PulseFormulaPhi.ml`, `PulseSummary.ml`, and
  `PulseLatentIssue.ml`.
- If Rust stores `neg_x = -x` as the reverse linear equation `x = -neg_x`, the imported condition
  must still survive summary recording/export as a caller-visible `-x == 0`-style guard, not
  collapse into a dead temp or `0 == 0`.
- `simplify_for_summary(...)` should rewrite dead arithmetic temps onto kept precondition vars
  before pruning phi facts.
- Local invalid accesses should only keep the Rust manifest+latent twin on non-manifest paths when
  the caller-sensitive signal comes from heap shape or imported call-side validity, not pure
  imported arithmetic.

## Cycle / cursor-rewrite latent-summary gotcha

When debugging wrapper/cycle null publication, do not assume that a field-derived cursor rewrite
should turn a callee-local latent null path into a manifest callee report.

- Cross-reference `PulseSummary.ml` / `PulseLatentIssue.ml` and validate against dumped OCaml
  summaries, not just issue counts.
- In the one-step cycle shape (`traverse_one_step_and_crash_if_equal_to_root`-style), OCaml keeps
  the callee summary latent-only and reifies the manifest null-deref only in the caller.
- Do not broaden Rust manifest-twin heuristics for field-derived cursor rewrites unless the
  caller-control / reachability check is done over canonical summary-space values and OCaml proves
  the broader behavior.

## Compliance debugging recipe

When investigating why a C test file produces different results from OCaml:

```bash
# 1. Run OCaml pulse on the file and dump summaries
infer -j 1 --pulse-only -o /tmp/debug_out -- clang -c file.c
infer debug -j 1 --dump-json-summaries -o /tmp/debug_out

# 2. Inspect OCaml's summaries (pre/post heap, attrs, disjuncts)
python3 -c "
import json
with open('/tmp/debug_out/all_summaries.json') as f:
    data = json.load(f)
for entry in data:
    name = entry[0][1]['c_name'][-1] if entry[0][0] == 'C' else '?'
    for checker_name, summary in entry[1]:
        if checker_name != 'pulse': continue
        pplist = summary.get('main', {}).get('pre_post_list', [])
        print(f'{name}: {len(pplist)} disjuncts')
        for i, pp in enumerate(pplist):
            exec_state = pp[0]
            if isinstance(pp[1], dict):
                pre = pp[1].get('pre', {})
                pre_heap = pre.get('heap', [])
                print(f'  [{i}] {exec_state}: pre_heap={len(pre_heap)}')
                for edge in pre_heap:
                    src = edge[0]
                    for e in edge[1]:
                        access = e[0][0] if isinstance(e[0], list) else e[0]
                        target = e[1][0] if isinstance(e[1], list) else e[1]
                        print(f'    {src} --{access}--> {target}')
"

# 3. Compare with Rust: write a temporary debug test in end_to_end.rs
# that parses the dump-textual output and prints per-proc summaries.

# 4. Match pre-state edges, disjunct counts, and execution states
# between OCaml and Rust to find the divergence point.
```

## Step-by-step tracing for compliance debugging

The most effective way to find analysis divergences is **per-instruction tracing** — comparing OCaml and Rust state transitions step by step. This is far more precise than comparing high-level signals (bug counts) or summaries.

**OCaml side:** Run with `--debug` to get per-node HTML pages with full state dumps:
```bash
infer --pulse-only --debug -j 1 -- clang -c file.c
# Output: infer-out/captured/*/nodes/*.html
# Each HTML page shows per-instruction: PRE STATE → exec_instr → STATE
# Includes: disjunct count, formula (linear_eqs, term_eqs, intervals, atoms),
# heap edges (v2 → { * → v4 }), attributes, per-disjunct execution
```

Single-procedure tracing: `--procedures-filter "func_name"` or `--focus-on func_name`.

Formula-level tracing: edit `PulseFormulaDebug.ml` line 15: `let debug = true` (recompile required).

**Rust side:** Run with `--debug-level-analysis 2` (or env `INFER_RS_TRACE=1`) to get per-instruction JSON traces matching OCaml's structure:
```bash
infer-rs --pulse-only --debug-level-analysis 2 -- file.sil
# Emits per-instruction: { proc, node, instr, disjuncts_before, disjuncts_after,
#   formula: { linear_eqs, atoms, intervals }, heap_edges, attrs }
```

**Comparison workflow:**
1. Generate OCaml HTML debug for a specific procedure
2. Generate Rust JSON trace for the same procedure (from same .sil input)
3. Compare instruction-by-instruction: disjunct counts, formula state, heap edges
4. First divergence point = root cause of the analysis difference

Key OCaml logging points to match:
- `AbstractInterpreter.ml`: `exec_instr %t`, `Got N disjuncts back`, `Fixpoint reached`
- `PulseCallOperations.ml`: `skipping unknown procedure`, `iter_call`, `Found ocaml model`
- `PulseInterproc.ml`: `Materializing PRE`, `applying callee path condition`, `Found [return]`
- `PulseFormula.ml` (Debug.p): `prune atom %a in %a`

## Build Requirements

Every commit must pass the fast check:

```bash
cd infer-rs
make check          # fmt + clippy + tests (~3s)
```

Equivalently, the three steps individually:

```bash
cargo fmt --check        # formatting
cargo clippy -- -D warnings  # lints (warnings are errors)
cargo test               # unit + integration tests (no external deps)
```

Fix clippy warnings before committing -- do not suppress them with `#[allow(...)]` unless there is a clear justification documented in a comment.

## Test Tracks

Two test tracks for different feedback loops:

| Command | What it runs | Speed |
|---------|-------------|-------|
| `make check` | fmt + clippy + all non-ignored tests | ~3s |
| `make check-full` | fmt + clippy + all tests including `#[ignore]` | ~minutes |

Cargo aliases are also available:

```bash
cargo lint          # clippy -- -D warnings
cargo test-fast     # non-ignored tests only
cargo test-full     # all tests including #[ignore] (spawns infer binary)
```

The `#[ignore]` tests include the C dump-textual sweep which compiles C source files through OCaml's `infer capture --dump-textual` and runs them through our Rust pipeline. These require the `infer` binary to be built.

## Code Style

- Use `cargo fmt` defaults (rustfmt). No custom rustfmt.toml.
- Derive traits in this order: `Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize`.
- Use `Box` to break recursive type cycles and for large enum variants (clippy will flag these).
- Prefer `impl Display` over custom `pp` methods for pretty printing.

## Crate Structure

The workspace lives in `infer-rs/` with crates under `infer-rs/crates/`:

- `sil` -- Core SIL types (Typ, Exp, Instr, Procdesc, Cfg, Tenv, BuiltinDecl)
- `textual` -- Textual IR parser, printer, transforms, to_sil conversion
- `absint` -- Abstract interpretation framework (RPO + WTO fixpoint)
- `analyses` -- Intraprocedural analyses (liveness, dead store reporter)
- `pulse` -- Pulse analysis engine (null deref, UAF, models)
- `diagnostics` -- Issue types, severity, reporting
- `ondemand` -- Parallel analysis runner with inter-procedural support
- `test-harness` -- Shared test infrastructure (Textual utils, fixtures, OCaml runner)
- `cli` -- CLI binary (`infer-rs`)

## Naming Conventions

- Rust module and type names should use idiomatic Rust style (`PtrKind`, not `Pk_pointer`; `IfKind::While`, not `Ik_while`).
- Keep a clear correspondence to the OCaml source. Add a doc comment noting the OCaml equivalent, e.g. `/// Mirrors OCaml's Typ.ikind`.
- Module file names use snake_case (`int_lit.rs`), types use CamelCase.

## Configuration and Flag Compatibility

The `config` crate holds all analysis configuration via `InferConfig`. It supports `.inferconfig` JSON files (shared with OCaml infer) and CLI flags.

**Adding a new config flag:**

1. Add the field to `InferConfig` in `config/src/lib.rs` with `#[serde(rename = "ocaml-flag-name")]`. The `rename` value must match OCaml's `--flag-name` from `Config.ml`.
2. Set the OCaml-matching default in `impl Default for InferConfig`.
3. If it should be a CLI flag, add `#[arg(long = "ocaml-flag-name")]` to `Cli` in `cli/src/main.rs` — the string must match the `serde(rename)` value.
4. Wire the CLI override in `Cli::to_config()`.

**What stays in sync automatically:** `.inferconfig` parsing (serde uses the rename), defaults.

**What you must keep in sync manually:** `#[arg(long)]` in CLI must match `#[serde(rename)]` in config. Both must match OCaml's `Config.ml`.

**OCaml `.inferconfig` compatibility:** unknown fields are silently ignored by serde (no `deny_unknown_fields`). CLI rejects unknown flags via clap.

**Global access:** call `config::init(cfg)` once at startup, then `config::get()` anywhere — no parameter threading needed (uses `OnceLock`).

**Config-dependent compliance work:** before treating a sweep mismatch as a Pulse logic bug, check
whether the source directory has a `.inferconfig` that changes modeling. The C Pulse suite uses
`pulse-model-free-pattern`, `pulse-model-malloc-pattern`, and
`pulse-model-realloc-pattern` to model wrapper functions. These patterns use OCaml `Str.regexp`
syntax from shared Infer configs, for example `\\(my\\|a\\)_malloc`.
Other supported config-driven models include `pulse-model-abort`,
`pulse-model-unreachable`, `pulse-model-return-{nonnull,this,first-arg,nullable}`,
`pulse-model-skip-pattern`, and `pulse-model-unknown-pure`.
`pulse-model-returns-copy-pattern` and `pulse-model-cheap-copy-type` are not simple generic
models; they depend on the OCaml unnecessary-copy pipeline and should not be "implemented" with
placeholder behavior.

- For CLI reproduction, pass `--inferconfig-path <path-to-.inferconfig>` explicitly.
- For library tests or custom harnesses, make sure config discovery starts from the intended source
  directory if the behavior depends on `.inferconfig`.
- The ignored store-textual sweep now mirrors OCaml config discovery by invoking the `infer-rs`
  CLI once per exported `.sil` from the originating source directory.
- OCaml walks upward from the starting working directory to filesystem root when searching for
  `.inferconfig`; it does not stop at `.git` or `.hg`.

## Relationship to OCaml Codebase

- The OCaml source of truth is in `infer/src/`. Read the `.mli` files for type definitions before modifying Rust types.
- The Rust types should be a faithful representation but not a mechanical transliteration. Use idiomatic Rust (enums, Box, Vec) rather than mimicking OCaml's representation.
- When the OCaml side changes a type that is mirrored in Rust, the Rust side must be updated to match.

## Testing

- Add `#[cfg(test)]` unit tests in each module for non-trivial logic.
- Roundtrip serialization tests (serialize then deserialize) are valuable for all core types.
- Integration tests comparing Rust and OCaml outputs go in a top-level `tests/` directory (future).

## Commits

- Keep commits focused: one logical change per commit.
- Run `cargo fmt && cargo clippy -- -D warnings && cargo test` before every commit.
- Use the `infer-rs` branch for all work.

## Rebasing onto updated main

When `main` moves forward with OCaml changes, follow this procedure to keep `infer-rs` in sync.

**1. Review what changed on main**

```bash
# Find the current merge base (last common ancestor)
git merge-base main infer-rs

# See the OCaml commits since our last rebase
git log --oneline $(git merge-base main infer-rs)..main

# Inspect the actual changes — focus on files we track
git diff $(git merge-base main infer-rs)..main -- \
  infer/src/textual/ \
  infer/src/pulse/ \
  infer/src/absint/ \
  infer/src/checkers/ \
  infer/src/IR/ \
  infer/tests/codetoanalyze/sil/

# For each relevant .ml/.mli change, read the diff carefully
```

Key areas to watch:
- `infer/src/IR/` — SIL types (Typ, Exp, Instr, Procdesc, Procname). Changes here may require updates to `sil` crate.
- `infer/src/textual/` — Textual parser, transforms, type verification. Changes here may require updates to `textual` crate.
- `infer/src/pulse*` — Pulse analysis. Changes may affect `pulse` crate models, transfer, or domain.
- `infer/src/absint/` — Abstract interpretation framework. Changes may affect `absint` crate.
- `infer/tests/codetoanalyze/sil/` — Test files we reference directly. New tests are opportunities; changed tests may break our assertions.

**2. Apply any needed changes to the Rust codebase**

Before rebasing, make adaptation commits on the `infer-rs` branch:
- Update Rust types to match changed OCaml types
- Update tests if OCaml test files we reference have changed
- Add new test coverage for new OCaml test files if applicable
- Ensure `cargo fmt --check && cargo clippy -- -D warnings && cargo test` passes

Commit these changes with a message like: `Adapt to main changes: <summary of what changed>`

**3. Rebase onto new main**

```bash
git rebase main
```

If conflicts arise:
- Conflicts in `infer/` (OCaml files): accept theirs — main is the source of truth for OCaml
- Conflicts in `infer-rs/` (Rust files): resolve manually, keeping our adaptations
- After resolving each conflict: `git add <files> && git rebase --continue`
- Run `cargo fmt && cargo clippy -- -D warnings && cargo test` after rebase completes
- If tests fail post-rebase, fix and amend the relevant commit or add a fixup commit
