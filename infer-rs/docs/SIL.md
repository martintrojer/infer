# SIL -- Smallfoot Intermediate Language

SIL is the core intermediate representation of Infer. All source languages are translated into SIL before analysis. It represents programs as control flow graphs (CFGs) of basic blocks containing SIL instructions.

## Core Files

All in `infer/src/IR/`:

| File | Lines | Description |
|------|-------|-------------|
| `Sil.ml/mli` | 373 | Instructions and metadata |
| `Typ.ml/mli` | 1,333 | Type system |
| `Exp.ml/mli` | 434 | Expressions |
| `Procdesc.ml/mli` | 1,567 | Procedure descriptions (CFGs) |
| `Cfg.ml/mli` | 103 | Top-level CFG (collection of Procdescs) |
| `Tenv.ml/mli` | 871 | Type environments |
| `Procname.ml/mli` | ~1,500 | Procedure name types (complex, language-aware) |
| `Struct.ml/mli` | ~400 | Struct type definitions |
| `Ident.ml/mli` | ~300 | Identifiers (temporaries) |
| `Pvar.ml/mli` | ~400 | Program variables |
| `Fieldname.ml/mli` | ~200 | Field names |
| `Binop.ml/mli` | ~200 | Binary operators |
| `Unop.ml/mli` | ~50 | Unary operators |
| `Const.ml/mli` | ~100 | Constants |

## Instructions (`Sil.instr`)

SIL has only 5 instruction kinds:

```ocaml
type instr =
  | Load of {id: Ident.t; e: Exp.t; typ: Typ.t; loc: Location.t}
      (* id = *e:typ -- load from heap *)
  | Store of {e1: Exp.t; typ: Typ.t; e2: Exp.t; loc: Location.t}
      (* *e1:typ = e2 -- store to heap *)
  | Prune of Exp.t * Location.t * bool * if_kind
      (* conditional pruning -- encodes control flow *)
  | Call of (Ident.t * Typ.t) * Exp.t * (Exp.t * Typ.t) list * Location.t * CallFlags.t
      (* ret = fun(args) -- function call *)
  | Metadata of instr_metadata
      (* auxiliary info: nullify, scope, lifetime, etc. *)
```

### Instruction Metadata

```ocaml
type instr_metadata =
  | Abstract of Location.t           (* abstraction point *)
  | CatchEntry of {try_id; loc}      (* C++ catch block entry *)
  | ExitScope of Var.t list * Location.t  (* remove temporaries *)
  | Nullify of Pvar.t * Location.t   (* nullify stack var *)
  | LoopBackEdge of {header_id}      (* loop back edge *)
  | LoopEntry of {header_id}         (* nested loop entry *)
  | LoopExit of {header_id}          (* loop exit *)
  | Skip                             (* no-op *)
  | TryEntry of {try_id; loc}        (* C++ try block entry *)
  | TryExit of {try_id; loc}         (* C++ try block exit *)
  | VariableLifetimeBegins of {pvar; typ; loc; is_cpp_structured_binding}
```

## Types (`Typ.t`)

```ocaml
type t = {desc: desc; quals: type_quals}

type desc =
  | Tint of ikind      (* integer types: char, short, int, long, etc. *)
  | Tfloat of fkind    (* float, double, long double *)
  | Tvoid              (* void *)
  | Tfun of function_prototype option  (* function type *)
  | Tptr of t * ptr_kind  (* pointer with kind *)
  | Tstruct of name    (* named struct/class type *)
  | TVar of string     (* type variable for templates *)
  | Tarray of {elt: t; length: IntLit.t option; stride: IntLit.t option}
```

### Integer Kinds
```ocaml
type ikind =
  | IChar | ISChar | IUChar | IBool
  | IInt | IUInt | IShort | IUShort
  | ILong | IULong | ILongLong | IULongLong
  | I128 | IU128
```

### Pointer Kinds
```ocaml
type ptr_kind =
  | Pk_pointer                   (* C/C++/Java/ObjC standard pointer *)
  | Pk_lvalue_reference          (* C++ & *)
  | Pk_rvalue_reference          (* C++ && *)
  | Pk_objc_weak                 (* ObjC __weak *)
  | Pk_objc_unsafe_unretained   (* ObjC __unsafe_unretained *)
  | Pk_objc_autoreleasing       (* ObjC __autoreleasing *)
  | Pk_objc_nullable_block      (* ObjC nullable block *)
  | Pk_objc_nonnull_block       (* ObjC nonnull block *)
```

### Type Names (Structs/Classes)
```ocaml
type name =
  | CStruct of QualifiedCppName.t
  | CUnion of QualifiedCppName.t
  | CppClass of {name; template_spec_info; is_union}
  | CSharpClass of CSharpClassName.t
  | ErlangType of ErlangTypeName.t
  | HackClass of HackClassName.t
  | JavaClass of JavaClassName.t
  | ObjcClass of QualifiedCppName.t
  | ObjcProtocol of QualifiedCppName.t
  | PythonClass of PythonClassName.t
  | SwiftClass of SwiftClassName.t
  | ObjcBlock of objc_block_sig
  | CFunction of c_function_sig
  | SwiftClosure of Mangled.t
```

## Expressions (`Exp.t`)

```ocaml
type t =
  | Var of Ident.t          (* temporary variable *)
  | UnOp of Unop.t * t * Typ.t option  (* unary op *)
  | BinOp of Binop.t * t * t  (* binary op *)
  | Exn of t                (* exception value *)
  | Closure of closure      (* lambda/closure *)
  | Const of Const.t        (* constant *)
  | Cast of Typ.t * t       (* type cast *)
  | Lvar of Pvar.t          (* address of program variable *)
  | Lfield of lfield_obj_data * Fieldname.t * Typ.t  (* field access *)
  | Lindex of t * t         (* array index *)
  | Sizeof of sizeof_data   (* sizeof expression *)
```

## Control Flow Graph (`Procdesc.t`)

A `Procdesc.t` represents a single procedure's CFG:

```ocaml
type t = {
  mutable attributes: ProcAttributes.t;
  mutable nodes: Node.t list;
  mutable nodes_num: int;
  mutable start_node: Node.t;
  mutable exit_node: Node.t;
  mutable loop_heads: NodeSet.t option;
  mutable wto: Node.t WeakTopologicalOrder.Partition.t option;
}
```

- **Nodes** (`Procdesc.Node.t`): Basic blocks with instructions
  - Start node, exit node, statement nodes, prune nodes, join nodes
  - Each node has successors, predecessors, and exception handlers
  - Nodes are **mutable records** with mutable `preds`, `succs`, `exn`, and `instrs` fields
- **Instructions**: Ordered list of `Sil.instr` per node (via `Instrs.not_reversed_t` -- an efficient reversible array)
- **Attributes** (`ProcAttributes.t`): Procedure metadata (formals, locals, return type, etc.)

### Node Internal Structure

```ocaml
type Node.t = {
  id: id;
  mutable dist_exit: int option;
  mutable wto_index: int;
  mutable exn: t list;           (* exception handler nodes *)
  mutable instrs: Instrs.not_reversed_t;
  kind: nodekind;
  loc: Location.t;
  mutable preds: t list;         (* predecessor nodes *)
  pname: Procname.t;
  mutable succs: t list;         (* successor nodes *)
  mutable code_block_exit: t option;
}
```

### Node Kinds
```ocaml
type nodekind =
  | Start_node
  | Exit_node
  | Stmt_node of stmt_nodekind  (* many sub-kinds for different statements *)
  | Join_node
  | Prune_node of bool * Sil.if_kind * prune_node_kind
  | Skip_node of string
```

## Type Environment (`Tenv.t`)

Internally a concurrent hash table (`Struct.t TypenameHash.t`) mapping type names to struct definitions.

```ocaml
type t = Struct.t TypenameHash.t
```

Key operations:
- `lookup : t -> Typ.Name.t -> Struct.t option` -- with C/C++ interop fallback (`CStruct` falls back to `CppClass` and vice versa)
- `mk_struct` -- create and register struct types
- `fold_supers` / `find_map_supers` -- class hierarchy traversal (with special Hack trait reordering)
- `resolve_method` -- virtual dispatch resolution across class hierarchies

Storage:
- Per-source-file tenvs stored in `capture.db` `source_files` table
- Global tenv stored as a marshalled file at `ResultsDir.get_path GlobalTypeEnvironment`
- Normalization (`Tenv.Normalizer.normalize`) maximizes structural sharing via `hash_normalize` before storage

## Struct Definitions (`Struct.t`)

```ocaml
type t = {
  fields: field list;              (* non-static fields *)
  statics: field list;             (* static fields *)
  supers: Typ.Name.t list;         (* superclasses *)
  objc_protocols: Typ.Name.t list;
  methods: tenv_method list;       (* defined methods *)
  exported_objc_methods: Procname.t list;
  annots: Annot.Item.t;
  class_info: ClassInfo.t;         (* NoInfo | CppClassInfo | JavaClassInfo | HackClassInfo *)
  dummy: bool;                     (* dummy struct for static methods *)
  source_file: SourceFile.t option;
}
```

Each `field` is `{name: Fieldname.t; typ: Typ.t; annot: Annot.Item.t; objc_property_attributes}`.

## Procedure Names (`Procname.t`)

Complex type covering all supported languages:
- C functions (with optional mangling)
- C++ methods (with class, template args)
- Java methods (with class, parameter types, return type)
- ObjC methods (with class, kind)
- Hack functions/methods
- Python functions/methods
- Erlang functions
- Rust functions
- Swift methods
- Block/closure names

## Serialization

SIL is serialized using OCaml's `Marshal` module (binary format) for storage in SQLite databases. This format is:
- OCaml-version-specific
- Not stable across OCaml compiler versions
- Not designed for cross-language interop
- The primary blocker for Rust interop at the DB level

**Hash normalization**: Before serialization, instructions are hash-normalized (`Sil.hash_normalize_instr`) to maximize structural sharing in the database. This means identical sub-expressions across different procedures share the same marshalled representation, reducing DB size.

**Alternative**: The **Textual** format provides a human-readable, stable serialization that can serve as the interop bridge.

## Key Considerations for Rust Port

1. **Type fidelity**: The Rust `Typ`, `Exp`, `Sil.instr` types must faithfully represent the OCaml originals
2. **Language-specific extensions**: `Procname` and `Typ.name` have many language-specific variants
3. **Mutability**: `Procdesc` nodes are mutable in OCaml (predecessors/successors modified in place). Rust will need a different approach (arena, indices, etc.)
4. **Deriving**: OCaml uses `ppx_deriving` for compare, equal, hash, sexp, yojson. Rust equivalents: `derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)`
5. **Graph structure**: CFG is a mutable graph -- consider petgraph or custom arena-based graph in Rust
