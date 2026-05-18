# EqZero local sideband e2e shape spec

Date: 2026-05-18  
Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-2`  
Source: `infer/tests/codetoanalyze/c/pulse/latent.c`  
OCaml run: `/home/mtrojer/infer/infer/bin/infer` on C with `--pulse-only --debug --debug-level 3`; summaries dumped with `infer debug --dump-json-summaries`. I captured both production default (`--pulse-force-continue=true`) and `--no-pulse-force-continue`; the Rust e2e pins are default-config unit fixtures, and match the OCaml default shape for `crash_after_two_nodes_bad`.  
Rust runs: HEAD `c8e19d914b` and attempt 3 `7bbd862f5c`, using the same textual fixtures as the two e2e tests (`test_e2e_deref_then_free_then_deref_keeps_npe_latent` and `test_e2e_latent_cycle_summary_shapes_match_ocaml_subset`) and default Rust config.

Artifacts:

- OCaml C debug dir: `/tmp/eqzero-ocaml-debug-force` (default force-continue) and `/tmp/eqzero-ocaml-debug` (`--no-pulse-force-continue`).
- OCaml summary JSON: `/tmp/eqzero-ocaml-debug-force/all_summaries.json`, `/tmp/eqzero-ocaml-debug/all_summaries.json`.
- OCaml HTML: `/tmp/eqzero-ocaml-debug-force/captured/latent.c.8b2bfb96330f167d/{deref_then_free_then_deref_bad.ee87bf63ca3dbce3.html,crash_after_two_nodes_bad.515e00c0a9b7f21a.html}`.
- Rust shape JSON extracted from e2e fixtures: `/tmp/e2e_default_shape_worker-2.json` (HEAD), `/tmp/e2e_default_shape_eqzero_attempt3.json` (attempt 3).

## Important config note

The user request says to use the same `--no-pulse-force-continue` config the tests use. The C-suite issues.exp does use `--no-pulse-force-continue`, but the two Rust e2e tests in question call `run_pulse_inter()` directly and do **not** initialize config, so they use Rust defaults (`pulse_force_continue=true`). This matters only for the cycle shape:

- OCaml default (`force_continue=true`) for `crash_after_two_nodes_bad`: `ContinueProgram, LatentInvalidAccess, LatentInvalidAccess, AbortProgram`.
- OCaml `--no-pulse-force-continue` for C `latent.c`: `LatentInvalidAccess, LatentInvalidAccess, AbortProgram`.
- Rust e2e golden pin is a Rust-side subset/normalization target: `ContinueProgram, LatentInvalidAccess, AbortProgram, AbortProgram`.

So the actionable guard for future Rust changes is the e2e pin plus the OCaml row semantics below, not raw OCaml `--no-pulse-force-continue` kind count alone.

## 6-shape comparison

### 1. `deref_then_free_then_deref_bad`

#### OCaml golden (C `latent.c`, both default and `--no-pulse-force-continue`)

Kinds in order:

1. `LatentInvalidAccess(x)`
2. `AbortProgram` with `AccessToInvalidAddress` / `USE_AFTER_FREE` (`CFree`) on `x`

Row details:

| row | kind | diagnostic payload | path condition | key post attrs / heap |
|---:|---|---|---|---|
| 0 | `LatentInvalidAccess(x)` | No embedded JSON `diagnostic` field; semantically a latent null deref from `PotentialInvalidAccessSummary` on `x`/`v2` | `v2 = 0`; term eqs `0=v2`, `42=v3` | `v1*->v2`, `v2*->v3`; `v2` has `Initialized, WrittenTo` **only**; `v3` has `Initialized, Invalid(ConstantDereference(42))` |
| 1 | `AbortProgram` | `AccessToInvalidAddress`, issue `USE_AFTER_FREE`, invalidation `CFree`, invalid address `x` | `0 < v2`; term eqs `42=v3`, `[a1 + 1]=v2` | `v2` has `Initialized, Invalid(CFree), WrittenTo`; `v3` keeps `Invalid(ConstantDereference(42))` |

Invariant: OCaml does **not** materialize `Invalid(ConstantDereference(0))` on the direct-formal pointee (`x.*` / `v2`) in the latent null/free split. The latent null obligation is sideband-derived; the stored constant `42` remains an ordinary concrete `Invalid` on the payload value.

#### Rust HEAD (`c8e19d914b`) e2e fixture

Kinds in order:

1. `LatentInvalidAccess`
2. `AbortProgram`

Row details:

| row | kind | diagnostic payload | path condition | key post attrs / heap |
|---:|---|---|---|---|
| 0 | `LatentInvalidAccess` | `NULLPTR_DEREFERENCE`, `ConstantDereference(0)`, addr `v2` | `v2 = 0@0` | `v1*->v2`, `v2*->v3`; **extra** `v2: Invalid(ConstantDereference(0))`; `v3: Invalid(ConstantDereference(42))` |
| 1 | `AbortProgram` | `USE_AFTER_FREE`, `CFree`, addr `v2` | `0 < v2@0` | `v2: Invalid(CFree)`; `v3: Invalid(ConstantDereference(42))` |

Divergence from OCaml: row 0 has the correct latent-NPE diagnostic payload required by the Rust e2e test, but gets there by persisting an OCaml-wrong concrete `Invalid(0)` attr on `x.*`.

#### Rust attempt 3 (`7bbd862f5c`) e2e fixture

Kinds in order:

1. `LatentInvalidAccess`
2. `AbortProgram`

Row details:

| row | kind | diagnostic payload | path condition | key post attrs / heap |
|---:|---|---|---|---|
| 0 | `LatentInvalidAccess` | **None** | `v2 = 0@0` | `v2` no longer has `Invalid(0)`; `v3: Invalid(ConstantDereference(42))`; extra stale/materialization surface `v6*->v2`, `v6: WrittenTo` |
| 1 | `AbortProgram` | `USE_AFTER_FREE`, `CFree`, addr `v2` | `0 < v2@0` | same UAF shape as HEAD |

Attempt-3 divergence: it fixed the OCaml attr invariant (`x.*` has no `Invalid(0)`) but dropped the Rust-visible latent invalid-access diagnostic payload. This breaks `test_e2e_deref_then_free_then_deref_keeps_npe_latent`, which explicitly requires `summary.pre_posts` to contain a `LatentInvalidAccess` with `NullptrDereference` diagnostic.

### 2. `crash_after_two_nodes_bad`

#### OCaml golden (C `latent.c`, default `pulse_force_continue=true`)

Kinds in order:

1. `ContinueProgram`
2. `LatentInvalidAccess(q)`
3. `LatentInvalidAccess(q->next)`
4. `AbortProgram` with `NULLPTR_DEREFERENCE`

Row details:

| row | kind | diagnostic payload | path condition | key post attrs / heap |
|---:|---|---|---|---|
| 0 | `ContinueProgram` | None | none | force-continue unknown-effect row; `q`/successor values are initialized/written, no invalid attrs |
| 1 | `LatentInvalidAccess(q)` | No embedded JSON diagnostic; sideband latent null on `q`/`v2` | `v2 = 0`; term eq `0=v2` | cycle heap `v1*->v2`, `v2.next->v3`, `v3*->v4`, `v4.next->v5`, `v5*->v2`; no `Invalid(0)` attrs |
| 2 | `LatentInvalidAccess(q->next)` | No embedded JSON diagnostic; sideband latent null on `q->next` / `v4` | `v4 = 0` and `v2 != 0`; term eq `0=v4`; atom `v2 != 0` | same cycle heap; no `Invalid(0)` attrs |
| 3 | `AbortProgram` | `AccessToInvalidAddress`, issue `NULLPTR_DEREFERENCE`, invalidation `ConstantDereference(0)`, invalid address `crash` | `v2 != 0`, `v4 != 0`, `[v2 - v4] != 0` | same cycle heap; no relevant invalid attrs on q/q->next |

Under `--no-pulse-force-continue`, OCaml drops row 0 and gives `LatentInvalidAccess(q), LatentInvalidAccess(q->next), AbortProgram`.

#### Rust HEAD (`c8e19d914b`) e2e fixture

Kinds in order (this is the current e2e pin):

1. `ContinueProgram`
2. `LatentInvalidAccess`
3. `AbortProgram`
4. `AbortProgram`

Row details:

| row | kind | diagnostic payload | path condition | key post attrs / heap |
|---:|---|---|---|---|
| 0 | `ContinueProgram` | None | none | unknown-effect force-continue row; no invalid attrs |
| 1 | `LatentInvalidAccess` | `NULLPTR_DEREFERENCE`, `ConstantDereference(0)`, addr `v2` | `v2 = 0@1` | cycle heap `v1*->v2`, `v2.node.next->v3`, `v3*->v4`, `v4.node.next->v5`, `v5*->v2`; no invalid attrs |
| 2 | `AbortProgram` | `NULLPTR_DEREFERENCE`, `ConstantDereference(0)`, addr `v7` | `v2 != 0@1` | one-node crash import shape: `v3*->v2`; no invalid attrs |
| 3 | `AbortProgram` | `NULLPTR_DEREFERENCE`, `ConstantDereference(0)`, addr `v9` | `v2 != v4@1`, `v2 != 0@1`, `v4 != 0@1` | two-node non-equal crash shape; no invalid attrs |

Rust HEAD intentionally normalizes/subsumes OCaml's second latent (`q->next`, condition `q != 0 && q->next == 0`) into a manifest abort row rather than exporting a second `LatentInvalidAccess`; this is the e2e subset expected by `test_e2e_latent_cycle_summary_shapes_match_ocaml_subset`.

#### Rust attempt 3 (`7bbd862f5c`) e2e fixture

Kinds in order:

1. `ContinueProgram`
2. `LatentInvalidAccess`
3. `AbortProgram`
4. `LatentInvalidAccess`
5. `AbortProgram`

Row details:

| row | kind | diagnostic payload | path condition | key post attrs / heap |
|---:|---|---|---|---|
| 0 | `ContinueProgram` | None | none | force-continue unknown-effect row; no invalid attrs |
| 1 | `LatentInvalidAccess` | `NULLPTR_DEREFERENCE`, `ConstantDereference(0)`, addr `v2` | `v2 = 0@1` | same q-null latent row as HEAD |
| 2 | `AbortProgram` | `NULLPTR_DEREFERENCE`, `ConstantDereference(0)`, addr `v7` | `v2 != 0@1` | one-node crash import shape; no invalid attrs |
| 3 | `LatentInvalidAccess` | **None** | `v2 != 0@1` | duplicate state shape of row 2 (`v3*->v2`) but with latent kind and no diagnostic |
| 4 | `AbortProgram` | `NULLPTR_DEREFERENCE`, `ConstantDereference(0)`, addr `v9` | `v2 != v4@1`, `v2 != 0@1`, `v4 != 0@1` | two-node non-equal crash shape; no invalid attrs |

Attempt-3 divergence: it adds an orphan diagnostic-less `LatentInvalidAccess` row on the `q != 0` / one-node crash path. This is neither the OCaml `q->next == 0` latent row nor the Rust e2e subset row. It should be treated as a malformed sideband-recovery artifact.

## Invariant breakdown for future attempts

1. **Do not persist local EqZero as ordinary `Invalid(ConstantDereference(0))` on heap/formal addresses.** For `deref_then_free_then_deref_bad` row 0, `x.*`/`v2` must have `Initialized, WrittenTo` only; the only `Invalid` in that row should be the stored constant payload `Invalid(ConstantDereference(42))` on `v3`.
2. **Still export a Rust-visible latent-NPE diagnostic for direct-formal EqZero latent rows.** The e2e guard requires `LatentInvalidAccess` with `IssueTypeId::NullptrDereference`. Since OCaml JSON stores `LatentInvalidAccess(address, must_be_valid)` rather than a concrete diagnostic payload, Rust needs a non-attr sideband/field that can synthesize the diagnostic without reintroducing `Invalid(0)`.
3. **Distinguish ordinary concrete `Invalid` attrs from EqZero sideband.** Stored constants such as `42`, UAF `CFree`, and imported/recovered invalid attrs must remain normal attributes and must not be stripped when consuming the EqZero sideband.
4. **A `LatentInvalidAccess` row must not be diagnostic-less unless it carries an explicit OCaml-style address/must-be-valid sideband that callers/reporting can reify.** Attempt 3 violates this twice: the direct-formal row loses its diagnostic, and the cycle path gains a diagnostic-less orphan latent row.
5. **Do not add new latent rows for abort states that already have the Rust/OCaml-subset manifest abort shape.** In `crash_after_two_nodes_bad`, the one-node crash import path (`v2 != 0`, heap with `v3*->v2`) must stay a single `AbortProgram`, not `AbortProgram` plus a sideband-only `LatentInvalidAccess` twin.
6. **Preserve ordering:**
   - `deref_then_free_then_deref_bad`: `[LatentInvalidAccess(NULLPTR sideband/diagnostic), AbortProgram(USE_AFTER_FREE)]`.
   - Rust e2e cycle subset: `[ContinueProgram, LatentInvalidAccess(q == 0), AbortProgram(one-node crash), AbortProgram(two-node crash)]`.
7. **Cycle sideband specificity:** If future work decides to model OCaml's full `q->next == 0` latent row, it must be a real `LatentInvalidAccess(q->next)` equivalent (condition `q != 0 && q->next == 0`, diagnostic reifiable as NPE) and the e2e expected shape must be updated deliberately. It must not appear as attempt 3's condition `q != 0` diagnostic-less row.
8. **Add pin tests beyond file-level counts:** file-level `latent.c 10/4` was insufficient. Any new implementation must pass both e2e tests and should assert (a) no `Invalid(0)` on `deref_then_free_then_deref_bad` latent row's direct-formal pointee, (b) latent row still has/reifies `NullptrDereference`, (c) no diagnostic-less latent orphan row in `crash_after_two_nodes_bad`.

## Concrete alternative approach (not attempt 3)

Instead of storing EqZero potential invalid access as an exported attribute and then stripping it during summary normalization, introduce an explicit non-attribute latent invalid-access sideband in the summary row model:

- Add an OCaml-shaped field to Rust `PrePost`/stopped-state metadata, e.g. `latent_invalid_access: Option<{addr, must_be_valid/location/history, issue_type}>`, separate from `diagnostic` and separate from `Attribute::Invalid`.
- Local EqZero + heap + MustBeValid populates this sideband and never writes `Invalid(0)`.
- Summary export/reporting can synthesize a `Diagnostic::AccessToInvalidAddress` from that sideband for Rust tests/callers that expect `pp.diagnostic`, but the post attrs remain OCaml-shaped.
- Abort-state recovery should only create a latent sideband when it has a precise EqZero target and should suppress/dedup against an existing `AbortProgram` with the same state/path. It must never create a diagnostic-less `LatentInvalidAccess` row.

This is a different design from attempt 3's transient attr/strip pipeline and should avoid both observed failures.
