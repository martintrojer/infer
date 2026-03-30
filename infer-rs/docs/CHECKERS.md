# Infer Checkers

Detailed breakdown of the analysis checkers, focusing on complexity and migration priority.

## Pulse -- The Primary Analyzer

**Location**: `infer/src/pulse/` (49,635 lines -- 21% of codebase)
**Languages**: All (Clang, Erlang, Hack, Java, CIL, Python, Rust, Swift)
**Type**: Interprocedural with specialization (disjunctive abstract interpretation)

### What It Does

Pulse is a separation-logic-based analysis engine that detects:
- Null pointer dereferences
- Use-after-free / dangling pointers
- Memory leaks
- Resource leaks
- Taint flows (sources -> sinks)
- Retain cycles (ObjC/Swift)
- Unnecessary copy detection (C++)
- Thread safety violations

### Architecture

```
PulseAbductiveDomain -- the core abstract state
    |
    +-- PulseBaseStack -- stack variable bindings
    +-- PulseBaseMemory -- heap graph (points-to)
    +-- PulseBaseAddressAttributes -- attributes per address
    +-- PulseFormula -- arithmetic constraints (uses sledge)
    +-- PulseDecompiler -- reverse mapping to source terms
```

Key components:
- **`PulseOperations.ml`** -- Core heap operations (read, write, allocate, free)
- **`PulseCallOperations.ml`** -- Call handling (dispatch, model application)
- **`PulseModels*.ml`** -- Per-language models (C, C++, Java, ObjC, Hack, Python, Erlang, Swift)
- **`PulseFormula.ml`** -- Linear arithmetic solver
- **`PulseTaintOperations.ml`** -- Taint analysis
- **`PulseInterproc.ml`** -- Inter-procedural analysis (summary application)
- **`PulseJoin.ml`** -- Abstract state joining
- **`PulseDiagnostic.ml`** -- Error reporting
- **`PulseSpecialization.ml`** -- Procedure specialization

### Disjunctive Analysis

Pulse maintains a set of disjunctive abstract states (not a single state). The `MakeDisjunctive` interpreter processes each disjunct independently and manages the set according to configurable policies (max disjuncts, join strategies).

### Pulse Summary Type

```ocaml
type pre_post_list = ExecutionDomain.summary list
type summary = {pre_post_list: pre_post_list; non_disj: NonDisjDomain.Summary.t}
type t = {main: summary; specialized: summary Specialization.Pulse.Map.t}
```

Each disjunct is an `ExecutionDomain.summary` (a Hoare triple of pre/post abstract heaps). The non-disjunctive component tracks cross-disjunct information like unnecessary copies.

### Migration Priority: HIGH (but complex)

Pulse is the most impactful checker to migrate due to:
- It's the largest module (50K lines)
- It runs on all languages
- Disjunctive analysis creates many states -- perfect for parallel processing
- The formula engine (sledge) could benefit from Rust's performance

But it's also the most complex, so it should be approached incrementally.

## RacerD -- Data Race Detector

**Location**: `infer/src/concurrency/` (6,293 lines)
**Languages**: Clang, Java, CIL
**Type**: Interprocedural + File-level reporting

Detects potential data races in multi-threaded code. Uses thread-aware abstract domains to track:
- Lock sets
- Thread identity (main thread, background thread, any)
- Memory accesses (reads/writes with locations)

Two phases:
1. Per-procedure analysis (`RacerDProcAnalysis`)
2. File-level reporting (`RacerDFileAnalysis`) -- compares accesses across procedures

### Migration Priority: MEDIUM

Moderate size, well-contained. File-level reporting is a good candidate for parallelization.

## Buffer Overrun (InferBO)

**Location**: `infer/src/bufferoverrun/` (16,747 lines)
**Languages**: Clang, Java
**Type**: Interprocedural (two-phase: analysis + checker)

Detects array out-of-bounds accesses using abstract interpretation with interval domains.

### Migration Priority: LOW-MEDIUM

Mature, stable. Less active development.

## Cost Analysis

**Location**: `infer/src/cost/` (3,555 lines)
**Languages**: Clang, Java, Hack
**Type**: Interprocedural (depends on BufferOverrun + Purity)

Computes computational cost bounds (execution time complexity).

### Migration Priority: LOW

Depends on BufferOverrun. Relatively small.

## Starvation / Deadlock

**Location**: `infer/src/concurrency/` (shared with RacerD)
**Languages**: Java, Clang
**Type**: Interprocedural + File-level

Detects potential deadlocks from lock ordering violations.

### Migration Priority: LOW-MEDIUM

## Lightweight Checkers (`infer/src/checkers/`)

These are simpler, often intraprocedural checkers:

| Checker | Lines | Description |
|---------|-------|-------------|
| Liveness | ~500 | Dead variable detection |
| Lineage | ~2000 | Data flow lineage (Erlang) |
| AnnotationReachability | ~500 | Annotation-based reachability |
| ScopeLeakage | ~300 | Java scope violations |
| SIOF | ~400 | C++ static init order |
| Impurity | ~200 | Function purity |
| SelfInBlock | ~400 | ObjC block issues |
| ConfigImpactAnalysis | ~800 | Config change analysis |
| SILValidation | ~600 | SIL IR validation |

### Migration Priority: MEDIUM-HIGH for validation, LOW for others

The simpler checkers make good "first checkers" to port for learning and validation.

## Checker Registration System

Checkers are registered in `registerCheckers.ml` with:
1. A `Checker.t` identifier
2. A list of `(callback_fun * Language.t)` pairs
3. Callback types: `Procedure`, `ProcedureWithSpecialization`, or `File`

The checker API:
```ocaml
(* Intraprocedural: no summaries *)
val intraprocedural : (IntraproceduralAnalysis.t -> unit) -> callback_fun

(* Interprocedural: produces and uses summaries *)
val interprocedural : 'payload Payloads.field -> checker -> callback_fun

(* File-level: runs after all procedures analyzed *)
val file : 'payload Payloads.field -> file_checker -> callback_fun
```

### Rust Equivalent

```rust
trait TransferFunctions {
    type Domain: AbstractDomain;
    type AnalysisData;

    fn exec_instr(
        &self,
        state: &Self::Domain,
        data: &Self::AnalysisData,
        node: &Node,
        instr: &Sil::Instr,
    ) -> Self::Domain;
}

trait Checker {
    type Summary;

    fn analyze(&self, pdesc: &ProcDesc) -> Self::Summary;
}
```

## Priority Summary for Migration

| Priority | Component | Rationale |
|----------|-----------|-----------|
| **Phase 1** | SIL types, Textual parser | Foundation for everything |
| **Phase 2** | Abstract interpretation framework | Core engine |
| **Phase 3** | Simple checkers (Liveness, SILValidation) | Validate the framework |
| **Phase 4** | Scheduling/orchestration | Unlock parallelism |
| **Phase 5** | Pulse (incremental) | Biggest impact |
| **Phase 6** | RacerD, Starvation | Additional checkers |
