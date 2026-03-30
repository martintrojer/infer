# infer-rs TODO

## OCaml parity gaps (should match OCaml behavior)

### Compliance gaps by impact (store-textual sweep: 52/55 files, NPE 130/135, Leaks 20/20, UAF 10/7)

**NPE Over-detection (FPs, +7 total):**

1. **sizeof array** (+2): sizeof.c. `<int[]>` textual loses array length. Blocked on OCaml upstream.
2. **nullptr_more.c** (+2): write_deref pre-edges too aggressive for some patterns.
3. **nullptr.c** (+1): interproc biabduction (call_incr_deref_with_alias_good aliased formals, call_no_return_good write-through-ptr+noreturn, FN_nullptr_deref_old_bad bonus TP).
4. **angelism.c** (+1): remaining interproc issue.
5. **offsetof_expr.c** (+1): `FN_test_offsetof_expr_nonlit_bad` — bonus TP (OCaml FN_).

**NPE Under-detection (FNs, -12 total):**

6. **Function pointer dispatch** (-5): funptr.c (11→6). Specialization loop wired in CLI. Direct dispatch + single-level specialization work. Remaining: returned funptr, struct callbacks, complex multi-arg patterns.
7. **Deep interproc / models** (-3): initlistexpr.c(-3).
8. **Latent chain / loop depth** (-2): latent.c. Two sub-issues: (a) `propagate_latent` chain needs `is_manifest` to check linear_eqs, blocked on `read_heap` pre-edge overwrite fix; (b) `traverse_and_crash_if_equal_to_root` produces fewer loop-unrolling disjuncts (rust=3, ocaml=7).
9. **Function-pointer wrappers** (-2): memory_leak.c NPEs.
10. **Other** (-1): compound_literal.c(-1).

**Leak differences (net 0, +3 / -3):**

11. **cleanup_attribute.c** (+2): `__attribute__((cleanup()))` GCC extension not modeled. Cleanup function (free) called automatically at scope exit — not modeled.
12. **nullptr.c** (-1): `null_alias_bad` — formal parameter overwritten with malloc, missing `restore_formals_for_summary` logic.
13. **memory_leak.c** (-1): funptr wrapper pattern (`malloc_func`).

**UAF over-detection (+3 FPs):**

14. **latent.c** (+3): Latent issue propagation FPs — `propagate_latent` chain not working, needs `read_heap` pre-edge overwrite fix for linear_eqs in `is_manifest`.
15. **interprocedural.c** (+1): `conditional_free_then_use_latent` reported at both callee and caller.
16. **specialization.c** (-1): Missing funptr-based UAF.

**Skipped files (3):** infinite.c (106 procs with infinite loops/Ackermann), recursion.c, recursion2.c — fixpoint exhaustion.

**Files matching OCaml (20 clean, NPE+Leaks):** enum.c, frontend.c, memcpy.c, getcwd.c, shift.c, var_arg.c, ternary.c, nullptr.c, lists.c, list_checks.c, uninit.c, assert_failure.c, specialization.c, arithmetic.c, integers.c, abduce.c, struct_values.c, interprocedural.c, memory_leak_more.c, issues_abort_execution.c, traces.c.

### Textual pipeline gaps

- **Closure/Apply/If expression conversion** (`to_sil.rs:314`): returns `Exp::zero()` placeholder.
- **DeclEnv enhancements** (`decls.rs`): Missing variadic proc tracking, generics status.
- Language-specific (defer): FixHackWrapper, FixHackInvokeClosure, TransformClosures, verify_variadic_position, SSA restoration.

### SIL test gaps (skipped procs)

- **Virtual dispatch in loads** (2 procs in static_types.sil)
- **Devirtualization return values** (5 procs in virt.sil)
- **Cross-file resolution** (3 procs across npe*.sil)

### Pulse gaps

- ~~**`__sil_cast` handling**~~: Fixed. `exp_to_sil` now converts `__sil_cast(<typ>, val)` → `Cast(typ, val)` matching OCaml. Zero-constant casts kept opaque. `and_positive` on non-null malloc returns.
- **Aliasing contradiction detection**: Caller aliasing callee's disjoint formals. Cross-ref: `PulseInterproc.ml` AliasingWithAllAliases.
- **ValueHistory threading**: Error trace reconstruction. Cross-ref: `PulseValueHistory.ml`.
- **Global variable handling** in summary application.
- ~~**Leak detection**~~: Implemented. MEMORY_LEAK_C. Sweep: expected 20, found 20 (was 11). `find_return_value` void fix + `getcwd` conditional alloc model + `is_known_nonzero` atom check + interproc attr ordering after formula.
- ~~**sizeof type evaluation**~~: Fixed for scalar types. `Typ::size_in_bytes()`. Remaining: `<int[]>` without array length.
- ~~**Write-through-pointer biabduction**~~: Fixed. `write_deref` pre-edge uses `read_heap` (old value), post-edge uses new value. Unlocked funptr write-through patterns.
- ~~**FunctionApplication**~~: Fixed. `fn_app_eqs` tracks pure unknown call returns with constant-canonicalized keys. `random()` modeled as nondet.
- ~~**Specialization loop**~~: Wired into CLI. Multi-level function pointer dispatch through call chains.
- ~~**Latent issues (basic)**~~: Implemented. AbortProgram + LatentAbortProgram in summaries. `is_manifest` checks formula atoms for formal-derived values (atoms only, not linear_eqs — linear_eqs unreliable due to `read_heap` pre-edge overwrite). Manifest errors reported at callee; latent errors propagated and re-evaluated at callers. Fixed traces.c (+1 NPE). Remaining: fix `read_heap` pre-edge overwrite → enable linear_eqs in `is_manifest` → fix `propagate_latent` chain (+2); LatentInvalidAccess for summary disjunct parity.

## Debugging tools

- **Per-instruction tracing**: `--debug-level-analysis 1` (debug) or `2` (trace). Also `RUST_LOG=pulse=debug`. Log lines prefixed with `[proc_name]` for parallel-safe filtering.
- **Comparison script**: `scripts/compare_traces.py` — parses OCaml `--debug` HTML and Rust log, side-by-side per-instruction with disjunct counts.
- **Compliance recipe**: see CLAUDE.md "Step-by-step tracing for compliance debugging".

## Code issues

- **`PartialEq` always false on `ExecutionDomain`**: Intentionally mimics OCaml's pointer equality.
- **`find_return_value` fallback heuristic**: Skips void procs (correct for leak detection). For non-void, takes last Load/Call across ALL nodes.
- **All call arg types set to `void`** in to_sil: formal types available via PulseSummary.formal_types for havoc decisions.
- **Prune `is_then_branch` hardcoded to `true`** in to_sil.
- **`DeclEnv` uses `format!()` as HashMap keys**: Needs location-insensitive key types.

## Test improvements

- ~~**`reset_counters()` global state**: Potential test flakiness.~~ Fixed: thread-local counters + per-procedure reset.

## Code improvements (low priority)

- **`AnnotItem::empty()` etc. duplicate `Default`**
- **`Procdesc` succs/preds HashMap**: Vec would be more cache-friendly.
- **`Tenv::get_supers` clones**: Use references or intern.
- **`DUMMY_LOCATION` LazyLock**: Could be `const`.
