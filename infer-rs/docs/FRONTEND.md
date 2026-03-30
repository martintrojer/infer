# Frontend / Capture Pipeline

The frontend (capture phase) translates source code into SIL CFGs stored in `capture.db`.

## Entry Points

- CLI: `infer capture -- <build command>`
- Driver: `infer/src/integration/Driver.ml`
- Per-language capture: `infer/src/integration/{Clang,Javac,Erlang,Hack,Python,Rust}.ml`

## Frontend Architecture

### Two Translation Strategies

**Direct-to-SIL** (legacy frontends):
```
Source -> [Language-specific AST] -> [Translator] -> SIL CFG -> capture.db
```
Used by: Clang (C/C++/ObjC), Java

**Via Textual** (modern frontends):
```
Source -> [External tool] -> [Intermediate format] -> [Textual emitter] -> .sil files -> [TextualParser] -> [TextualSil] -> SIL CFG -> capture.db
```
Used by: Python, Rust, Swift/LLVM, Hack

### Per-Language Details

#### Clang (C/C++/ObjC) -- `infer/src/clang/` (15,465 lines)

The most mature frontend. Uses Facebook's `facebook-clang-plugins` to get a serialized Clang AST, then translates it directly to SIL.

Key files:
- `cFrontend.ml` -- Main entry point
- `cFrontend_decl.ml` -- Declaration translation
- `cTrans.ml` (in subdirectories) -- Statement/expression translation
- `cContext.ml` -- Translation context
- `cAst_utils.ml` -- AST utilities

#### Java -- `infer/src/java/` (3,268 lines)

Translates Java bytecode (`.class` files) to SIL using the Sawja library.

Key files:
- `jFrontend.ml` -- Main entry point
- `jTrans.ml` -- Bytecode to SIL translation
- `jTransType.ml` -- Type translation
- `jClasspath.ml` -- Classpath handling

#### Python -- `infer/src/python/` (10,660 lines)

Translates Python bytecode to Textual. Uses a custom FFI to call CPython's compiler.

Pipeline: `Python source -> CPython bytecode -> PyIR -> Textual -> SIL`

Key files:
- `FFI.ml` -- CPython FFI
- `PyIR.ml` -- Python IR representation
- `PyIR2Textual.ml` -- PyIR to Textual conversion
- `PyIRTypeInference.ml` -- Type inference

#### Erlang -- `infer/src/erlang/` (4,262 lines)

Translates Erlang AST (from `erlc` compiler) to SIL.

Key files:
- `ErlangTranslator.ml` -- Main translator
- `ErlangAst.ml` -- Erlang AST types
- `ErlangEnvironment.ml` -- Translation environment

#### Rust -- `infer/src/rust/` (3,004 lines)

Translates Rust MIR (via the Charon tool) to Textual.

Pipeline: `Rust source -> rustc -> MIR -> Charon -> ULLBC JSON -> RustMir2Textual -> Textual -> SIL`

Key file: `RustMir2Textual.ml`

Note: The Charon tool is vendored at `dependencies/charon/` and contains extensive Rust code (~100+ files). It is a Rust compiler plugin that extracts MIR from Rust programs into a serialized JSON format. The OCaml side uses the `charon` OCaml library (`dependencies/charon/charon-ml/`) which provides OCaml bindings/types for Charon's output format.

#### LLVM/Swift -- `infer/src/llvm/` (4,571 lines)

Translates LLVM IR (via sledge's LLAIR format) to Textual. Primary use case is Swift.

Pipeline: `Swift source -> swiftc -> LLVM IR -> LLAIR -> Llair2Textual -> Textual -> SIL`

Key files:
- `LlvmFrontend.ml` -- Entry point
- `Llair2Textual.ml` -- LLAIR to Textual
- `Llair2TextualProc.ml` -- Procedure translation
- `Llair2TextualType.ml` -- Type translation

#### Hack

Uses an external Hackc compiler that emits Textual directly. The integration is in `infer/src/integration/Hack.ml`.

## Capture Database (`capture.db`)

### Schema

```sql
CREATE TABLE procedures (
  proc_uid TEXT PRIMARY KEY NOT NULL,
  proc_attributes BLOB NOT NULL,   -- Marshal'd ProcAttributes.t
  cfg BLOB,                        -- Marshal'd Procdesc.t option
  callees BLOB NOT NULL            -- Marshal'd Procname.t list
);

CREATE TABLE source_files (
  source_file TEXT PRIMARY KEY,
  type_environment BLOB NOT NULL,  -- Marshal'd Tenv.t
  integer_type_widths BLOB,        -- Marshal'd IntegerWidths.t
  procedure_names BLOB NOT NULL,   -- Marshal'd Procname.t list
  freshly_captured INT NOT NULL
);
```

All BLOB fields use OCaml's `Marshal.to_string` for serialization.

**Write path**:
1. `Cfg.store` iterates all `Procdesc.t` in the cfg, calls `Attributes.store` for each procedure
2. `Attributes.store` serializes `ProcAttributes.t`, `Procdesc.t` (as cfg blob), and callees, then calls `DBWriter.replace_attributes`
3. `SourceFiles.add` stores source file metadata (tenv, proc_names, integer_type_widths) via `DBWriter.add_source_file`. Handles merging when the same source file is captured multiple times.

**Parallel capture**: Each worker writes to its own secondary database. After capture completes, `MergeCapture.merge_captured_targets` merges per-worker databases into the main `capture.db`. The database uses WAL journaling and `PRAGMA synchronous=OFF` for performance.

## Implications for Rust Port

### Frontend Rewrite Considerations

- **Clang frontend**: Large and complex. Depends on `facebook-clang-plugins`. Not a good first candidate.
- **Java frontend**: Depends on Sawja OCaml library. Moderate complexity.
- **Textual-based frontends**: These are the best candidates because:
  - Textual format is stable and text-based
  - Rust can implement a Textual parser independently
  - The actual language-specific translation (Python->Textual, Rust->Textual) could stay in OCaml initially

### Database Interop Options

1. **Textual files as interchange**: Frontends emit `.sil` files, Rust reads them
   - Pro: No binary compatibility needed
   - Con: Additional I/O, no random access

2. **Rust reads capture.db directly**: Requires deserializing OCaml Marshal format
   - Pro: Works with existing tooling
   - Con: Extremely fragile, version-dependent

3. **New serialization format**: Both OCaml and Rust write/read a common format (e.g., protobuf, FlatBuffers, or JSON)
   - Pro: Clean, well-defined boundary
   - Con: Requires OCaml changes

4. **Rust capture.db**: Rust frontend writes its own capture.db with Rust serialization
   - Pro: Full control
   - Con: Incompatible with OCaml analysis until fully migrated

**Recommendation**: Start with option 1 (Textual files) for the initial phase, then move to option 3 for production.
