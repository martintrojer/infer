# Backend / Analysis Pipeline

The backend reads `capture.db`, runs analysis checkers over the SIL CFGs, and produces reports.

## Entry Points

- CLI: `infer analyze`
- Main driver: `infer/src/backend/InferAnalyze.ml`
- On-demand analysis: `infer/src/backend/ondemand.ml`
- Checker registration: `infer/src/backend/registerCheckers.ml`

## Analysis Pipeline

```
capture.db
    |
    v
[Register checkers] -- registerCheckers.ml
    |
    v
[Build schedule]     -- FileScheduler / SyntacticCallGraph / RestartScheduler
    |
    v
[For each procedure]:
    |
    +-> [Load Procdesc from DB]
    +-> [Pre-analysis] -- preanal.ml (liveness, devirtualization)
    +-> [Run checkers] -- on-demand, inter-procedural
    +-> [Store summaries] -- Summary.ml -> analysis.db
    |
    v
[File-level checkers] -- e.g., RacerD file reporting
    |
    v
[Report generation] -- JSON, text, SARIF reports
```

## Abstract Interpretation Framework (`infer/src/absint/`)

### Core Interfaces

**`AbstractDomain.ml`** -- Abstract domain interface:
```ocaml
module type Comparable = sig
  type t
  val pp : F.formatter -> t -> unit
  val leq : lhs:t -> rhs:t -> bool
end

module type S = sig
  include Comparable
  val join : t -> t -> t                 (* least upper bound *)
  val widen : prev:t -> next:t -> num_iters:int -> t  (* widening *)
end

module type WithBottom = sig include S; val bottom : t; val is_bottom : t -> bool end
module type WithTop = sig include S; val top : t; val is_top : t -> bool end
module type WithBottomTop = sig include WithBottom; val top : t; val is_top : t -> bool end

(* For disjunctive analyses like Pulse *)
module type Disjunct = sig
  include Comparable
  val equal_fast : t -> t -> bool
  val is_normal : t -> bool
  val is_exceptional : t -> bool
  val is_executable : t -> bool
  val exceptional_to_normal : t -> t
end
```

**Domain combinators** (ready-made building blocks):
`BottomLifted`, `TopLifted`, `BottomTopLifted`, `Pair`, `Flat`, `Stacked`, `FiniteSet`, `InvertedSet`, `Map`, `InvertedMap`, `SafeInvertedMap`, `FiniteMultiMap`, `BooleanAnd`, `BooleanOr`, `CountDomain`, `DownwardIntDomain`

**`TransferFunctions.ml`** -- Transfer function interface:
```ocaml
module type SIL = sig
  module CFG : ProcCfg.S
  module Domain : AbstractDomain.S
  type analysis_data
  val exec_instr : Domain.t -> analysis_data -> CFG.Node.t -> Sil.instr -> Domain.t
  val pp_session_name : CFG.Node.t -> F.formatter -> unit
end

(* Disjunctive analysis interface, used by Pulse *)
module type DisjReady = sig
  module CFG : ProcCfg.S
  module DisjDomain : AbstractDomain.Disjunct
  module NonDisjDomain : AbstractDomain.WithBottomTop
  type analysis_data
  val exec_instr : limit:int -> DisjDomain.t * NonDisjDomain.t -> analysis_data ->
    CFG.Node.t -> Sil.instr -> DisjDomain.t list * NonDisjDomain.t
  val exec_instr_non_disj : NonDisjDomain.t -> analysis_data -> CFG.Node.t ->
    Sil.instr -> NonDisjDomain.t
  val remember_dropped_disjuncts : DisjDomain.t list -> NonDisjDomain.t -> NonDisjDomain.t
end
```

**`AbstractInterpreter.ml`** -- Fixpoint computation (5 variants):
- `MakeRPO` -- Forward, reverse post-order scheduling (fast for straight-line code)
- `MakeWTO` -- Forward, weak topological order / Bourdoncle's SCC (better for loops, supports widening + narrowing with modes `Widen | WidenThenNarrow | Narrow`)
- `MakeDisjunctive` -- WTO-based, disjunctive domain (list of disjuncts + non-disjunctive state). Used by **Pulse**. Respects `DConfig.join_policy` (max disjuncts) and `DConfig.widen_policy` (max iterations)
- `MakeBackwardRPO` -- Backward RPO with exceptional flow handling
- `MakeBackwardWTO` -- Backward WTO with exceptional flow handling

The fixpoint engine handles:
- **Widening** at loop heads when `is_loop_head && not is_narrowing`
- **Narrowing** with configurable `max_narrows` limit
- **Convergence check** via `Domain.leq`

### Analysis Types

1. **Intraprocedural**: Analyzes one procedure in isolation
2. **Interprocedural**: Uses summaries from callees (on-demand)
3. **File-level**: Post-processing after all procedures analyzed (e.g., RacerD)

### On-Demand Analysis (`ondemand.ml`)

When a checker encounters a call:
1. Look up callee's summary in analysis.db cache
2. If not found, trigger analysis of the callee
3. Use the callee's summary to continue the caller's analysis
4. Handle mutual recursion via `ProcLocker` and restart

**Mutual recursion handling**: `ActiveProcedures` maintains a `Hash_queue` of currently-analyzed procedures. When a cycle is detected:
- If `number_of_recursion_restarts >= limit` or this is the cycle start, return `Error MutualRecursionCycle` (cut the cycle)
- Otherwise, raise `RecursiveCycleException.RecursiveCycle` to restart analysis from the "best" cycle start (smallest arity procedure)

**Analysis filtering**: `AnalysisRequest.t` controls which callbacks fire:
```ocaml
type t = All | One of PayloadId.t | CheckerWithoutPayload of checker_without_payload
```
When `One payload_id`, only the checker that produces that payload (and its transitive dependencies) runs.

### Summaries (`Summary.ml`)

```ocaml
type t = {
  payloads: Payloads.t;        (* per-checker analysis results *)
  sessions: int;                (* nodes visited count *)
  stats: Stats.t;              (* execution statistics *)
  proc_name: Procname.t;
  err_log: Errlog.t;           (* detected issues *)
  dependencies: Dependencies.t; (* analysis-time dependencies *)
  is_complete_result: bool;
}
```

### Payloads (`Payloads.ml`)

Each checker stores its results in a typed payload field. All fields are `SafeLazy.t option` -- **lazily deserialized** from SQLite to avoid loading unnecessary payloads:

```ocaml
type t = {
  annot_map: AnnotationReachabilityDomain.t SafeLazy.t option;
  buffer_overrun_analysis: BufferOverrunAnalysisSummary.t SafeLazy.t option;
  buffer_overrun_checker: BufferOverrunCheckerSummary.t SafeLazy.t option;
  config_impact_analysis: ConfigImpactAnalysis.Summary.t SafeLazy.t option;
  cost: CostDomain.summary SafeLazy.t option;
  disjunctive_demo: DisjunctiveDemo.domain SafeLazy.t option;
  static_constructor_stall_checker: StaticConstructorStallChecker.Summary.t SafeLazy.t option;
  lab_resource_leaks: ResourceLeakDomain.summary SafeLazy.t option;
  litho_required_props: LithoDomain.summary SafeLazy.t option;
  pulse: PulseSummary.t SafeLazy.t option;
  purity: PurityDomain.summary SafeLazy.t option;
  racerd: RacerDDomain.summary SafeLazy.t option;
  scope_leakage: ScopeLeakage.Summary.t SafeLazy.t option;
  siof: SiofDomain.Summary.t SafeLazy.t option;
  lineage: Lineage.Summary.t SafeLazy.t option;
  lineage_shape: LineageShape.Summary.t SafeLazy.t option;
  starvation: StarvationDomain.summary SafeLazy.t option;
}
```

Loading modes:
- **Lazy**: `Payloads.SQLite.lazy_load` -- each field loads from DB on first access
- **Eager**: `Payloads.SQLite.eager_load` -- all columns loaded at once

Summaries are cached in memory via `Summary.OnDisk.Cache` (a `Procname.Cache` with a two-layer structure: `procname -> analysis_request -> summary`), with optional LRU eviction for multicore mode.

### Dependencies Tracking (`Dependencies.ml`)

```ocaml
type complete = {
  summary_loads: Procname.t list;      (* which summaries were loaded *)
  recursion_edges: Procname.Set.t;     (* which edges were recursion cuts *)
  other_proc_names: Procname.t list;
  used_tenv_sources: SourceFile.t list; (* which type environments were consulted *)
}
type t = Partial | Complete of complete
```

Dependencies are `Partial` (mutable) during analysis and frozen to `Complete` when stored. Used for incremental analysis and replay scheduling.

## Registered Checkers

From `registerCheckers.ml`, the active checkers and their languages:

| Checker | Languages | Type | Description |
|---------|-----------|------|-------------|
| **Pulse** | Clang, Erlang, Hack, Java, CIL, Python, Rust, Swift | Interprocedural+Specialization | Primary analysis: null deref, memory safety, taint, leaks |
| **RacerD** | Clang, Java, CIL | Interprocedural+File | Data race detection |
| **Starvation** | Java, Clang | Interprocedural+File | Deadlock detection |
| **BufferOverrunAnalysis** | Clang, Java | Interprocedural | Array out-of-bounds |
| **BufferOverrunChecker** | Clang, Java | Interprocedural(2) | Reports from BO analysis |
| **Cost** | Clang, Java, Hack | Interprocedural(3) | Computational cost |
| **Liveness** | Clang | Intraprocedural | Dead variable detection |
| **Lineage** | Erlang | Interprocedural | Data lineage tracking |
| **AnnotationReachability** | Clang, Erlang, Java, Swift | Interprocedural | Annotation-based reachability |
| **ConfigImpactAnalysis** | Clang, Java | Interprocedural | Config change impact |
| **ScopeLeakage** | Java | Interprocedural | Scope-based leak detection |
| **SIOF** | Clang | Interprocedural | Static init order fiasco |
| **Impurity** | Java, Clang, Hack | Intraprocedural(dep) | Function purity |
| **SelfInBlock** | Clang | Intraprocedural | ObjC self-in-block |
| **ParameterNotNullChecked** | Clang | Intraprocedural | Missing null checks |
| **SILValidation** | Java, Clang, Erlang | Intraprocedural | SIL IR validation |

## Scheduling

### File Scheduler
Analyzes all procedures in a source file together, files processed in parallel.

### Syntactic Call Graph Scheduler
Builds a call graph from captured procedures and analyzes in reverse topological order (callees before callers).

### Restart Scheduler
Handles lock contention: if a procedure is locked by another worker, skip it and retry later.

### Replay Scheduler
Replays a previous analysis schedule for reproducibility.

## Multi-processing

```
Orchestrator Process
    |
    +-- Worker 1: analyze_target(File/Procname)
    +-- Worker 2: analyze_target(File/Procname)
    +-- Worker N: analyze_target(File/Procname)
    |
    v
Shared: capture.db (read), analysis.db (write via DBWriter)
```

Workers communicate through:
- `ProcessPool` -- fork-based parallelism
- `DomainPool` -- OCaml 5 domain-based parallelism
- `ProcLocker` -- file-lock-based mutual exclusion per procedure
- `DBWriter` -- serialized database writes

## Analysis Database (`analysis.db`)

### Schema
```sql
CREATE TABLE specs (
  proc_uid TEXT PRIMARY KEY NOT NULL,
  proc_name BLOB NOT NULL,
  report_summary BLOB NOT NULL,
  summary_metadata BLOB,
  -- per-checker payload columns:
  pulse BLOB,
  racerd BLOB,
  buffer_overrun_analysis BLOB,
  ...
);

CREATE TABLE issue_logs (
  checker STRING NOT NULL,
  source_file TEXT NOT NULL,
  issue_log BLOB NOT NULL,
  PRIMARY KEY (checker, source_file)
);
```

## Implications for Rust Port

### Best Candidates for Rust Rewrite

1. **Abstract interpretation framework** -- the core fixpoint engine is well-defined and benefits from multi-threading
2. **Pulse** -- largest single module, performance-critical, would benefit most from parallelism
3. **Scheduling/orchestration** -- natural fit for Rust's concurrency primitives
4. **Summary storage** -- replace Marshal with a cross-language format

### Challenges

1. **On-demand analysis**: Requires inter-process/inter-thread coordination
2. **Summary format**: Currently OCaml Marshal, needs a new format
3. **Checker API**: Each checker implements `TransferFunctions` -- need a Rust trait equivalent
4. **Functor-heavy design**: OCaml uses functors extensively for `AbstractInterpreter.Make(TransferFunctions)` -- Rust generics are a natural fit
5. **Process pool**: OCaml fork-based model doesn't map to Rust. Use rayon or tokio instead.
