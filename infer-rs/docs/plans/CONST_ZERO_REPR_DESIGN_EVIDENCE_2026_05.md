# scout_const_zero_null_repr_design evidence

Workspace: `/home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-1` at HEAD `e612cfeb52` (read-only scout; no source edits).

Chosen case: `allocate_all_in_array` in `infer/tests/codetoanalyze/c/pulse/memory_leak.c`.

Why this case: smaller than `free_all_in_array`; it exercises the same array-index/null coalescing without the extra `free()` invalidation semantics.

## Commands / artifacts captured

Working directory for generated artifacts: `/tmp/scout_const_zero_repr`.

OCaml debug HTML:

```sh
cd /tmp/scout_const_zero_repr
/home/mtrojer/infer/infer/bin/infer -j 1 --pulse-only --debug --debug-level-analysis 3 \
  --procedures-filter 'allocate_all_in_array' -o ocaml-out -- clang -c memory_leak.c
/home/mtrojer/infer/infer/bin/infer debug -j 1 --dump-json-summaries -o ocaml-out
```

Key OCaml files:

- HTML: `/tmp/scout_const_zero_repr/ocaml-out/captured/memory_leak.c.98872a27ea989bb3/nodes/allocate_all_in_array.ebc1913684252980_node9.html`
- Text extraction of that HTML: `/tmp/scout_const_zero_repr/allocate_all_in_array.ebc1913684252980_node9.txt`
- Summary JSON for just this proc: `/tmp/ocaml_allocate_all_summary.json`
- Full OCaml summaries: `/tmp/scout_const_zero_repr/ocaml-out/all_summaries.json`

Rust trace:

```sh
cd /tmp/scout_const_zero_repr
/home/mtrojer/infer/infer/bin/infer capture --dump-textual -j 1 -o capture-out -- clang -c memory_leak.c
cd /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-1/infer-rs
cargo run -q -p infer-rs -- --pulse-only --debug-level-analysis 2 \
  --procedures-filter 'allocate_all_in_array' -j 1 \
  -o /tmp/scout_const_zero_repr/rust-out /tmp/scout_const_zero_repr/memory_leak.sil \
  > /tmp/scout_const_zero_repr/rust_trace.stdout \
  2> /tmp/scout_const_zero_repr/rust_trace.stderr
```

Key Rust files:

- SIL: `/tmp/scout_const_zero_repr/memory_leak.sil`
- Trace: `/tmp/scout_const_zero_repr/rust_trace.stderr`

Comparator triage for context:

```sh
cd /home/mtrojer/.local/state/mu/workspaces/infer-rs/worker-1/infer-rs
INFER_RS_C_TRIAGE_FILES=memory_leak.c RUST_TEST_THREADS=1 \
  cargo test -p pulse --test end_to_end test_summary_comparison_c_triage -- --ignored --nocapture \
  > /tmp/scout_const_zero_repr/triage.stdout \
  2> /tmp/scout_const_zero_repr/triage.stderr
```

## Observed mismatch for `allocate_all_in_array`

The summary comparator still reports `allocate_all_in_array` as different. Representative excerpts from `/tmp/scout_const_zero_repr/triage.stderr`:

```text
allocate_all_in_array:
  main[0] pre_heap missing=["array.* -[v3]-> v4"] extra=["array.* -[v4]-> v5"];
          post_heap missing=["array.* -[v3]-> v4", "v4 -*-> v3"]
                    extra=["array.* -[v4]-> v5", "v5 -*-> v6"];
          post_attrs missing=["v3:[Invalid(ConstantDereference(0))]", "v4:[Initialized, WrittenTo]"]
                     extra=["v3:[Allocated(CMalloc), Uninitialized]", "v4:[Invalid(ConstantDereference(0))]", ...];
          phi missing=["eq:v3=0"] extra=["atom:0 < v3", "atom:0 < v6", "eq:v4=0"]
  main[3] post_attrs missing=["v3:[Allocated(CMalloc), Uninitialized]", "v6:[Allocated(CMalloc), Uninitialized]"]
                     extra=["v3:[Invalid(ConstantDereference(0))]", "v6:[Invalid(ConstantDereference(0))]"];
          phi missing=["atom:0 < v3", "atom:0 < v6"] extra=["eq:v3=0", "eq:v6=0"]
```

This is not a disjunct-count mismatch: both sides have four `ContinueProgram` summary disjuncts for this proc. It is a representative-pairing mismatch: OCaml has already made some malloc-null return representatives identical to the loop-index zero representative, while Rust keeps separate known-zero AVs.

## OCaml evidence: coalescence happens during producer-time equality insertion

The important HTML node is `node9`, first instruction:

```text
n$4=_fun_malloc(sizeof(t=int;nbytes=4;nullable=false):int) [line 220, column 16];
```

Before the malloc model runs, OCaml already has the loop index `i = 0` stored as value `v5`:

```text
PRE STATE:
Current post: i=0 value: v5
attributes: { Invalid ... ConstantDereference(is assigned to the null pointer) }
...
phi: linear_eqs: v5 = 0
     && term_eqs: 0=v5
     && intervals: v5=0
     && term_eqs_occurrences: v5->r(0)
```

Immediately when OCaml runs the malloc model's null case, the debug HTML reports:

```text
Found ocaml model for call

new eq: v7 = v5
incorporating new eq: v7=v5
incorporating new eq: v5=0
incorporating new eq: v5=0
```

After that, the state has a UF equality between the fresh malloc-null return and the pre-existing zero representative:

```text
STATE:
#0: ContinueProgram
...
phi: var_eqs: v5=v7
     && linear_eqs: v5 = 0 ∧ v6 = 4
     && term_eqs: 0=v5∧4=v6
     && intervals: v5=0 ∧ v6=4
...
roots={ n$4=Unknown (v5, is assigned to the null pointer at line 220 ...),
        &i=Unknown (v4, ...),
        &array=Unknown (v1, ...) }
```

Later in the same HTML, array accesses are stored under `[v5]`, not a separate malloc-null zero value:

```text
v3 -> { [v5] -> ... }
```

And in the dumped OCaml summary (`/tmp/ocaml_allocate_all_summary.json`), the final all-null disjunct makes both array slots' pointees the same zero representative:

```json
"post": {
  "heap": [
    ["v3", [
      [["ArrayAccess", ...], "v11"], ["v19", "_"],
      [["ArrayAccess", ...], "v5" ], ["v9",  "_"]
    ]],
    ["v9",  [[["Dereference"], ["v5", "_"]]]],
    ["v19", [[["Dereference"], ["v5", "_"]]]]
  ],
  "attrs": [
    ["v5", [["Invalid", ["ConstantDereference", "0"], ...]]],
    ["v11", [["Invalid", ["ConstantDereference", "1"], ...]]]
  ]
},
"path_condition": {
  "phi": {
    "term_eqs": [
      [["Const", {"num":"0", "den":"1"}], "v5"],
      [["Const", {"num":"1", "den":"1"}], "v11"]
    ]
  }
}
```

## Exact OCaml mechanism

The coalescence is **not** primarily in `Summary.of_post`, `BaseMemory.canonicalize`, or `PulseFormula.get_var_repr`. It is a producer-time formula normalization / new-equality propagation effect:

1. Loop initialization `i = 0` evaluates the integer constant through `PulseOperations.ml`:

   - `PulseOperations.eval_to_value_origin`, `Const (Cint i)`
   - calls `PulseArithmetic.absval_of_int`
   - `PulseFormula.absval_of_int` first calls `Formula.get_term_eq formula.phi (Term.of_intlit i)` and otherwise creates a fresh value and records `v = i`.

   This creates the first `term_eqs: 0=v5` for the literal zero and invalidates that same `v5` as `ConstantDereference(0)`.

2. Malloc's null-return model is `PulseModelsC.ml`:

   ```ocaml
   let result_null =
     let* ret_addr = fresh ~more:"(null case)" () in
     assign_ret ret_addr @@> and_eq_int ret_addr IntLit.zero
     @@> invalidate path ... (ConstantDereference IntLit.zero) ret_addr
   ```

   The fresh null return (`v7` in the HTML) is constrained with `and_eq_int ret_addr 0`.

3. `and_eq_int` goes through:

   - `PulseArithmetic.and_eq_int`
   - `PulseArithmetic.and_eq_const`
   - `PulseFormula.and_equal`
   - `PulseFormulaPhi.Normalizer.solve_normalized_term_eq`
   - `PulseFormulaPhi.Unsafe.add_term_eq`

   `add_term_eq` checks `get_term_eq phi 0`. Since `0=v5` already exists, adding `0=v7` returns an existing-value conflict (`Some v5`) rather than inserting a second zero term binding.

4. `PulseFormulaPhi.Normalizer.discharge_new_eq_opt` calls `merge_vars v7 v5`, producing `Equal(v7,v5)` (`new eq: v7 = v5` in the HTML).

5. `PulseAbductiveDomain.incorporate_new_eqs` immediately applies `Equal(v7,v5)` as a substitution over the abductive state (stack, heap, attrs). This is where the fresh malloc-null return stops being distinct in the symbolic state.

6. Later, summary construction only preserves and materializes the already-existing equality:

   - `PulseAbductiveDomain.filter_for_summary` first calls `canonicalize`.
   - `canonicalize` uses `Formula.get_var_repr` to rewrite heap roots, edge targets, array indices, stack, and post attrs.
   - `Formula.simplify` then drops most `linear_eqs` in summaries but keeps the `term_eqs` needed for summary-visible constants.

Therefore, the answer to the task's four candidate locations is:

- `Formula.simplify`: **No** for the key coalescence. It is not returning the decisive `Equal(v7,v5)` here. The equality already existed before summary filtering. `simplify` mostly reduces summary formula shape and can return `EqZero`, but the observed `Equal` was produced during `and_eq_int` in the malloc model.
- `PulseFormula.get_var_repr`: **No** as a source. It is UF-only: `Formula.get_repr formula.phi v`. It observes the UF equality produced earlier.
- `Summary.of_post`: **No** as the source. It calls `filter_for_summary`; it does not choose the constant-0/null representative from scratch.
- `BaseMemory.canonicalize`: **No** as the source. It rewrites heap roots/edge targets/`ArrayAccess` indices through `get_var_repr` after the equality exists. It is the materialization step, not the representative-selection step.

The exact source mechanism is: **`PulseFormulaPhi` term equality duplicate detection for constants (`term_eqs`) during `and_eq_int` / `and_equal const`, followed by `merge_vars` and `PulseAbductiveDomain.incorporate_new_eqs` substitution**. `PulseFormula.absval_of_int` is also part of the producer story because it deliberately reuses `term_eqs` for literal constants when evaluating source constants; malloc's null case reaches the same `term_eqs` path through `and_eq_int` rather than through `absval_of_int` directly.

## Rust side-by-side evidence

In Rust trace (`/tmp/scout_const_zero_repr/rust_trace.stderr`), the corresponding state keeps separate known-zero AVs.

Example final/backedge disjunct excerpts:

```text
path_condition: Formula {
  phi: Phi {
    var_eqs: VarUF { parent: {}, rank: {} },
    linear_eqs: {
      AbstractValue(4):  constant 0,   // loop-init/index zero, Invalid at line 219
      AbstractValue(7):  constant 0,   // first malloc-null return
      AbstractValue(21): constant 0,   // second malloc-null return
      ...
    },
    term_eqs: {},
    intervals: { AbstractValue(4): [0,0], AbstractValue(7): [0,0], AbstractValue(21): [0,0], ... }
  },
  const_cache: {0: AbstractValue(4), 1: AbstractValue(14), 2: AbstractValue(30)}
}
```

The Rust heap keeps the array index canonicalized to the loop zero (`AbstractValue(4)`), but malloc-null pointees remain separate zero values:

```text
AbstractValue(2): Edges {
  ... ArrayAccess(..., AbstractValue(4)) -> AbstractValue(10),
      ArrayAccess(..., AbstractValue(14)) -> AbstractValue(26) ...
}
AbstractValue(10): Dereference -> AbstractValue(7)   // malloc null, zero, Invalid(ConstantDereference(0))
AbstractValue(26): Dereference -> AbstractValue(21)  // another malloc null, zero, Invalid(ConstantDereference(0))
```

There is no UF equality in Rust (`var_eqs` empty), and `term_eqs` is empty in the trace. This is exactly the distinction the summary comparator reports: OCaml pairs array index / pointee / invalidation values through a shared representative in some disjuncts, while Rust pairs them as distinct zero representatives.

## Follow-up scope for `bug_array_access_const_null_coalescing_summary`

Do **not** fix this in `BaseMemory.canonicalize` alone: that would only rewrite by existing UF equalities. The missing Rust behavior is earlier: creation/import of a second value equal to an already-known constant must sometimes produce a `NewEq::Equal(old,new)` and be incorporated, matching OCaml term-equality collision behavior.

Concrete files/functions to inspect/change in follow-up:

OCaml source-of-truth functions:

- `infer/src/pulse/PulseOperations.ml`: `eval_to_value_origin`, `Const (Cint i)` branch.
- `infer/src/pulse/PulseModelsC.ml`: `alloc_common_dsl` null case (`fresh`, `assign_ret`, `and_eq_int`, `invalidate`).
- `infer/src/pulse/PulseArithmetic.ml`: `absval_of_int`, `and_eq_int`, `and_eq_const`, `map_path_condition_common`.
- `infer/src/pulse/PulseFormula.ml`: `absval_of_int`, `and_equal`, `Intervals.incorporate_new_eqs`, `get_var_repr`, `simplify`.
- `infer/src/pulse/PulseFormulaPhi.ml`: `Unsafe.add_term_eq`, `Normalizer.solve_normalized_term_eq`, `add_term_eq_and_solve_new_eq_opt`, `discharge_new_eq_opt`, `merge_vars`, `add_lin_eq_to_new_eqs`.
- `infer/src/pulse/PulseAbductiveDomain.ml`: `incorporate_new_eqs`, `canonicalize`, `filter_for_summary`, `Summary.of_post_`.
- `infer/src/pulse/PulseBaseMemory.ml`: `Edges.canonicalize`, `canonicalize` (materialization only).
- `infer/src/pulse/PulseAccess.ml`: `Access.canonicalize` for `ArrayAccess` indices.

Rust candidate implementation files/functions:

- `infer-rs/crates/pulse/src/formula/phi.rs`:
  - `Phi::and_const_eq`
  - `Phi::add_linear_eq`
  - constant/term indexing (`term_eqs`, `term_value_index`, or a new constant-value index)
  - emit `NewEq::Equal(existing_const_rep, new_const_rep)` when adding `v = c` collides with an existing summary-visible constant representative, matching OCaml `add_term_eq` behavior.
- `infer-rs/crates/pulse/src/formula/mod.rs`:
  - `Formula::and_equal_const`, `Formula::and_equal`, `Formula::get_var_repr` (should remain UF-only).
- `infer-rs/crates/pulse/src/abductive.rs`:
  - `AbductiveDomain::absval_of_int`
  - `AbductiveDomain::and_equal_const`
  - `AbductiveDomain::incorporate_new_eqs`
  - `const_cache` interaction; current trace shows `const_cache: {0: AV4}` while other zero AVs remain un-unioned.
- `infer-rs/crates/pulse/src/models/c.rs`:
  - `allocate_or_null` / `fresh_or_null` null branches (`write_id_with_history`, `and_equal_const`, invalidation order).
- `infer-rs/crates/pulse/src/operations.rs`:
  - integer constant evaluation branch (`Const Cint`) and invalidation of the returned literal value.
- `infer-rs/crates/pulse/src/summary.rs`:
  - summary normalization/export only as validation; avoid making this a summary-only hack unless producer-time coalescing proves unsafe.
- `infer-rs/crates/pulse/src/base_memory.rs` and `infer-rs/crates/pulse/src/access.rs`:
  - verify only; existing canonicalization should be sufficient once UF equalities exist.
- Tests/harness:
  - `infer-rs/crates/pulse/tests/end_to_end.rs` ignored summary triage or focused regression.
  - `infer-rs/crates/test-harness/src/summary_compare.rs` only if comparator masking is proven wrong (not indicated here).

Revised effort estimate: **1.0-1.5 days** for the follow-up after the EqZero/PotentialInvalidAccess sideband task lands. The scout narrows the root to formula producer-time constant collision + `incorporate_new_eqs`; no broad BaseMemory or Summary rewrite should be needed. Budget split: ~0.5d implement OCaml-like constant collision/equality propagation and audit null model ordering, ~0.5d focused `allocate_all_in_array` / `free_all_in_array` regression and triage, ~0.5d contingency for EqZero sideband interactions / avoiding unsafe global zero unification of allocated roots.
