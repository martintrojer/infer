# Store-Textual Notes

## Capture System

`capture.db` has two relevant tables (see `infer/src/base/Database.ml`):

- `procedures`
  Key columns: `proc_uid`, `proc_attributes`, `cfg`, `callees`
- `source_files`
  Key columns: `source_file`, `type_environment`, `procedure_names`, `freshly_captured`, `textual`

All BLOB columns use OCaml Marshal format. The `textual` column stores the textual SIL payload used
by `--store-textual`.

## Two Capture Paths

1. Direct SIL capture
   Frontends such as Clang build `Cfg.t` / `Tenv.t` in OCaml and store them through
   `SourceFiles.add` + `Cfg.store`.
2. Textual SIL capture
   Textual-based frontends emit Textual, then OCaml parses, verifies, transforms, converts to SIL,
   and stores it through `TextualParser.TextualFile.capture`.

## `--store-textual`

`--store-textual` is a boolean flag in `Config.ml`. When enabled, Infer stores textual SIL in the
`source_files.textual` column instead of writing `.sil` files directly to disk.

Support note:

- Clang stores textual by converting SIL back through `TextualOfSil.to_string`
- Textual-based frontends go through `TextualFile.capture`
- exact frontend coverage depends on the Infer revision that produced the capture
- older snapshots had narrower support matrices
- upstream/master is now landing broader `--store-textual` support for Java, Hack, Python,
  Swift/LLVM, and Rust as well

Do not rely on old "Rust-only" or "frontend X is not wired yet" assumptions without checking the
actual Infer revision in use.

## `--export-textual`

`infer debug --export-textual <dir>` extracts the stored textual payload and writes:

- one exported `.sil` file per source file
- a `manifest.json` mapping source paths to exported `.sil` files and procedure lists

Implementation lives in `InferCommandImplementation.ml`.

## Current infer-rs Usage

The published compliance numbers for `infer-rs` use:

1. `infer --store-textual`
2. `infer debug --export-textual`
3. `infer-rs` on each exported `.sil` from the original source directory

Running from the original source directory matters because it preserves OCaml-style upward
`.inferconfig` lookup during the Rust run.

## Accepted Fidelity Limitation: `sizeof.c`

`sizeof.c` is an accepted `--store-textual` / `--export-textual` fidelity limitation, not an
active Pulse bug.

What happens today:

- OCaml `TextualOfSil` exports `Sizeof {typ}` as textual `Typ`, for example `<int[]>`
- that export drops `nbytes`, `dynamic_length`, and array-extent details
- the exported `sizeof.c` textual contains conditions such as:
  - `__sil_gt(<int[]>, __sil_cast(<int>, 2))`
  - `__sil_divf(<int[]>, <int>)`
- Rust parses `<int[]>` as `Exp::Typ(int[])`
- Rust `to_sil` lowers that back to `Exp::Sizeof { typ = int[]; nbytes = None }`

By the time Pulse runs, the array size information is already gone. As a result, Rust cannot
constant-fold branches such as:

- `sizeof(c) > 2`
- `(sizeof(c) / sizeof(c[0])) != 2`

This is why the authoritative store-textual sweep still reports two extra
`NULLPTR_DEREFERENCE`s in `sizeof.c`.

Important policy:

- do not add Rust-side Pulse workarounds just to force `sizeof.c` back to zero
- treat this as an interface/export limitation until the textual boundary preserves richer
  `Sizeof` information

Relevant code paths:

- OCaml export: `infer/src/textual/TextualOfSil.ml`
- Rust parse: `infer-rs/crates/textual/src/parser.rs`
- Rust Textual → SIL lowering: `infer-rs/crates/textual/src/to_sil.rs`
- Rust Pulse `sizeof` evaluation: `infer-rs/crates/pulse/src/operations.rs`

## Accepted Fidelity Limitation: duplicate C proc identities

OpenSSL exposed a second exported-Textual fidelity limit that is separate from
`sizeof.c`.

What happens today:

- `capture.db` stores procedures by OCaml proc UID, not just by plain textual proc name
- some C procedures that share the same plain name still have distinct stored proc UIDs
- `infer debug --export-textual` writes per-source `.sil` files plus `manifest.json`, but that
  exported surface can drop the hashed proc UID suffix and keep only the plain proc name
- example:
  - OCaml capture keeps `tls1_sha512_final_raw{25e69bf71b156bed23a6f9e772c42969}`
  - exported textual side only preserves `tls1_sha512_final_raw`

What infer-rs does today:

- empty exported `define` stubs are now treated as undefined, so a real body correctly wins over an
  empty `@?` stub during multi-file direct `.sil` merge
- infer-rs does **not** invent synthetic names for real+real collisions after export

Policy:

- do not guess replacement proc identities in Rust just to make merged direct-Textual analysis look
  cleaner
- treat real+real plain-name collisions as an exported-Textual fidelity limit until the textual
  boundary preserves the OCaml proc UID (or equivalent identity metadata)

## Local OCaml Export Fix: cleanup metadata on hot loops

Rust now lowers OCaml-exported `__sil_metadata_*` helper calls back to SIL
metadata instructions during Textual→SIL conversion. This matches
`infer/src/textual/TextualOfSil.ml` `InstrBridge.of_sil_metadata`, and the
Rust side has focused unit tests for the supported metadata families.

The local OCaml exporter in the sibling `infer/` repo now regenerates C/Java
textual from freshly loaded procdescs after preanalysis/WTO setup instead of
blindly dumping the raw `source_files.textual` string. Other integrations still
fall back to the stored payload.

What the current OpenSSL probe shows after that exporter fix:

- a focused OCaml regression in
  `infer/tests/codetoanalyze/c/export-textual/metadata.c` now verifies that
  exported C textual contains
  `__sil_metadata_abstract`, `__sil_metadata_nullify`,
  `__sil_metadata_exit_scope`, and
  `__sil_metadata_variable_lifetime_begins`
- a fresh single-file OpenSSL `wp_block.c` export now carries the same cleanup
  metadata families instead of only
  `__sil_metadata_variable_lifetime_begins`
- that removes the old export-boundary explanation for the hotspot, but it
  does not fix Rust Pulse by itself: the fresh filtered Rust
  `whirlpool_block` run on the regenerated export now finishes in `4m52s`
  with `1222` retained states, `max_node_disjuncts=8`, and top retained nodes
  `29,31,32,33,35,36,37,38`
- Rust Pulse now also executes exported `Metadata::ExitScope` semantically
  instead of treating metadata as a no-op. Cross-ref: OCaml `Pulse.ml`
  `Metadata (ExitScope ...)` and `PulseAbductiveDomain.Stack.remove_vars`.
- with that Rust-side fix in place, a fresh rerun on the same richer export
  was interrupted at `2m05s` with `1022` retained states after earlier
  checkpoints of `668 / max_node_disjuncts=6` at `10.3s`,
  `768 / 6` at `31.0s`, and `922 / 6` at `1m12s`
- that is a real retained-state improvement, but not the whole remaining
  answer: later revisits still climb back to `max_node_disjuncts=8`, so the
  current hotspot is narrower Rust-side retained-state convergence work, not
  an exporter/importer metadata gap

Policy:

- keep the more faithful exporter behavior even if the Rust OpenSSL numbers
  temporarily get worse
- do not add a Rust-side Pulse workaround whose purpose is to compensate for
  the old missing-metadata export bug
- treat the remaining `whirlpool_block` cost as Rust-side analysis work unless
  a new missing textual construct is identified

## Other Notes

- frontend coverage for `--store-textual` is moving upstream; treat old per-language support
  statements as revision-sensitive
- the accepted `sizeof.c` limitation above is independent of that frontend coverage change: it is
  about fidelity of exported textual `Sizeof` expressions, not about whether the frontend writes
  into the `textual` column at all
