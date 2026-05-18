// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Alpha-equivalence helpers for Pulse states.
//!
//! Mirrors the intent of OCaml's `PulseAbductiveDomain.leq`, which compares
//! states modulo abstract-value renaming rather than raw identifier equality.
//!
//! ### Design note: structural canonical state
//!
//! Earlier iterations of this module spelled the canonical state with
//! `Vec<String>` for every section. Profiling `OBJ_bsearch_ex_`
//! (worker-perf-6, post-formula-structural-fix) showed the residual
//! `core::fmt::write` / `format_inner` / `String::write_str` cost was
//! coming from `canonical_heap` / `canonical_attrs` / `canonical_stack` /
//! `canonical_dynamic_types`, plus the byte-wise `String::cmp` driving
//! `smallsort::insert_tail` (15.6% self-time) when sorting those
//! `Vec<String>`s.
//!
//! Every section of `CanonicalState` is therefore now a structural enum
//! tree:
//!   - `CanonFormulaEntry` for the formula (landed in 6e9e4fa56c).
//!   - `CanonStackEntry` for `pre_stack` / `post_stack`.
//!   - `CanonHeapEdge` for `pre_heap` / `post_heap`.
//!   - `CanonAttrEntry` (wrapping `CanonAttribute`) for `pre_attrs` /
//!     `post_attrs`.
//!   - `CanonDynamicType` for `dynamic_types`.
//!
//! Derived `Ord`/`Eq`/`Hash` on these enums replace the per-entry
//! `format!` allocation that the legacy path used purely to drive
//! `sort` / `==`. `AbstractValue`s are mapped through `ValueSortKey`
//! (a small `Copy` enum), so structural keys are alpha-invariant.
//!
//! Timestamps on `Attribute::MustBeValid` / `MustBeInitialized` /
//! `WrittenTo` are stripped at the structural-key boundary because the
//! per-state `next_attr_timestamp` counter is bumped by intervening work
//! between fixpoint iterations and is therefore not alpha-invariant.
//! This matches the long-standing behaviour of the legacy `canonical_attr`
//! formatter (see the cross-ref comment there) and is exercised by
//! `test_alpha_equivalent_states_ignore_attribute_timestamps`.
//!
//! `debug_canonical_dump` renders the structural form back to the legacy
//! sorted `Vec<String>` shape so the [pulse-progress] log line shape is
//! byte-for-byte unchanged. The cross-check tests
//! `structural_canonical_matches_*_string_form` assert that this
//! rendering reproduces the pre-structural `String`-only output exactly
//! on a small fixture, so any future drift between the two
//! representations is caught at test time.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use sil::fieldname::Fieldname;
use sil::location::Location;
use sil::typ::Typ;
use sil::var::Var;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::Attribute;
use crate::base_attrs::BaseAddressAttributes;
use crate::base_memory::BaseMemory;
use crate::base_stack::BaseStack;
use crate::formula::atom::Atom;
use crate::formula::lin_arith::{LinArith, Q};
use crate::formula::phi::{FnAppActual, FnAppKey, TermEq};
use crate::formula::term::Term;
use crate::formula::Operand;
use crate::invalidation::MustBeValidReason;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonValue {
    Unrestricted(u32),
    Restricted(u32),
}

/// Allocation-free sort key for an `AbstractValue` during canonicalization.
///
/// Variant declaration order is the comparison order — chosen so that
/// already-mapped (canonical) values come before still-unmapped values,
/// matching the legacy lexicographic intuition that mapped names like
/// `r0` / `u0` are "smaller" than placeholder names like `?r5` / `?u5`.
/// The exact order is not load-bearing: every consumer is internal to
/// this module and only requires a deterministic total order to drive
/// `sort_by_cached_key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ValueSortKey {
    CanonRestricted(u32),
    CanonUnrestricted(u32),
    UnmappedRestricted(u64),
    UnmappedUnrestricted(u64),
}

/// Allocation-free sort key for `Access` during canonicalization.
///
/// `Field` carries the full `Fieldname` (class + field name) so that the
/// structural form is round-trip-faithful to the legacy `canonical_heap`
/// String shape, which formatted edges via `Display for Fieldname`
/// (`"{class}.{field}"`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum AccessSortKey {
    Dereference,
    Field(Fieldname),
    Array { typ: String, index: ValueSortKey },
}

/// Allocation-free sort key for an outgoing memory edge.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EdgeSortKey {
    access: AccessSortKey,
    target: ValueSortKey,
}

// ---------------------------------------------------------------------------
// Structural canonical entries for the non-formula sections.
//
// `Vec<CanonStackEntry>` etc. replace `Vec<String>`. Derived `Ord`/`Eq`/`Hash`
// drive the hot fixpoint comparisons / hashes without going through `format!`.
// `AbstractValue`s are mapped through `ValueSortKey` so the structural form
// is alpha-invariant.
// ---------------------------------------------------------------------------

/// One stack binding in canonical form: `var -> canonical addr`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonStackEntry {
    /// `Var` already derives `Ord`/`Hash`/`Eq`, no `format!` needed.
    var: Var,
    addr: ValueSortKey,
}

/// One outgoing heap edge in canonical form.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonHeapEdge {
    src: ValueSortKey,
    access: AccessSortKey,
    target: ValueSortKey,
}

/// One attribute on a canonical address.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonAttrEntry {
    addr: ValueSortKey,
    attr: CanonAttribute,
}

/// Structural canonical attribute.
///
/// Variants that carry `AbstractValue`s (`ReturnedFromUnknown`) are mapped
/// through `ValueSortKey` so the result is alpha-invariant.
/// Variants that carry a `Timestamp` (`MustBeValid` / `MustBeInitialized`
/// / `WrittenTo`) drop the timestamp — the per-state
/// `next_attr_timestamp` counter is bumped by intervening work between
/// fixpoint iterations, so two iterations of the same procedure can
/// assign different timestamps to the same logical attribute. Mirrors
/// the long-standing behaviour of the legacy `canonical_attr` formatter
/// and is covered by `test_alpha_equivalent_states_ignore_attribute_timestamps`.
///
/// All other variants carry no `AbstractValue` and no `Timestamp`, so
/// they participate as-is via the `Other` arm; the wrapped `Attribute`
/// already derives `Ord`/`Hash`/`Eq`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(clippy::large_enum_variant)]
enum CanonAttribute {
    ReturnedFromUnknown(Vec<ValueSortKey>),
    MustBeValid(Location, Option<MustBeValidReason>),
    MustBeInitialized(Location),
    WrittenTo(Location),
    Other(Box<Attribute>),
}

/// One dynamic-type binding in canonical form.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonDynamicType {
    addr: ValueSortKey,
    typ: Typ,
}

// ---------------------------------------------------------------------------
// Structural canonical formula entries.
//
// These mirror the formula AST but with `ValueSortKey` in place of
// `AbstractValue`. Derived `Ord`/`Eq`/`Hash` replace the legacy
// `partial_*_label` String allocation. They participate both as sort
// keys (during `propagate_*` / `assign_remaining_*` fixpoints) and as the
// final canonical representation of the formula (`CanonicalState.formula`).
// ---------------------------------------------------------------------------

/// Structural counterpart of `LinArith` with values replaced by `ValueSortKey`s.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonLinArith {
    /// Sorted-by-`ValueSortKey`, then by coefficient. `Vec` (not `BTreeMap`)
    /// so the slice is contiguous and `Ord` derives lexicographic comparison.
    vars: Vec<(ValueSortKey, CanonQ)>,
    constant: CanonQ,
}

/// Comparable wrapper around `Q` (= `Ratio<i64>`).
///
/// `Q` is `Eq`/`Ord`/`Hash` already; this newtype just localises the
/// representation choice in case we want to swap it later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonQ {
    num: i64,
    den: i64,
}

impl CanonQ {
    fn from_q(q: &Q) -> Self {
        Self {
            num: *q.numer(),
            den: *q.denom(),
        }
    }
}

/// Structural counterpart of `Operand` with values replaced by `ValueSortKey`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonOperand {
    /// "const:N" in the legacy String form.
    Const(i64),
    /// "uN"/"rN"/"?uN"/"?rN" in the legacy String form.
    Var(ValueSortKey),
}

/// One node in a flat post-order encoding of a `CanonTerm`.
///
/// The flat encoding lets a `Term` of arbitrary depth be canonicalised
/// with a single heap allocation (one `Box<[CanonTermNode]>`) instead
/// of one `Box<CanonTerm>` per internal node. This is a hot-path win:
/// worker-profile-1 saw `drop_in_place<CanonTerm>` at 13.7% inclusive
/// on `DES_ede3_cfb_encrypt` because every `partial_term_key` call
/// produced an O(depth) tree of boxed nodes only to be dropped after
/// the sort/cmp.
///
/// Variant order is irrelevant for correctness; the canonical formula
/// is sorted at the end of `canonical_formula`, and `alpha_equivalent`
/// compares two equally canonicalised `CanonicalState`s for full
/// equality. See the module-level "Design note".
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonTermNode {
    Var(ValueSortKey),
    Const(i64),
    Neg,
    Not,
    IsZero,
    Add,
    Sub,
    Mult,
}

/// Structural counterpart of `Term`.
///
/// Leaves (`Var` / `Const`) are stored inline so trivial terms never
/// allocate. Composite terms are stored as a single boxed slice of
/// `CanonTermNode`s in post-order: the rebuild stack walks the slice
/// left-to-right, pushing leaves and folding operator nodes off the
/// stack. Two terms with identical post-order sequences are equal
/// (derived `Eq`/`Ord`/`Hash`), and the encoding is canonical (the
/// post-order of a given AST is unique), so structural equality and
/// cmp/hash are byte-identical to the previous boxed-tree form modulo
/// variant ordering.
///
/// Variant declaration order between `Var`/`Const`/`Tree` is
/// irrelevant for correctness; it only changes the in-memory ordering
/// of `CanonicalState.formula` entries before the final
/// `parts.sort()`, which both sides of an `alpha_equivalent` comparison
/// see consistently. The legacy `String` shape (used by
/// `debug_canonical_dump` and `structural_canonical_matches_string_form`)
/// re-sorts on the formatted strings, so the debug surface is
/// byte-identical.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonTerm {
    Var(ValueSortKey),
    Const(i64),
    Tree(Box<[CanonTermNode]>),
}

/// Structural counterpart of `Atom`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonAtom {
    Equal(CanonTerm, CanonTerm),
    NotEqual(CanonTerm, CanonTerm),
    LessEqual(CanonTerm, CanonTerm),
    LessThan(CanonTerm, CanonTerm),
}

/// Structural counterpart of a (lhs, `TermEq`) pair.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonTermEq {
    lhs: ValueSortKey,
    op: sil::binop::Binop,
    op_lhs: CanonOperand,
    op_rhs: CanonOperand,
}

/// Structural counterpart of a (FnAppKey, ret) pair.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CanonFnApp {
    callee: String,
    actuals: Vec<CanonOperand>,
    ret: ValueSortKey,
}

/// Structural counterpart of an interval bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonBound {
    /// Variant order chosen so that finite values fall between the two
    /// infinities, matching their numerical position on the real line.
    MinusInfinity,
    Int(i64),
    PlusInfinity,
}

impl CanonBound {
    fn from_bound(b: &crate::formula::citv::Bound) -> Self {
        match b {
            crate::formula::citv::Bound::MinusInfinity => Self::MinusInfinity,
            crate::formula::citv::Bound::Int(i) => Self::Int(*i),
            crate::formula::citv::Bound::PlusInfinity => Self::PlusInfinity,
        }
    }
}

/// Structural counterpart of `CItv`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonCItv {
    Between(CanonBound, CanonBound),
    Outside(i64, i64),
}

impl CanonCItv {
    fn from_citv(c: &crate::formula::citv::CItv) -> Self {
        match c {
            crate::formula::citv::CItv::Between(lo, hi) => {
                Self::Between(CanonBound::from_bound(lo), CanonBound::from_bound(hi))
            }
            crate::formula::citv::CItv::Outside(lo, hi) => Self::Outside(*lo, *hi),
        }
    }
}

/// One entry in the canonical formula.
///
/// Variant order is irrelevant for correctness — `CanonicalState.formula`
/// is sorted at the end of `canonical_formula`, then compared
/// element-wise with derived `Eq`. We keep the `Vec` sorted so the same
/// formula always serialises to the same `Vec`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonFormulaEntry {
    /// Union-find equality `lhs -> rhs`.
    VarEq(ValueSortKey, ValueSortKey),
    /// Linear equality `lhs = lin`.
    LinearEq(ValueSortKey, CanonLinArith),
    /// Boolean atom (eq/neq/le/lt over terms).
    Atom(CanonAtom),
    /// `lhs = op(op_lhs, op_rhs)` — recorded by `Phi::term_eqs`.
    TermEq(CanonTermEq),
    /// Concrete integer interval for a value.
    Interval(ValueSortKey, CanonCItv),
    /// `is_int` predicate.
    IsInt(ValueSortKey),
    /// Function application equality.
    FnApp(CanonFnApp),
}

impl std::fmt::Display for CanonValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unrestricted(i) => write!(f, "u{i}"),
            Self::Restricted(i) => write!(f, "r{i}"),
        }
    }
}

/// Structural canonical state — every section is a `Vec` of structural
/// entries with derived `Ord`/`Eq`/`Hash`. See the module-level
/// "Design note".
#[derive(Debug, PartialEq, Eq)]
struct CanonicalState {
    pre_stack: Vec<CanonStackEntry>,
    post_stack: Vec<CanonStackEntry>,
    pre_heap: Vec<CanonHeapEdge>,
    post_heap: Vec<CanonHeapEdge>,
    pre_attrs: Vec<CanonAttrEntry>,
    post_attrs: Vec<CanonAttrEntry>,
    formula: Vec<CanonFormulaEntry>,
    /// Cross-ref: OCaml `path_condition.type_constraints` participates in
    /// `PulseAbductiveDomain.leq`. We track dynamic-type bindings
    /// separately on `AbductiveDomain.dynamic_types`, but they affect
    /// downstream analysis (specialization, function-pointer
    /// resolution), so they must participate in `alpha_equivalent` for
    /// the fixpoint to converge.
    dynamic_types: Vec<CanonDynamicType>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DebugSignature {
    hash: u64,
    pre_stack: usize,
    post_stack: usize,
    pre_heap: usize,
    post_heap: usize,
    pre_attrs: usize,
    post_attrs: usize,
    formula: usize,
    dynamic_types: usize,
}

impl std::fmt::Display for DebugSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hash={:016x} pre[s={} h={} a={}] post[s={} h={} a={}] formula={} dyn={}",
            self.hash,
            self.pre_stack,
            self.pre_heap,
            self.pre_attrs,
            self.post_stack,
            self.post_heap,
            self.post_attrs,
            self.formula,
            self.dynamic_types,
        )
    }
}

pub(crate) fn debug_signature(state: &AbductiveDomain) -> DebugSignature {
    let canonical = canonicalize(state);
    let hash = stable_hash_state(&canonical.state);
    DebugSignature {
        hash,
        pre_stack: canonical.state.pre_stack.len(),
        post_stack: canonical.state.post_stack.len(),
        pre_heap: canonical.state.pre_heap.len(),
        post_heap: canonical.state.post_heap.len(),
        pre_attrs: canonical.state.pre_attrs.len(),
        post_attrs: canonical.state.post_attrs.len(),
        formula: canonical.state.formula.len(),
        dynamic_types: canonical.state.dynamic_types.len(),
    }
}

fn append_debug_section(out: &mut String, name: &str, lines: Vec<String>) {
    out.push_str(name);
    out.push_str(":\n");
    if lines.is_empty() {
        out.push_str("  <empty>\n");
    } else {
        for line in lines {
            out.push_str("  ");
            out.push_str(&line);
            out.push('\n');
        }
    }
}

pub(crate) fn debug_canonical_dump(state: &AbductiveDomain) -> String {
    let CanonicalState {
        pre_stack,
        post_stack,
        pre_heap,
        post_heap,
        pre_attrs,
        post_attrs,
        formula,
        dynamic_types,
    } = canonicalize(state).state;

    // Render the structural sections back into the legacy sorted
    // `Vec<String>` shape so the [pulse-progress] / debug dump shape is
    // byte-identical to the pre-structural-key implementation. This is a
    // debug surface; the formatting cost is irrelevant outside of
    // `--debug` runs.
    let mut out = String::new();
    append_debug_section(&mut out, "pre_stack", format_canon_stack_legacy(&pre_stack));
    append_debug_section(
        &mut out,
        "post_stack",
        format_canon_stack_legacy(&post_stack),
    );
    append_debug_section(&mut out, "pre_heap", format_canon_heap_legacy(&pre_heap));
    append_debug_section(&mut out, "post_heap", format_canon_heap_legacy(&post_heap));
    append_debug_section(&mut out, "pre_attrs", format_canon_attrs_legacy(&pre_attrs));
    append_debug_section(
        &mut out,
        "post_attrs",
        format_canon_attrs_legacy(&post_attrs),
    );
    append_debug_section(&mut out, "formula", format_canon_formula_legacy(&formula));
    append_debug_section(
        &mut out,
        "dynamic_types",
        format_canon_dynamic_types_legacy(&dynamic_types),
    );
    out.pop();
    out
}

/// Compare two states modulo abstract-value renaming.
///
/// Equivalent to [`eq_canonical`] composed with [`canonicalize_state`] on
/// both sides; see [`canonicalize_state`] for the cross-product
/// optimisation used by `DisjunctiveDomain::leq` on `ExecutionDomain`.
pub fn alpha_equivalent(lhs: &AbductiveDomain, rhs: &AbductiveDomain) -> bool {
    eq_canonical(&canonicalize_state(lhs), &canonicalize_state(rhs))
}

/// Compare two state values modulo the same alpha-renaming used by
/// [`alpha_equivalent`].
///
/// This is stricter than raw [`AbstractValue`] equality: the states must be
/// semantically equivalent, and the designated values must land on the same
/// canonical value within that equivalence.
///
/// Equivalent to [`eq_canonical_with_value`] composed with
/// [`canonicalize_state`] on both sides.
pub fn alpha_equivalent_value(
    lhs: &AbductiveDomain,
    lhs_value: AbstractValue,
    rhs: &AbductiveDomain,
    rhs_value: AbstractValue,
) -> bool {
    let lhs = canonicalize_state(lhs);
    let rhs = canonicalize_state(rhs);
    eq_canonical_with_value(&lhs, lhs_value, &rhs, rhs_value)
}

/// Pre-canonicalise an [`AbductiveDomain`] for repeated alpha-equivalence
/// comparisons.
///
/// Use this when the same state participates in many leq comparisons
/// (e.g. `DisjunctiveDomain::leq`'s N×M cross-product over
/// `ExecutionDomain`s). Pre-canonicalising each side once and then
/// driving the inner loop with [`eq_canonical`] / [`eq_canonical_with_value`]
/// turns 2·N·M canonicalize calls into N+M, with byte-identical
/// equality semantics:
/// `alpha_equivalent(a, b) == eq_canonical(&canonicalize_state(a), &canonicalize_state(b))`
/// and analogously for `alpha_equivalent_value`. The
/// `alpha_equivalent_matches_canonicalize_then_eq_canonical_*` tests
/// pin this invariant.
pub fn canonicalize_state(state: &AbductiveDomain) -> CanonicalAbductive {
    CanonicalAbductive {
        inner: canonicalize(state),
    }
}

/// Pre-canonicalised state suitable for repeated [`eq_canonical`] /
/// [`eq_canonical_with_value`] calls. Opaque on purpose; the only
/// supported operations are the two `eq_canonical*` helpers below.
pub struct CanonicalAbductive {
    inner: CanonicalizedState,
}

/// Compare two pre-canonicalised states for alpha-equivalence.
///
/// `eq_canonical(&canonicalize_state(a), &canonicalize_state(b))` is
/// byte-identical to `alpha_equivalent(a, b)`.
pub fn eq_canonical(lhs: &CanonicalAbductive, rhs: &CanonicalAbductive) -> bool {
    lhs.inner.state == rhs.inner.state
}

/// Compare two pre-canonicalised states *and* designated values for
/// alpha-equivalence.
///
/// `eq_canonical_with_value(&canonicalize_state(a), av, &canonicalize_state(b), bv)`
/// is byte-identical to `alpha_equivalent_value(a, av, b, bv)`.
pub fn eq_canonical_with_value(
    lhs: &CanonicalAbductive,
    lhs_value: AbstractValue,
    rhs: &CanonicalAbductive,
    rhs_value: AbstractValue,
) -> bool {
    lhs.inner.state == rhs.inner.state
        && matches!(
            (
                lhs.inner.value_label(lhs_value),
                rhs.inner.value_label(rhs_value),
            ),
            (Some(lhs_label), Some(rhs_label)) if lhs_label == rhs_label
        )
}

fn canonicalize(state: &AbductiveDomain) -> CanonicalizedState {
    // Cross-ref: OCaml `PulseAbductiveDomain.leq` compares the full formula
    // plus the stack-reachable pre/post graph. It does not compare Rust-only
    // helper caches such as `must_be_valid`, and it ignores disconnected
    // retained heap/attr garbage.
    let pre_reachable = reachable_from_stack(&state.pre.stack, &state.pre.heap);
    let mut post_reachable = reachable_from_stack(&state.post.stack, &state.post.heap);
    post_reachable.extend(written_stack_roots(&state.post.stack, &state.post.attrs));
    let mut canon = Canonicalizer::default();
    canon.seed_from_stack(&state.pre.stack);
    canon.seed_from_stack(&state.post.stack);

    loop {
        let before = canon.len();
        canon.propagate_memory(&state.pre.heap);
        canon.propagate_memory(&state.post.heap);
        canon.propagate_attrs(&state.pre.attrs);
        canon.propagate_attrs(&state.post.attrs);
        canon.propagate_formula(state);
        if canon.len() == before {
            break;
        }
    }

    canon.assign_remaining(state, &pre_reachable, &post_reachable);

    CanonicalizedState {
        state: CanonicalState {
            pre_stack: canonical_stack(&state.pre.stack, &canon),
            post_stack: canonical_stack(&state.post.stack, &canon),
            pre_heap: canonical_heap(&state.pre.heap, &pre_reachable, &canon),
            post_heap: canonical_heap(&state.post.heap, &post_reachable, &canon),
            pre_attrs: canonical_attrs(&state.pre.attrs, &pre_reachable, &canon),
            post_attrs: canonical_attrs(&state.post.attrs, &post_reachable, &canon),
            formula: canonical_formula(state, &canon),
            dynamic_types: canonical_dynamic_types(state, &canon),
        },
        canon,
    }
}

/// Stable per-state hash used by `debug_signature`.
///
/// Every section is now structural; we feed each entry through its
/// derived `Hash` impl. Section boundaries are marked with a `0xff`
/// sentinel so two states that differ only in where one section ends
/// and the next begins do not alias.
fn stable_hash_state(state: &CanonicalState) -> u64 {
    let mut h = FnvHasher::new();
    hash_section(&mut h, &state.pre_stack);
    hash_section(&mut h, &state.post_stack);
    hash_section(&mut h, &state.pre_heap);
    hash_section(&mut h, &state.post_heap);
    hash_section(&mut h, &state.pre_attrs);
    hash_section(&mut h, &state.post_attrs);
    hash_section(&mut h, &state.formula);
    hash_section(&mut h, &state.dynamic_types);
    h.finish()
}

fn hash_section<T: Hash>(h: &mut FnvHasher, entries: &[T]) {
    h.write_u64(entries.len() as u64);
    for entry in entries {
        entry.hash(h);
        h.write_u8(0xff);
    }
}

/// FNV-1a hasher with a stable fixed seed so canonical hashes are
/// reproducible across runs (unlike `std::collections::hash_map::DefaultHasher`
/// which uses a randomised seed).
struct FnvHasher {
    state: u64,
}

impl FnvHasher {
    fn new() -> Self {
        Self {
            state: 0xcbf29ce484222325u64,
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }

    fn write_u8(&mut self, b: u8) {
        self.write_bytes(&[b]);
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.state
    }

    fn write(&mut self, bytes: &[u8]) {
        self.write_bytes(bytes);
    }
}

fn keyed_sorted_vec<T, K: Ord>(
    items: impl Iterator<Item = T>,
    mut key: impl FnMut(&T) -> K,
) -> Vec<(K, T)> {
    // `sort_by_cached_key` also computes each key once, but it allocates a
    // second side vector for the cached keys.  The canonicalizer pass already
    // needs a temporary ordered list, so store the key next to the borrowed
    // item and sort that one vector instead.  `sort_by` is stable, preserving
    // the previous tie behaviour for equal partial keys.
    let mut entries: Vec<_> = items.map(|item| (key(&item), item)).collect();
    entries.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
    entries
}

fn written_stack_roots(
    stack: &crate::base_stack::BaseStack,
    attrs: &crate::base_attrs::BaseAddressAttributes,
) -> std::collections::HashSet<AbstractValue> {
    let mut roots = std::collections::HashSet::new();
    for (_var, addr) in stack.iter() {
        if attrs
            .get(addr)
            .is_some_and(|attrs| attrs.get_written_to().is_some())
        {
            roots.insert(*addr);
        }
    }
    roots
}

fn canonical_dynamic_types(
    state: &AbductiveDomain,
    canon: &Canonicalizer,
) -> Vec<CanonDynamicType> {
    let mut entries: Vec<CanonDynamicType> = state
        .iter_dynamic_types()
        .filter_map(|(addr, typ)| {
            canon.get(addr).map(|_| CanonDynamicType {
                addr: canon.partial_value_key(addr),
                typ: typ.clone(),
            })
        })
        .collect();
    entries.sort();
    entries
}

struct CanonicalizedState {
    state: CanonicalState,
    canon: Canonicalizer,
}

impl CanonicalizedState {
    fn value_label(&self, value: AbstractValue) -> Option<CanonValue> {
        self.canon.get(value)
    }
}

#[derive(Default)]
struct Canonicalizer {
    values: BTreeMap<AbstractValue, CanonValue>,
    restricted_values: Vec<AbstractValue>,
    unrestricted_values: Vec<AbstractValue>,
    next_unrestricted: u32,
    next_restricted: u32,
}

impl Canonicalizer {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn get(&self, value: AbstractValue) -> Option<CanonValue> {
        self.values.get(&value).copied()
    }

    fn map_value(&mut self, value: AbstractValue) -> CanonValue {
        if let Some(existing) = self.values.get(&value).copied() {
            return existing;
        }

        let canon = if value.is_restricted() {
            self.next_restricted += 1;
            self.restricted_values.push(value);
            CanonValue::Restricted(self.next_restricted)
        } else {
            self.next_unrestricted += 1;
            self.unrestricted_values.push(value);
            CanonValue::Unrestricted(self.next_unrestricted)
        };
        self.values.insert(value, canon);
        canon
    }

    fn map_returned_from_unknown_values(&mut self, attr: &Attribute) {
        if let Attribute::ReturnedFromUnknown(values) = attr {
            for value in values {
                self.map_value(*value);
            }
        }
    }

    fn map_edge_values(&mut self, access: &Access, target: AbstractValue) {
        if let Access::ArrayAccess(_, index) = access {
            self.map_value(*index);
        }
        self.map_value(target);
    }

    fn map_term_eq_values(&mut self, lhs: AbstractValue, term_eq: &TermEq) {
        self.map_value(lhs);
        for value in operand_values(&term_eq.lhs) {
            self.map_value(value);
        }
        for value in operand_values(&term_eq.rhs) {
            self.map_value(value);
        }
    }

    fn seed_from_stack(&mut self, stack: &BaseStack) {
        self.map_stack_addrs(stack);
    }

    fn map_stack_addrs(&mut self, stack: &BaseStack) {
        for (_, (_, addr)) in keyed_sorted_vec(stack.iter(), |(var, _)| format!("{var}")) {
            self.map_value(*addr);
        }
    }

    fn propagate_memory(&mut self, memory: &BaseMemory) {
        // Cross-ref: OCaml `GraphComparison.isograph_map_from_stack` grows only
        // from already-mapped graph roots; avoid sorting unmapped heap cells.
        self.propagate_mapped_values(|this, src| this.propagate_memory_source(memory, src));
    }

    fn propagate_mapped_values(&mut self, mut f: impl FnMut(&mut Self, AbstractValue)) {
        let mut i = 0;
        while i < self.restricted_values.len() {
            let value = self.restricted_values[i];
            f(self, value);
            i += 1;
        }
        let mut i = 0;
        while i < self.unrestricted_values.len() {
            let value = self.unrestricted_values[i];
            f(self, value);
            i += 1;
        }
    }

    fn propagate_memory_source(&mut self, memory: &BaseMemory, src: AbstractValue) {
        let Some(edges) = memory.get_edges(src) else {
            return;
        };
        self.map_sorted_edges(edges.iter());
    }

    fn propagate_attrs(&mut self, attrs: &BaseAddressAttributes) {
        self.propagate_mapped_values(|this, addr| this.propagate_attrs_addr(attrs, addr));
    }

    fn propagate_attrs_addr(&mut self, attrs: &BaseAddressAttributes, addr: AbstractValue) {
        let Some(attrs) = attrs.get(&addr) else {
            return;
        };
        for attr in attrs.iter() {
            self.map_returned_from_unknown_values(attr);
        }
    }

    fn propagate_formula(&mut self, state: &AbductiveDomain) {
        let phi = state.path_condition.phi();

        for (_, (lhs, rhs)) in keyed_sorted_vec(phi.var_eqs.iter_equalities(), |(lhs, rhs)| {
            (self.partial_value_key(*lhs), self.partial_value_key(*rhs))
        }) {
            if self.get(lhs).is_some() || self.get(rhs).is_some() {
                self.map_value(lhs);
                self.map_value(rhs);
            }
        }

        for (_, (lhs, lin)) in keyed_sorted_vec(phi.linear_eqs.iter(), |(lhs, _)| {
            self.partial_value_key(**lhs)
        }) {
            if self.get(*lhs).is_some()
                || lin.get_variables().any(|value| self.get(value).is_some())
            {
                self.map_value(*lhs);
                for value in lin.get_variables() {
                    self.map_value(value);
                }
            }
        }

        // Cross-ref: `partial_atom_key` is structural (no `String`
        // allocation), unlike the legacy `partial_atom_label`.
        for (_, atom) in keyed_sorted_vec(phi.atoms.iter(), |atom| self.partial_atom_key(atom)) {
            let vars = atom.all_vars();
            if vars.iter().any(|value| self.get(*value).is_some()) {
                for value in vars {
                    self.map_value(value);
                }
            }
        }

        for (_, (lhs, term_eq)) in keyed_sorted_vec(phi.term_eqs.iter(), |(lhs, term_eq)| {
            self.partial_term_eq_key(**lhs, term_eq)
        }) {
            let lhs_mapped = self.get(*lhs).is_some();
            let lhs_values = operand_values(&term_eq.lhs);
            let rhs_values = operand_values(&term_eq.rhs);
            if lhs_mapped
                || lhs_values.iter().any(|value| self.get(*value).is_some())
                || rhs_values.iter().any(|value| self.get(*value).is_some())
            {
                self.map_value(*lhs);
                for value in lhs_values {
                    self.map_value(value);
                }
                for value in rhs_values {
                    self.map_value(value);
                }
            }
        }

        for (_, (value, _)) in keyed_sorted_vec(phi.intervals.iter(), |(value, _)| {
            self.partial_value_key(**value)
        }) {
            if self.get(*value).is_some() {
                continue;
            }
            if phi
                .linear_eqs
                .get(value)
                .is_some_and(|lin| lin.get_variables().any(|var| self.get(var).is_some()))
            {
                self.map_value(*value);
            }
        }

        for (_, value) in keyed_sorted_vec(phi.is_int_vars.iter().copied(), |value| {
            self.partial_value_key(*value)
        }) {
            if self.get(value).is_some() {
                continue;
            }
            if phi
                .linear_eqs
                .get(&value)
                .is_some_and(|lin| lin.get_variables().any(|var| self.get(var).is_some()))
            {
                self.map_value(value);
            }
        }

        for (_, (key, ret)) in keyed_sorted_vec(phi.iter_fn_app_eqs(), |(_, ret)| {
            self.partial_value_key(**ret)
        }) {
            let ret_mapped = self.get(*ret).is_some();
            if ret_mapped
                || key.actuals.iter().any(|actual| match actual {
                    FnAppActual::Const(_) => false,
                    FnAppActual::Var(value) => self.get(*value).is_some(),
                })
            {
                for actual in &key.actuals {
                    if let FnAppActual::Var(value) = actual {
                        self.map_value(*value);
                    }
                }
                self.map_value(*ret);
            }
        }
    }

    fn assign_remaining(
        &mut self,
        state: &AbductiveDomain,
        pre_reachable: &std::collections::HashSet<AbstractValue>,
        post_reachable: &std::collections::HashSet<AbstractValue>,
    ) {
        self.assign_remaining_stack(&state.pre.stack);
        self.assign_remaining_stack(&state.post.stack);
        self.assign_remaining_memory(&state.pre.heap, pre_reachable);
        self.assign_remaining_memory(&state.post.heap, post_reachable);
        self.assign_remaining_attrs(&state.pre.attrs, pre_reachable);
        self.assign_remaining_attrs(&state.post.attrs, post_reachable);
        self.assign_remaining_formula(state);
    }

    fn assign_remaining_stack(&mut self, stack: &BaseStack) {
        self.map_stack_addrs(stack);
    }

    fn map_sorted_edges<'a>(
        &mut self,
        edges: impl Iterator<Item = (&'a Access, &'a AbstractValue)>,
    ) {
        for (_, (access, target)) in keyed_sorted_vec(edges, |(access, target)| {
            self.partial_edge_key(access, **target)
        }) {
            self.map_edge_values(access, *target);
        }
    }

    fn assign_remaining_memory(
        &mut self,
        memory: &BaseMemory,
        reachable: &std::collections::HashSet<AbstractValue>,
    ) {
        for (_, (src, edges)) in
            keyed_sorted_vec(memory.iter(), |(src, _)| self.partial_value_key(**src))
        {
            if !reachable.contains(src) {
                continue;
            }
            self.map_value(*src);
            self.map_sorted_edges(edges.iter());
        }
    }

    fn assign_remaining_attrs(
        &mut self,
        attrs: &BaseAddressAttributes,
        reachable: &std::collections::HashSet<AbstractValue>,
    ) {
        for (_, (addr, attrs)) in
            keyed_sorted_vec(attrs.iter(), |(addr, _)| self.partial_value_key(**addr))
        {
            if !reachable.contains(addr) {
                continue;
            }
            self.map_value(*addr);
            for attr in attrs.iter() {
                self.map_returned_from_unknown_values(attr);
            }
        }
    }

    fn assign_remaining_formula(&mut self, state: &AbductiveDomain) {
        let phi = state.path_condition.phi();

        for (_, (lhs, rhs)) in keyed_sorted_vec(phi.var_eqs.iter_equalities(), |(lhs, rhs)| {
            (self.partial_value_key(*lhs), self.partial_value_key(*rhs))
        }) {
            self.map_value(lhs);
            self.map_value(rhs);
        }

        for (_, (lhs, lin)) in keyed_sorted_vec(phi.linear_eqs.iter(), |(lhs, lin)| {
            self.partial_linear_eq_key(**lhs, lin)
        }) {
            self.map_value(*lhs);
            for value in lin.get_variables() {
                self.map_value(value);
            }
        }

        for (_, atom) in keyed_sorted_vec(phi.atoms.iter(), |atom| self.partial_atom_key(atom)) {
            for value in atom.all_vars() {
                self.map_value(value);
            }
        }

        for (_, (lhs, term_eq)) in keyed_sorted_vec(phi.term_eqs.iter(), |(lhs, term_eq)| {
            self.partial_term_eq_key(**lhs, term_eq)
        }) {
            self.map_term_eq_values(*lhs, term_eq);
        }

        for (_, (value, _)) in keyed_sorted_vec(phi.intervals.iter(), |(value, interval)| {
            (
                self.partial_value_key(**value),
                CanonCItv::from_citv(interval),
            )
        }) {
            self.map_value(*value);
        }

        for (_, value) in keyed_sorted_vec(phi.is_int_vars.iter().copied(), |value| {
            self.partial_value_key(*value)
        }) {
            self.map_value(value);
        }

        for (_, (key, ret)) in keyed_sorted_vec(phi.iter_fn_app_eqs(), |(key, ret)| {
            self.partial_fn_app_key(key, **ret)
        }) {
            for actual in &key.actuals {
                if let FnAppActual::Var(value) = actual {
                    self.map_value(*value);
                }
            }
            self.map_value(*ret);
        }
    }

    /// Cheap allocation-free sort key for an `AbstractValue`.
    fn partial_value_key(&self, value: AbstractValue) -> ValueSortKey {
        match self.get(value) {
            Some(CanonValue::Restricted(i)) => ValueSortKey::CanonRestricted(i),
            Some(CanonValue::Unrestricted(i)) => ValueSortKey::CanonUnrestricted(i),
            None => {
                let id = value.raw().unsigned_abs();
                if value.is_restricted() {
                    ValueSortKey::UnmappedRestricted(id)
                } else {
                    ValueSortKey::UnmappedUnrestricted(id)
                }
            }
        }
    }

    fn partial_edge_key(&self, access: &Access, target: AbstractValue) -> EdgeSortKey {
        EdgeSortKey {
            access: self.partial_access_key(access),
            target: self.partial_value_key(target),
        }
    }

    fn partial_access_key(&self, access: &Access) -> AccessSortKey {
        match access {
            Access::Dereference => AccessSortKey::Dereference,
            Access::FieldAccess(field) => AccessSortKey::Field(field.clone()),
            Access::ArrayAccess(typ, index) => AccessSortKey::Array {
                typ: format!("{typ}"),
                index: self.partial_value_key(*index),
            },
        }
    }

    fn partial_operand_key(&self, operand: &Operand) -> CanonOperand {
        match operand {
            Operand::AbstractValue(value) => CanonOperand::Var(self.partial_value_key(*value)),
            Operand::ConstOperand(value) => CanonOperand::Const(*value),
        }
    }

    fn partial_term_key(&self, term: &Term) -> CanonTerm {
        // Trivial terms stay inline (zero allocations). Composite terms
        // are flattened into a single boxed post-order slab — one
        // allocation regardless of depth, vs O(depth) `Box<CanonTerm>`
        // allocations under the previous representation.
        match term {
            Term::Var(value) => CanonTerm::Var(self.partial_value_key(*value)),
            Term::Const(value) => CanonTerm::Const(*value),
            _ => {
                let mut nodes: Vec<CanonTermNode> = Vec::new();
                self.flatten_term(term, &mut nodes);
                CanonTerm::Tree(nodes.into_boxed_slice())
            }
        }
    }

    /// Post-order DFS over `Term`, pushing `CanonTermNode`s into `out`.
    ///
    /// Children are emitted before their parent, so a stack-based
    /// rebuild (see `format_canon_term`) walks the slice left-to-right.
    /// Two equal `Term`s produce identical sequences, so the derived
    /// `Eq`/`Ord`/`Hash` on `Box<[CanonTermNode]>` agree with the
    /// previous structural definition modulo variant ordering of
    /// `CanonTerm` itself (which is not load-bearing — see the
    /// `CanonTerm` doc comment).
    fn flatten_term(&self, term: &Term, out: &mut Vec<CanonTermNode>) {
        match term {
            Term::Var(value) => out.push(CanonTermNode::Var(self.partial_value_key(*value))),
            Term::Const(value) => out.push(CanonTermNode::Const(*value)),
            Term::Add(lhs, rhs) => {
                self.flatten_term(lhs, out);
                self.flatten_term(rhs, out);
                out.push(CanonTermNode::Add);
            }
            Term::Sub(lhs, rhs) => {
                self.flatten_term(lhs, out);
                self.flatten_term(rhs, out);
                out.push(CanonTermNode::Sub);
            }
            Term::Mult(lhs, rhs) => {
                self.flatten_term(lhs, out);
                self.flatten_term(rhs, out);
                out.push(CanonTermNode::Mult);
            }
            Term::Neg(inner) => {
                self.flatten_term(inner, out);
                out.push(CanonTermNode::Neg);
            }
            Term::Not(inner) => {
                self.flatten_term(inner, out);
                out.push(CanonTermNode::Not);
            }
            Term::IsZero(inner) => {
                self.flatten_term(inner, out);
                out.push(CanonTermNode::IsZero);
            }
        }
    }

    fn partial_atom_key(&self, atom: &Atom) -> CanonAtom {
        match atom {
            Atom::Equal(lhs, rhs) => {
                CanonAtom::Equal(self.partial_term_key(lhs), self.partial_term_key(rhs))
            }
            Atom::NotEqual(lhs, rhs) => {
                CanonAtom::NotEqual(self.partial_term_key(lhs), self.partial_term_key(rhs))
            }
            Atom::LessEqual(lhs, rhs) => {
                CanonAtom::LessEqual(self.partial_term_key(lhs), self.partial_term_key(rhs))
            }
            Atom::LessThan(lhs, rhs) => {
                CanonAtom::LessThan(self.partial_term_key(lhs), self.partial_term_key(rhs))
            }
        }
    }

    fn partial_lin_arith_key(&self, lin: &LinArith) -> CanonLinArith {
        let mut vars: Vec<(ValueSortKey, CanonQ)> = lin
            .vars
            .iter()
            .map(|(value, coeff)| (self.partial_value_key(*value), CanonQ::from_q(coeff)))
            .collect();
        vars.sort();
        CanonLinArith {
            vars,
            constant: CanonQ::from_q(&lin.constant),
        }
    }

    fn partial_linear_eq_key(
        &self,
        lhs: AbstractValue,
        lin: &LinArith,
    ) -> (ValueSortKey, CanonLinArith) {
        (self.partial_value_key(lhs), self.partial_lin_arith_key(lin))
    }

    fn partial_term_eq_key(&self, lhs: AbstractValue, term_eq: &TermEq) -> CanonTermEq {
        CanonTermEq {
            lhs: self.partial_value_key(lhs),
            op: term_eq.op.clone(),
            op_lhs: self.partial_operand_key(&term_eq.lhs),
            op_rhs: self.partial_operand_key(&term_eq.rhs),
        }
    }

    fn partial_fn_app_key(&self, key: &FnAppKey, ret: AbstractValue) -> CanonFnApp {
        CanonFnApp {
            callee: key.callee.clone(),
            actuals: key
                .actuals
                .iter()
                .map(|actual| match actual {
                    FnAppActual::Const(v) => CanonOperand::Const(*v),
                    FnAppActual::Var(v) => CanonOperand::Var(self.partial_value_key(*v)),
                })
                .collect(),
            ret: self.partial_value_key(ret),
        }
    }

    /// Map an `Attribute` to its structural canonical form.
    ///
    /// `AbstractValue`s in payloads are mapped through `ValueSortKey` so
    /// the result is alpha-invariant. `Timestamp`s on `MustBeValid` /
    /// `MustBeInitialized` / `WrittenTo` are dropped — see the
    /// `CanonAttribute` doc comment.
    fn partial_attribute_key(&self, attr: &Attribute) -> CanonAttribute {
        match attr {
            Attribute::ReturnedFromUnknown(values) => CanonAttribute::ReturnedFromUnknown(
                values.iter().map(|v| self.partial_value_key(*v)).collect(),
            ),
            Attribute::MustBeValid(_ts, loc, reason) => {
                CanonAttribute::MustBeValid(loc.clone(), reason.clone())
            }
            Attribute::MustBeInitialized(_ts, loc) => {
                CanonAttribute::MustBeInitialized(loc.clone())
            }
            Attribute::WrittenTo(_ts, loc) => CanonAttribute::WrittenTo(loc.clone()),
            other => CanonAttribute::Other(Box::new(other.clone())),
        }
    }
}

fn canonical_stack(stack: &BaseStack, canon: &Canonicalizer) -> Vec<CanonStackEntry> {
    let mut entries: Vec<CanonStackEntry> = stack
        .iter()
        .map(|(var, addr)| CanonStackEntry {
            var: var.clone(),
            addr: canon.partial_value_key(*addr),
        })
        .collect();
    entries.sort();
    entries
}

fn canonical_heap(
    memory: &BaseMemory,
    reachable: &std::collections::HashSet<AbstractValue>,
    canon: &Canonicalizer,
) -> Vec<CanonHeapEdge> {
    let mut edges: Vec<CanonHeapEdge> = Vec::new();
    for (src, accesses) in memory.iter() {
        if !reachable.contains(src) {
            continue;
        }
        let src_key = canon.partial_value_key(*src);
        for (access, target) in accesses.iter() {
            edges.push(CanonHeapEdge {
                src: src_key,
                access: canon.partial_access_key(access),
                target: canon.partial_value_key(*target),
            });
        }
    }
    edges.sort();
    edges
}

fn canonical_attrs(
    attrs: &BaseAddressAttributes,
    reachable: &std::collections::HashSet<AbstractValue>,
    canon: &Canonicalizer,
) -> Vec<CanonAttrEntry> {
    let mut entries: Vec<CanonAttrEntry> = Vec::new();
    for (addr, attrs) in attrs.iter() {
        if !reachable.contains(addr) {
            continue;
        }
        let addr_key = canon.partial_value_key(*addr);
        for attr in attrs.iter() {
            entries.push(CanonAttrEntry {
                addr: addr_key,
                attr: canon.partial_attribute_key(attr),
            });
        }
    }
    entries.sort();
    entries
}

/// Build the structural canonical formula. Replaces the legacy
/// `Vec<String>` formula with `Vec<CanonFormulaEntry>`. See the
/// module-level "Design note".
fn canonical_formula(state: &AbductiveDomain, canon: &Canonicalizer) -> Vec<CanonFormulaEntry> {
    let mut parts: Vec<CanonFormulaEntry> = Vec::new();
    let phi = state.path_condition.phi();

    for (lhs, rhs) in phi.var_eqs.iter_equalities() {
        parts.push(CanonFormulaEntry::VarEq(
            canon.partial_value_key(lhs),
            canon.partial_value_key(rhs),
        ));
    }

    for (lhs, lin) in phi.linear_eqs.iter() {
        parts.push(CanonFormulaEntry::LinearEq(
            canon.partial_value_key(*lhs),
            canon.partial_lin_arith_key(lin),
        ));
    }

    for atom in phi.atoms.iter() {
        parts.push(CanonFormulaEntry::Atom(canon.partial_atom_key(atom)));
    }

    for (lhs, term_eq) in phi.term_eqs.iter() {
        parts.push(CanonFormulaEntry::TermEq(
            canon.partial_term_eq_key(*lhs, term_eq),
        ));
    }

    for (lhs, interval) in phi.intervals.iter() {
        parts.push(CanonFormulaEntry::Interval(
            canon.partial_value_key(*lhs),
            CanonCItv::from_citv(interval),
        ));
    }

    for value in phi.is_int_vars.iter() {
        parts.push(CanonFormulaEntry::IsInt(canon.partial_value_key(*value)));
    }

    for (key, ret) in phi.iter_fn_app_eqs() {
        parts.push(CanonFormulaEntry::FnApp(
            canon.partial_fn_app_key(key, *ret),
        ));
    }

    parts.sort();
    parts
}

fn reachable_from_stack(
    stack: &BaseStack,
    heap: &BaseMemory,
) -> std::collections::HashSet<AbstractValue> {
    // Cross-ref: OCaml `PulseAbductiveDomain.GraphComparison.isograph_map_from_stack`.
    // The OCaml `leq` relation compares only stack-reachable heap/attr state
    // and ignores disconnected retained garbage at fixpoint nodes.
    let mut reachable = std::collections::HashSet::new();
    let mut worklist: Vec<_> = stack.iter().map(|(_var, addr)| *addr).collect();
    while let Some(addr) = worklist.pop() {
        if !reachable.insert(addr) {
            continue;
        }
        if let Some(edges) = heap.get_edges(addr) {
            for (_access, target) in edges.iter() {
                worklist.push(*target);
            }
        }
    }
    reachable
}

fn operand_values(operand: &Operand) -> Vec<AbstractValue> {
    match operand {
        Operand::AbstractValue(value) => vec![*value],
        Operand::ConstOperand(_) => Vec::new(),
    }
}

fn format_q(q: &CanonQ) -> String {
    if q.den == 1 {
        q.num.to_string()
    } else {
        format!("{}/{}", q.num, q.den)
    }
}

// ---------------------------------------------------------------------------
// Legacy String formatters for the structural canonical formula.
//
// These reproduce the byte-for-byte shape of the pre-structural
// `partial_*_label` / `canonical_*` `String` form. They are used by:
//   - `debug_canonical_dump` (a debug surface that prints the canonical
//     state — the [pulse-progress] log shape relies on this remaining
//     stable so it can be diffed across builds).
//   - the `structural_canonical_matches_string_form` cross-check test.
//
// The hot fixpoint path (`alpha_equivalent`, `debug_signature.hash`)
// does NOT touch these formatters.
// ---------------------------------------------------------------------------

fn format_value_key(key: &ValueSortKey) -> String {
    match key {
        ValueSortKey::CanonRestricted(i) => format!("r{i}"),
        ValueSortKey::CanonUnrestricted(i) => format!("u{i}"),
        ValueSortKey::UnmappedRestricted(i) => format!("?r{i}"),
        ValueSortKey::UnmappedUnrestricted(i) => format!("?u{i}"),
    }
}

fn format_canon_operand(op: &CanonOperand) -> String {
    match op {
        CanonOperand::Const(v) => format!("const:{v}"),
        CanonOperand::Var(v) => format_value_key(v),
    }
}

fn format_canon_term(term: &CanonTerm) -> String {
    match term {
        CanonTerm::Var(v) => format_value_key(v),
        CanonTerm::Const(v) => format!("const:{v}"),
        CanonTerm::Tree(nodes) => format_canon_term_nodes(nodes),
    }
}

/// Rebuild the legacy `String` shape from a flat post-order slab.
///
/// Mirrors the recursive `format_canon_term` shape that the previous
/// `Box<CanonTerm>` representation produced, so
/// `structural_canonical_matches_string_form` and `debug_canonical_dump`
/// remain byte-identical.
fn format_canon_term_nodes(nodes: &[CanonTermNode]) -> String {
    let mut stack: Vec<String> = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            CanonTermNode::Var(v) => stack.push(format_value_key(v)),
            CanonTermNode::Const(v) => stack.push(format!("const:{v}")),
            CanonTermNode::Neg => {
                let t = stack.pop().expect("malformed CanonTerm post-order: Neg");
                stack.push(format!("neg({t})"));
            }
            CanonTermNode::Not => {
                let t = stack.pop().expect("malformed CanonTerm post-order: Not");
                stack.push(format!("not({t})"));
            }
            CanonTermNode::IsZero => {
                let t = stack.pop().expect("malformed CanonTerm post-order: IsZero");
                stack.push(format!("is_zero({t})"));
            }
            CanonTermNode::Add => {
                let r = stack
                    .pop()
                    .expect("malformed CanonTerm post-order: Add rhs");
                let l = stack
                    .pop()
                    .expect("malformed CanonTerm post-order: Add lhs");
                stack.push(format!("add({l},{r})"));
            }
            CanonTermNode::Sub => {
                let r = stack
                    .pop()
                    .expect("malformed CanonTerm post-order: Sub rhs");
                let l = stack
                    .pop()
                    .expect("malformed CanonTerm post-order: Sub lhs");
                stack.push(format!("sub({l},{r})"));
            }
            CanonTermNode::Mult => {
                let r = stack
                    .pop()
                    .expect("malformed CanonTerm post-order: Mult rhs");
                let l = stack
                    .pop()
                    .expect("malformed CanonTerm post-order: Mult lhs");
                stack.push(format!("mul({l},{r})"));
            }
        }
    }
    debug_assert_eq!(
        stack.len(),
        1,
        "CanonTerm post-order must rebuild to a single root"
    );
    stack.pop().expect("empty CanonTerm post-order slab")
}

fn format_canon_atom(atom: &CanonAtom) -> String {
    match atom {
        CanonAtom::Equal(l, r) => {
            format!("eq:{}:{}", format_canon_term(l), format_canon_term(r))
        }
        CanonAtom::NotEqual(l, r) => {
            format!("neq:{}:{}", format_canon_term(l), format_canon_term(r))
        }
        CanonAtom::LessEqual(l, r) => {
            format!("le:{}:{}", format_canon_term(l), format_canon_term(r))
        }
        CanonAtom::LessThan(l, r) => {
            format!("lt:{}:{}", format_canon_term(l), format_canon_term(r))
        }
    }
}

fn format_canon_lin_arith(lin: &CanonLinArith) -> String {
    let mut vars: Vec<String> = lin
        .vars
        .iter()
        .map(|(v, c)| format!("{}*{}", format_q(c), format_value_key(v)))
        .collect();
    vars.sort();
    let constant_is_zero = lin.constant.num == 0;
    if constant_is_zero {
        vars.join("+")
    } else if vars.is_empty() {
        format_q(&lin.constant)
    } else {
        format!("{}+{}", vars.join("+"), format_q(&lin.constant))
    }
}

fn format_canon_citv(c: &CanonCItv) -> String {
    // Reproduce the `Debug` form of the legacy `CItv` — used only by
    // `debug_canonical_dump` and the cross-check test, so we can re-derive
    // it by reconstructing the legacy `CItv` and `Debug`-formatting it.
    use crate::formula::citv::{Bound, CItv};
    fn to_bound(b: &CanonBound) -> Bound {
        match b {
            CanonBound::MinusInfinity => Bound::MinusInfinity,
            CanonBound::Int(i) => Bound::Int(*i),
            CanonBound::PlusInfinity => Bound::PlusInfinity,
        }
    }
    let citv = match c {
        CanonCItv::Between(lo, hi) => CItv::Between(to_bound(lo), to_bound(hi)),
        CanonCItv::Outside(lo, hi) => CItv::Outside(*lo, *hi),
    };
    format!("{citv:?}")
}

fn format_canon_fn_app(fa: &CanonFnApp) -> String {
    let actuals = fa
        .actuals
        .iter()
        .map(format_canon_operand)
        .collect::<Vec<_>>()
        .join(",");
    format!("{}({})->{}", fa.callee, actuals, format_value_key(&fa.ret))
}

fn format_canon_formula_entry(entry: &CanonFormulaEntry) -> String {
    match entry {
        CanonFormulaEntry::VarEq(lhs, rhs) => {
            format!("uf:{}->{}", format_value_key(lhs), format_value_key(rhs))
        }
        CanonFormulaEntry::LinearEq(lhs, lin) => {
            format!(
                "lin:{}={}",
                format_value_key(lhs),
                format_canon_lin_arith(lin)
            )
        }
        CanonFormulaEntry::Atom(atom) => format!("atom:{}", format_canon_atom(atom)),
        CanonFormulaEntry::TermEq(te) => {
            format!(
                "term_eq:{}:{}:{}:{}",
                format_value_key(&te.lhs),
                te.op,
                format_canon_operand(&te.op_lhs),
                format_canon_operand(&te.op_rhs)
            )
        }
        CanonFormulaEntry::Interval(lhs, citv) => {
            format!(
                "interval:{}:{}",
                format_value_key(lhs),
                format_canon_citv(citv)
            )
        }
        CanonFormulaEntry::IsInt(lhs) => format!("is_int:{}", format_value_key(lhs)),
        CanonFormulaEntry::FnApp(fa) => format!("fn_app:{}", format_canon_fn_app(fa)),
    }
}

/// Render the structural formula as the legacy sorted `Vec<String>` form.
///
/// The legacy code applied a final `parts.sort()` over the assembled
/// `Vec<String>`, mixing entries across sections (atoms, term_eqs,
/// linear_eqs, ...) by lexicographic prefix order. We reproduce that
/// here so `debug_canonical_dump` and the cross-check test remain
/// byte-identical to the pre-structural-key implementation.
fn format_canon_formula_legacy(formula: &[CanonFormulaEntry]) -> Vec<String> {
    let mut out: Vec<String> = formula.iter().map(format_canon_formula_entry).collect();
    out.sort();
    out
}

// ---- Legacy String renderers for the structural non-formula sections. ----
//
// These reproduce the byte-for-byte shape of the pre-structural
// `canonical_stack` / `canonical_heap` / `canonical_attrs` /
// `canonical_dynamic_types` `Vec<String>` forms. They are used by
// `debug_canonical_dump` (the [pulse-progress] log shape relies on this
// remaining stable) and by the `structural_canonical_matches_*`
// cross-check tests. The hot fixpoint path does NOT touch them.

fn format_canon_access(access: &AccessSortKey) -> String {
    match access {
        AccessSortKey::Dereference => "deref".to_string(),
        // Legacy `canonical_access` formatted `FieldAccess(field)` via
        // `Display for Fieldname` (`"{class}.{field}"`); `AccessSortKey::Field`
        // now carries the full `Fieldname` so the rendered output is
        // byte-identical to the pre-structural form.
        AccessSortKey::Field(field) => format!("field:{field}"),
        AccessSortKey::Array { typ, index } => {
            format!("array:{typ}:{}", format_value_key(index))
        }
    }
}

fn format_canon_attribute(attr: &CanonAttribute) -> String {
    match attr {
        CanonAttribute::ReturnedFromUnknown(values) => {
            let values = values
                .iter()
                .map(format_value_key)
                .collect::<Vec<_>>()
                .join(",");
            format!("ReturnedFromUnknown({values})")
        }
        CanonAttribute::MustBeValid(loc, reason) => {
            format!("MustBeValid({loc}, {reason:?})")
        }
        CanonAttribute::MustBeInitialized(loc) => format!("MustBeInitialized({loc})"),
        CanonAttribute::WrittenTo(loc) => format!("WrittenTo({loc})"),
        CanonAttribute::Other(attr) => format!("{attr:?}"),
    }
}

fn format_canon_stack_legacy(stack: &[CanonStackEntry]) -> Vec<String> {
    // Legacy `canonical_stack` rendered each entry as `"{var}={addr}"`
    // and then sorted the resulting `Vec<String>` lexicographically.
    // The structural `Vec<CanonStackEntry>` is sorted by `(Var, addr)`,
    // which is NOT the same lexicographic order as the rendered string
    // form, so we re-sort the output here to preserve the legacy
    // [pulse-progress] log shape exactly.
    let mut out: Vec<String> = stack
        .iter()
        .map(|e| format!("{}={}", e.var, format_value_key(&e.addr)))
        .collect();
    out.sort();
    out
}

fn format_canon_heap_legacy(heap: &[CanonHeapEdge]) -> Vec<String> {
    let mut out: Vec<String> = heap
        .iter()
        .map(|e| {
            format!(
                "{}:{}->{}",
                format_value_key(&e.src),
                format_canon_access(&e.access),
                format_value_key(&e.target)
            )
        })
        .collect();
    out.sort();
    out
}

fn format_canon_attrs_legacy(attrs: &[CanonAttrEntry]) -> Vec<String> {
    let mut out: Vec<String> = attrs
        .iter()
        .map(|e| {
            format!(
                "{}:{}",
                format_value_key(&e.addr),
                format_canon_attribute(&e.attr)
            )
        })
        .collect();
    out.sort();
    out
}

fn format_canon_dynamic_types_legacy(dyn_types: &[CanonDynamicType]) -> Vec<String> {
    let mut out: Vec<String> = dyn_types
        .iter()
        .map(|e| format!("dyn:{}={:?}", format_value_key(&e.addr), e.typ))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use absint::disjunctive::DisjunctiveDomain;
    use absint::domain::{AbstractDomain, Comparable};
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::qualified_cpp_name::QualifiedCppName;
    use sil::typ::{Typ, TypeName};
    use sil::var::Var;

    use num_traits::Zero;

    use super::*;
    use crate::attribute::Allocator;
    use crate::execution_domain::ExecutionDomain;

    fn make_pdesc_with_formals(formals: &[&str]) -> Procdesc {
        let pname = Procname::c_from_string("state_cmp_test");
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        pdesc.formals = formals
            .iter()
            .map(|name| (Mangled::from_string(*name), Typ::void(), Default::default()))
            .collect();
        pdesc
    }

    fn make_state(with_dummy_fresh_values: usize, with_disconnected_leak: bool) -> AbductiveDomain {
        let pdesc = make_pdesc_with_formals(&["x"]);
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pdesc.proc_name.clone());
        let var = Var::ProgramVar(Box::new(pvar));
        let formal_addr = state.post.stack.find(&var).unwrap();

        for _ in 0..with_dummy_fresh_values {
            let _ = AbstractValue::mk_fresh();
        }

        let pointee = state.read_heap(formal_addr, Access::Dereference);
        let field_value = AbstractValue::mk_fresh();
        let field = Access::FieldAccess(sil::fieldname::Fieldname::make(
            sil::typ::TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string(
                "Node",
            )),
            "next",
        ));
        state.write_heap(pointee, field, field_value);
        state.allocate(field_value, Allocator::CMalloc, Location::dummy());

        if with_disconnected_leak {
            let leaked = AbstractValue::mk_fresh();
            state.allocate(leaked, Allocator::CMalloc, Location::dummy());
        }

        state
    }

    fn add_extra_reachable_edge(state: &mut AbductiveDomain) {
        let formal_addr = state
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .expect("formal should exist");
        let pointee = state.read_heap(formal_addr, Access::Dereference);
        let extra = AbstractValue::mk_fresh();
        let field = Access::FieldAccess(sil::fieldname::Fieldname::make(
            sil::typ::TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string(
                "Node",
            )),
            "prev",
        ));
        state.write_heap(pointee, field, extra);
        state.allocate(extra, Allocator::CMalloc, Location::dummy());
    }

    /// Pin the contract that the `canonicalize_state` + `eq_canonical`
    /// split is byte-identical to `alpha_equivalent` on every shape
    /// state pair the existing suite covers (alpha-renamed, disconnected
    /// garbage, helper sets, attribute timestamps, dynamic types, and
    /// reachable-heap diff). Without this lock, the
    /// `DisjunctiveDomain<ExecutionDomain>::leq` cross-product
    /// optimisation could silently drift away from `alpha_equivalent`'s
    /// equality.
    #[test]
    fn alpha_equivalent_matches_canonicalize_then_eq_canonical_state_only() {
        // Every (lhs, rhs) pair below is a fixture used elsewhere in
        // this module. For each one we assert the structural identity
        //   alpha_equivalent(a, b)
        // ==
        //   eq_canonical(&canonicalize_state(a), &canonicalize_state(b))
        // both ways.
        fn check(lhs: &AbductiveDomain, rhs: &AbductiveDomain) {
            let lhs_canon = canonicalize_state(lhs);
            let rhs_canon = canonicalize_state(rhs);
            assert_eq!(
                alpha_equivalent(lhs, rhs),
                eq_canonical(&lhs_canon, &rhs_canon),
                "alpha_equivalent must equal eq_canonical(canonicalize(lhs), canonicalize(rhs))"
            );
            assert_eq!(
                alpha_equivalent(rhs, lhs),
                eq_canonical(&rhs_canon, &lhs_canon),
                "alpha_equivalent must equal eq_canonical(canonicalize(rhs), canonicalize(lhs))"
            );
        }

        // alpha-renamed via burning IDs.
        AbstractValue::reset_counters();
        let a = make_state(0, false);
        AbstractValue::reset_counters();
        let b = make_state(2, false);
        check(&a, &b);

        // disconnected garbage on rhs.
        AbstractValue::reset_counters();
        let a = make_state(0, false);
        AbstractValue::reset_counters();
        let b = make_state(2, true);
        check(&a, &b);

        // attribute timestamps differ.
        AbstractValue::reset_counters();
        let mut a = make_state(0, false);
        AbstractValue::reset_counters();
        let mut b = make_state(0, false);
        let formal_a = a.post.stack.iter().next().map(|(_, addr)| *addr).unwrap();
        let formal_b = b.post.stack.iter().next().map(|(_, addr)| *addr).unwrap();
        let loc = Location::dummy();
        a.add_attr(formal_a, Attribute::MustBeValid(1, loc.clone(), None));
        b.add_attr(formal_b, Attribute::MustBeValid(99, loc.clone(), None));
        check(&a, &b);

        // structurally distinct via extra reachable edge.
        AbstractValue::reset_counters();
        let a = make_state(0, false);
        AbstractValue::reset_counters();
        let mut b = make_state(2, false);
        add_extra_reachable_edge(&mut b);
        check(&a, &b);

        // identity on the same state must always agree.
        AbstractValue::reset_counters();
        let a = make_state(1, false);
        check(&a, &a);
    }

    /// Same contract for the value variant (used by
    /// `LatentInvalidAccess` leq). Locks `alpha_equivalent_value` ==
    /// `eq_canonical_with_value(canonicalize(lhs), av, canonicalize(rhs), bv)`.
    #[test]
    fn alpha_equivalent_value_matches_canonicalize_then_eq_canonical_with_value() {
        // Equivalent states + matching values: must be true under both APIs.
        AbstractValue::reset_counters();
        let mut a = make_state(0, false);
        AbstractValue::reset_counters();
        let mut b = make_state(3, false);
        let formal_a = a.post.stack.iter().next().map(|(_, addr)| *addr).unwrap();
        let formal_b = b.post.stack.iter().next().map(|(_, addr)| *addr).unwrap();
        let pointee_a = a.read_heap(formal_a, Access::Dereference);
        let pointee_b = b.read_heap(formal_b, Access::Dereference);

        let a_canon = canonicalize_state(&a);
        let b_canon = canonicalize_state(&b);
        assert_eq!(
            alpha_equivalent_value(&a, pointee_a, &b, pointee_b),
            eq_canonical_with_value(&a_canon, pointee_a, &b_canon, pointee_b),
        );
        // Mismatched values on equivalent states: both APIs must say false.
        assert_eq!(
            alpha_equivalent_value(&a, formal_a, &b, pointee_b),
            eq_canonical_with_value(&a_canon, formal_a, &b_canon, pointee_b),
        );
        // States not equivalent (extra reachable edge): both APIs say false.
        AbstractValue::reset_counters();
        let mut c = make_state(0, false);
        add_extra_reachable_edge(&mut c);
        let formal_c = c.post.stack.iter().next().map(|(_, addr)| *addr).unwrap();
        let pointee_c = c.read_heap(formal_c, Access::Dereference);
        let c_canon = canonicalize_state(&c);
        assert_eq!(
            alpha_equivalent_value(&a, pointee_a, &c, pointee_c),
            eq_canonical_with_value(&a_canon, pointee_a, &c_canon, pointee_c),
        );
    }

    /// End-to-end: with the new `disjunctive_leq_subset` hook on
    /// `ExecutionDomain`, the cross-product `DisjunctiveDomain::leq`
    /// observation must be byte-identical to the previous (default) impl.
    /// We exercise the alpha-renamed cross-product on a multi-disjunct
    /// pair so the optimised path is actually taken.
    #[test]
    fn disjunctive_leq_with_canonical_optimisation_matches_default_semantics() {
        AbstractValue::reset_counters();
        let a1 = make_state(0, false);
        AbstractValue::reset_counters();
        let a2 = make_state(2, false);
        AbstractValue::reset_counters();
        let b1 = make_state(0, false);
        AbstractValue::reset_counters();
        let mut b2 = make_state(2, false);
        add_extra_reachable_edge(&mut b2);

        // Build two domains: lhs has {a1, a2}, rhs has {b1, b2}. b1 is
        // alpha-equivalent to a1 (and a2); b2 is structurally distinct.
        let lhs = DisjunctiveDomain {
            disjuncts: vec![
                ExecutionDomain::ContinueProgram(a1),
                ExecutionDomain::ContinueProgram(a2),
            ],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };
        let rhs = DisjunctiveDomain {
            disjuncts: vec![
                ExecutionDomain::ContinueProgram(b1),
                ExecutionDomain::ContinueProgram(b2),
            ],
            max_disjuncts: 20,
            max_widen_iters: 3,
            had_dropped_disjuncts: false,
        };

        // Reference implementation: explicit N·M cross-product with
        // the unoptimised `alpha_equivalent` path.
        let reference = lhs
            .disjuncts
            .iter()
            .all(|l| rhs.disjuncts.iter().any(|r| l.leq(r)));
        assert!(
            reference,
            "lhs should be subsumed by rhs in the reference impl"
        );
        // The DisjunctiveDomain leq path now goes through the
        // `disjunctive_leq_subset` hook, which on `ExecutionDomain`
        // pre-canonicalises each disjunct once. Result must match.
        assert_eq!(reference, lhs.leq(&rhs));
    }

    #[test]
    fn test_alpha_equivalent_states_ignore_raw_value_renaming() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let state2 = make_state(2, false);

        let exec1 = ExecutionDomain::ContinueProgram(state1);
        let exec2 = ExecutionDomain::ContinueProgram(state2);

        assert!(exec1.leq(&exec2));
        assert!(exec2.leq(&exec1));
    }

    #[test]
    fn test_debug_signature_matches_alpha_equivalent_states() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let state2 = make_state(2, false);

        assert_eq!(debug_signature(&state1), debug_signature(&state2));
    }

    #[test]
    fn test_alpha_equivalent_states_do_not_dedup_during_fast_join() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let state2 = make_state(3, false);

        let lhs = DisjunctiveDomain::singleton(ExecutionDomain::ContinueProgram(state1), 20, 3);
        let rhs = DisjunctiveDomain::singleton(ExecutionDomain::ContinueProgram(state2), 20, 3);
        let joined = lhs.join(&rhs);

        assert_eq!(joined.disjuncts.len(), 2);
    }

    #[test]
    fn test_alpha_equivalent_states_still_collapse_during_widen() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let state2 = make_state(3, false);

        let lhs = DisjunctiveDomain::singleton(ExecutionDomain::ContinueProgram(state1), 20, 3);
        let rhs = DisjunctiveDomain::singleton(ExecutionDomain::ContinueProgram(state2), 20, 3);
        let widened = lhs.widen(&rhs, 1);

        assert_eq!(widened.disjuncts.len(), 1);
    }

    #[test]
    fn test_disconnected_state_is_ignored_by_alpha_equivalence_like_ocaml_leq() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let state2 = make_state(2, true);

        let exec1 = ExecutionDomain::ContinueProgram(state1);
        let exec2 = ExecutionDomain::ContinueProgram(state2);

        assert!(exec1.leq(&exec2));
        assert!(exec2.leq(&exec1));
    }

    #[test]
    fn test_helper_sets_do_not_affect_alpha_equivalence() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let mut state2 = make_state(2, false);

        let formal_addr = state2
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .expect("formal should exist");
        state2.mark_must_be_valid(formal_addr);
        state2.add_need_dynamic_type_specialization(formal_addr);

        let exec1 = ExecutionDomain::ContinueProgram(state1);
        let exec2 = ExecutionDomain::ContinueProgram(state2);

        assert!(exec1.leq(&exec2));
        assert!(exec2.leq(&exec1));
    }

    /// Regression: each fixpoint iteration of the same procedure can
    /// assign different `Timestamp` values to the same logical
    /// `MustBeValid` / `MustBeInitialized` / `WrittenTo` attribute,
    /// because the per-state `next_attr_timestamp` counter is bumped by
    /// intervening work between iterations. Two states that differ only
    /// in those timestamps must still be alpha-equivalent so that the
    /// outer fixpoint converges.
    ///
    /// On whole-program OpenSSL, this regression broke convergence on
    /// `OBJ_bsearch_ex_` after the third re-analysis (callees changed),
    /// driving `max_visit_count` past `10001` (the `pulse_max_widens`
    /// safety cap).
    #[test]
    fn test_alpha_equivalent_states_ignore_attribute_timestamps() {
        use crate::attribute::Attribute;
        use sil::location::Location;

        AbstractValue::reset_counters();
        let mut state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let mut state2 = make_state(0, false);

        let formal_addr_1 = state1
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .expect("formal should exist");
        let formal_addr_2 = state2
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .expect("formal should exist");

        // Same logical attributes, different timestamps: state1 sees
        // ts=1, state2 sees ts=99. Locations are equal.
        let loc = Location::dummy();
        state1.add_attr(formal_addr_1, Attribute::MustBeValid(1, loc.clone(), None));
        state2.add_attr(formal_addr_2, Attribute::MustBeValid(99, loc.clone(), None));

        let exec1 = ExecutionDomain::ContinueProgram(state1);
        let exec2 = ExecutionDomain::ContinueProgram(state2);

        assert!(
            exec1.leq(&exec2),
            "states differing only in attribute timestamps should be leq"
        );
        assert!(
            exec2.leq(&exec1),
            "states differing only in attribute timestamps should be leq"
        );
    }

    #[test]
    fn test_dynamic_types_participate_in_alpha_equivalence() {
        fn add_same_dynamic_type(state: &mut AbductiveDomain) {
            let formal_addr = state
                .post
                .stack
                .iter()
                .next()
                .map(|(_var, addr)| *addr)
                .expect("formal should exist");
            state.add_dynamic_type_unsafe(
                formal_addr,
                Typ::mk_struct(TypeName::CStruct(QualifiedCppName::from_string("Callable"))),
            );
        }

        AbstractValue::reset_counters();
        let mut state1 = make_state(0, false);
        add_same_dynamic_type(&mut state1);
        AbstractValue::reset_counters();
        let mut state2 = make_state(2, false);
        add_same_dynamic_type(&mut state2);

        let exec1 = ExecutionDomain::ContinueProgram(state1.clone());
        let exec2 = ExecutionDomain::ContinueProgram(state2.clone());
        assert!(exec1.leq(&exec2));
        assert!(exec2.leq(&exec1));

        let exec_with_dyn = ExecutionDomain::ContinueProgram(state1);
        AbstractValue::reset_counters();
        let exec_without_dyn = ExecutionDomain::ContinueProgram(make_state(0, false));
        assert!(!exec_with_dyn.leq(&exec_without_dyn));
        assert!(!exec_without_dyn.leq(&exec_with_dyn));
    }

    #[test]
    fn test_reachable_heap_difference_is_not_considered_equivalent() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let mut state2 = make_state(2, false);
        add_extra_reachable_edge(&mut state2);

        let exec1 = ExecutionDomain::ContinueProgram(state1);
        let exec2 = ExecutionDomain::ContinueProgram(state2);

        assert!(!exec1.leq(&exec2));
        assert!(!exec2.leq(&exec1));
    }

    #[test]
    fn test_alpha_equivalent_array_zero_collision_reuses_constant_index_repr() {
        fn make_array_zero_state(burn_ids: usize) -> AbductiveDomain {
            let pdesc = make_pdesc_with_formals(&["array"]);
            let mut state = AbductiveDomain::mk_initial(&pdesc);
            let pvar = Pvar::mk(Mangled::from_string("array"), pdesc.proc_name.clone());
            let formal_var = Var::ProgramVar(Box::new(pvar));
            let formal_addr = state.post.stack.find(&formal_var).unwrap();

            for _ in 0..burn_ids {
                let _ = AbstractValue::mk_fresh();
            }

            let array = state.read_heap(formal_addr, Access::Dereference);
            let index_zero = AbstractValue::mk_fresh();
            assert!(state.and_equal_const(index_zero, 0).is_sat());

            // Producer-time null from malloc: the constant-collision path must
            // merge this fresh known-zero value into the earlier index-zero
            // representative before the ArrayAccess edge is summarized.
            let malloc_null = AbstractValue::mk_fresh();
            assert!(state
                .and_equal_const_with_constant_collision(malloc_null, 0)
                .is_sat());
            assert_eq!(
                state.get_var_repr(malloc_null),
                state.get_var_repr(index_zero)
            );

            let slot = AbstractValue::mk_fresh();
            state.write_heap(
                array,
                Access::ArrayAccess(Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)), index_zero),
                slot,
            );
            state
                .post
                .heap
                .add_edge(slot, Access::Dereference, malloc_null);
            state
        }

        AbstractValue::reset_counters();
        let lhs = make_array_zero_state(0);
        AbstractValue::reset_counters();
        let rhs = make_array_zero_state(5);

        assert!(
            alpha_equivalent(&lhs, &rhs),
            "array-index/null zero collision must be stable under alpha-isograph renaming\nlhs:\n{}\nrhs:\n{}",
            debug_canonical_dump(&lhs),
            debug_canonical_dump(&rhs)
        );
    }

    #[test]
    fn test_debug_signature_changes_for_reachable_heap_difference() {
        AbstractValue::reset_counters();
        let state1 = make_state(0, false);
        AbstractValue::reset_counters();
        let mut state2 = make_state(2, false);
        add_extra_reachable_edge(&mut state2);

        assert_ne!(debug_signature(&state1), debug_signature(&state2));
    }

    /// Scout brief perf_explore_linear_const_audit (2026-05-10) FIRST
    /// EXPERIMENT: dropping `intervals`, `is_int`, `term_value_index`,
    /// `fn_app_eqs`, and dead atoms must NOT alter the canonical formula
    /// fingerprint that `state_cmp::alpha_equivalent` derives for the
    /// stack-reachable subgraph. If this drifts, the GC has eaten
    /// load-bearing equality info and we must STOP per the scope guards.
    ///
    /// Two perspectives are checked:
    ///   (a) For a state that contains ONLY reachable formula facts, the
    ///       canonical_formula fingerprint is identical before and after
    ///       running the intermediate GC — it has nothing to drop.
    ///   (b) For a state that ALSO contains unreachable formula facts,
    ///       running the GC produces a state that is alpha-equivalent to
    ///       the GC of an identical companion state — i.e. the GC is
    ///       deterministic and only touches the dead subgraph.
    #[test]
    fn test_intermediate_formula_gc_preserves_alpha_equivalent_fingerprint() {
        // (a) Reachable-only fixture: GC must be a no-op for canonical
        // formula fingerprints.
        AbstractValue::reset_counters();
        let mut state_reachable_only = make_state(0, false);
        let formal_addr = state_reachable_only
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .expect("formal should exist");
        let pointee = state_reachable_only.read_heap(formal_addr, Access::Dereference);
        assert!(state_reachable_only.and_equal_const(pointee, 5).is_sat());

        let signature_before = debug_signature(&state_reachable_only);
        state_reachable_only.shrink_post_to_stack_reachable_with_formula_gc();
        let signature_after = debug_signature(&state_reachable_only);
        assert_eq!(
            signature_before, signature_after,
            "intermediate GC must not change canonical_formula fingerprint when every fact is reachable"
        );

        // (b) States with the same reachable subgraph but different dead
        // formula facts must collapse to the same fingerprint after GC.
        AbstractValue::reset_counters();
        let mut state_with_dead = make_state(0, false);
        AbstractValue::reset_counters();
        let mut state_with_more_dead = make_state(0, false);

        let inject_reachable = |state: &mut AbductiveDomain| {
            let formal_addr = state
                .post
                .stack
                .iter()
                .next()
                .map(|(_var, addr)| *addr)
                .expect("formal should exist");
            let pointee = state.read_heap(formal_addr, Access::Dereference);
            assert!(state.and_equal_const(pointee, 5).is_sat());
        };
        inject_reachable(&mut state_with_dead);
        inject_reachable(&mut state_with_more_dead);

        // Differently-sized dead vocabularies, all unreachable from the
        // post stack. We use only fact families that
        // `prune_unreachable_simple_facts` actually drops in the FIRST
        // EXPERIMENT (is_int, fn_app_eqs); we deliberately do NOT plant
        // linear_eqs / term_eqs here because they are not pruned and would
        // survive into the canonical fingerprint.
        for i in 0..3u32 {
            let dead_actual = AbstractValue::mk_fresh();
            let dead_ret = AbstractValue::mk_fresh();
            state_with_dead.path_condition.and_is_int(dead_actual);
            assert!(state_with_dead
                .path_condition
                .and_fn_app(dead_ret, &format!("__dead_a_{i}"), &[dead_actual])
                .is_sat());
        }
        for i in 0..6u32 {
            let dead_actual = AbstractValue::mk_fresh();
            let dead_ret = AbstractValue::mk_fresh();
            state_with_more_dead.path_condition.and_is_int(dead_actual);
            assert!(state_with_more_dead
                .path_condition
                .and_fn_app(dead_ret, &format!("__dead_b_{i}"), &[dead_actual])
                .is_sat());
        }

        state_with_dead.shrink_post_to_stack_reachable_with_formula_gc();
        state_with_more_dead.shrink_post_to_stack_reachable_with_formula_gc();

        let exec_a = ExecutionDomain::ContinueProgram(state_with_dead);
        let exec_b = ExecutionDomain::ContinueProgram(state_with_more_dead);
        assert!(
            exec_a.leq(&exec_b) && exec_b.leq(&exec_a),
            "states differing only in dead formula facts must be alpha-equivalent after GC"
        );
    }

    // ---- Tests specific to the structural canonical formula. ----

    /// The legacy String-formula path computed `Vec<String>` with this
    /// shape; we keep it inline as a debug helper so we can cross-check
    /// the structural formula against it on a small fixture. Mirrors the
    /// pre-structural-key implementation byte-for-byte, including the
    /// final `parts.sort()` over the assembled vector (which mixes
    /// sections lexicographically by their `"uf:"` / `"lin:"` / `"atom:"`
    /// / `"term_eq:"` / `"interval:"` / `"is_int:"` / `"fn_app:"`
    /// prefix).
    /// Pre-structural `canonical_stack` shape, kept inline as a debug
    /// helper for `structural_canonical_matches_non_formula_string_form`.
    fn legacy_canonical_stack_strings(
        stack: &crate::base_stack::BaseStack,
        canon: &Canonicalizer,
    ) -> Vec<String> {
        let mut entries: Vec<_> = stack
            .iter()
            .map(|(var, addr)| format!("{var}={}", canon.get(*addr).unwrap()))
            .collect();
        entries.sort();
        entries
    }

    /// Pre-structural `canonical_heap` shape, kept inline as a debug
    /// helper for `structural_canonical_matches_non_formula_string_form`.
    fn legacy_canonical_heap_strings(
        memory: &crate::base_memory::BaseMemory,
        reachable: &std::collections::HashSet<AbstractValue>,
        canon: &Canonicalizer,
    ) -> Vec<String> {
        fn legacy_access(access: &Access, canon: &Canonicalizer) -> String {
            match access {
                Access::Dereference => "deref".to_string(),
                Access::FieldAccess(field) => format!("field:{field}"),
                Access::ArrayAccess(typ, index) => {
                    format!("array:{typ}:{}", canon.get(*index).unwrap())
                }
            }
        }
        let mut edges = Vec::new();
        for (src, accesses) in memory.iter() {
            if !reachable.contains(src) {
                continue;
            }
            for (access, target) in accesses.iter() {
                edges.push(format!(
                    "{}:{}->{}",
                    canon.get(*src).unwrap(),
                    legacy_access(access, canon),
                    canon.get(*target).unwrap()
                ));
            }
        }
        edges.sort();
        edges
    }

    /// Pre-structural `canonical_attrs` shape, kept inline as a debug
    /// helper for `structural_canonical_matches_non_formula_string_form`.
    /// Reproduces the timestamp-stripping behaviour of the production
    /// `canonical_attr` formatter.
    fn legacy_canonical_attrs_strings(
        attrs: &crate::base_attrs::BaseAddressAttributes,
        reachable: &std::collections::HashSet<AbstractValue>,
        canon: &Canonicalizer,
    ) -> Vec<String> {
        fn legacy_attr(attr: &Attribute, canon: &Canonicalizer) -> String {
            match attr {
                Attribute::ReturnedFromUnknown(values) => {
                    let values = values
                        .iter()
                        .map(|v| canon.get(*v).unwrap().to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    format!("ReturnedFromUnknown({values})")
                }
                Attribute::MustBeValid(_ts, loc, reason) => {
                    format!("MustBeValid({loc}, {reason:?})")
                }
                Attribute::MustBeInitialized(_ts, loc) => format!("MustBeInitialized({loc})"),
                Attribute::WrittenTo(_ts, loc) => format!("WrittenTo({loc})"),
                _ => format!("{attr:?}"),
            }
        }
        let mut entries = Vec::new();
        for (addr, addr_attrs) in attrs.iter() {
            if !reachable.contains(addr) {
                continue;
            }
            for attr in addr_attrs.iter() {
                entries.push(format!(
                    "{}:{}",
                    canon.get(*addr).unwrap(),
                    legacy_attr(attr, canon)
                ));
            }
        }
        entries.sort();
        entries
    }

    /// Pre-structural `canonical_dynamic_types` shape, kept inline as a
    /// debug helper for `structural_canonical_matches_non_formula_string_form`.
    fn legacy_canonical_dynamic_types_strings(
        state: &AbductiveDomain,
        canon: &Canonicalizer,
    ) -> Vec<String> {
        let mut entries: Vec<_> = state
            .iter_dynamic_types()
            .filter_map(|(addr, typ)| canon.get(addr).map(|label| (label, typ)))
            .collect();
        entries.sort_by_key(|(label, _)| *label);
        entries
            .into_iter()
            .map(|(label, typ)| format!("dyn:{label}={typ:?}"))
            .collect()
    }

    fn legacy_canonical_formula_strings(state: &AbductiveDomain) -> Vec<String> {
        let canonical = canonicalize(state);
        let canon = &canonical.canon;
        let phi = state.path_condition.phi();
        let mut parts: Vec<String> = Vec::new();

        for (lhs, rhs) in phi.var_eqs.iter_equalities() {
            parts.push(format!(
                "uf:{}->{}",
                canon.get(lhs).unwrap(),
                canon.get(rhs).unwrap()
            ));
        }
        for (lhs, lin) in phi.linear_eqs.iter() {
            let mut vars: Vec<String> = lin
                .vars
                .iter()
                .map(|(value, coeff)| {
                    let q_str = if coeff.denom() == &1 {
                        coeff.numer().to_string()
                    } else {
                        format!("{}/{}", coeff.numer(), coeff.denom())
                    };
                    format!("{}*{}", q_str, canon.get(*value).unwrap())
                })
                .collect();
            vars.sort();
            let lin_str = if lin.constant.is_zero() {
                vars.join("+")
            } else if vars.is_empty() {
                if lin.constant.denom() == &1 {
                    lin.constant.numer().to_string()
                } else {
                    format!("{}/{}", lin.constant.numer(), lin.constant.denom())
                }
            } else {
                let const_str = if lin.constant.denom() == &1 {
                    lin.constant.numer().to_string()
                } else {
                    format!("{}/{}", lin.constant.numer(), lin.constant.denom())
                };
                format!("{}+{}", vars.join("+"), const_str)
            };
            parts.push(format!("lin:{}={}", canon.get(*lhs).unwrap(), lin_str));
        }
        fn legacy_term_str(t: &Term, canon: &Canonicalizer) -> String {
            match t {
                Term::Var(v) => canon.get(*v).unwrap().to_string(),
                Term::Const(v) => format!("const:{v}"),
                Term::Add(l, r) => {
                    format!(
                        "add({},{})",
                        legacy_term_str(l, canon),
                        legacy_term_str(r, canon)
                    )
                }
                Term::Sub(l, r) => {
                    format!(
                        "sub({},{})",
                        legacy_term_str(l, canon),
                        legacy_term_str(r, canon)
                    )
                }
                Term::Mult(l, r) => {
                    format!(
                        "mul({},{})",
                        legacy_term_str(l, canon),
                        legacy_term_str(r, canon)
                    )
                }
                Term::Neg(t) => format!("neg({})", legacy_term_str(t, canon)),
                Term::Not(t) => format!("not({})", legacy_term_str(t, canon)),
                Term::IsZero(t) => format!("is_zero({})", legacy_term_str(t, canon)),
            }
        }
        for atom in phi.atoms.iter() {
            let s = match atom {
                Atom::Equal(l, r) => {
                    format!(
                        "eq:{}:{}",
                        legacy_term_str(l, canon),
                        legacy_term_str(r, canon)
                    )
                }
                Atom::NotEqual(l, r) => {
                    format!(
                        "neq:{}:{}",
                        legacy_term_str(l, canon),
                        legacy_term_str(r, canon)
                    )
                }
                Atom::LessEqual(l, r) => {
                    format!(
                        "le:{}:{}",
                        legacy_term_str(l, canon),
                        legacy_term_str(r, canon)
                    )
                }
                Atom::LessThan(l, r) => {
                    format!(
                        "lt:{}:{}",
                        legacy_term_str(l, canon),
                        legacy_term_str(r, canon)
                    )
                }
            };
            parts.push(format!("atom:{s}"));
        }
        fn legacy_op_str(op: &Operand, canon: &Canonicalizer) -> String {
            match op {
                Operand::AbstractValue(v) => canon.get(*v).unwrap().to_string(),
                Operand::ConstOperand(v) => format!("const:{v}"),
            }
        }
        for (lhs, te) in phi.term_eqs.iter() {
            parts.push(format!(
                "term_eq:{}:{}:{}:{}",
                canon.get(*lhs).unwrap(),
                te.op,
                legacy_op_str(&te.lhs, canon),
                legacy_op_str(&te.rhs, canon)
            ));
        }
        for (v, ci) in phi.intervals.iter() {
            parts.push(format!("interval:{}:{ci:?}", canon.get(*v).unwrap()));
        }
        for v in phi.is_int_vars.iter() {
            parts.push(format!("is_int:{}", canon.get(*v).unwrap()));
        }
        for (key, ret) in phi.iter_fn_app_eqs() {
            let actuals = key
                .actuals
                .iter()
                .map(|a| match a {
                    FnAppActual::Const(c) => format!("const:{c}"),
                    FnAppActual::Var(v) => canon.get(*v).unwrap().to_string(),
                })
                .collect::<Vec<_>>()
                .join(",");
            parts.push(format!(
                "fn_app:{}({})->{}",
                key.callee,
                actuals,
                canon.get(*ret).unwrap()
            ));
        }
        parts.sort();
        parts
    }

    /// Cross-check: on a small fixture exercising every formula entry
    /// kind (atom, linear_eq, term_eq, var_eq, interval, is_int, fn_app),
    /// the structural canonical formula — when formatted via the legacy
    /// `String` shape — must match what the legacy `String`-only path
    /// would have produced. This guards against drift between the two
    /// representations: any divergence here means the structural form
    /// has lost or duplicated information.
    #[test]
    fn structural_canonical_matches_string_form() {
        AbstractValue::reset_counters();
        let mut state = make_state(0, false);
        let formal_addr = state
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .expect("formal should exist");
        let pointee = state.read_heap(formal_addr, Access::Dereference);

        // atom + linear_eq via and_equal_const
        assert!(state.and_equal_const(pointee, 7).is_sat());

        // is_int + fn_app
        let arg = AbstractValue::mk_fresh();
        let ret = AbstractValue::mk_fresh();
        state.path_condition.and_is_int(arg);
        assert!(state
            .path_condition
            .and_fn_app(ret, "external_fn", &[arg])
            .is_sat());

        let canonical = canonicalize(&state);
        let structural_strings = format_canon_formula_legacy(&canonical.state.formula);
        let legacy_strings = legacy_canonical_formula_strings(&state);

        assert_eq!(
            structural_strings, legacy_strings,
            "structural canonical formula (formatted) must match legacy String-form output"
        );
    }

    /// Cross-check: the structural canonical stack/heap/attrs/dynamic_types
    /// sections, when formatted via the legacy `String` shape, must match
    /// what the pre-structural `String`-only `canonical_*` helpers would
    /// have produced byte-for-byte. Guards against drift in the rendered
    /// `[pulse-progress]` log shape.
    #[test]
    fn structural_canonical_matches_non_formula_string_form() {
        // Fixture exercises every non-formula section:
        //   stack:        the formal `x`
        //   heap:         deref + field edges
        //   attrs:        Allocated (Other), MustBeValid (timestamp-stripped)
        //   dynamic_types: Callable on the formal pointee
        AbstractValue::reset_counters();
        let mut state = make_state(0, false);
        let formal_addr = state
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .expect("formal should exist");
        state.mark_must_be_valid(formal_addr);
        state.add_dynamic_type_unsafe(
            formal_addr,
            Typ::mk_struct(TypeName::CStruct(QualifiedCppName::from_string("Callable"))),
        );

        let canonical = canonicalize(&state);
        let pre_reachable = reachable_from_stack(&state.pre.stack, &state.pre.heap);
        let post_reachable = reachable_from_stack(&state.post.stack, &state.post.heap);

        // Stack: legacy form was `"{var}={addr}"`, sorted lexicographically.
        let legacy_post_stack = legacy_canonical_stack_strings(&state.post.stack, &canonical.canon);
        assert_eq!(
            format_canon_stack_legacy(&canonical.state.post_stack),
            legacy_post_stack,
            "structural post_stack rendering must match legacy String form"
        );

        // Heap.
        let legacy_post_heap =
            legacy_canonical_heap_strings(&state.post.heap, &post_reachable, &canonical.canon);
        assert_eq!(
            format_canon_heap_legacy(&canonical.state.post_heap),
            legacy_post_heap,
            "structural post_heap rendering must match legacy String form"
        );

        // Attrs (timestamps stripped on both sides, same as production canonical_attr).
        let legacy_post_attrs =
            legacy_canonical_attrs_strings(&state.post.attrs, &post_reachable, &canonical.canon);
        assert_eq!(
            format_canon_attrs_legacy(&canonical.state.post_attrs),
            legacy_post_attrs,
            "structural post_attrs rendering must match legacy String form"
        );

        // Pre side too — sanity, even though it is empty for this fixture.
        let legacy_pre_attrs =
            legacy_canonical_attrs_strings(&state.pre.attrs, &pre_reachable, &canonical.canon);
        assert_eq!(
            format_canon_attrs_legacy(&canonical.state.pre_attrs),
            legacy_pre_attrs,
            "structural pre_attrs rendering must match legacy String form"
        );

        // Dynamic types.
        let legacy_dyn = legacy_canonical_dynamic_types_strings(&state, &canonical.canon);
        assert_eq!(
            format_canon_dynamic_types_legacy(&canonical.state.dynamic_types),
            legacy_dyn,
            "structural dynamic_types rendering must match legacy String form"
        );
    }

    /// Sanity: the structural canonical formula compares equal under
    /// alpha-renaming of the underlying `AbstractValue` IDs, the same
    /// way `alpha_equivalent` does. This is the property the structural
    /// representation exists to make cheap.
    #[test]
    fn structural_canonical_formula_is_alpha_invariant() {
        AbstractValue::reset_counters();
        let mut state1 = make_state(0, false);
        let formal1 = state1
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .unwrap();
        let pointee1 = state1.read_heap(formal1, Access::Dereference);
        assert!(state1.and_equal_const(pointee1, 7).is_sat());

        AbstractValue::reset_counters();
        let mut state2 = make_state(5 /* burn IDs */, false);
        let formal2 = state2
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .unwrap();
        let pointee2 = state2.read_heap(formal2, Access::Dereference);
        assert!(state2.and_equal_const(pointee2, 7).is_sat());

        let f1 = canonicalize(&state1).state.formula;
        let f2 = canonicalize(&state2).state.formula;
        assert_eq!(
            f1, f2,
            "structural canonical formula must be invariant under AbstractValue renaming"
        );
    }

    /// Sanity: structurally different formulas must NOT compare equal
    /// under the structural canonical key.
    #[test]
    fn structural_canonical_formula_distinguishes_different_shapes() {
        AbstractValue::reset_counters();
        let mut state1 = make_state(0, false);
        let formal1 = state1
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .unwrap();
        let pointee1 = state1.read_heap(formal1, Access::Dereference);
        assert!(state1.and_equal_const(pointee1, 7).is_sat());

        AbstractValue::reset_counters();
        let mut state2 = make_state(0, false);
        let formal2 = state2
            .post
            .stack
            .iter()
            .next()
            .map(|(_var, addr)| *addr)
            .unwrap();
        let pointee2 = state2.read_heap(formal2, Access::Dereference);
        // Different constant ⇒ different canonical formula.
        assert!(state2.and_equal_const(pointee2, 99).is_sat());

        let f1 = canonicalize(&state1).state.formula;
        let f2 = canonicalize(&state2).state.formula;
        assert_ne!(
            f1, f2,
            "structurally different formulas must not collapse under the structural canonical key"
        );
    }

    /// Cross-check for the flat-slab `CanonTerm` representation
    /// introduced by `perf_fix_canon_term_box_representation`.
    ///
    /// Walks a deep, mixed `Term` tree (nested `Add`/`Mult`/`Neg`/`Sub`
    /// over `Var` and `Const` leaves) through `partial_term_key` and
    /// asserts:
    ///   - the rendered legacy `String` shape matches the recursive
    ///     definition byte-for-byte (locks `format_canon_term_nodes`
    ///     against the previous `Box<CanonTerm>` formatter);
    ///   - alpha-renaming of the underlying `AbstractValue` IDs
    ///     produces the same `CanonTerm` (structural alpha-invariance
    ///     for terms);
    ///   - structurally different terms produce different `CanonTerm`s
    ///     (no over-collapsing under the new representation);
    ///   - leaf terms (`Var`/`Const`) stay inline (no allocation,
    ///     `Tree` arm is reserved for composites).
    #[test]
    fn structural_canon_term_flat_slab_round_trips_legacy_string_form() {
        // Recursive legacy shape, copy of the previous
        // `format_canon_term` body, used here as the oracle.
        fn legacy_term_str(t: &Term, canon: &Canonicalizer) -> String {
            match t {
                Term::Var(v) => format_value_key(&canon.partial_value_key(*v)),
                Term::Const(v) => format!("const:{v}"),
                Term::Add(l, r) => format!(
                    "add({},{})",
                    legacy_term_str(l, canon),
                    legacy_term_str(r, canon)
                ),
                Term::Sub(l, r) => format!(
                    "sub({},{})",
                    legacy_term_str(l, canon),
                    legacy_term_str(r, canon)
                ),
                Term::Mult(l, r) => format!(
                    "mul({},{})",
                    legacy_term_str(l, canon),
                    legacy_term_str(r, canon)
                ),
                Term::Neg(t) => format!("neg({})", legacy_term_str(t, canon)),
                Term::Not(t) => format!("not({})", legacy_term_str(t, canon)),
                Term::IsZero(t) => format!("is_zero({})", legacy_term_str(t, canon)),
            }
        }

        AbstractValue::reset_counters();
        let v1 = AbstractValue::mk_fresh();
        let v2 = AbstractValue::mk_fresh();
        let v3 = AbstractValue::mk_fresh();
        // Build  Add(Mult(Neg(Var(v1)), Sub(Var(v2), Const(3))), IsZero(Not(Var(v3))))
        let term = Term::Add(
            Box::new(Term::Mult(
                Box::new(Term::Neg(Box::new(Term::Var(v1)))),
                Box::new(Term::Sub(Box::new(Term::Var(v2)), Box::new(Term::Const(3)))),
            )),
            Box::new(Term::IsZero(Box::new(Term::Not(Box::new(Term::Var(v3)))))),
        );

        // Canonicalise with a Canonicalizer that has mapped v1/v2/v3.
        let mut canon = Canonicalizer::default();
        for v in [v1, v2, v3] {
            canon.map_value(v);
        }

        let key = canon.partial_term_key(&term);
        // Composite must use the flat slab.
        assert!(
            matches!(key, CanonTerm::Tree(_)),
            "composite Term must canonicalise to CanonTerm::Tree, got {key:?}"
        );

        // Round-trip: the rebuilt String matches the legacy recursive
        // shape byte-for-byte.
        assert_eq!(
            format_canon_term(&key),
            legacy_term_str(&term, &canon),
            "flat-slab CanonTerm must format identically to the legacy recursive shape"
        );

        // Alpha-invariance: rebuilding the same shape with fresh
        // `AbstractValue` IDs (mapped through a fresh Canonicalizer in
        // the same order) yields an identical `CanonTerm`.
        AbstractValue::reset_counters();
        for _ in 0..7 {
            let _ = AbstractValue::mk_fresh(); // burn IDs
        }
        let w1 = AbstractValue::mk_fresh();
        let w2 = AbstractValue::mk_fresh();
        let w3 = AbstractValue::mk_fresh();
        let term2 = Term::Add(
            Box::new(Term::Mult(
                Box::new(Term::Neg(Box::new(Term::Var(w1)))),
                Box::new(Term::Sub(Box::new(Term::Var(w2)), Box::new(Term::Const(3)))),
            )),
            Box::new(Term::IsZero(Box::new(Term::Not(Box::new(Term::Var(w3)))))),
        );
        let mut canon2 = Canonicalizer::default();
        for v in [w1, w2, w3] {
            canon2.map_value(v);
        }
        assert_eq!(
            canon2.partial_term_key(&term2),
            key,
            "alpha-renamed term must produce identical CanonTerm under the flat-slab representation"
        );

        // Distinguishability: changing one leaf changes the CanonTerm.
        let term_diff = Term::Add(
            Box::new(Term::Mult(
                Box::new(Term::Neg(Box::new(Term::Var(w1)))),
                Box::new(Term::Sub(Box::new(Term::Var(w2)), Box::new(Term::Const(4)))), // 4 not 3
            )),
            Box::new(Term::IsZero(Box::new(Term::Not(Box::new(Term::Var(w3)))))),
        );
        assert_ne!(
            canon2.partial_term_key(&term_diff),
            key,
            "structurally distinct terms must canonicalise to distinct CanonTerms"
        );

        // Leaves remain inline (no allocation in the slab arm).
        assert!(matches!(
            canon.partial_term_key(&Term::Var(v1)),
            CanonTerm::Var(_)
        ));
        assert!(matches!(
            canon.partial_term_key(&Term::Const(42)),
            CanonTerm::Const(42)
        ));
    }
}
