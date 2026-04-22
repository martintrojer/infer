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
}

impl std::fmt::Display for DebugSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "hash={:016x} pre[s={} h={} a={}] post[s={} h={} a={}] formula={}",
            self.hash,
            self.pre_stack,
            self.pre_heap,
            self.pre_attrs,
            self.post_stack,
            self.post_heap,
            self.post_attrs,
            self.formula,
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
    }
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
    hash
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
        entries.sort_by_key(|(src, _)| self.partial_value_label(*src));
        for (src, edges) in entries {
            if self.get(src).is_none() {
                continue;
            }
            let mut edge_entries: Vec<_> = edges
                .iter()
                .map(|(access, target)| (access, *target))
                .collect();
            edge_entries.sort_by_key(|(access, target)| self.partial_edge_label(access, *target));
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
        entries.sort_by_key(|(addr, _)| self.partial_value_label(*addr));
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
        equalities.sort_by_key(|(lhs, rhs)| {
            (
                self.partial_value_label(*lhs),
                self.partial_value_label(*rhs),
            )
        });
        for (lhs, rhs) in equalities {
            if self.get(lhs).is_some() || self.get(rhs).is_some() {
                self.map_value(lhs);
                self.map_value(rhs);
            }
        }

        let mut linear_eqs: Vec<_> = phi.linear_eqs.iter().collect();
        linear_eqs.sort_by_key(|(lhs, lin)| self.partial_linear_eq_label(**lhs, lin));
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
        intervals.sort_by_key(|(value, interval)| {
            (self.partial_value_label(**value), format!("{interval:?}"))
        });
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
        is_int_vars.sort_by_key(|value| self.partial_value_label(*value));
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
        fn_apps.sort_by_key(|(key, ret)| self.partial_fn_app_label(key, **ret));
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
        entries.sort_by_key(|(src, _)| self.partial_value_label(*src));
        for (src, edges) in entries {
            if !reachable.contains(&src) {
                continue;
            }
            self.map_value(src);
            let mut edge_entries: Vec<_> = edges
                .iter()
                .map(|(access, target)| (access, *target))
                .collect();
            edge_entries.sort_by_key(|(access, target)| self.partial_edge_label(access, *target));
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
        entries.sort_by_key(|(addr, _)| self.partial_value_label(*addr));
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
        is_int_vars.sort_by_key(|value| self.partial_value_label(*value));
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

    fn partial_edge_label(&self, access: &Access, target: AbstractValue) -> String {
        format!(
            "{}->{}",
            self.partial_access_label(access),
            self.partial_value_label(target)
        )
    }

    fn partial_access_label(&self, access: &Access) -> String {
        match access {
            Access::Dereference => "deref".to_string(),
            Access::FieldAccess(field) => format!("field:{field}"),
            Access::ArrayAccess(typ, index) => {
                format!("array:{typ}:{}", self.partial_value_label(*index))
            }
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
    use sil::typ::Typ;
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
}
