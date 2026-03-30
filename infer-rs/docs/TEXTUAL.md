# Textual IR Format

Textual is a human-readable, text-based intermediate representation for SIL. It serves as a stable interface between language frontends and the Infer analysis engine.

## Role in the Pipeline

```
Newer frontends (Python, Rust, Swift, Hack)
    |
    v
Textual (.sil files)    <-- human-readable text format
    |
    v
[TextualParser]         <-- Menhir-based parser
    |
    v
Textual AST             <-- in-memory representation
    |
    v
[TextualSil]            <-- conversion to SIL
    |
    v
SIL CFG                 <-- stored in capture.db
```

Older frontends (Clang, Java) bypass Textual and emit SIL directly.

## Source Files

All in `infer/src/textual/` (~10,778 lines):

| File | Description |
|------|-------------|
| `Textual.ml/mli` | Core AST types |
| `TextualParser.ml/mli` | Parser driver |
| `TextualMenhir.mly` | Menhir grammar |
| `TextualLexer.ml/mli` | Lexer |
| `TextualSil.ml/mli` | Textual-to-SIL conversion |
| `TextualOfSil.ml/mli` | SIL-to-Textual conversion (for debugging/testing) |
| `TextualBasicVerification.ml/mli` | Basic verification passes |
| `TextualTypeVerification.ml/mli` | Type checking |
| `TextualVerification.ml/mli` | Combined verification |
| `TextualDecls.ml/mli` | Declaration handling |
| `TextualTransform.ml/mli` | AST transformations |
| `LineMap.ml/mli` | Source line mapping |

## Textual AST Types

### Languages
```ocaml
module Lang : sig
  type t = C | Hack | Java | Python | Rust | Swift | ObjectiveC
end
```

### Types
```ocaml
module Typ : sig
  type t =
    | Int           (* integer *)
    | Float         (* float *)
    | Null
    | Void          (* void *)
    | Fun of function_prototype option  (* function type *)
    | Ptr of t * Attr.t list   (* pointer with attributes *)
    | Struct of TypeName.t     (* named struct *)
    | Array of t               (* array *)
end
```

Note: Textual types are simpler than SIL types -- they omit integer widths, float kinds, and pointer kinds, deferring these details to attributes.

### Expressions
```ocaml
module Exp : sig
  type t =
    | Var of Ident.t              (* temporary *)
    | Load of {exp; typ}          (* heap load *)
    | Lvar of VarName.t           (* variable address *)
    | Field of {exp; field}       (* field access *)
    | Index of t * t              (* array index *)
    | Const of Const.t            (* constant *)
    | If of {cond; then_; else_}  (* conditional expression *)
    | Call of {proc; args; kind}  (* function call *)
    | Closure of {proc; captured; params; attributes}
    | Apply of {closure; args}    (* closure application *)
    | Typ of Typ.t                (* type expression *)
end
```

Key differences from SIL expressions:
- `Call` is an expression (not an instruction) in Textual
- `If` conditional expressions exist
- `Load` is an expression (not an instruction)
- No `Sizeof`, `Cast` (handled via builtins)
- `Closure` and `Apply` for first-class functions

### Instructions
```ocaml
module Instr : sig
  type t =
    | Load of {id; exp; typ; loc}   (* id <- *exp *)
    | Store of {exp1; typ; exp2; loc}  (* *exp1 <- exp2 *)
    | Prune of {exp; loc}           (* assume exp *)
    | Let of {id; exp; loc}         (* id = exp, including calls *)
end
```

Key difference: `Let` binds the result of an expression (including calls) to an identifier.

### Terminators
```ocaml
module Terminator : sig
  type t =
    | If of {bexp; then_; else_}  (* conditional branch *)
    | Ret of Exp.t                (* return *)
    | Jump of node_call list      (* jump to labeled nodes *)
    | Throw of Exp.t              (* throw exception *)
    | Unreachable                 (* unreachable code *)
end
```

### Nodes (Basic Blocks)
```ocaml
module Node : sig
  type t = {
    label: NodeName.t;
    ssa_parameters: (Ident.t * Typ.t) list;  (* SSA phi-like parameters *)
    exn_succs: NodeName.t list;
    last: Terminator.t;
    instrs: Instr.t list;
    last_loc: Location.t;
    label_loc: Location.t;
  }
end
```

### Procedure Declarations and Definitions
```ocaml
module ProcDesc : sig
  type t = {
    procdecl: ProcDecl.t;
    nodes: Node.t list;
    start: NodeName.t;
    params: VarName.t list;
    locals: (VarName.t * Typ.annotated) list;
    exit_loc: Location.t;
  }
end
```

### Module (Top-Level)
```ocaml
module Module : sig
  type decl =
    | Global of Global.t
    | Struct of Struct.t
    | Procdecl of ProcDecl.t
    | Proc of ProcDesc.t

  type t = {attrs: Attr.t list; decls: decl list; sourcefile: SourceFile.t}
end
```

## Textual Syntax Example

```
.source_language = "hack"

type Cell = { value: int; next: *Cell }

define .static Cell.create(value: int) : *Cell {
  #entry:
    n0 = __sil_allocate(<Cell>)
    store n0.Cell.value <- value : int
    store n0.Cell.next <- null : *Cell
    ret n0
}
```

## Conversion Pipeline

### Textual -> SIL (full pipeline for Textual-based frontends)

The canonical pattern used by all Textual-based frontends (e.g., `Rust.ml`, `Python.ml`):

1. Parse source into language-specific IR (e.g., PyIR, Charon ULLBC)
2. Translate to `Textual.Module.t` (e.g., `PyIR2Textual.mk_module`, `RustMir2Textual.mk_module`)
3. **Verify**: `TextualVerification.verify_strict` -- structural and type checking
4. **Transform**: `TextualTransform.run_exn lang` -- runs SSA restoration, closure application fixing, Hackc mistranslation fixes
5. **Convert**: `TextualSil.module_to_sil lang module_ decls` -> `(Cfg.t * Tenv.t)`
6. **Capture**: `TextualParser.TextualFile.capture ~use_global_tenv sil` -- stores in capture.db

The `TextualTransform` step (step 4) is important -- it performs several fixups:
- SSA restoration (ensuring SSA form)
- Closure application fixing
- Hackc-specific mistranslation corrections
- Other language-specific normalizations

### `TextualSil.module_to_sil`

The core conversion function. Bridges include detailed type-name mapping (e.g., `TypeNameBridge` maps Textual type names to `Typ.Name.t` variants like `JavaClass`, `HackClass`, `PythonClass`, `SwiftClass` based on the source language).

### SIL -> Textual (`TextualOfSil.ml`)

Used for debugging and testing (triggered by `--dump-textual` flag). Converts SIL CFGs back to Textual format.

## Significance for Rust Port

Textual is the **ideal interop boundary** for the Rust port because:

1. **Text-based**: No binary format compatibility needed
2. **Well-defined grammar**: Can implement parser/printer in Rust independently
3. **Used by newer frontends**: Python, Rust, Swift, Hack all emit Textual
4. **Simpler than SIL**: Fewer language-specific quirks
5. **Bidirectional**: Can convert SIL <-> Textual for testing

### Strategy
- Implement a Rust Textual parser (replaces Menhir parser)
- Implement a Rust Textual-to-SIL converter
- Use Textual files as the bridge between OCaml frontends and Rust analysis
- For testing: OCaml emits Textual, Rust parses it, both analyze and compare
