// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Alpha-equivalence helpers for Pulse states.
//!
//! Mirrors the intent of OCaml's `PulseAbductiveDomain.leq`, which compares
//! states modulo abstract-value renaming rather than raw identifier equality.

use std::collections::BTreeMap;

use num_traits::Zero;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::Attribute;
use crate::base_attrs::BaseAddressAttributes;
use crate::base_memory::BaseMemory;
use crate::base_stack::BaseStack;
use crate::formula::atom::Atom;
use crate::formula::lin_arith::LinArith;
use crate::formula::term::Term;
use crate::formula::Operand;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum CanonValue {
    Unrestricted(u32),
    Restricted(u32),
}

/// Allocation-free sort key for `Canonicalizer::partial_value_label`.
///
/// Variant order intentionally matches the lexicographic ordering of the
/// `String` form returned by `partial_value_label` (`"r0"` < `"u0"` <
/// `"?r0"` < `"?u0"`) so that callers that previously sorted by the
/// `String` see the same iteration order without paying per-key heap
/// allocations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ValueSortKey {
    CanonRestricted(u32),
    CanonUnrestricted(u32),
    UnmappedRestricted(u64),
    UnmappedUnrestricted(u64),
}

/// Allocation-free sort key for `Canonicalizer::partial_access_label`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum AccessSortKey {
    Dereference,
    Field(String),
    Array { typ: String, index: ValueSortKey },
}

/// Allocation-free sort key for `Canonicalizer::partial_edge_label`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct EdgeSortKey {
    access: AccessSortKey,
    target: ValueSortKey,
}

impl std::fmt::Display for CanonValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unrestricted(i) => write!(f, "u{i}"),
            Self::Restricted(i) => write!(f, "r{i}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CanonicalState {
    pre_stack: Vec<String>,
    post_stack: Vec<String>,
    pre_heap: Vec<String>,
    post_heap: Vec<String>,
    pre_attrs: Vec<String>,
    post_attrs: Vec<String>,
    formula: Vec<String>,
    /// Cross-ref: OCaml `path_condition.type_constraints` participates in
    /// `PulseAbductiveDomain.leq`. We track dynamic-type bindings
    /// separately on `AbductiveDomain.dynamic_types`, but they affect
    /// downstream analysis (specialization, function-pointer
    /// resolution), so they must participate in `alpha_equivalent` for
    /// the fixpoint to converge.
    dynamic_types: Vec<String>,
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

    let mut out = String::new();
    append_debug_section(&mut out, "pre_stack", pre_stack);
    append_debug_section(&mut out, "post_stack", post_stack);
    append_debug_section(&mut out, "pre_heap", pre_heap);
    append_debug_section(&mut out, "post_heap", post_heap);
    append_debug_section(&mut out, "pre_attrs", pre_attrs);
    append_debug_section(&mut out, "post_attrs", post_attrs);
    append_debug_section(&mut out, "formula", formula);
    append_debug_section(&mut out, "dynamic_types", dynamic_types);
    out.pop();
    out
}

/// Compare two states modulo abstract-value renaming.
pub fn alpha_equivalent(lhs: &AbductiveDomain, rhs: &AbductiveDomain) -> bool {
    canonicalize(lhs).state == canonicalize(rhs).state
}

/// Compare two state values modulo the same alpha-renaming used by
/// [`alpha_equivalent`].
///
/// This is stricter than raw [`AbstractValue`] equality: the states must be
/// semantically equivalent, and the designated values must land on the same
/// canonical value within that equivalence.
pub fn alpha_equivalent_value(
    lhs: &AbductiveDomain,
    lhs_value: AbstractValue,
    rhs: &AbductiveDomain,
    rhs_value: AbstractValue,
) -> bool {
    let lhs = canonicalize(lhs);
    let rhs = canonicalize(rhs);
    lhs.state == rhs.state
        && matches!(
            (lhs.value_label(lhs_value), rhs.value_label(rhs_value)),
            (Some(lhs_label), Some(rhs_label)) if lhs_label == rhs_label
        )
}

fn canonicalize(state: &AbductiveDomain) -> CanonicalizedState {
    // Cross-ref: OCaml `PulseAbductiveDomain.leq` compares the full formula
    // plus the stack-reachable pre/post graph. It does not compare Rust-only
    // helper caches such as `must_be_valid`, and it ignores disconnected
    // retained heap/attr garbage.
    let pre_reachable = reachable_from_stack(&state.pre.stack, &state.pre.heap);
    let post_reachable = reachable_from_stack(&state.post.stack, &state.post.heap);
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

fn stable_hash_state(state: &CanonicalState) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    hash_section(&mut hash, &state.pre_stack);
    hash_section(&mut hash, &state.post_stack);
    hash_section(&mut hash, &state.pre_heap);
    hash_section(&mut hash, &state.post_heap);
    hash_section(&mut hash, &state.pre_attrs);
    hash_section(&mut hash, &state.post_attrs);
    hash_section(&mut hash, &state.formula);
    hash_section(&mut hash, &state.dynamic_types);
    hash
}

fn canonical_dynamic_types(state: &AbductiveDomain, canon: &Canonicalizer) -> Vec<String> {
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

fn hash_section(hash: &mut u64, lines: &[String]) {
    stable_hash_bytes(hash, &(lines.len() as u64).to_le_bytes());
    for line in lines {
        stable_hash_bytes(hash, line.as_bytes());
        stable_hash_bytes(hash, &[0xff]);
    }
}

fn stable_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
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
        if let Some(existing) = self.get(value) {
            return existing;
        }

        let canon = if value.is_restricted() {
            self.next_restricted += 1;
            CanonValue::Restricted(self.next_restricted)
        } else {
            self.next_unrestricted += 1;
            CanonValue::Unrestricted(self.next_unrestricted)
        };
        self.values.insert(value, canon);
        canon
    }

    fn seed_from_stack(&mut self, stack: &BaseStack) {
        let mut entries: Vec<_> = stack
            .iter()
            .map(|(var, addr)| (format!("{var}"), *addr))
            .collect();
        entries.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
        for (_, addr) in entries {
            self.map_value(addr);
        }
    }

    fn propagate_memory(&mut self, memory: &BaseMemory) {
        let mut entries: Vec<_> = memory.iter().map(|(src, edges)| (*src, edges)).collect();
        entries.sort_by_key(|(src, _)| self.partial_value_key(*src));
        for (src, edges) in entries {
            if self.get(src).is_none() {
                continue;
            }
            let mut edge_entries: Vec<_> = edges
                .iter()
                .map(|(access, target)| (access, *target))
                .collect();
            edge_entries.sort_by_key(|(access, target)| self.partial_edge_key(access, *target));
            for (access, target) in edge_entries {
                if let Access::ArrayAccess(_, index) = access {
                    self.map_value(*index);
                }
                self.map_value(target);
            }
        }
    }

    fn propagate_attrs(&mut self, attrs: &BaseAddressAttributes) {
        let mut entries: Vec<_> = attrs.iter().map(|(addr, attrs)| (*addr, attrs)).collect();
        entries.sort_by_key(|(addr, _)| self.partial_value_key(*addr));
        for (addr, attrs) in entries {
            if self.get(addr).is_none() {
                continue;
            }
            for attr in attrs.iter() {
                if let Attribute::ReturnedFromUnknown(values) = attr {
                    for value in values {
                        self.map_value(*value);
                    }
                }
            }
        }
    }

    fn propagate_formula(&mut self, state: &AbductiveDomain) {
        let phi = state.path_condition.phi();

        let mut equalities: Vec<_> = phi.var_eqs.iter_equalities().collect();
        equalities
            .sort_by_key(|(lhs, rhs)| (self.partial_value_key(*lhs), self.partial_value_key(*rhs)));
        for (lhs, rhs) in equalities {
            if self.get(lhs).is_some() || self.get(rhs).is_some() {
                self.map_value(lhs);
                self.map_value(rhs);
            }
        }

        let mut linear_eqs: Vec<_> = phi.linear_eqs.iter().collect();
        linear_eqs.sort_by_key(|(lhs, _)| self.partial_value_key(**lhs));
        for (lhs, lin) in linear_eqs {
            let vars: Vec<_> = std::iter::once(*lhs).chain(lin.get_variables()).collect();
            if vars.iter().any(|value| self.get(*value).is_some()) {
                for value in vars {
                    self.map_value(value);
                }
            }
        }

        let mut atoms: Vec<_> = phi.atoms.iter().collect();
        atoms.sort_by_key(|atom| self.partial_atom_label(atom));
        for atom in atoms {
            let vars = atom.all_vars();
            if vars.iter().any(|value| self.get(*value).is_some()) {
                for value in vars {
                    self.map_value(value);
                }
            }
        }

        let mut term_eqs: Vec<_> = phi.term_eqs.iter().collect();
        term_eqs.sort_by_key(|(lhs, term_eq)| self.partial_term_eq_label(**lhs, term_eq));
        for (lhs, term_eq) in term_eqs {
            let vars: Vec<_> = std::iter::once(*lhs)
                .chain(operand_values(&term_eq.lhs))
                .chain(operand_values(&term_eq.rhs))
                .collect();
            if vars.iter().any(|value| self.get(*value).is_some()) {
                for value in vars {
                    self.map_value(value);
                }
            }
        }

        let mut intervals: Vec<_> = phi.intervals.iter().collect();
        intervals.sort_by_key(|(value, _)| self.partial_value_key(**value));
        for (value, _) in intervals {
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

        let mut is_int_vars: Vec<_> = phi.is_int_vars.iter().copied().collect();
        is_int_vars.sort_by_key(|value| self.partial_value_key(*value));
        for value in is_int_vars {
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

        let mut fn_apps: Vec<_> = phi.iter_fn_app_eqs().collect();
        fn_apps.sort_by_key(|(_, ret)| self.partial_value_key(**ret));
        for (key, ret) in fn_apps {
            let vars: Vec<_> = key
                .actuals
                .iter()
                .filter_map(|actual| match actual {
                    crate::formula::phi::FnAppActual::Const(_) => None,
                    crate::formula::phi::FnAppActual::Var(value) => Some(*value),
                })
                .chain(std::iter::once(*ret))
                .collect();
            if vars.iter().any(|value| self.get(*value).is_some()) {
                for value in vars {
                    self.map_value(value);
                }
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
        let mut entries: Vec<_> = stack
            .iter()
            .map(|(var, addr)| (format!("{var}"), *addr))
            .collect();
        entries.sort_by(|lhs, rhs| lhs.0.cmp(&rhs.0));
        for (_, addr) in entries {
            self.map_value(addr);
        }
    }

    fn assign_remaining_memory(
        &mut self,
        memory: &BaseMemory,
        reachable: &std::collections::HashSet<AbstractValue>,
    ) {
        let mut entries: Vec<_> = memory.iter().map(|(src, edges)| (*src, edges)).collect();
        entries.sort_by_key(|(src, _)| self.partial_value_key(*src));
        for (src, edges) in entries {
            if !reachable.contains(&src) {
                continue;
            }
            self.map_value(src);
            let mut edge_entries: Vec<_> = edges
                .iter()
                .map(|(access, target)| (access, *target))
                .collect();
            edge_entries.sort_by_key(|(access, target)| self.partial_edge_key(access, *target));
            for (access, target) in edge_entries {
                if let Access::ArrayAccess(_, index) = access {
                    self.map_value(*index);
                }
                self.map_value(target);
            }
        }
    }

    fn assign_remaining_attrs(
        &mut self,
        attrs: &BaseAddressAttributes,
        reachable: &std::collections::HashSet<AbstractValue>,
    ) {
        let mut entries: Vec<_> = attrs.iter().map(|(addr, attrs)| (*addr, attrs)).collect();
        entries.sort_by_key(|(addr, _)| self.partial_value_key(*addr));
        for (addr, attrs) in entries {
            if !reachable.contains(&addr) {
                continue;
            }
            self.map_value(addr);
            for attr in attrs.iter() {
                if let Attribute::ReturnedFromUnknown(values) = attr {
                    for value in values {
                        self.map_value(*value);
                    }
                }
            }
        }
    }

    fn assign_remaining_formula(&mut self, state: &AbductiveDomain) {
        let phi = state.path_condition.phi();

        let mut equalities: Vec<_> = phi.var_eqs.iter_equalities().collect();
        equalities.sort_by_key(|(lhs, rhs)| {
            (
                self.partial_value_label(*lhs),
                self.partial_value_label(*rhs),
            )
        });
        for (lhs, rhs) in equalities {
            self.map_value(lhs);
            self.map_value(rhs);
        }

        let mut linear_eqs: Vec<_> = phi.linear_eqs.iter().collect();
        linear_eqs.sort_by_key(|(lhs, lin)| self.partial_linear_eq_label(**lhs, lin));
        for (lhs, lin) in linear_eqs {
            self.map_value(*lhs);
            for value in lin.get_variables() {
                self.map_value(value);
            }
        }

        let mut atoms: Vec<_> = phi.atoms.iter().collect();
        atoms.sort_by_key(|atom| self.partial_atom_label(atom));
        for atom in atoms {
            for value in atom.all_vars() {
                self.map_value(value);
            }
        }

        let mut term_eqs: Vec<_> = phi.term_eqs.iter().collect();
        term_eqs.sort_by_key(|(lhs, term_eq)| self.partial_term_eq_label(**lhs, term_eq));
        for (lhs, term_eq) in term_eqs {
            self.map_value(*lhs);
            for value in operand_values(&term_eq.lhs) {
                self.map_value(value);
            }
            for value in operand_values(&term_eq.rhs) {
                self.map_value(value);
            }
        }

        let mut intervals: Vec<_> = phi.intervals.iter().collect();
        intervals.sort_by_key(|(value, interval)| {
            (self.partial_value_label(**value), format!("{interval:?}"))
        });
        for (value, _) in intervals {
            self.map_value(*value);
        }

        let mut is_int_vars: Vec<_> = phi.is_int_vars.iter().copied().collect();
        is_int_vars.sort_by_key(|value| self.partial_value_key(*value));
        for value in is_int_vars {
            self.map_value(value);
        }

        let mut fn_apps: Vec<_> = phi.iter_fn_app_eqs().collect();
        fn_apps.sort_by_key(|(key, ret)| self.partial_fn_app_label(key, **ret));
        for (key, ret) in fn_apps {
            for actual in &key.actuals {
                if let crate::formula::phi::FnAppActual::Var(value) = actual {
                    self.map_value(*value);
                }
            }
            self.map_value(*ret);
        }
    }

    fn partial_value_label(&self, value: AbstractValue) -> String {
        self.get(value).map_or_else(
            || {
                let kind = if value.is_restricted() { 'r' } else { 'u' };
                format!("?{kind}{}", value.raw().unsigned_abs())
            },
            |canon| canon.to_string(),
        )
    }

    /// Cheap allocation-free sort key for `partial_value_label`.
    ///
    /// Profile shows `Canonicalizer::partial_value_label` as the single
    /// hottest function on the `whirlpool_block` slice (>20% of
    /// self-time), driven by the `sort_by_key` calls in `propagate_*`.
    /// Each call allocates a `String` purely for `Ord` comparison.
    /// `partial_value_key` returns a comparable tuple that is
    /// order-equivalent to the `String` form’s lexicographic order on
    /// `"u"`/`"r"`/`"?u"`/`"?r"` prefixes, without allocating.
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
            Access::FieldAccess(field) => AccessSortKey::Field(field.field_name.clone()),
            Access::ArrayAccess(typ, index) => AccessSortKey::Array {
                typ: format!("{typ}"),
                index: self.partial_value_key(*index),
            },
        }
    }

    fn partial_linear_eq_label(&self, lhs: AbstractValue, lin: &LinArith) -> String {
        format!(
            "{}={}",
            self.partial_value_label(lhs),
            self.partial_lin_arith_label(lin)
        )
    }

    fn partial_lin_arith_label(&self, lin: &LinArith) -> String {
        let mut vars: Vec<_> = lin
            .vars
            .iter()
            .map(|(value, coeff)| {
                format!("{}*{}", format_q(coeff), self.partial_value_label(*value))
            })
            .collect();
        vars.sort();
        if lin.constant.is_zero() {
            vars.join("+")
        } else if vars.is_empty() {
            format_q(&lin.constant)
        } else {
            format!("{}+{}", vars.join("+"), format_q(&lin.constant))
        }
    }

    fn partial_atom_label(&self, atom: &Atom) -> String {
        match atom {
            Atom::Equal(lhs, rhs) => {
                format!(
                    "eq:{}:{}",
                    self.partial_term_label(lhs),
                    self.partial_term_label(rhs)
                )
            }
            Atom::NotEqual(lhs, rhs) => {
                format!(
                    "neq:{}:{}",
                    self.partial_term_label(lhs),
                    self.partial_term_label(rhs)
                )
            }
            Atom::LessEqual(lhs, rhs) => {
                format!(
                    "le:{}:{}",
                    self.partial_term_label(lhs),
                    self.partial_term_label(rhs)
                )
            }
            Atom::LessThan(lhs, rhs) => {
                format!(
                    "lt:{}:{}",
                    self.partial_term_label(lhs),
                    self.partial_term_label(rhs)
                )
            }
        }
    }

    fn partial_term_label(&self, term: &Term) -> String {
        match term {
            Term::Var(value) => self.partial_value_label(*value),
            Term::Const(value) => format!("const:{value}"),
            Term::Add(lhs, rhs) => {
                format!(
                    "add({},{})",
                    self.partial_term_label(lhs),
                    self.partial_term_label(rhs)
                )
            }
            Term::Sub(lhs, rhs) => {
                format!(
                    "sub({},{})",
                    self.partial_term_label(lhs),
                    self.partial_term_label(rhs)
                )
            }
            Term::Mult(lhs, rhs) => {
                format!(
                    "mul({},{})",
                    self.partial_term_label(lhs),
                    self.partial_term_label(rhs)
                )
            }
            Term::Neg(inner) => format!("neg({})", self.partial_term_label(inner)),
            Term::Not(inner) => format!("not({})", self.partial_term_label(inner)),
            Term::IsZero(inner) => format!("is_zero({})", self.partial_term_label(inner)),
        }
    }

    fn partial_operand_label(&self, operand: &Operand) -> String {
        match operand {
            Operand::AbstractValue(value) => self.partial_value_label(*value),
            Operand::ConstOperand(value) => format!("const:{value}"),
        }
    }

    fn partial_term_eq_label(
        &self,
        lhs: AbstractValue,
        term_eq: &crate::formula::phi::TermEq,
    ) -> String {
        format!(
            "{}:{}:{}:{}",
            self.partial_value_label(lhs),
            term_eq.op,
            self.partial_operand_label(&term_eq.lhs),
            self.partial_operand_label(&term_eq.rhs)
        )
    }

    fn partial_fn_app_label(
        &self,
        key: &crate::formula::phi::FnAppKey,
        ret: AbstractValue,
    ) -> String {
        let actuals = key
            .actuals
            .iter()
            .map(|actual| match actual {
                crate::formula::phi::FnAppActual::Const(value) => format!("const:{value}"),
                crate::formula::phi::FnAppActual::Var(value) => self.partial_value_label(*value),
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{}({})->{}",
            key.callee,
            actuals,
            self.partial_value_label(ret)
        )
    }
}

fn canonical_stack(stack: &BaseStack, canon: &Canonicalizer) -> Vec<String> {
    let mut entries: Vec<_> = stack
        .iter()
        .map(|(var, addr)| format!("{var}={}", canon.get(*addr).unwrap()))
        .collect();
    entries.sort();
    entries
}

fn canonical_heap(
    memory: &BaseMemory,
    reachable: &std::collections::HashSet<AbstractValue>,
    canon: &Canonicalizer,
) -> Vec<String> {
    let mut edges = Vec::new();
    for (src, accesses) in memory.iter() {
        if !reachable.contains(src) {
            continue;
        }
        for (access, target) in accesses.iter() {
            edges.push(format!(
                "{}:{}->{}",
                canon.get(*src).unwrap(),
                canonical_access(access, canon),
                canon.get(*target).unwrap()
            ));
        }
    }
    edges.sort();
    edges
}

fn canonical_attrs(
    attrs: &BaseAddressAttributes,
    reachable: &std::collections::HashSet<AbstractValue>,
    canon: &Canonicalizer,
) -> Vec<String> {
    let mut entries = Vec::new();
    for (addr, attrs) in attrs.iter() {
        if !reachable.contains(addr) {
            continue;
        }
        for attr in attrs.iter() {
            entries.push(format!(
                "{}:{}",
                canon.get(*addr).unwrap(),
                canonical_attr(attr, canon)
            ));
        }
    }
    entries.sort();
    entries
}

fn canonical_formula(state: &AbductiveDomain, canon: &Canonicalizer) -> Vec<String> {
    let mut parts = Vec::new();
    let phi = state.path_condition.phi();

    let mut equalities: Vec<_> = phi.var_eqs.iter_equalities().collect();
    equalities.sort_by_key(|(lhs, rhs)| (canon.get(*lhs).unwrap(), canon.get(*rhs).unwrap()));
    for (lhs, rhs) in equalities {
        parts.push(format!(
            "uf:{}->{}",
            canon.get(lhs).unwrap(),
            canon.get(rhs).unwrap()
        ));
    }

    let mut linear_eqs: Vec<_> = phi.linear_eqs.iter().collect();
    linear_eqs.sort_by_key(|(lhs, _)| canon.get(**lhs).unwrap());
    for (lhs, lin) in linear_eqs {
        parts.push(format!(
            "lin:{}={}",
            canon.get(*lhs).unwrap(),
            canonical_lin_arith(lin, canon)
        ));
    }

    let mut atoms: Vec<_> = phi.atoms.iter().collect();
    atoms.sort_by_key(|atom| canonical_atom(atom, canon));
    for atom in atoms {
        parts.push(format!("atom:{}", canonical_atom(atom, canon)));
    }

    let mut term_eqs: Vec<_> = phi.term_eqs.iter().collect();
    term_eqs.sort_by_key(|(lhs, _)| canon.get(**lhs).unwrap());
    for (lhs, term_eq) in term_eqs {
        parts.push(format!(
            "term_eq:{}:{}:{}:{}",
            canon.get(*lhs).unwrap(),
            term_eq.op,
            canonical_operand(&term_eq.lhs, canon),
            canonical_operand(&term_eq.rhs, canon)
        ));
    }

    let mut intervals: Vec<_> = phi.intervals.iter().collect();
    intervals.sort_by_key(|(value, _)| canon.get(**value).unwrap());
    for (value, interval) in intervals {
        parts.push(format!(
            "interval:{}:{interval:?}",
            canon.get(*value).unwrap()
        ));
    }

    let mut is_int_vars: Vec<_> = phi.is_int_vars.iter().copied().collect();
    is_int_vars.sort_by_key(|value| canon.get(*value).unwrap());
    for value in is_int_vars {
        parts.push(format!("is_int:{}", canon.get(value).unwrap()));
    }

    let mut fn_apps: Vec<_> = phi.iter_fn_app_eqs().collect();
    fn_apps.sort_by_key(|(key, ret)| {
        let actuals = key
            .actuals
            .iter()
            .map(|actual| canonical_fn_app_actual(actual, canon))
            .collect::<Vec<_>>()
            .join(",");
        format!("{}({})->{}", key.callee, actuals, canon.get(**ret).unwrap())
    });
    for (key, ret) in fn_apps {
        let actuals = key
            .actuals
            .iter()
            .map(|actual| canonical_fn_app_actual(actual, canon))
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

fn canonical_access(access: &Access, canon: &Canonicalizer) -> String {
    match access {
        Access::Dereference => "deref".to_string(),
        Access::FieldAccess(field) => format!("field:{field}"),
        Access::ArrayAccess(typ, index) => {
            format!("array:{typ}:{}", canon.get(*index).unwrap())
        }
    }
}

fn canonical_attr(attr: &Attribute, canon: &Canonicalizer) -> String {
    match attr {
        Attribute::ReturnedFromUnknown(values) => {
            let values = values
                .iter()
                .map(|value| canon.get(*value).unwrap().to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("ReturnedFromUnknown({values})")
        }
        // Cross-ref: OCaml `PulseAttribute` carries `Timestamp.t` on these
        // attributes for trace ordering, but the timestamp itself is not
        // semantically meaningful for fixpoint convergence. Two iterations
        // of the same procedure analysis can assign different timestamps
        // to the same logical attribute (because `next_attr_timestamp` is
        // bumped by intervening work), so including the timestamp here
        // breaks `state_cmp::alpha_equivalent` and forces the worklist to
        // re-visit nodes long after the analysis is semantically converged.
        // Drop the timestamp from the canonical key while keeping the
        // location and reason fields.
        Attribute::MustBeValid(_ts, loc, reason) => {
            format!("MustBeValid({loc}, {reason:?})")
        }
        Attribute::MustBeInitialized(_ts, loc) => {
            format!("MustBeInitialized({loc})")
        }
        Attribute::WrittenTo(_ts, loc) => format!("WrittenTo({loc})"),
        _ => format!("{attr:?}"),
    }
}

fn canonical_lin_arith(lin: &LinArith, canon: &Canonicalizer) -> String {
    let mut vars: Vec<_> = lin
        .vars
        .iter()
        .map(|(value, coeff)| format!("{}*{}", format_q(coeff), canon.get(*value).unwrap()))
        .collect();
    vars.sort();
    if lin.constant.is_zero() {
        vars.join("+")
    } else if vars.is_empty() {
        format_q(&lin.constant)
    } else {
        format!("{}+{}", vars.join("+"), format_q(&lin.constant))
    }
}

fn canonical_atom(atom: &Atom, canon: &Canonicalizer) -> String {
    match atom {
        Atom::Equal(lhs, rhs) => {
            format!(
                "eq:{}:{}",
                canonical_term(lhs, canon),
                canonical_term(rhs, canon)
            )
        }
        Atom::NotEqual(lhs, rhs) => {
            format!(
                "neq:{}:{}",
                canonical_term(lhs, canon),
                canonical_term(rhs, canon)
            )
        }
        Atom::LessEqual(lhs, rhs) => {
            format!(
                "le:{}:{}",
                canonical_term(lhs, canon),
                canonical_term(rhs, canon)
            )
        }
        Atom::LessThan(lhs, rhs) => {
            format!(
                "lt:{}:{}",
                canonical_term(lhs, canon),
                canonical_term(rhs, canon)
            )
        }
    }
}

fn canonical_term(term: &Term, canon: &Canonicalizer) -> String {
    match term {
        Term::Var(value) => canon.get(*value).unwrap().to_string(),
        Term::Const(value) => format!("const:{value}"),
        Term::Add(lhs, rhs) => format!(
            "add({},{})",
            canonical_term(lhs, canon),
            canonical_term(rhs, canon)
        ),
        Term::Sub(lhs, rhs) => format!(
            "sub({},{})",
            canonical_term(lhs, canon),
            canonical_term(rhs, canon)
        ),
        Term::Mult(lhs, rhs) => format!(
            "mul({},{})",
            canonical_term(lhs, canon),
            canonical_term(rhs, canon)
        ),
        Term::Neg(inner) => format!("neg({})", canonical_term(inner, canon)),
        Term::Not(inner) => format!("not({})", canonical_term(inner, canon)),
        Term::IsZero(inner) => format!("is_zero({})", canonical_term(inner, canon)),
    }
}

fn canonical_operand(operand: &Operand, canon: &Canonicalizer) -> String {
    match operand {
        Operand::AbstractValue(value) => canon.get(*value).unwrap().to_string(),
        Operand::ConstOperand(value) => format!("const:{value}"),
    }
}

fn canonical_fn_app_actual(
    actual: &crate::formula::phi::FnAppActual,
    canon: &Canonicalizer,
) -> String {
    match actual {
        crate::formula::phi::FnAppActual::Const(value) => format!("const:{value}"),
        crate::formula::phi::FnAppActual::Var(value) => canon.get(*value).unwrap().to_string(),
    }
}

fn operand_values(operand: &Operand) -> Vec<AbstractValue> {
    match operand {
        Operand::AbstractValue(value) => vec![*value],
        Operand::ConstOperand(_) => Vec::new(),
    }
}

fn format_q(q: &crate::formula::lin_arith::Q) -> String {
    if q.denom() == &1 {
        q.numer().to_string()
    } else {
        format!("{}/{}", q.numer(), q.denom())
    }
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
}
