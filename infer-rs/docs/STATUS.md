# infer-rs Status

## Summary

**~30,000 lines of Rust across 11 crates. 350+ tests. Latest authoritative store-textual sweep: 52 of 55 C pulse test files pass the full pipeline. NPE detection: expected 135, found 138. Leak detection: expected 20, found 23. UAF detection: expected 7, found 8. Latent issue support, write-through-pointer biabduction, must_be_valid interproc, specialization loop, FunctionApplication, sizeof/float evaluation, per-instruction tracing. funptr.c: 6/11. specialization.c: 5/5.**

Recent correctness / robustness fixes:
- OCaml-style `NewEq` incorporation is now wired back into the abductive state: formula equalities rewrite heap/attrs/tracking sets instead of staying solver-only. This restored the missing aliased-specialization behavior in `specialization.c`.
- Specialized-alias reasoning now affects actual heap semantics, not just formula representatives: `call_test_alias_bad`, `call_test_unalias_bad`, and `call_may_double_free_if_alias_bad` are all back in the direct `specialization.c` run.
- `apply_summary` now preserves `AbortProgram` summaries instead of dropping them, matching OCaml `PulseCallOperations.apply_callee` more closely.
- OCaml-style prune-condition depth tracking now distinguishes local conditions from callee-imported ones in manifest classification, which restores `assert.c` and `ternary.c` without reintroducing the latent/base reporting bug.
- `ExecutionDomain` / formula equality is now semantic, so `DisjunctiveDomain` subset checks and deduplication behave correctly.
- CLI infer autodiscovery now matches the repo layout and checks sibling `../infer/bin/infer`.
- Unsupported Textual `Closure` / `Apply` / residual `If` expressions now fail conversion explicitly instead of lowering to placeholder `0`.
- OCaml capture parity, CLI multi-file, and inline Pulse smoke tests now assert concrete behavior instead of mostly "did not crash".

Three CLI modes:
- **Full pipeline**: `infer-rs --pulse-only -- clang -c file.c` (capture + export + analyze)
- **Existing capture**: `infer-rs --pulse-only` (export from capture.db + analyze)
- **Direct .sil**: `infer-rs --pulse-only file.sil` (debugging)

Two analysis pipelines:
- **Liveness**: `.sil` → parse → transforms → to_sil → backward analysis → DEAD_STORE reporting
- **Pulse**: `.sil` → parse → transforms → to_sil → forward analysis → NULLPTR_DEREFERENCE / USE_AFTER_FREE detection

Source location remapping: `LineMap` maps `.sil` line numbers back to original C source via `@[line:col]` annotations (C/C++ frontend) and `// .line` directives (Rust frontend).

Pulse features:

- **WTO fixpoint with DisjunctiveDomain** matching OCaml's `MakeDisjunctive`
- **Multi-disjunct summaries** with PrePostKind (ContinueProgram / ExitProgram / AbortProgram / LatentAbortProgram)
- **Biabduction**: pre-state tracking, pre-materialization with formal-value mapping, pre-condition violation detection
- **Latent issue support**: `is_manifest` classification, LatentAbortProgram propagation through call chains, caller-side re-evaluation
- **Summary specialization**: HeapPath-based dynamic type specialization for function pointer dispatch, recursive multi-level specialization, `needs_specialization` propagation
- **Summary normalization**: strip unreachable attrs matching OCaml's `discard_unreachable`
- **Interprocedural path condition filtering**: translate callee formula atoms/equations to caller, reject inapplicable pre_posts when callee constraints contradict caller state
- **Unknown call havoc**: type-aware, havocs memory reachable from pointer-typed args for C extern stubs
- **Formula solver**: union-find, linear arithmetic, atoms, term equalities, CItv integer intervals, `is_int` reasoning, LessThan implication checks, FunctionApplication tracking
- **Path-sensitive constant folding**: comparison ops, Mult/DivI/DivF/Mod, Shiftlt/Shiftrt, BAnd/BOr/BXor
- **`__sil_*` builtin conversion**: 23+ binops, 3 unops, allocate, cast, cfun
- **C models**: malloc/free/realloc, new/delete, exit/abort/\_\_assert\_rtn (noreturn), fopen/getcwd (null/non-null), memcpy/memmove, \_\_builtin\_expect, 18 stdio arg checks
- **Memory leak detection**: unreachable allocated-not-freed addresses at summary creation, `find_return_value` void fix, `getcwd` conditional alloc, `is_known_nonzero` atom check
- **Function pointer dispatch** via `__call_c_function_ptr` + Closure attributes
- **Noreturn detection** propagated interprocedurally
- **Deterministic analysis**: thread-local counters + BTreeMap in core structures
- **Diagnostic dedup** by invalidation origin (handles duplicate SIL nodes from short-circuit `&&`/`||`)
- **Equality incorporation**: solver-discovered equalities now rewrite `pre`/`post`, heap access indices, attrs, `must_be_valid`, and specialization-tracking sets

## Migration Phases

| Phase | Status | Description |
|-------|--------|-------------|
| 0: Project Setup | ✅ Done | Workspace, CI rules, crate structure |
| 1: Core SIL Types | ✅ Done | Typ, Exp, Instr, Procdesc, Cfg, Tenv, BuiltinDecl (1,777 lines) |
| 2: Textual Parser | ✅ Done | Lexer, parser, printer, transforms, to_sil (5,743 lines) |
| 3: Abstract Interpretation | ✅ Done | Domain traits, RPO + WTO fixpoint, forward/backward (1,503 lines) |
| 4: Liveness Checker | ✅ Done | Backward liveness, dead store reporter (994 lines) |
| 5: Database Layer | ⬜ TODO | rusqlite for capture.db |
| 6: Analysis Driver | ✅ Done | Parallel runner, call graph, blocking dedup, file callbacks (1,184 lines) |
| 7: Pulse | ✅ MVP | Formula, models, prune, interproc, WTO+DisjunctiveDomain, constant folding (4,683 lines) |
| 8: Additional Checkers | ⬜ TODO | SILValidation, BufferOverrun, RacerD, etc. |
| 9: Frontend Support | ⬜ TODO | Keep OCaml frontends, Rust reads Textual |

## Crate Map

```
infer-rs/
  Cargo.toml                    workspace root
  CLAUDE.md                     development rules (fmt, clippy -D warnings, test)
  test-data/                    test fixtures (.sil files)
  crates/                         (tokei: code / total lines)
    sil/          (1,877 / 2,335) core SIL types + BuiltinDecl registry + Specialization
    textual/      (5,743 / 6,613) Textual IR parser, printer, verification, transforms, to_sil
    absint/       (1,578 / 2,037) abstract interpretation framework (RPO + WTO fixpoint + DisjunctiveDomain)
    analyses/       (994 / 1,222) intraprocedural analyses (liveness, dead store reporter)
    test-harness/   (665 /   823) test infrastructure: Textual utils, OCaml infer runner, fixtures
    ondemand/     (1,184 / 1,480) parallel analysis runner with inter-procedural support
    diagnostics/    (214 /   267) issue types, severity, issue reporting
    config/         (200 /   250) configuration: .inferconfig, global OnceLock, OCaml flag compat, manifest parsing
    pulse/        (5,200 / 6,400) Pulse analysis engine (null deref, UAF, models, interproc, specialization, WTO+DisjunctiveDomain)
    cli/            (436 /   537) CLI binary (clap, ondemand integration, config wiring)
```

### sil crate
Core SIL intermediate representation types. All types derive `Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize`.

| Module | Mirrors OCaml | Key types |
|--------|---------------|-----------|
| `typ.rs` | `Typ.ml/mli` | IKind, FKind, PtrKind, TypeQuals, TemplateArg, TemplateSpecInfo, TypeName (14 variants), TypeDesc, Typ |
| `exp.rs` | `Exp.ml/mli` | Exp (11 variants), Closure, SizeofData, LfieldObjData |
| `instr.rs` | `Sil.ml/mli` | Instr (Load/Store/Prune/Call/Metadata), InstrMetadata (11 variants), IfKind |
| `procdesc.rs` | `Procdesc.ml/mli` | Node, NodeKind, Procdesc (index-based CFG with BTreeSet edges) |
| `procname.rs` | `Procname.ml/mli` | Procname (10 language variants) with arity-based overload disambiguation for Hack/Python |
| `cfg.rs` | `Cfg.ml/mli` | Cfg (HashMap<Procname, Procdesc>) |
| `tenv.rs` | `Tenv.ml/mli` | Tenv (HashMap<TypeName, Struct>) with transitive super traversal |
| `strukt.rs` | `Struct.ml/mli` | Struct, Field, ClassInfo (Hack/Java class kinds), TenvMethod |
| `int_lit.rs` | `IntLit.ml/mli` | IntLit (arbitrary precision via num-bigint, custom serde) |
| `builtin_decl.rs` | `BuiltinDecl.ml/mli` | Builtin function registry (malloc, free, __new, __delete, etc.) with `is_declared()` and `match_builtin()` |
| `specialization.rs` | `IR/Specialization.ml` | HeapPath (Pvar/FieldAccess/Dereference), PulseSpecialization (dynamic_types map) |
| Others | Various | Ident, Pvar, Var, Fieldname, Binop, Unop, Const, CallFlags, CapturedVar, Mangled, QualifiedCppName, Location, SourceFile, Annot |

### textual crate
Textual IR parser, printer, transforms, and SIL conversion. Depends on `sil` and `logos`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `tokens.rs` | TextualMenhir tokens | Token enum shared between lexer and parser |
| `lexer.rs` | `TextualLexer.ml` | Three-stage pipeline: logos raw tokens → `::` ident merge → compound token adapter |
| `ast.rs` | `Textual.mli` | Full Textual AST: Location, Name, TypeName, Typ, Exp, BoolExp, Instr, Terminator, Node, ProcDesc, Module |
| `parser.rs` | `TextualMenhir.mly` | Recursive-descent parser. Handles `_ = expr` wildcard let-bindings and `<typ>` type expressions |
| `printer.rs` | `TextualOfSil.ml` | Pretty printer with structural roundtrip verification |
| `decls.rs` | `TextualDecls.ml` | Declaration environment (globals, structs, procs) |
| `verification.rs` | `TextualBasicVerification.ml` | Structural checks: unknown labels, unknown fields, wrong arg count |
| `type_check.rs` | `TextualTypeVerification.ml` | Type inference: fill `typ: None`, arrow deref insertion, SSA param registration, builtin return types |
| `transform.rs` | `TextualTransform.ml` | Complete transform pipeline: fix_closure_app → type_check → remove_effects → let_propagation → out_of_ssa |
| `to_sil.rs` | `TextualSil.ml` | Textual→SIL conversion with arity-aware procname construction |

### absint crate
Abstract interpretation framework. Depends on `sil`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `domain.rs` | `AbstractDomain.mli` | Comparable, AbstractDomain, WithBottom, WithTop traits. BottomLifted, TopLifted. Pair, BTreeSet, BTreeMap combinators. BooleanAnd, BooleanOr. |
| `transfer.rs` | `TransferFunctions.mli` | TransferFunctions trait |
| `wto.rs` | `WeakTopologicalOrder.ml` | Bourdoncle's WTO algorithm. Partition enum (Vertex/Component). Iterative DFS with SCC detection. |
| `interp.rs` | `AbstractInterpreter.ml` | RPO + WTO fixpoint engines. Forward/Backward via CfgDirection. WTO widens only at loop heads. |
| `disjunctive.rs` | `AbstractInterpreter.MakeDisjunctive` | DisjunctiveDomain: bounded list of disjuncts with join=union, widen=stop-after-N, leq=subset. |

### analyses crate
Intraprocedural analyses. Depends on `sil`, `absint`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `liveness.rs` | `checkers/liveness.ml` | Backward liveness. LiveVarSet domain. Gen on read, kill on write. Dead store reporter (DEAD_STORE issues). |

### pulse crate
Pulse analysis engine. Depends on `sil`, `diagnostics`, `num-rational`. See [PULSE.md](PULSE.md) for architecture.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `abstract_value.rs` | `PulseAbstractValue.ml` | Fresh symbolic addresses (newtype i64), thread-local counters, per-procedure reset |
| `access.rs` | `PulseAccess.ml` | FieldAccess, ArrayAccess, Dereference |
| `invalidation.rs` | `PulseInvalidation.ml` | How addresses become invalid (CFree, ConstantDereference, etc.) |
| `attribute.rs` | `PulseAttribute.ml` | Address attributes (~25 variants), Attributes set |
| `formula/` | `PulseFormula*.ml` | Constraint solver: union-find, linear arithmetic, atoms, term AST, term equalities (v = binop(x,y)), atom contradiction |
| `base_stack.rs` | `PulseBaseStack.ml` | Var → AbstractValue stack map |
| `base_memory.rs` | `PulseBaseMemory.ml` | AbstractValue → Edges heap graph |
| `base_attrs.rs` | `PulseBaseAddressAttributes.ml` | AbstractValue → Attributes map, check_valid |
| `base_domain.rs` | `PulseBaseDomain.ml` | Composite {stack, heap, attrs} |
| `abductive.rs` | `PulseAbductiveDomain.ml` | Post-state + formula, validity checking, OCaml-style `NewEq` incorporation |
| `operations.rs` | `PulseOperations.ml` | eval, eval_deref, write_deref, check_addr_access, eval_or_fresh |
| `transfer.rs` | `Pulse.ml` | SIL instruction → state transition. Prune, UnOp folding (LNot/Neg/BNot), path sensitivity |
| `models/mod.rs` | `PulseModels*.ml` | Model dispatch: builtins first, then name-based. Models take priority over summaries |
| `models/c.rs` | `PulseModelsC.ml` | C models: malloc/free, new/delete, exit/abort (noreturn), fopen (null/non-null), 18 stdio arg-validity checks |
| `summary.rs` | `PulseSummary.ml` | PulseSummary with Vec<PrePost> (multi-disjunct), specialized summaries, needs_specialization HeapPaths, is_noreturn flag |
| `specialization.rs` | `PulseSpecialization.ml` | apply() binds HeapPaths to Closure attrs, make_specialization_from_caller(), eval_for_prune |
| `interproc.rs` | `PulseInterproc.ml` | apply_summary: callee→caller effect propagation, formal-value mapping for write-through-pointer, preserve abort summaries |
| `diagnostic.rs` | `PulseDiagnostic.ml` | AccessToInvalidAddress, MemoryLeak, RetainCycle |
| `execution_domain.rs` | `PulseExecutionDomain.ml` | ContinueProgram, AbortProgram, ExitProgram |
| `checker.rs` | `Pulse.ml` + `PulseCallOperations.ml` | analyze, analyze_with_specialization, select_pre_posts, __call_c_function_ptr dispatch, propagate_specialization_need |

### ondemand crate
Parallel analysis runner. Depends on `sil`, `absint`, `rayon`, `dashmap`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `checker.rs` | `registerCheckers.ml` | `IntraChecker`, `InterChecker` (with `analyze_specialized`), `FileChecker` traits. `AnalysisContext` includes Cfg for specialization re-analysis. |
| `summary.rs` | `Summary.ml` | `SummaryStore<S>` with `DashMap<Procname, Arc<OnceLock<S>>>` for blocking dedup |
| `callgraph.rs` | `SyntacticCallGraph.ml` | Call graph from Cfg, bottom-up wave scheduling with SCC cycle detection |
| `runner.rs` | `ondemand.ml` | `run_intra`, `run_inter` (blocking dedup), `run_file_callbacks`, `run_parallel` |

### diagnostics crate
Issue reporting types. Depends on `sil`.

| Module | Mirrors OCaml | Description |
|--------|---------------|-------------|
| `issue_type.rs` | `IssueType.ml` | Severity, Category, IssueTypeId enum (single source of truth for issue type strings matching OCaml), IssueType |
| `issue.rs` | `Errlog.ml` + `Reporting.ml` | Issue, IssueLog with sort, merge, JSON export, issues.exp format |

### test-harness crate
Shared test infrastructure. Depends on `sil`, `textual`, `serde_json`.

| Module | Description |
|--------|-------------|
| `textual_utils.rs` | `parse_and_convert()`, `parse_file_and_convert()`, `TestModule` with label→node_id lookup |
| `infer_runner.rs` | `InferRunner`: OCaml infer integration, `store_textual_and_export()`, `dump_textual_for_c()`, `analyze_pulse_c()`, report.json parsing, `compare_issues()` |
| `fixtures.rs` | `test_data_dir()`, `ocaml_c_test_dir()`, `parse_issues_exp()`, `issues_for_file()`, `load_fixture()` |
| `summary_compare.rs` | `parse_ocaml_summaries()`, `SummaryFacts`, `compare_summaries()`, `ComparisonReport` |

## Key Design Decisions

1. **Lexer**: logos + hand-written compound token adapter. Evaluated lalrpop but LALR(1) conflicts made it impractical.
2. **Parser**: Recursive descent mirroring TextualMenhir.mly. Supports `_ = expr` (OCaml prints but can't parse this).
3. **CFG**: Index-based (`Vec<Node>` + `HashMap<NodeId, BTreeSet<NodeId>>`). BTreeSet for edge sets.
4. **Transforms**: Complete pipeline matching OCaml's `TextualTransform.run`. NNF/DNF for boolean decomposition, iterative flattening with RemoveIf interleaving.
5. **Procname arity**: Hack/Python procnames encode arity (e.g. `C.f#2`) for overload disambiguation, matching OCaml behavior.
6. **Analysis runner**: No SQLite in the loop. `DashMap<Procname, Arc<OnceLock<S>>>` for blocking dedup — first thread computes, others wait. Bottom-up wave scheduling with SCC cycle detection. File-level callbacks for cross-procedure checkers.
7. **Backward analysis**: Same fixpoint engine as forward, parametrized by CfgDirection trait.
8. **Model dispatch**: `sil::builtin_decl` registry mirrors OCaml's `BuiltinDecl.ml`. Models match by identity via `match_builtin()`, not ad-hoc string comparison.
9. **Interprocedural via ondemand**: CLI wires Pulse as an `InterChecker` into the ondemand runner. Bottom-up call graph scheduling ensures callee summaries are available before callers. Parallel via rayon.
10. **Disjunctive interpreter**: `DisjunctiveDomain<D>` in absint implements `AbstractDomain` with join=union, widen=stop-after-N, leq=subset. Pulse checker uses `compute_fixpoint_wto` with this domain, matching OCaml's `MakeDisjunctive(PulseTransferFunctions)` exactly. No custom iteration loops.
11. **Configuration**: `config` crate with global `OnceLock<InferConfig>`. Set once at startup via `config::init()`, read anywhere via `config::get()`. Supports `.inferconfig` JSON (OCaml-compatible, unknown fields ignored). `#[serde(rename)]` is the single source of truth for flag names.
12. **Summary specialization**: `sil::specialization` (HeapPath, PulseSpecialization) mirrors `IR/Specialization.ml`. `pulse::specialization::apply()` now supports alias groups as well as dynamic types, so aliased actuals can be re-analyzed with the correct heap semantics before dispatch/reporting. Recursive specialization through multi-level call chains. `needs_specialization` propagation from callees to callers enables the ultimate caller (with known Closure) to trigger the chain. `eval_for_prune` evaluates constants without Invalid marking for comparison contexts. Cross-ref: `PulseSpecialization.ml`, `PulseCallOperations.ml` iter_call, `Pulse.ml` analyze with specialization.
13. **Call graph Cfun scanning**: `CallGraph::from_cfg` scans ALL Cfun references in ALL expressions (Store values, Call args, Load expressions), not just Call.fun_exp. Captures function pointer targets for dependency scheduling.
14. **Biabduction formal-value mapping**: `apply_summary` maps each formal's loaded value (one deref from stack) to the actual value, ensuring write-through-pointer patterns propagate correctly. Without this, writes go one indirection level too deep.

## OCaml Test Porting Status

### Unit tests
51 of 78 OCaml unit tests ported (65%):

| OCaml file | Total | Ported | Remaining |
|---|---|---|---|
| `abstractInterpreterTests.ml` | 15 | 12 | 3 (try/catch) |
| `livenessTests.ml` | 25 | 17 | 8 (exceptions, closures, while) |
| `TextualParserTest.ml` | 10 | 9 | 1 (snapshot) |
| `TextualTransformTest.ml` | 12 | 7 | 5 (closure-to-obj, hackc, if-subexpr) |
| `TextualKeepGoingVerificationTest.ml` | 3 | 3 | 0 |
| `TextualSilTest.ml` | 10 | 3 | 7 (tenv annotations, instanceof) |
| `TextualTest.ml` | 2 | 0 | 2 (procname, linemap) |
| `TextualRestoreSSATest.ml` | 1 | 0 | 1 |

Remaining 27 tests are blocked on: exceptions (7), closure-to-object (3), tenv annotations (5), Hack-specific (2), while/loop_as_if (2), restore_ssa (1), instanceof (1), procname/linemap/snapshot (4), multi-module merge (1), if-in-subexpr (1).

### OCaml SIL Pulse end-to-end tests
10 of 18 OCaml `.sil` test files covered with assertion tests. Tests reference OCaml source files directly (no copies). Custom/merged fixtures stay in `test-data/pulse/`.

| OCaml file | Status | Notes |
|---|---|---|
| `alloc.sil` | ✅ Full pass | 2 procs, all OK |
| `npe.sil` | ✅ 9/10 | skip `external_call_and_npe_bad` (cross-file) |
| `npe_with_load_in_exp.sil` | ✅ 15/16 | skip `external_call_and_npe_bad` (cross-file) |
| `npe_without_types.sil` | ✅ 15/16 | skip `external_call_and_npe_bad` (cross-file) |
| `to_sil_bug.sil` | ✅ 1/3 | skip 2 deep interproc (pointer-to-pointer) |
| `ocaml_model.sil` | ✅ Full pass | 1 proc, unmodeled call handling |
| `static_types.sil` | ⚠️ 4/6 | skip 2 _bad: chained virtual calls in loads |
| `virt.sil` | ⚠️ 14/20 | skip 1 miss + 4 FP: devirt return values |
| `npe_external_oo.sil` | ✅ Full pass | merged fixture (5 procs, OO dispatch) |
| `externalObjOrientRetNull.sil` | ✅ helper | covered via merged fixture |
| `externals.sil` | — helper | defines `external_return_null` for npe.sil |
| `importedFunctions.sil` | — helper | defines funcs for typesAcrossFiles.sil |
| `npeWithExternalObjOrient.sil` | ✅ | covered via merged fixture |
| `basic.sil` | ⬜ N/A | taint analysis (not implemented) |
| `overload.sil` | ⬜ N/A | taint analysis (not implemented) |
| `overload_use.sil` | ⬜ N/A | taint analysis (not implemented) |
| `exncfg.sil` | ⬜ no issues | exception CFG; no-panic covered by bulk test |
| `textual_models.sil` | ⬜ no issues | Hack builtins; no-panic covered by bulk test |
| `typesAcrossFiles.sil` | ⬜ no issues | type edge cases; no-panic covered by bulk test |

**Skipped procs by root cause:**
- **Cross-file resolution** (3 procs): callee defined in companion `.sil` file, not available in single-file analysis
- **Deep interproc** (2 procs): pointer-to-pointer summary propagation not yet implemented
- **Virtual dispatch in loads** (2 procs): `n0.OO.get_null().B.f` chained method call resolution
- **Devirtualization** (5 procs): 1 miss (virtual dispatch through interprocedural call chain), 4 FP (return value not evaluated through prune conditions after devirtualized call)

### C source → store-textual → export → Rust pipeline (pulse)
52 of 55 C source files pass through the full pipeline: C source → OCaml `infer --store-textual` → `infer debug --export-textual` → manifest.json → Rust parse → Pulse analysis.
This is the authoritative compliance benchmark because it matches the CLI capture/export path.

Run with: `cargo test --release --test end_to_end test_store_textual_sweep -- --ignored --nocapture`

The repo also keeps a separate `capture --dump-textual` sweep as a secondary regression test for the raw dumped `.sil` path. That sweep is useful for parser/to_sil debugging, but its numbers are not the published compliance baseline.

**Pipeline status (55 files):**

| Status | Count | Details |
|---|---|---|
| OK | 52 | parsed + analyzed, 653 procs, 158 issues |
| SKIP | 3 | infinite.c, recursion.c, recursion2.c (fixpoint exhaustion) |
| FAIL_PARSE | 0 | |
| TIMEOUT | 0 | |

**NULLPTR_DEREFERENCE comparison vs OCaml `issues.exp`: expected 135, found 128 (deterministic).**

This lower total is a correctness improvement, not a regression to paper over. The previous higher
count was partly inflated by latent issues being published as manifest base reports. The current
sweep reflects the real remaining NPE miss/FP backlog after fixing that latent/base confusion.

Under-detection (we miss issues OCaml finds):
- **Function pointer dispatch**: funptr.c (11→6). Specialization infrastructure is in place; remaining FNs need deeper call-chain specialization.
- **Function-pointer wrappers**: memory_leak.c (3→1 NPEs). Uses `static void* (*const malloc_func)(size_t) = malloc` — needs funptr dispatch.
- **Deep interproc / models**: initlistexpr.c (4→1), compound_literal.c (2→1).
- **Loop depth / traces**: latent.c (5→3), traces.c (5→4).

Recent win in this area:
- `assert.c` (1→1) and `ternary.c` (3→3) are now back after adding OCaml-style condition-depth tracking for prune conditions.

Over-detection (we report more than OCaml):
- **sizeof array** (+2): sizeof.c. `<int[]>` textual loses array length.
- **nullptr_more.c** (+2): write_deref pre-edges too aggressive.
- **Other** (+3): angelism.c (+1), nullptr.c (+1), offsetof_expr.c (+1).

**MEMORY_LEAK_C comparison vs OCaml `issues.exp`: expected 20, found 24.**

Per-file differences:
- cleanup_attribute.c (+2): GCC cleanup attribute not modeled
- memory_leak.c (+2): mixed funptr-wrapper misses plus independent leak FPs

**USE_AFTER_FREE comparison vs OCaml `issues.exp`: expected 7, found 6 (-1 FN).**

The previous `+3` UAF over-detection was caused by latent issues being published as manifest. That
is now fixed. The only remaining UAF gap is:

- specialization.c (-1): aliased-actual UAF in `call_may_double_free_if_alias_bad`

**Notable direct-match win:** `interprocedural.c` is back to the direct OCaml issue set after the
latent/manifest reporting fix.

**Summary comparison (15 files, via `infer debug --dump-json-summaries`):**

This snapshot is stale relative to the latest latent-reporting fix and should be re-run before using
the exact counts below.

Disjunct count mismatches (older snapshot, before latest latent fix):
- `ocaml=3, rust=2` (23 procs): caller-side latent propagation missing
- `ocaml=5, rust=2` (16 procs): mostly fopen patterns (multiple error paths)
- `ocaml=2, rust=1` (10 procs): LatentInvalidAccess or noreturn gaps

Other mismatches: null_attrs (22 procs, rust missing Invalid attrs), noreturn (21 procs).

## Known Issues / Gaps

1. **Type verification partial**: Type inference and hole-filling ported. Missing: DFS node ordering, terminator type checking (Ret/Jump SSA args/Throw), Store type-compatibility validation, restore_ssa on ident conflicts.
2. **to_sil partial lowering**: Expression conversion handles common cases. Unsupported Closure / Apply / residual If now fail conversion explicitly instead of lowering to placeholder values, but full lowering is still missing. OCaml ~1200 lines; ours ~600.
3. **Liveness simplified**: No exception handling. Dead store reporter lacks suppression heuristics.
4. **Pulse interprocedural gaps** vs OCaml:
   - Latent/base publishing now goes through summary classification instead of raw abort scans.
   - Missing: `LatentInvalidAccess`, `pre_heap_has_assumptions` parity in `Formula.is_manifest`, latent issue type reporting
   - No aliasing contradiction detection (cross-ref: `PulseInterproc.ml` AliasingWithAllAliases)
   - No `ValueHistory` threading for error trace reconstruction (cross-ref: `PulseValueHistory.ml`)
   - No global variable handling in summary application
   - Specialization implemented for function pointers; dynamic type specialization for OO not yet done
5. **Pulse formula**: Union-find + linear arithmetic + atoms + term equalities + atom contradiction + CItv integer intervals + is_int integer reasoning. Missing: simplex tableau, non-linear terms.
6. **Pulse models**: C models cover malloc/free/realloc, new/delete, exit/abort/__infer_fail/__assert_rtn (noreturn), fopen (null/non-null), memcpy/memmove (dest+src validity), 18 stdio arg checks, `__call_c_function_ptr` (function pointer dispatch). Missing: list API. No Java, Hack, ObjC models.
7. **No summary persistence**: In-memory only. Optional disk persistence planned.
8. **Closure-to-object transform**: Closures left as-is rather than transformed to object allocations.
9. **Tenv annotations**: `.final`, `.abstract`, `.kind`, `.constant` struct attributes not handled in to_sil.

## What's Next (ranked by impact and tractability)

### 1. Recover true NPE misses after the latent fix
The NPE total is now lower for the right reason. The next work should recover real misses without
reintroducing latent/base confusion:

- funptr.c (11→6)
- initlistexpr.c (4→1)
- compound_literal.c (2→1)
- memory_leak.c (3→1)
- latent.c (5→3)
- traces.c (5→4)

### 2. Fix the remaining UAF miss
specialization.c is the only remaining UAF gap (`call_may_double_free_if_alias_bad`). Most likely
root cause: missing alias-specialization / aliasing-contradiction parity.

### 3. Leak cleanup
- cleanup_attribute.c (+2): model GCC cleanup attr
- memory_leak.c (+2): separate wrapper misses from unrelated leak FPs

### 4. `is_manifest` parity
The current Rust implementation now has OCaml-style condition-depth tracking plus benign
non-null/disequality handling, so the remaining gap is smaller:

- `pre_heap_has_assumptions`
- `LatentInvalidAccess`
- latent issue type reporting

### Other gaps

**Leak compliance (expected 20, found 24):**
Per-file differences cancel out: cleanup_attribute.c (+2, GCC cleanup attr), nullptr.c (-1, formal overwrite), memory_leak.c (-1, funptr wrapper).

**SIL test gaps (from skipped procs):**
- Virtual dispatch in loads (2 procs in static_types.sil)
- Devirtualization return values (5 procs in virt.sil)
- Deep interproc pointer-to-pointer (2 procs in to_sil_bug.sil)
- Cross-file resolution (3 procs across npe*.sil)

**Features:**
- LatentInvalidAccess — second latent mechanism for must_be_valid addresses
- Full `is_manifest` — match OCaml's allocated-pointer heuristic
- Complete type verification — DFS ordering, terminator type checking, restore_ssa
- More Pulse models — Java, Hack, ObjC
- Summary persistence — serde+bincode to disk
- WidenThenNarrow — OCaml's default WTO mode
- Taint analysis — unblocks 3 SIL test files
- Tenv annotations — `.final`, `.abstract`, `.kind` in to_sil
- Database layer — rusqlite for capture.db
