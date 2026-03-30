# Infer Architecture Overview

## Codebase Statistics

- **Total OCaml source**: ~235,000 lines across ~1,243 files
- **Largest module**: `pulse/` (49,635 lines) -- the primary analysis engine
- **Core IR**: `IR/` (15,673 lines) -- SIL types and procedure descriptions
- **Abstract interpretation framework**: `absint/` (11,865 lines)
- **Build system**: OCaml 5.3.0 (with flambda) using dune 3.16 + opam
- **Existing Rust code**: Only in vendored `dependencies/charon/` (Rust MIR extraction tool)

### Lines by Module

| Module | Lines | Description |
|--------|-------|-------------|
| `pulse/` | 49,635 | Pulse analysis engine (primary checker) |
| `base/` | 18,707 | Configuration, DB, logging, utilities |
| `bufferoverrun/` | 16,747 | Buffer overrun analysis |
| `IR/` | 15,673 | SIL types, Cfg, Procdesc, Tenv |
| `clang/` | 15,465 | Clang/C/C++/ObjC frontend |
| `sledge/` | 13,818 | SMT/arithmetic solver |
| `checkers/` | 12,772 | Various lightweight checkers |
| `absint/` | 11,865 | Abstract interpretation framework |
| `textual/` | 10,778 | Textual IR format (parser/printer) |
| `python/` | 10,660 | Python frontend |
| `integration/` | 9,134 | Build system integrations, reporting |
| `backend/` | 6,708 | Analysis driver, scheduling, summaries |
| `concurrency/` | 6,293 | Starvation/RacerD checkers |
| `llvm/` | 4,571 | LLVM/Swift frontend |
| `erlang/` | 4,262 | Erlang frontend |
| `cost/` | 3,555 | Cost analysis |
| `java/` | 3,268 | Java frontend |
| `rust/` | 3,004 | Rust frontend (MIR-based) |
| `unit/` | 2,661 | Unit tests |
| `topl/` | 921 | Temporal property language |

## High-Level Pipeline

```
Source Code
    |
    v
[Frontend / Capture]  -- language-specific translation
    |
    v
capture.db (SQLite)   -- procedures table: proc_attributes, cfg (binary OCaml Marshal)
    |                  -- source_files table: type_environment, procedure_names
    v
[Backend / Analysis]  -- abstract interpretation, checkers
    |
    v
analysis.db (SQLite)  -- specs table: summaries per procedure
    |                  -- issue_logs table: issues per checker+source
    v
Report Files          -- JSON, text, SARIF, XML reports
```

All outputs go into the `infer-out/` directory.

## Directory Structure (`infer/src/`)

### Core Infrastructure

- **`base/`** -- Foundation layer: configuration (`Config.ml` -- massive file), command-line parsing, database access (`Database.ml`, `DBWriter.ml`), logging, process pools, utilities
- **`IR/`** -- The SIL intermediate representation: types (`Typ.ml`), expressions (`Exp.ml`), instructions (`Sil.ml`), procedure descriptions (`Procdesc.ml`), CFG (`Cfg.ml`), type environments (`Tenv.ml`), procedure names (`Procname.ml`)
- **`istd/`** -- Internal standard library extensions
- **`absint/`** -- Abstract interpretation framework: `AbstractInterpreter.ml`, `TransferFunctions.ml`, `AbstractDomain.ml`, pattern matching, access paths

### Frontends (Capture)

- **`clang/`** -- Clang AST to SIL translation (C, C++, Objective-C)
- **`java/`** -- Java bytecode to SIL
- **`erlang/`** -- Erlang AST to SIL
- **`python/`** -- Python bytecode to Textual to SIL
- **`llvm/`** -- LLVM IR (via sledge/llair) to Textual to SIL (used for Swift)
- **`rust/`** -- Rust MIR (via Charon) to Textual to SIL
- **`textual/`** -- Textual IR parser/printer and SIL conversion

### Backend (Analysis)

- **`backend/`** -- Analysis driver (`InferAnalyze.ml`), on-demand analysis (`ondemand.ml`), summaries (`Summary.ml`), schedulers, checker registration (`registerCheckers.ml`)
- **`pulse/`** -- Pulse: the primary separation-logic-based analysis. Handles memory safety, null dereference, resource leaks, taint analysis, and more
- **`bufferoverrun/`** -- Buffer overrun and integer overflow analysis
- **`checkers/`** -- Lighter-weight checkers: liveness, lineage, annotation reachability, scope leakage, SIL validation, etc.
- **`concurrency/`** -- RacerD (data race detector) and Starvation analysis
- **`cost/`** -- Computational cost analysis
- **`topl/`** -- Temporal properties (finite automata over API sequences)

### Supporting

- **`integration/`** -- Build system drivers (Buck, Gradle, Maven, etc.), report generation, differential analysis
- **`sledge/`** -- Arithmetic/SMT solver used by Pulse's formula engine. Contains sub-libraries: `llair/` (LLVM Low-level Analysis IR), `nonstdlib/` (containers, zarith), `ppx_dbg/` (debug PPX)
- **`semdiff/`** -- Semantic diffing (Python AST comparison via congruence closure)
- **`atd/`** -- ATD schema definitions and generated JSON serialization for reports, config, commands
- **`opensource/`** -- Stub implementations of Facebook-internal modules for open-source builds
- **`labs/`** -- Educational lab exercises for learning to write Infer checkers
- **`inferppx/`** -- Custom PPX deriver used across the project

## Key Abstractions

### The Compiler Model

Infer operates as a compiler with distinct phases:

```
Source --> [Frontend] --> SIL CFG --> [Pre-analysis] --> [Analysis] --> [Reporting]
```

1. **Frontend/Capture**: Translates source into SIL CFGs stored in `capture.db`
2. **Pre-analysis** (`preanal.ml`): Liveness analysis, devirtualization on the CFG
3. **Analysis**: Runs checkers over the CFG using abstract interpretation
4. **Reporting**: Generates human-readable reports from analysis results

### Supported Languages

```ocaml
type t = Clang | CIL | Erlang | Hack | Java | Python | Rust | Swift
```

### Two IR Levels

1. **SIL** (Smallfoot Intermediate Language) -- the core IR, tightly coupled to OCaml types
2. **Textual** -- a human-readable text format for SIL, used as the translation target by newer frontends (Python, Rust, Swift/LLVM, Hack)

Newer frontends translate to Textual first, which is then converted to SIL. Older frontends (Clang, Java) translate directly to SIL.

### Database Schema

**capture.db** (`procedures` table):
- `proc_uid TEXT PRIMARY KEY` -- unique procedure identifier
- `proc_attributes BLOB` -- OCaml Marshalled `ProcAttributes.t`
- `cfg BLOB` -- OCaml Marshalled `Procdesc.t option`
- `callees BLOB` -- OCaml Marshalled callee list

**capture.db** (`source_files` table):
- `source_file TEXT PRIMARY KEY`
- `type_environment BLOB` -- OCaml Marshalled `Tenv.t`
- `integer_type_widths BLOB`
- `procedure_names BLOB`
- `freshly_captured INT`

**analysis.db** (`specs` table):
- `proc_uid TEXT PRIMARY KEY`
- `proc_name BLOB`
- `report_summary BLOB`
- `summary_metadata BLOB`
- Per-checker payload columns (e.g., `pulse BLOB`, `racerd BLOB`, etc.)

**analysis.db** (`issue_logs` table):
- `checker STRING`
- `source_file TEXT`
- `issue_log BLOB`

All BLOB columns use OCaml's `Marshal` format, which is a key challenge for Rust interop.

**Database configuration**:
- Uses WAL (Write-Ahead Logging) journaling for concurrent access
- `PRAGMA synchronous=OFF` and configurable page/cache sizes for performance
- Supports parallel capture via per-worker secondary databases that are merged afterward (`MergeCapture.merge_captured_targets`)

**Global type environment**: In addition to per-file tenvs in `source_files`, a global `Tenv.t` is stored via `Marshal.to_channel` to a file at `ResultsDir.get_path GlobalTypeEnvironment`.

### Multi-processing Model

Infer supports three execution modes:
1. **Sequential** (`-j 1`): Single-threaded
2. **Multi-process** (`-j N`): Fork-based `ProcessPool` with worker processes
3. **Multi-core** (`--multicore`): OCaml 5 domain-based parallelism via `DomainPool`

The analysis uses various schedulers:
- `FileScheduler` -- analyze by source file
- `SyntacticCallGraph` -- analyze in reverse topological order of the call graph
- `RestartScheduler` -- restart on lock contention
- `ReplayScheduler` -- replay a previous analysis schedule

### Key OCaml Dependencies

- **Core ecosystem**: `core`, `base`, `stdio`, `ppx_jane` (Jane Street standard library)
- **Serialization**: `atd`/`atdgen` (JSON codegen), `yojson`, `biniou`, `xmlm`
- **Parsing**: `menhir` (parser generator), `sedlex` (unicode lexer)
- **Java**: `javalib`, `sawja` (bytecode analysis)
- **Python**: `pyml` (CPython FFI, vendored)
- **Rust**: `charon` (MIR extraction, vendored in `dependencies/`)
- **Database**: `sqlite3`
- **Graphs**: `ocamlgraph`
- **Parallelism**: `parmap` (parallel map)
- **Math**: `zarith` (arbitrary precision)
- **CLI**: `cmdliner`
- **Profiling**: `memtrace`
- **Testing**: `ounit2`, `ppx_expect`, `ppx_inline_test`
- **Containers**: `containers`, `iter`, `bheap`, `unionFind`
