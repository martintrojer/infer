# scout_array_access_constant_null_coalescing design note

Baseline at HEAD da98dd6b09 (worker-1 workspace, no source edits):
- memory_leak.c scoped triage: Matching 37, Differences 9, rust_only 5. Array residuals are allocate_all_in_array/free_all_in_array.
- latent.c scoped triage: Matching 10, Differences 4. Residuals are linked-list/apply-post/null-latent surfaces; not ArrayAccess-specific.
- specialization.c scoped triage: Matching 20, Differences 1. may_double_free_if_alias is EqZero/PotentialInvalidAccessSummary + alias/apply-post, not ArrayAccess-specific.

OCaml flow:
- PulseFormula.absval_of_int uses Formula.get_term_eq Term.of_intlit. For integer literal 0 this reuses the existing variable bound to linear constant 0. eval_const then Invalid(ConstantDereference(0)) is put on that same value.
- PulseFormula.get_var_repr is pure UF representative only: Formula.get_repr formula.phi v. It does not itself fold linear constants into a literal representative.
- PulseBaseMemory.Edges.canonicalize rewrites edge targets and ArrayAccess indices through get_var_repr. BaseMemory.canonicalize rewrites heap roots and prunes aliasing contradictions if two non-empty roots collapse. find_edge_opt has a fallback for ArrayAccess direct miss: canonicalize requested access and all existing edge keys through get_var_repr and retry.
- PulseAbductiveDomain.filter_for_summary calls canonicalize before restore_formals_for_summary/discard/simplify; this is where heap roots/targets/ArrayAccess indices are physically rewritten in summaries.
- PulseInterproc translate_access_to_caller materializes callee ArrayAccess indices via subst_find_or_new, and materialize_pre delays ArrayAccess until after conjoin_callee_arith so callee index equalities are available before eval_edge.

Rust flow:
- Formula::get_var_repr/Phi::get_repr is also pure UF only. Known constants live in linear_eqs/intervals; no canonical 0 fold happens there.
- AbductiveDomain::absval_of_int scans linear_eqs for an existing constant and reuses it. eval_const invalidates that value. canonicalize_for_access separately keeps const_cache constant->value and can union a new known-constant value with a cached one.
- BaseMemory/Edges canonicalize ArrayAccess indices via map_values/subst_var/subst_var_or_unsat and read fallback find_with_history_canonicalized. base_memory.rs already has worker-2 a8ba9a20b5 alias-contradiction pruning in subst_var_or_unsat / summary canonicalize path.
- Summary normalize calls canonicalize_with_current_path_condition_or_unsat, but this only applies UF equalities (plus subst_var_or_unsat); it does not collapse arbitrary known-constant values unless those values have already been unioned through const_cache/absval_of_int.

Likely root:
- This is separate from a8ba9a20b5. Worker-2 fixed non-empty heap-root alias contradictions once representatives are equal. The remaining ArrayAccess residual has no such equality: Rust keeps three roles distinct in several memory_leak.c disjuncts (array index known-zero, malloc/free null-return representative, and pointee Invalid(ConstantDereference(0)) value). OCaml has a term_eqs/absval_of_int route that more often reuses one constant-zero variable, so ArrayAccess index and null invalidation/pointee coalesce before summary comparator alpha pairing.
- Missing Rust piece is a principled summary-time or producer-time unification of known integer constants (especially 0) across all summary-visible values, not another alias-contradiction prune. Doing it naively in get_var_repr would be unsafe because OCaml get_var_repr is UF-only and because forcing every known-zero allocated root to the literal invalidated zero can change EqZero sideband/invalid-access semantics.

Recommended tasks filed:
1. scout_const_zero_null_repr_design: small experiment/design, 0.5d, prove exact OCaml representative-selection with targeted dumps/unit before editing semantics.
2. bug_array_access_const_null_coalescing_summary: 1.5d, implement minimal producer/summary coalescing after EqZero sideband task, files formula/mod.rs/phi.rs, abductive.rs, base_memory.rs, interproc.rs/operations.rs as needed.
3. bug_summary_alpha_isograph_arrayaccess_constants: 0.5d, add fixture/regression comparator tests after fix.
