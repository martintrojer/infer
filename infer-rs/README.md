# infer-rs: Rust Implementation of Infer's Pulse Analysis

Rust port of [Infer](https://fbinfer.com/)'s Pulse analysis engine for memory safety checking (null dereferences, use-after-free, memory leaks).

Current authoritative store-textual sweep: 52/55 C files. NPE: expected 131, found 134. Leaks: expected 20, found 20. UAF: expected 7, found 7.

**OpenSSL whole-program benchmark (74-file partial corpus, latest B-track):**

| metric                | OCaml (-j 1) | Rust (latest, -j 4)                                      |
|-----------------------|--------------|-----------------------------------------------------------|
| wall time             | `42.9s`      | `294.94s` latest out-of-box rebaseline |
| max RSS               | `~1.17 GB`   | `~12.6 GB` max RSS (`~8.55 GB` peak footprint)          |
| procs analyzed        | `570 / 570`  | `570 / 570`                                              |
| heap+wall aborts      | n/a          | `19 / 570` (`~3.3%`)                                       |
| max visit count       | n/a          | `4`                                                       |
| exit                  | clean (0)    | clean (0)                                                |

Whole-program slowdown vs OCaml in the latest out-of-box rebaseline:
**`~6.9×`**, down from `~70×` and
OOM-killed at the start of the perf sessions. The `OBJ_bsearch_ex_`
`max_visit_count=10001` pathology is no longer the dominant OpenSSL
story in the latest convergence probe; the long tail has shifted to
bounded-visit DES-family large-state procedures. Defaults:
`pulse-max-heap-mb = 2048`, `pulse-max-wall-secs = 60`; pass `0` to
disable each cap. Full summary in `docs/STATUS.md` and `docs/plans/`.
Use `scripts/bench_openssl_partial.sh` for repeated runs/medians.

Historical OpenSSL benchmark notes follow.

Exact summary-equality work now uses a semantic driver instead of raw JSON diffs:
`crates/test-harness/src/summary_compare.rs` canonicalizes both
`main.pre_post_list` and specialized summaries from `specialization.c`.
Current verified checkpoint:

- main summaries: `21 / 21` procedures match
- combined per-procedure harness: `Matching: 21`

The real exported `latent.c` issue-set compare is also exact again at
`(procedure, line, issue-type)`: `17` Rust issues, `17` OCaml issues, with no
Rust-only or OCaml-only entries. The remaining work in that cluster is now
summary-surface parity inside exported summaries, not current report counts.

OpenSSL benchmark status on this host:

- shared capture/export setup is now stable: repo clang on `PATH`, `CC=clang`,
  explicit macOS SDK `-isysroot`, and `./Configure darwin64-x86_64-cc no-asm`
- Rust now parses the full exported corpus: `753 / 753` `.sil` files, `0`
  parse errors
- on the latest fresh shared capture
  (`/tmp/infer-rs-openssl-20260417-095315-rebase-j`), OCaml
  `infer analyze --pulse-only --results-dir infer-out -j 1` completed in
  `589.76s` with about `2.55 GB` max RSS
- the rebased macOS parallel path is no longer stuck in the old immediate
  `-j > 1` startup failure: direct `.sil` Rust runs reached about `793%` CPU at
  `-j 8` and about `375%` CPU at `-j 4`
- but whole-program Rust timing is still blocked by memory growth in merged
  interprocedural runs on this benchmark:
  `-j 8` terminated abnormally after `190.81s` at about `24.5 GB` max RSS, and
  `-j 4` terminated abnormally after `690.77s` at about `33.2 GB` max RSS
- focused `whirlpool_block` tracing first showed that the dominant multiplier
  is retained fixpoint state, not just the current frontier: the old Rust
  baseline reached `2995` retained post snapshots at `36.1s` while the live
  frontier still held only `20` disjuncts
- `state_cmp` now mirrors OCaml `PulseAbductiveDomain.leq` more closely by
  comparing only the stack-reachable heap / attr graph and ignoring Rust-only
  helper caches such as `must_be_valid`, with focused regression tests for the
  reachable-vs-disconnected split
- that `state_cmp` cleanup was semantically correct but did not materially move
  the `whirlpool_block` hotspot by itself
- the large OpenSSL reduction came from matching OCaml's WTO revisit
  semantics: a new `TransferFunctions::exec_node(...)` hook lets Pulse mirror
  `AbstractInterpreter.MakeDisjunctiveTransferFunctions.exec_node_instrs`, so
  revisits re-execute only pre disjuncts that are new versus the retained node
  pre and join those results into the retained post
- on `whirlpool_block`, the new WTO parity path cut the traced Rust retained
  state from `2995` snapshots at `36.1s` to `366` at `10.1s`, `466` at
  `30.3s`, and `566` at `71.1s`; at `10.1s` the live frontier was already down
  to `1` disjunct and the retained max per node was `3`
- the matching OCaml `whirlpool_block` debug run on the same shared capture
  finished in `1m31s` and retained far less final post state:
  `152` post snapshots across `178` CFG nodes, about `98727` post heap nodes,
  `53889` edges, and `39663` attr entries, with no final node retaining more
  than `1` disjunct
- Rust Textual→SIL now lowers OCaml-exported `__sil_metadata_*` helper calls
  back to SIL metadata, with focused tests in `crates/textual/src/to_sil.rs`
- the sibling OCaml `infer` repo now locally fixes `infer debug
  --export-textual` for C/Java by regenerating textual from loaded procdescs
  after preanalysis/WTO setup; a focused C regression now locks in exported
  `abstract` / `nullify` / `exit_scope` /
  `variable_lifetime_begins` helpers
- on a fresh single-file OpenSSL `wp_block.c` export, those cleanup metadata
  helpers are now present, so the old export-boundary explanation is gone; the
  Rust filtered `whirlpool_block` run on that richer input now finishes in
  `4m52s` with `1222` retained states and top retained nodes
  `29,31,32,33,35,36,37,38`, so this remains Rust-side fixpoint work, not a
  missing textual import
- Rust Pulse now also executes exported `Metadata::ExitScope` semantically
  instead of treating all metadata as a no-op: dead post-stack temps/locals
  are removed while pre-rooted formals stay available for summary
  construction, matching the OCaml `ExitScope` intent
- that `ExitScope` fix materially improves the transient curve on the same
  richer single-file hotspot, but not its final fixpoint shape: a completed
  selected-node rerun now finishes in `4m58s` with `1222` retained states
  across `204` CFG nodes, `max_visit_count=4`, `max_node_disjuncts=8`,
  `pre_posts=2`, and the same top retained nodes
  `29,31,32,33,35,36,37,38`
- Rust Pulse now also executes exported `Metadata::VariableLifetimeBegins`
  semantically for non-global locals, mirroring OCaml
  `PulseOperations.realloc_pvar`: rebind the local to a fresh slot and mark
  ordinary scalar/pointer locals uninitialized unless it is a structured
  binding
- that `VariableLifetimeBegins` fix is also correctness-first and also leaves
  the final hotspot endpoint unchanged: a completed selected-node rerun now
  finishes in `4m56s` with the same `1222` retained states,
  `max_visit_count=4`, `max_node_disjuncts=8`, and top retained nodes
  `29,31,32,33,35,36,37,38`
- representative retained PRE/POST dumps for hot nodes `29`, `31`, and `38`
  are also structurally unchanged before vs after that fix: the selected
  blocks keep identical line counts, abstract-value counts, invalid/initialized
  attribute counts, `must_be_valid` counts, and variable-name sets; the raw
  block hashes still differ, but the first visible diff is local ordering
  (`ctx` vs `n`) rather than a new retained-state shape
- the remaining gap is therefore sharper: the selected nodes map to the line
  `540` `r++` / load / prune block and the line `752-755`
  `S.q[...] = L*` stores, while matching OCaml HTML on those lines ends with
  `Got 1 disjunct back` and smaller final PRE widths there, so the next work
  stays on retained-state convergence rather than exporter fidelity
- OCaml Pulse also keeps `Nullify` and `Abstract` as no-op metadata, so the
  next step is retained-state comparison on that block, not more metadata
  plumbing
- `--pulse-recency-limit 32` is now available as an opt-in OCaml-style
  experiment, but it is intentionally not the default: default-enabling it
  reintroduced the real `nullptr.c` `FN_nullptr_deref_old_bad` false
  negative, and on `whirlpool_block` it left the state shape essentially
  unchanged
- the first dominant hotspot, `ssl_set_client_disabled`, dropped from about
  `1m09s` to about `5.2s` after restoring the OCaml-style
  `equal_fast` / semantic-`leq` split
- there is still no final apples-to-apples whole-program timing claim: the
  remaining gap is the smaller residual loop-head convergence difference on
  `whirlpool_block` (Rust still above OCaml's `152`-snapshot final shape on
  the shared capture), retained invariant-map storage across hot procedures,
  remaining local Pulse cost, abnormal termination in merged runs, and
  exported-Textual proc-identity loss for some duplicate C names, not
  merge/callgraph setup
- current working interpretation of that memory gap: the primary hotspot
  driver still looks like a residual OCaml-parity / convergence gap, not just
  Rust copy overhead. The biggest `whirlpool_block` reductions so far came
  from parity fixes (`equal_fast` split and WTO revisit `exec_node(...)`
  parity), and the latest selected-node alpha signatures still form
  `4` growth tiers x `2` variants with exact `35:POST == 31:PRE`, which points
  at loop-cycle state growth rather than missed alpha-dedup.
- clone / storage cost is still a real secondary amplifier: Pulse states are
  still deeply owned, merged whole-program runs still hit tens of GB RSS, and
  the structural-sharing track is meant to reduce that physical duplication
  without pretending to solve the remaining semantic `whirlpool_block` split
  by itself
- the narrowed probe is now more faithful too: callgraph / filtering /
  summary-collection retain implicit global initializer deps for loads rooted
  in globals. Rust now also supports OCaml's `pulse-max-cfg-size` default
  (`15000`), so the filtered `whirlpool_block` repro keeps
  `__infer_globals_initializer_Cx` in the retained set but skips analyzing it
  as a large procedure, matching the OCaml single-file baseline more closely.
- with that skip in place, the fresh filtered release repro returns to the old
  hotspot shape instead of exploding on the initializer: `3` retained procs,
  `4m42s` total elapsed, and the familiar `1222` retained-state /
  `29,31,32,33,35,36,37,38 -> 8d:4v` checkpoint for `whirlpool_block`.
- the earlier forced-retention probe is still informative as an upper bound:
  when `__infer_globals_initializer_Cx` is actually analyzed, it grows to
  about `16k` live heap nodes by itself and pushes the following
  `whirlpool_block` slice into multi-million retained heap / edge totals. That
  hidden cost surface still matters for future scalability work, but the
  default OCaml-compatible path now skips it.

On the performance side, Rust now mirrors the OCaml split between cheap
disjunct equality and semantic subsumption more closely. `Comparable` has an
explicit `equal_fast(...)` hook, `DisjunctiveDomain::{join,dedup}` use that
cheap equality (the Rust analogue of OCaml
`PulseExecutionDomain.equal_fast` / `AbstractInterpreter.MakeDisjunctiveTransferFunctions.join_up_to`),
and loop widening still uses semantic `leq`. On the filtered OpenSSL hotspot
`ssl_set_client_disabled`, that keeps the exact same execution shape
(`173` transfer steps, `20` disjunct cap, hottest node `33:24`) while cutting
runtime from about `1m09s` to about `5.2s`, which shows the old cost was in hot
disjunct dedup/join comparisons rather than extra transfer work.

The ondemand summary store now also shares cached summaries through `Arc`
handles instead of cloning large summaries per caller. That helps avoid some
copy cost, but the new `live-fixpoint` heartbeat shows it is not the main
OpenSSL memory fix: on `whirlpool_block`, retained per-node invariant-map state
is still the dominant multiplier.

The newer `state_cmp` / WTO revisit work sharpens that further. `state_cmp`
now aligns with OCaml on stack-reachable graph comparison, but the main OpenSSL
win came from the new `exec_node(...)` hook: Pulse now mirrors OCaml
`exec_node_instrs` and stops re-executing already-known pre disjuncts on WTO
revisits. On `whirlpool_block`, that drops retained snapshots from `2995` at
`36.1s` in the old Rust trace to `366` at `10.1s` and `566` at `71.1s` in the
new trace. Rust is still above OCaml's final `152`, so the next fix direction
is the smaller residual loop-head convergence gap on the nodes that still
retain up to `4` disjuncts, not a new workaround cap.

A newer OCaml-backed latent-summary fix also preserves imported arithmetic guards through summary
recording and export, including reverse-pivoted linear equalities such as `neg_x = -x` that the
solver stores as `x = -neg_x`. Summary condition recording now keeps the caller-visible `-x`
shape instead of collapsing it to a dead temp or `0 == 0`, `simplify_for_summary(...)` rewrites
those dead arithmetic temps before pruning phi facts, and local invalid accesses only keep the
manifest+latent twin on non-manifest paths when the caller-sensitive signal comes from heap shape
or imported call-side validity rather than pure imported arithmetic. This restores
`if_negative_then_crash_latent` / `test_e2e_negated_actual_keeps_arithmetic_latent_summary`
without regressing the earlier cyclic-field-write latent-null behavior.

The latest OCaml-backed specialization pass now mirrors OCaml's dynamic-type path more closely:
the abductive domain tracks known dynamic types directly, `PulseSpecialization.apply` now seeds
dynamic-type constraints (the Rust analogue of `PulseArithmetic.and_dynamic_type_is_unsafe`)
instead of exporting `Closure(...)` attrs, specialization keys use `TypeName::CFunction(...)`,
and `__call_c_function_ptr` resolves known dynamic types before falling back to direct closure
attrs. `invoke` now matches.

Unknown-call fallback and summary export were also tightened again: bare pointer and bare `Tfun`
actuals materialize their missing pointee cell before havoc, unknown-call returns record
`ReturnedFromUnknown(actuals)`, specialized latent abort diagnostics are cached sideband and
rehydrated on apply, and summary normalization recreates caller-visible non-zero
`Invalid(ConstantDereference(k))` attrs when a value is only known constant through phi. Together
these changes bring `may_double_free_if_alias` to parity and restore the missing specialized
recursive invalidation surface in `two_pointers_recursion_bad`.

`specialization.c` is now fully clean in the semantic harness. The last analyzer-side fix keeps
OCaml's summary-import behavior for missing callee pre-edges: imported pre cells are now abduced
onto the caller with `read_heap(...)`, except for callee formal-stack bookkeeping cells for
value-style actuals, which still must not be replayed or the old `v -*-> v` self-edge bug comes
back. On the comparator side, `summary_compare.rs` now collapses witness atoms before anchored
affine rewrites, derives `is_int(...)` through exact-RHS, inverse-scaling, and eq-closure over
exported equalities, and drops redundant formula-only integer witnesses once an anchored closure is
available. The focused regressions around `invoke_itself_bad` and `two_pointers_recursion_bad` are
now green without widening the raw Rust summaries.

The comparator now also canonicalizes summary conditions through the same
affine/equality closure it already used for phi. That keeps OCaml's hidden
recursive actual conditions like `0 < a1` aligned with Rust's visible affine
form like `0 < add(-1, i.*)`, and drops the redundant exact-one upper-bound
artifact `add(-1, x) <= 0` when phi already fixes `x = 1`. This restored the
specialized-summary harness back to `Matching: 21` immediately after the
disjunctive `equal_fast` split, confirming the speedup did not come from
semantic drift.

Recent OCaml-backed parity work also restored `traces.c`: Rust now preserves branch-conditioned
null provenance from both real `Prune` instructions and model-generated
`free(NULL)` / `free(non-null)` splits, so locally branch-proven direct-formal null dereferences
stay manifest/suppressed while caller-controlled ones stay latent.

The latest interproc correctness pass restored two OCaml-backed behaviors that the previous
summary-parity work had regressed: leaf `MustBeValid` preconditions are now checked/imported even
when the callee pre value has no outgoing pre-heap edges, and summary replay now skips callee
formal-stack bookkeeping cells for value-style actuals while still replaying true lvalue/by-ref
actuals. This fixes the lost latent precondition regressions and removes the old bogus
`invoke(id)` / `add_one` / `add_two` `v -*-> v` self-edge without papering over the remaining
semantic mismatches.

The latest specialization/summarization cleanup tightened three more OCaml-backed surfaces:
summary normalization now strips post-summary `Initialized` attrs from hidden formal/local stack
roots after `restore_formals_for_summary`, unresolved `__call_c_function_ptr` now
conservatively initializes values reachable from the function pointer and actual roots before
model / unknown-call handling, and normal integer literal evaluation now reuses existing formula
representatives like OCaml `PulseFormula.absval_of_int`. Summary normalization also now filters
pre/post attrs with the same suitability rules as OCaml `PulseAttribute`, which drops exported
post-summary `ComparedToNullInThisProcedure` invalidations while keeping real caller-visible
invalidations such as `ConstantDereference(0)`. Together these fixes remove the old unresolved-call
`Initialized` gap from `invoke`-style summaries, bring `test_unalias` to parity, and stop
over-exporting the wrapper shape in `call_may_double_free_if_alias_bad`.

A newer OCaml-backed model/export pass now also conservatively initializes actual roots before
entering known models, matching the `OCamlModel` path in `Pulse.ml`, and exports
continue-derived latent invalid-access summaries with `diagnostic=None`, reconstructing the
diagnostic again during summary import. That aligned `may_double_free_if_alias` with OCaml on
latent diagnostic shape and caller-visible pointee `Initialized` attrs; the later visible-constant
invalidation fix and exact-RHS `is_int` anchoring closed the last remaining summary-surface delta
there.

The config surface now also supports OCaml's `pulse-force-continue` flag through both
`.inferconfig` and CLI override. Rust summaries now retain OCaml-style
`has_dropped_disjuncts` metadata, and known-callee calls only fall back to unknown-call semantics
when the selected summary is empty or marked incomplete for the same reason. That narrow
force-continue path is correct and tested. The latest checker pass also restores the missing
skipped-call continue for selected alias-specialized latent-invalid-access summaries with no
`ContinueProgram`, which is the OCaml-backed shape behind
`call_may_double_free_if_alias_bad`. Together with the newer integer-literal interning and
post-summary attr filtering, those fixes remain part of the current
`21 / 21` main-summary and `Matching: 21` widened-summary checkpoint.
`invoke`, `invoke_itself_bad`, `call_may_double_free_if_alias_bad`,
`may_double_free_if_alias`, `test_unalias`, and `two_pointers_recursion_bad`
now match.

`memory_leak.c` is also back at parity after history-aware invalid-access provenance/dedup. The
remaining published NPE delta is:

- `nullptr.c` (+1): accepted correctness-positive divergence. Rust reports the real
  `FN_nullptr_deref_old_bad` null dereference that OCaml intentionally misses because of recency
  forgetting in that test.
- `sizeof.c` (+2): accepted `--store-textual` / `--export-textual` fidelity limitation. Exported
  Textual lowers array `sizeof(...)` expressions to `<int[]>` without `nbytes` or array length, so
  the Rust roundtrip cannot constant-fold those branches.

The latest latent-invalid-access export pass also restores the real exported
`latent.c` issue surface to exact parity. Rust now keeps recovered caller abort
pre/posts in summaries while suppressing only duplicate manifest publication,
restores OCaml's local `AbortProgram` + `LatentAbortProgram` twin for
`traverse_and_crash_if_equal_to_root`, dedups latent invalid accesses by
caller-visible heap path rather than raw access location, and keeps mixed
local+imported direct-formal latent-invalid shapes out of the published report
surface. The local-vs-callsite publication split is tighter again too:
one-step cycle callees stay latent-only while callers reify the manifest null
deref, and local trailing field-write aborts keep both recovered latent null
paths plus the trailing manifest diagnostic without reintroducing the real
`FN_crash_after_six_nodes_bad` duplicate-manifest bug. The real-fixture
`FN_nonlatent_use_after_free_bad{,2}`, `latent_use_after_free`,
`manifest_use_after_free`, and `main` summary shapes now line up again on the
validated exported `latent.c` compare too: Rust coalesces the surviving zero
shared direct-formal latent-invalid path only after the mixed-depth /
path-local filters have accepted it, and summary import now conjoins equality
between caller actuals when multiple callee direct-formal deref roots map to
one shared summary value instead of arbitrarily binding that value to the first
actual. That keeps the OCaml-looking `{b == 0}` latent-invalid summary shape
without regressing `main` back to a manifest `NULLPTR_DEREFERENCE`. Serialized
Pulse issue traces are a little richer now too: the existing one-line
qualifier stays the same, the `trace` field appends minimal
invalidation/access history signatures, and `report.json` now also carries a
minimal structured `bug_trace` / `bug_trace_{length,max_depth}` payload plus
flat OCaml-style `bug_type` / `severity` / `category` aliases, stable `key`,
`node_key`, `hash`, `procedure_start_line`, and empty `extras`, all derived
from the same issue metadata for easier latent-chain debugging. The structured
bug trace is also a little closer to OCaml now: access traces reorder caller
provenance before a synthetic `when calling ... here` step and then descend
into the callee parameter/value that triggers the final invalid access,
invalidation traces synthesize the outer callee formal before the inner call
when a translated callee diagnostic preserves only the deeper formal, and
modelled allocation call/return pairs such as `malloc` collapse to one
`allocated by call to ... (modelled)` step. Caller-reified UAF qualifiers are
also a bit closer to OCaml now: when the access history still identifies the
outer callee call, the top-level `qualifier` switches from the generic
"accessing address that ..." wording to a `The call to ... may trigger ...`
shape.

The latest correctness pass also keeps recoverable invalid-access paths from continuing after the
error has already been classified: transfer-side load/store recoverable errors and C-model
recoverable errors now stop instead of exporting `ContinueProgram + AbortProgram`, with focused
regressions for null-formal stores and double-free. That cleanup is correct and stays, but it does
not explain the now-clean `specialization.c` comparator by itself. A broader checker-side attempt
to recover non-exit latent invalid accesses when another path reaches exit was cross-checked
against OCaml and reverted because it over-published latent summaries in `test_alias`,
`test_unalias`, and wrapper / recursion cases. A separate direct-formal-load synthetic repro was
also cross-checked against OCaml: `formal_load_then_exit` exports a single `ContinueProgram`, so
Rust now locks that down with `checker::tests::test_formal_load_then_exit_stays_continue_only`
instead of keeping the old ignored test alive as a target.

The latest direct-formal ordering fix closes the main `may_double_free_if_alias` shape bug without
cheating the counts: Rust now stamps `MustBeValid` / `MustBeInitialized` summary attrs with real
monotonic per-state timestamps instead of hardcoding `0`, and latent-invalid-access summary
shaping now orders direct-formal accesses by `(timestamp, location)` rather than raw `.sil`
location alone. That removes the extra latent branch and brings the raw main summary down to the
OCaml shape of `x == 0`, `x > 0 && y == 0`, and `x > 0 && y > 0`. After the later
integer-literal interning, summary-import fix, and eq-closure comparator pass, `specialization.c`
is now fully matched; the remaining parity work is outside this harness.

See [docs/STATUS.md](docs/STATUS.md) for detailed compliance data and
[docs/STORE_TEXTUAL.md](docs/STORE_TEXTUAL.md) for the accepted exported-Textual limitation.

## CLI Usage

### Full pipeline (recommended)

Capture C/C++ source with OCaml infer, then analyze with infer-rs:

```bash
infer-rs -j 4 --pulse-only -- clang -c file.c
infer-rs -j 4 --pulse-only -- make
infer-rs -j 4 --pulse-only -- gcc -c src/*.c
```

This shells out to `infer --store-textual` for capture, exports textual SIL from
`capture.db`, then analyzes all exported `.sil` files.

### Analyze existing capture

If you've already run `infer capture --store-textual`, just point infer-rs at the results:

```bash
# Uses ./infer-out by default
infer-rs --pulse-only

# Or specify results directory
infer-rs --pulse-only --results-dir /path/to/infer-out
```

`--results-dir` is the convenient end-to-end path and includes the
`infer debug --export-textual` step. For fair Rust-only timing, export textual
once and run `infer-rs` directly on the `.sil` files instead.

### Direct .sil files (debugging)

Analyze textual SIL files directly, bypassing capture:

```bash
infer-rs --pulse-only file.sil
infer-rs --pulse-only *.sil
```

When multiple `.sil` files are passed, `infer-rs` parses them in parallel, merges the resulting
`Cfg`/`Tenv`, and then runs analysis once over the unified program so cross-file calls can resolve
to in-memory summaries.

For focused debugging, `--procedures-filter` mirrors OCaml's regex filter. In interprocedural mode
the filtered run keeps matching root procedures plus their transitive callees, so narrowing to one
hot procedure does not silently drop the summaries it depends on:

```bash
infer-rs --pulse-only --procedures-filter 'ssl_set_client_disabled' t1_lib.sil
infer-rs --pulse-only --procedures-filter 't1_lib\\.c:ssl_set_client_disabled' *.sil
```

This direct-`.sil` mode is also the right path for apples-to-apples Rust timing
after textual export, because it excludes capture/export overhead.

### CLI flags

| Flag | Description |
|------|-------------|
| `--pulse-only` | Run only the Pulse checker |
| `--liveness-only` | Run only the liveness/dead store checker |
| `--pulse-report-issues-for-tests` | Emit suppressed Pulse issues as distinguished `*** SUPPRESSED ***` test reports |
| `--pulse-force-continue BOOL` | Override OCaml-compatible force-continue fallback for incomplete known callees |
| `-j N` / `--jobs N` | Number of parallel worker threads (default: all CPUs) |
| `-o` / `--output DIR` | Output directory for report.json (default: `infer-rs-out`) |
| `--results-dir DIR` | Infer results directory containing capture.db (default: `infer-out`) |
| `--infer-bin PATH` | Path to infer binary (default: auto-detect) |
| `-q` / `--quiet` | Suppress progress output |
| `--pulse-max-disjuncts N` | Max disjuncts per program point (default: 20) |
| `--pulse-widen-threshold N` | Loop-head visit threshold before widening (default: 3) |
| `--pulse-recency-limit N` | OCaml-style heap-edge recency cap experiment (unset by default in Rust) |
| `--pulse-intraprocedural-only` | Disable inter-procedural analysis |
| `--max-widens N` | Max widenings before fixpoint gives up (default: 10000) |
| `--debug-level-analysis N` | 0=quiet, 1=per-instruction, 2=full state dumps |
| `--trace-ondemand` | Emit on-demand scheduler progress snapshots through the logger |
| `--procedures-filter REGEX` | OCaml-compatible procedure filter: `proc_regex` or `source_regex:proc_regex` |
| `--debug-fixpoint-nodes N1,N2,...` | Rust-only debug aid: dump final retained fixpoint shape for selected CFG node IDs |
| `--inferconfig-path FILE` | Path to .inferconfig file |
| `--pulse-model-abort PROCNAME` | Model an exact procname as non-returning (repeatable) |
| `--pulse-model-unreachable PROCNAME` | Model an exact procname as unreachable (repeatable) |
| `--pulse-model-free-pattern REGEX` | Model matching functions as wrappers to `free(3)` |
| `--pulse-model-malloc-pattern REGEX` | Model matching functions as wrappers to `malloc(3)` |
| `--pulse-model-realloc-pattern REGEX` | Model matching functions as wrappers to `realloc(3)` |
| `--pulse-model-return-nonnull REGEX` | Model matching functions as returning a non-null value |
| `--pulse-model-return-this REGEX` | Model matching methods as returning `this` / `self` |
| `--pulse-model-return-first-arg REGEX` | Model matching methods as returning the first source-language arg |
| `--pulse-model-return-nullable REGEX` | Model matching methods as returning null-or-non-null |
| `--pulse-model-skip-pattern REGEX` | Skip matching functions and treat them as unknown calls |
| `--pulse-model-unknown-pure REGEX` | Model matching functions as unknown pure calls (repeatable) |

`pulse-model-{free,malloc,realloc}-pattern`,
`pulse-model-return-{nonnull,this,first-arg,nullable}`, `pulse-model-skip-pattern`, and
`pulse-model-unknown-pure` are compatible with OCaml Infer's shared `.inferconfig` files,
including the `Str.regexp` syntax used in test suites such as `\\(my\\|a\\)_malloc`.
`pulse-max-disjuncts`, `pulse-widen-threshold`, and `max-widens` are also
compatible with shared `.inferconfig` and the matching CLI flags.
`pulse-force-continue` is also compatible with shared `.inferconfig`; the Rust default remains
`true` to match OCaml.
`pulse-recency-limit` is also compatible with shared `.inferconfig`, but Rust
intentionally leaves it unset by default. OCaml defaults that flag to `32`;
enabling the same cap by default in Rust reintroduces the real
`FN_nullptr_deref_old_bad` false negative, so the cap is kept as an explicit
experiment knob rather than baseline behavior.
`pulse-model-{abort,unreachable}` follow OCaml's exact-procname list semantics.
`trace-ondemand` is also `.inferconfig`/CLI compatible with OCaml's flag name. Unless `RUST_LOG`
is already set, enabling it defaults the logger to include `ondemand=info`, which emits wave
start/end lines plus periodic scheduler snapshots with completed summaries,
throughput, and ETA. Long-running Pulse procedures also emit `pulse-progress`
heartbeats for the active frontier and `live-fixpoint` heartbeats for retained
invariant-map state, which is the main debugging surface for the current
OpenSSL memory investigation.
`procedures-filter` is also `.inferconfig`/CLI compatible with OCaml's flag name and split
syntax. A single regex filters procnames; `source_regex:proc_regex` filters both source file and
procname.
`debug-fixpoint-nodes` is a Rust-only debug knob for narrowed WTO/disjunctive
parity work. Use it with `RUST_LOG=pulse=debug` (or broader `debug`) to log
the final retained disjunct / visit counts for selected CFG nodes, and add
`--debug-level-analysis 2` when you need the full retained PRE/POST dumps for
those nodes.
`pulse-model-returns-copy-pattern` is still intentionally unsupported because Rust does not yet
implement OCaml's non-disjunctive unnecessary-copy tracking.

### infer binary discovery

The `infer` binary is found automatically in this order:

1. `--infer-bin <path>` CLI flag
2. `INFER_BIN` environment variable
3. `../infer/bin/infer` relative to the workspace root (in-repo builds)
4. `infer` on `PATH`

### Output

- `<output>/report.json` — JSON report matching OCaml's format
- stdout — issues in `issues.exp` format for comparison with OCaml test expectations
- Exit code: 0 = no issues, 1 = error, 2 = issues found

## Building and Testing

```bash
cd infer-rs
make check          # fmt + clippy + unit/integration tests (~3s)
make check-full     # + C dump-textual sweep (~60s, needs infer binary)
# authoritative compliance sweep used for STATUS.md:
cargo test -p pulse --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture
```

`make check-full` exercises the older `capture --dump-textual` path as a secondary regression check.
`make check` / `make check-full` intentionally run the cargo test phase with
`RUST_TEST_THREADS=1`: the Pulse `end_to_end` integration binary still shares
global analysis state, and cargo's default parallel test scheduling can
interleave whole end-to-end runs and deadlock that harness even though the
analysis logic itself is green.
The published compliance numbers in [docs/STATUS.md](docs/STATUS.md) come from the
`--store-textual` + `--export-textual` sweep because that matches the CLI pipeline.
That ignored sweep now invokes `infer-rs` once per exported `.sil` from the originating source
directory, so OCaml-style upward `.inferconfig` discovery is part of the published totals. The
sweep helper also rebuilds `infer-rs` once per test process so the published numbers do not
silently reuse a stale `target/{debug,release}/infer-rs` binary.

## Project Structure

```
infer-rs/
  crates/
    sil/             Core SIL types (Typ, Exp, Instr, Procdesc, Cfg, Tenv)
    textual/         Textual IR parser, printer, transforms, to_sil
    absint/          Abstract interpretation (RPO + WTO fixpoint)
    analyses/        Liveness analysis + dead store reporter
    pulse/           Pulse engine (null deref, UAF, models, interproc)
    diagnostics/     Issue types and reporting
    ondemand/        Parallel analysis runner
    test-harness/    Test infra: OCaml runner, semantic summary comparison
    config/          Configuration (.inferconfig, CLI flags)
    cli/             CLI binary (infer-rs)
  docs/              STATUS, PULSE, architecture docs
  scripts/           Trace comparison tools
  test-data/         Test fixtures (.sil files)
```

## How It Works

1. OCaml's `infer capture --store-textual` captures C/C++ source and stores textual SIL in `capture.db`
2. `infer debug --export-textual` exports `.sil` files + `manifest.json` mapping source→sil→procedures
   (for local C/Java work, the sibling OCaml exporter now regenerates from
   loaded procdescs after preanalysis/WTO so exported textual matches the
   analysis CFG more closely)
3. `infer-rs` parses textual SIL files in parallel, transforms them, and converts each module to SIL
4. The per-file `Cfg`/`Tenv` results are merged into one in-memory program
5. Pulse analysis runs: WTO fixpoint with disjunctive domain, biabduction, interprocedural summary application
   and OCaml-backed latent/manifest invalid-access classification
6. Reports NULLPTR_DEREFERENCE, USE_AFTER_FREE, MEMORY_LEAK_C issues with original source file paths

## Key Design Decisions

- **Textual as bridge**: human-readable SIL serialization is the interop boundary between OCaml and Rust
- **Cross-reference OCaml**: analysis logic follows OCaml's approach, cross-referenced against source in `infer/src/pulse/`
- **Correctness over counts**: keep semantically correct OCaml-backed behavior even when sweep totals move temporarily; accepted divergences are documented instead of hidden
- **Test through comparison**: compare against OCaml's `issues.exp` for compliance
- **Per-instruction tracing**: `--debug-level-analysis` + `scripts/compare_traces.py` for debugging divergences
- **Retained-block comparison**: `scripts/compare_fixpoint_blocks.py` compares
  selected Rust retained PRE/POST dumps across two runs and reports coarse
  structural signatures plus the first normalized diff
- **Scheduler tracing for long runs**: `--trace-ondemand` uses the logger to expose wave progress and ETA during merged interproc analysis, and now also emits `pulse-progress` frontier heartbeats plus `live-fixpoint` retained-state heartbeats for long-running procedures

## Documentation

- [docs/STATUS.md](docs/STATUS.md) — Compliance data, crate map, migration phases
- [docs/PULSE.md](docs/PULSE.md) — Pulse engine architecture
- [TODO.md](TODO.md) — Remaining gaps and backlog
- [CLAUDE.md](CLAUDE.md) — Development rules, build requirements, rebase recipe
