# Pulse Architecture

Pulse is infer's primary analysis engine: a separation-logic-based abstract interpreter that detects memory safety bugs (null dereferences, use-after-free, memory leaks), resource leaks, and retain cycles. It is ~25-30K lines of OCaml across ~60 modules.

For current implementation status, see [STATUS.md](STATUS.md).

## OCaml Pulse Layering (bottom-up)

```
Layer 0: Leaf types          AbstractValue, Timestamp, SatUnsat, Result, Access
Layer 1: Value metadata      Invalidation, ValueHistory, ValueOrigin, CallEvent, Trace
Layer 2: Formula/arithmetic  LinArit, Term, Atom, Tableau, FormulaPhi, Formula, CItv
Layer 3: BasicInterface      Re-exports Layer 0-2 under short names
Layer 4: Attributes          Attribute (~30 variants), Attributes (set)
Layer 5: Base domain         BaseStack, BaseMemory, BaseAddressAttributes, BaseDomain
Layer 6: DomainInterface     Re-exports Layer 3-5
Layer 7: Abductive domain    AbductiveDomain, CanonValue, Decompiler, Arithmetic
Layer 8: Operations          Operations, CallOperations, Interproc, Models*, Diagnostic
Layer 9: Top-level           Pulse.ml (transfer functions), Report
```

## Rust Crate Structure

All Pulse code lives in a single `pulse` crate:

```
crates/pulse/
  src/
    lib.rs
    abstract_value.rs     Layer 0: fresh address type (thread-local counters)
    access.rs             Layer 0: FieldAccess | ArrayAccess | Dereference
    sat_unsat.rs          Layer 0: Sat/Unsat result monad
    pulse_result.rs       Layer 0: Ok | Recoverable | FatalError tri-state
    invalidation.rs       Layer 1: how an address became invalid
    attribute.rs          Layer 4: address attributes (all variants needed)
    formula/              Layer 2: constraint solver
      mod.rs              Formula API
      lin_arith.rs        Linear combinations (Q coefficients)
      term.rs             Expression AST
      atom.rs             Boolean atoms (LessEqual, Equal, etc.)
      var_uf.rs           Union-find for equality classes
      phi.rs              Core solver (normalization, propagation, fn_app_eqs)
      citv.rs             Concrete integer intervals (CItv)
    base_stack.rs         Layer 5: Var → AbstractValue map
    base_memory.rs        Layer 5: AbstractValue → Edges heap graph
    base_attrs.rs         Layer 5: AbstractValue → Attributes map
    base_domain.rs        Layer 5: {stack, heap, attrs} composite
    abductive.rs          Layer 7: pre/post state + formula, must_be_valid, check_valid
    operations.rs         Layer 8: eval, read, write, invalidate, allocate, eval_or_fresh
    models/mod.rs         Layer 8: model dispatch via builtin_decl::match_builtin
    models/c.rs           Layer 8: C models (malloc/free/realloc, new/delete, fopen, random, etc.)
    specialization.rs     Layer 8: function pointer dispatch specialization
    interproc.rs          Layer 8: summary application, biabduction, callee→caller mapping
    summary.rs            Layer 8: summary creation, latent/manifest invalid-access classification
    transfer.rs           Layer 9: SIL instruction → state transition (prune extracts constraints and branch-condition provenance)
    diagnostic.rs         Layer 8: error classification and reporting
    execution_domain.rs   Layer 9: ContinueProgram | AbortProgram | ...
    checker.rs            Entry point: analyze + to_issue_log
```

## What This Detects

```c
// Null dereference (intraprocedural)
void null_deref() {
    int *p = NULL;
    *p = 42;          // ERROR: null dereference
}

// Null dereference (interprocedural, with formula)
int *may_return_null() {
    if (condition) return NULL;
    return malloc(sizeof(int));
}
void caller() {
    int *p = may_return_null();
    *p = 42;          // ERROR: possible null dereference (path-sensitive)
}

// Use after free
void uaf() {
    int *p = malloc(sizeof(int));
    free(p);
    *p = 42;          // ERROR: use after free
}

// Retain cycle (ObjC/Swift)
@implementation Foo
- (void)setup {
    self.delegate = self;  // ERROR: retain cycle
}
@end
```

## Key Design Differences from OCaml

| Aspect | OCaml | Rust |
|--------|-------|------|
| Abstract values | `int` with module signature | `AbstractValue(i64)` newtype, thread-local counters |
| History | Deep linked-list of events | Simplified (not ported) |
| Formula | Inlined solver (was sledge) | Port: union-find, linear arith, atoms, CItv, FunctionApplication |
| Result monad | Custom `PulseResult` with `let*` | `enum PulseResult<T>` with combinators |
| Memory edges | `RecencyMap` (bounded) | `BTreeMap` (deterministic iteration) |
| Biabduction | MustBeValid attrs, write_access | must_be_valid set, write_deref pre-edges |
| Parallelism | Per-procedure via ondemand | Same, via `ondemand` crate with rayon |
| Models | ~10K lines across languages | C models: malloc/free/realloc, new/delete, exit/abort, fopen/getcwd, random, 18 stdio |
| Model dispatch | `ProcnameDispatcher` DSL | `builtin_decl::match_builtin` identity matching |
| Specialization | HeapPath-based dynamic types | Same: specialization loop in CLI + checker |
| Null-path provenance | Branch-conditioned summary/report logic | `UsedAsBranchCond` attrs + depth-0 recorded model/prune conditions |

## Excluded

- **Taint analysis**: separate concern, can be added later if needed.
- **TOPL**: temporal property language — orthogonal to Pulse core.

See [TODO.md](../TODO.md) for remaining gaps and backlog.
