// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Core formula state: the constraint solver.
//!
//! Mirrors OCaml's `PulseFormulaPhi.ml` (simplified).
//!
//! Combines:
//! - Union-find for equality classes (`var_eqs`)
//! - Linear equations: `x = c + a₁·y₁ + a₂·y₂ + ...` (`linear_eqs`)
//! - Constant equalities: `x = c` (derived from linear_eqs)
//! - Atom constraints: disequalities, inequalities (`atoms`)
//!
//! When a new equality `x = y` is learned, it propagates through linear_eqs
//! and atoms to maintain consistency.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use num_traits::Zero;

use crate::abstract_value::AbstractValue;
use crate::sat_unsat::SatUnsat;

use super::atom::Atom;
use super::lin_arith::{LinArith, Q};
use super::term::Term;
use super::var_uf::VarUF;

/// A newly discovered equality from constraint propagation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewEq {
    /// A variable is now known to equal zero.
    EqZero(AbstractValue),
    /// Two variables are now known to be equal.
    Equal(AbstractValue, AbstractValue),
}

/// The core formula state.
///
/// Mirrors OCaml's `FormulaPhi.t` fields:
/// - `var_eqs`: union-find
/// - `linear_eqs`: map from vars to linear expressions
/// - `atoms`: disequality/inequality constraints
///
/// Cross-ref: OCaml's `PulseFormulaPhi.ml` type `phi`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Phi {
    /// Union-find for variable equality classes.
    pub var_eqs: VarUF,
    /// Linear equations: `x = linear_expr` where x is the simplest variable.
    /// Invariant: domain and range mention distinct variables.
    pub linear_eqs: BTreeMap<AbstractValue, LinArith>,
    /// Atom constraints (disequalities, inequalities).
    pub atoms: BTreeSet<Atom>,
    /// Term equalities: `v = binop(x, y)`. Used by prune to resolve
    /// boolean variables back to their defining comparison, enabling
    /// path condition propagation (e.g., `r = (x < 0); prune(r)` → `x < 0`).
    /// Mirrors OCaml's `term_eqs` in `Formula.t`.
    pub term_eqs: BTreeMap<AbstractValue, TermEq>,
    /// Concrete integer intervals: `v ∈ [l, u]` or `v ∉ [l, u]`.
    /// Cross-ref: OCaml's `PulseFormulaPhi.ml` `intervals` field,
    /// `PulseCItv.ml` for the interval type.
    pub intervals: BTreeMap<AbstractValue, super::citv::CItv>,
    /// Variables known to be integers (from integer-typed loads).
    /// When a variable in this set gets a non-integer rational solution
    /// from the linear solver, the path is Unsat.
    /// Cross-ref: OCaml uses `IsInt` atoms in `PulseFormulaTerm.ml`.
    pub is_int_vars: BTreeSet<AbstractValue>,
    /// Function application equalities: maps (callee, actuals) → return_var.
    /// When the same pure function is called twice with the same args, the
    /// second call's return var is unified with the first's. This enables
    /// pruning `f()==f()` comparisons.
    /// Cross-ref: OCaml PulseFormulaPhi.ml term_eqs (Term→Var direction),
    /// PulseCallOperations.ml L220-235 (FunctionApplicationOperand).
    fn_app_eqs: BTreeMap<FnAppKey, AbstractValue>,
}

/// Key for function application deduplication.
/// Two calls with the same callee and same actual values
/// should return the same result (for pure functions).
/// Actuals are canonicalized: known constants use their value,
/// unknown values use their canonical abstract value representative.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FnAppKey {
    pub callee: String,
    pub actuals: Vec<FnAppActual>,
}

/// A canonicalized actual argument for function application keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FnAppActual {
    /// Known constant value (e.g., 10, 0).
    Const(i64),
    /// Unknown value identified by its canonical representative.
    Var(AbstractValue),
}

/// A term equality: records that a variable equals a binary operation on two operands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermEq {
    pub op: sil::binop::Binop,
    pub lhs: super::Operand,
    pub rhs: super::Operand,
}

impl Phi {
    pub fn ttrue() -> Self {
        Self::default()
    }

    /// Get the canonical representative of a variable.
    pub fn get_repr(&self, v: AbstractValue) -> AbstractValue {
        self.var_eqs.find_immut(v)
    }

    /// Check if a variable is known to equal zero.
    pub fn is_known_zero(&self, v: AbstractValue) -> bool {
        let repr = self.get_repr(v);
        if let Some(lin) = self.linear_eqs.get(&repr) {
            if let Some(q) = lin.get_as_const() {
                return q.is_zero();
            }
        }
        false
    }

    /// Get the known constant value of a variable, if any.
    pub fn get_known_const(&self, v: AbstractValue) -> Option<Q> {
        let repr = self.get_repr(v);
        self.linear_eqs.get(&repr).and_then(|l| l.get_as_const())
    }

    /// Record that two variables are equal: v1 = v2.
    pub fn and_var_equal(&mut self, v1: AbstractValue, v2: AbstractValue) -> SatUnsat<Vec<NewEq>> {
        // Merge equality classes
        let Some((merged, kept)) = self.var_eqs.union(v1, v2) else {
            return SatUnsat::Sat(Vec::new()); // already equal
        };

        let mut new_eqs = vec![NewEq::Equal(merged, kept)];

        // Propagate through linear_eqs: substitute merged → kept
        self.propagate_equality(merged, kept, &mut new_eqs)
    }

    /// Record that a variable equals a linear expression: v = lin.
    pub fn and_linear_eq(&mut self, v: AbstractValue, lin: LinArith) -> SatUnsat<Vec<NewEq>> {
        let repr = self.get_repr(v);
        let lin = self.normalize_linear(&lin);

        // Solve repr = lin → repr - lin = 0
        let diff = LinArith::of_var(repr).sub(&lin);
        match diff.solve_eq_zero() {
            SatUnsat::Unsat => SatUnsat::Unsat,
            SatUnsat::Sat(None) => SatUnsat::Sat(Vec::new()), // trivially true
            SatUnsat::Sat(Some((x, solution))) => {
                // x = solution
                self.add_linear_eq(x, solution)
            }
        }
    }

    /// Record that a variable equals a constant.
    pub fn and_const_eq(&mut self, v: AbstractValue, c: i64) -> SatUnsat<Vec<NewEq>> {
        // Also record in the interval domain.
        // Cross-ref: OCaml PulseFormula.ml incorporates_new_eqs adds
        // CItv.equal_to for EqZero, and add_interval_ for constants.
        let repr = self.get_repr(v);
        if self
            .add_interval(repr, super::citv::CItv::equal_to(c))
            .is_unsat()
        {
            return SatUnsat::Unsat;
        }
        self.and_linear_eq(v, LinArith::of_int(c))
    }

    /// Add or intersect an interval for a variable.
    ///
    /// Returns Unsat if the intersection with existing interval is empty.
    /// Cross-ref: OCaml `PulseFormulaPhi.add_interval_`.
    pub fn add_interval(&mut self, v: AbstractValue, citv: super::citv::CItv) -> SatUnsat<()> {
        let repr = self.get_repr(v);
        let refined = if let Some(existing) = self.intervals.get(&repr) {
            match existing.intersection(&citv) {
                None => return SatUnsat::Unsat,
                Some(better) => better,
            }
        } else {
            citv
        };
        self.intervals.insert(repr, refined.clone());

        if let super::citv::CItv::Between(
            super::citv::Bound::Int(lower),
            super::citv::Bound::Int(upper),
        ) = refined
        {
            if lower == upper
                && self.get_known_const(repr) != Some(Q::from_integer(lower))
                && self.and_linear_eq(repr, LinArith::of_int(lower)).is_unsat()
            {
                return SatUnsat::Unsat;
            }
        }
        SatUnsat::Sat(())
    }

    /// Get the interval for a variable, if any.
    pub fn get_interval(&self, v: AbstractValue) -> Option<&super::citv::CItv> {
        let repr = self.get_repr(v);
        self.intervals.get(&repr)
    }

    /// Mark a variable as integer-typed.
    ///
    /// When the linear solver later derives a non-integer rational
    /// constant for this variable, the path is Unsat.
    /// Cross-ref: OCaml `PulseFormula.ml and_is_int` + `PulseFormulaTerm.ml IsInt`.
    pub fn mark_is_int(&mut self, v: AbstractValue) {
        let repr = self.get_repr(v);
        self.is_int_vars.insert(repr);
    }

    /// Record a function application: ret_val = f(actuals).
    ///
    /// If the same function with the same actuals was already recorded,
    /// unify ret_val with the previous return var. This ensures `f()==f()`
    /// comparisons resolve to `v==v` which is always true.
    ///
    /// Cross-ref: OCaml PulseCallOperations.ml L220-235,
    /// PulseFormulaPhi.ml L700-722 add_term_eq collision detection.
    pub fn and_fn_app(
        &mut self,
        ret_val: AbstractValue,
        callee: &str,
        actuals: &[AbstractValue],
    ) -> SatUnsat<Vec<NewEq>> {
        let key = FnAppKey {
            callee: callee.to_string(),
            actuals: actuals
                .iter()
                .map(|a| {
                    let repr = self.get_repr(*a);
                    // Use constant value when known for better dedup
                    if let Some(q) = self.get_known_const(repr) {
                        if q.is_integer() {
                            return FnAppActual::Const(*q.numer() / *q.denom());
                        }
                    }
                    FnAppActual::Var(repr)
                })
                .collect(),
        };
        let ret_repr = self.get_repr(ret_val);
        if let Some(&existing) = self.fn_app_eqs.get(&key) {
            // Same function, same args → unify return values
            log::debug!(
                "  [fn_app] collision: {callee}({:?}) → unify {ret_repr} = {existing}",
                key.actuals
            );
            self.and_var_equal(ret_repr, existing)
        } else {
            self.fn_app_eqs.insert(key, ret_repr);
            SatUnsat::Sat(Vec::new())
        }
    }

    /// Check if a variable is known to be integer-typed.
    pub fn is_marked_int(&self, v: AbstractValue) -> bool {
        let repr = self.get_repr(v);
        self.is_int_vars.contains(&repr)
    }

    /// Iterate over remembered pure-function applications.
    pub fn iter_fn_app_eqs(&self) -> impl Iterator<Item = (&FnAppKey, &AbstractValue)> {
        self.fn_app_eqs.iter()
    }

    /// Record a disequality or inequality atom.
    pub fn and_atom(&mut self, atom: Atom) -> SatUnsat<Vec<NewEq>> {
        let resolved = self.resolve_atom(&atom);
        if let Some(trivial) = resolved.is_trivially_true() {
            if trivial {
                return SatUnsat::Sat(Vec::new());
            } else {
                return SatUnsat::Unsat;
            }
        }
        // Check if the negation of the new atom is already in the set
        let negated = resolved.negate();
        if self.atoms.contains(&negated) {
            return SatUnsat::Unsat;
        }
        // Check implied contradictions beyond direct negation:
        // - LessThan(a,b) ∧ LessThan(b,a) → Unsat (a<b ∧ b<a impossible)
        // - LessThan(a,b) ∧ Equal(a,b) → Unsat (a<b ∧ a=b impossible)
        // - Equal(a,b) ∧ LessThan(a,b) → Unsat (a=b ∧ a<b impossible)
        // Cross-ref: OCaml PulseFormula checks these via interval refinement.
        match &resolved {
            Atom::LessThan(a, b) => {
                if self.atoms.contains(&Atom::LessThan(b.clone(), a.clone()))
                    || self.atoms.contains(&Atom::Equal(a.clone(), b.clone()))
                    || self.atoms.contains(&Atom::Equal(b.clone(), a.clone()))
                {
                    return SatUnsat::Unsat;
                }
            }
            Atom::Equal(a, b) => {
                if self.atoms.contains(&Atom::LessThan(a.clone(), b.clone()))
                    || self.atoms.contains(&Atom::LessThan(b.clone(), a.clone()))
                {
                    return SatUnsat::Unsat;
                }
            }
            _ => {}
        }
        self.atoms.insert(resolved);
        SatUnsat::Sat(Vec::new())
    }

    /// Normalize an atom against the current phi without recording it.
    ///
    /// Cross-ref: OCaml `PulseFormula.ml` `prune_atom` normalizes a translated
    /// condition against the current caller-side phi before remembering it.
    pub(crate) fn normalize_condition_atom(&self, atom: &Atom) -> Atom {
        self.resolve_condition_atom(atom)
    }

    /// Summary-time condition simplification.
    ///
    /// Cross-ref: OCaml `PulseFormula.QuantifierElimination.subst_var_atoms_for_conditions`.
    /// Keep caller-visible precondition variables as-written, but substitute
    /// other variables through the current phi so exported summaries do not
    /// remember dead alias/equality bookkeeping.
    pub(crate) fn simplify_condition_atom_for_summary(
        &self,
        atom: &Atom,
        precondition_vocabulary: &HashSet<AbstractValue>,
        keep: &HashSet<AbstractValue>,
    ) -> Atom {
        fn is_preferred_visible_var(lhs: AbstractValue, rhs: AbstractValue) -> bool {
            if lhs.is_unrestricted() && rhs.is_restricted() {
                true
            } else if lhs.is_restricted() && rhs.is_unrestricted() {
                false
            } else {
                lhs.raw().unsigned_abs() < rhs.raw().unsigned_abs()
            }
        }

        fn visible_summary_var(
            phi: &Phi,
            v: AbstractValue,
            _precondition_vocabulary: &HashSet<AbstractValue>,
            keep: &HashSet<AbstractValue>,
        ) -> Option<AbstractValue> {
            if keep.contains(&v) {
                return Some(v);
            }

            let repr = phi.get_repr(v);
            keep.iter()
                .copied()
                .filter(|candidate| phi.get_repr(*candidate) == repr)
                .min_by(|lhs, rhs| {
                    if is_preferred_visible_var(*lhs, *rhs) {
                        std::cmp::Ordering::Less
                    } else if is_preferred_visible_var(*rhs, *lhs) {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Equal
                    }
                })
        }

        fn coeff_term(coeff: &Q, term: Term) -> Term {
            if *coeff == Q::from_integer(1) {
                term
            } else if *coeff == Q::from_integer(-1) {
                Term::Neg(Box::new(term))
            } else {
                Term::Mult(
                    Box::new(Term::Const(*coeff.numer() / *coeff.denom())),
                    Box::new(term),
                )
            }
        }

        fn add_term(lhs: Option<Term>, rhs: Term) -> Term {
            match lhs {
                Some(lhs) => match (lhs.as_const(), rhs.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x + y),
                    _ => Term::Add(Box::new(lhs), Box::new(rhs)),
                },
                None => rhs,
            }
        }

        fn simplify_visible_linear_term(
            phi: &Phi,
            v: AbstractValue,
            precondition_vocabulary: &HashSet<AbstractValue>,
            keep: &HashSet<AbstractValue>,
            visited: &mut HashSet<AbstractValue>,
        ) -> Option<Term> {
            if let Some(visible_var) = visible_summary_var(phi, v, precondition_vocabulary, keep) {
                return Some(Term::Var(visible_var));
            }

            let repr = phi.get_repr(v);
            if !visited.insert(repr) {
                return None;
            }

            let simplified = phi.linear_eqs.get(&repr).and_then(|lin| {
                let mut result = (!lin.constant.is_zero())
                    .then_some(Term::Const(*lin.constant.numer() / *lin.constant.denom()));
                for (&dep, coeff) in &lin.vars {
                    let dep_term = simplify_visible_linear_term(
                        phi,
                        dep,
                        precondition_vocabulary,
                        keep,
                        visited,
                    )?;
                    result = Some(add_term(result, coeff_term(coeff, dep_term)));
                }
                Some(result.unwrap_or(Term::Const(0)))
            });

            visited.remove(&repr);
            simplified
        }

        fn simplify_term(
            phi: &Phi,
            term: &Term,
            precondition_vocabulary: &HashSet<AbstractValue>,
            keep: &HashSet<AbstractValue>,
        ) -> Term {
            match term {
                Term::Var(v) => {
                    if let Some(visible_var) =
                        visible_summary_var(phi, *v, precondition_vocabulary, keep)
                    {
                        Term::Var(visible_var)
                    } else if let Some(term) = simplify_visible_linear_term(
                        phi,
                        *v,
                        precondition_vocabulary,
                        keep,
                        &mut HashSet::new(),
                    ) {
                        term
                    } else {
                        phi.resolve_term(term)
                    }
                }
                Term::Const(_) => term.clone(),
                Term::Add(a, b) => {
                    let a = simplify_term(phi, a, precondition_vocabulary, keep);
                    let b = simplify_term(phi, b, precondition_vocabulary, keep);
                    match (a.as_const(), b.as_const()) {
                        (Some(x), Some(y)) => Term::Const(x + y),
                        _ => Term::Add(Box::new(a), Box::new(b)),
                    }
                }
                Term::Sub(a, b) => {
                    let a = simplify_term(phi, a, precondition_vocabulary, keep);
                    let b = simplify_term(phi, b, precondition_vocabulary, keep);
                    match (a.as_const(), b.as_const()) {
                        (Some(x), Some(y)) => Term::Const(x - y),
                        _ => Term::Sub(Box::new(a), Box::new(b)),
                    }
                }
                Term::Mult(a, b) => {
                    let a = simplify_term(phi, a, precondition_vocabulary, keep);
                    let b = simplify_term(phi, b, precondition_vocabulary, keep);
                    match (a.as_const(), b.as_const()) {
                        (Some(x), Some(y)) => Term::Const(x * y),
                        _ => Term::Mult(Box::new(a), Box::new(b)),
                    }
                }
                Term::Neg(a) => {
                    let a = simplify_term(phi, a, precondition_vocabulary, keep);
                    if let Some(x) = a.as_const() {
                        Term::Const(-x)
                    } else {
                        Term::Neg(Box::new(a))
                    }
                }
                Term::Not(a) => {
                    let a = simplify_term(phi, a, precondition_vocabulary, keep);
                    if let Some(x) = a.as_const() {
                        Term::Const(if x == 0 { 1 } else { 0 })
                    } else {
                        Term::Not(Box::new(a))
                    }
                }
                Term::IsZero(a) => {
                    let a = simplify_term(phi, a, precondition_vocabulary, keep);
                    if let Some(x) = a.as_const() {
                        Term::Const(if x == 0 { 1 } else { 0 })
                    } else {
                        Term::IsZero(Box::new(a))
                    }
                }
            }
        }

        match atom {
            Atom::Equal(a, b) => Atom::Equal(
                simplify_term(self, a, precondition_vocabulary, keep),
                simplify_term(self, b, precondition_vocabulary, keep),
            ),
            Atom::NotEqual(a, b) => Atom::NotEqual(
                simplify_term(self, a, precondition_vocabulary, keep),
                simplify_term(self, b, precondition_vocabulary, keep),
            ),
            Atom::LessEqual(a, b) => Atom::LessEqual(
                simplify_term(self, a, precondition_vocabulary, keep),
                simplify_term(self, b, precondition_vocabulary, keep),
            ),
            Atom::LessThan(a, b) => Atom::LessThan(
                simplify_term(self, a, precondition_vocabulary, keep),
                simplify_term(self, b, precondition_vocabulary, keep),
            ),
        }
    }

    /// Simplify: remove constraints mentioning unreachable variables.
    pub fn simplify(&mut self, reachable: &HashSet<AbstractValue>) {
        let var_eqs = &self.var_eqs;
        let is_reachable = |v: AbstractValue| reachable.contains(&var_eqs.find_immut(v));
        let operand_is_reachable = |operand: &super::Operand| match operand {
            super::Operand::AbstractValue(v) => is_reachable(*v),
            super::Operand::ConstOperand(_) => true,
        };

        self.atoms.retain(|atom| {
            let vars = atom.all_vars();
            vars.iter().all(|v| is_reachable(*v))
        });
        self.linear_eqs
            .retain(|v, lin| is_reachable(*v) && lin.get_variables().all(is_reachable));
        self.term_eqs.retain(|v, term_eq| {
            is_reachable(*v)
                && operand_is_reachable(&term_eq.lhs)
                && operand_is_reachable(&term_eq.rhs)
        });
        self.intervals.retain(|v, _| is_reachable(*v));
        self.is_int_vars.retain(|v| is_reachable(*v));
        self.fn_app_eqs.retain(|key, ret| {
            is_reachable(*ret)
                && key.actuals.iter().all(|actual| match actual {
                    FnAppActual::Const(_) => true,
                    FnAppActual::Var(v) => is_reachable(*v),
                })
        });
    }

    /// Forget pure constraints mentioning the given canonical values while
    /// preserving the remaining heap-facing equality classes.
    pub fn forget_constraints_involving(&mut self, ignored: &HashSet<AbstractValue>) {
        if ignored.is_empty() {
            return;
        }

        let touches_ignored = |v: AbstractValue| ignored.contains(&self.var_eqs.find_immut(v));
        let operand_touches_ignored = |operand: &super::Operand| match operand {
            super::Operand::AbstractValue(v) => touches_ignored(*v),
            super::Operand::ConstOperand(_) => false,
        };

        self.atoms
            .retain(|atom| atom.all_vars().into_iter().all(|v| !touches_ignored(v)));
        self.linear_eqs.retain(|v, lin| {
            !touches_ignored(*v) && lin.get_variables().all(|var| !touches_ignored(var))
        });
        self.term_eqs.retain(|v, term_eq| {
            !touches_ignored(*v)
                && !operand_touches_ignored(&term_eq.lhs)
                && !operand_touches_ignored(&term_eq.rhs)
        });
        self.intervals.retain(|v, _interval| !touches_ignored(*v));
        self.is_int_vars.retain(|v| !touches_ignored(*v));
        self.fn_app_eqs.retain(|key, ret| {
            !touches_ignored(*ret)
                && key.actuals.iter().all(|actual| match actual {
                    FnAppActual::Const(_) => true,
                    FnAppActual::Var(v) => !touches_ignored(*v),
                })
        });
    }

    /// Forget pure constraints involving the given canonical values while
    /// preserving integer-type facts on those values.
    ///
    /// Cross-ref: OCaml direct-formal latent summaries such as
    /// `may_double_free_if_alias` still retain `IsInt` facts for later
    /// restored caller-visible values even after pruning later-formal success
    /// guards from the path condition.
    pub fn forget_non_type_constraints_involving(&mut self, ignored: &HashSet<AbstractValue>) {
        if ignored.is_empty() {
            return;
        }

        let touches_ignored = |v: AbstractValue| ignored.contains(&self.var_eqs.find_immut(v));
        let operand_touches_ignored = |operand: &super::Operand| match operand {
            super::Operand::AbstractValue(v) => touches_ignored(*v),
            super::Operand::ConstOperand(_) => false,
        };

        self.atoms
            .retain(|atom| atom.all_vars().into_iter().all(|v| !touches_ignored(v)));
        self.linear_eqs.retain(|v, lin| {
            !touches_ignored(*v) && lin.get_variables().all(|var| !touches_ignored(var))
        });
        self.term_eqs.retain(|v, term_eq| {
            !touches_ignored(*v)
                && !operand_touches_ignored(&term_eq.lhs)
                && !operand_touches_ignored(&term_eq.rhs)
        });
        self.intervals.retain(|v, _interval| !touches_ignored(*v));
        self.fn_app_eqs.retain(|key, ret| {
            !touches_ignored(*ret)
                && key.actuals.iter().all(|actual| match actual {
                    FnAppActual::Const(_) => true,
                    FnAppActual::Var(v) => !touches_ignored(*v),
                })
        });
    }

    // --- Internal helpers ---

    /// Add a linear equation `x = solution` to the solver.
    fn add_linear_eq(&mut self, x: AbstractValue, solution: LinArith) -> SatUnsat<Vec<NewEq>> {
        let mut new_eqs = Vec::new();

        // If x already has an equation, solve the two together.
        // Normalize both sides first so known constants are substituted,
        // enabling detection of non-integer solutions (e.g., AV(2) = 5/2).
        if let Some(existing) = self.linear_eqs.remove(&x) {
            let norm_existing = self.normalize_linear(&existing);
            let norm_solution = self.normalize_linear(&solution);
            let diff = norm_existing.sub(&norm_solution);
            match diff.solve_eq_zero() {
                SatUnsat::Unsat => return SatUnsat::Unsat,
                SatUnsat::Sat(None) => {} // consistent
                SatUnsat::Sat(Some((y, new_sol))) => {
                    // Discovered y = new_sol
                    match self.add_linear_eq(y, new_sol) {
                        SatUnsat::Unsat => return SatUnsat::Unsat,
                        SatUnsat::Sat(eqs) => new_eqs.extend(eqs),
                    }
                }
            }
        }

        // Check if solution is just a constant
        if let Some(q) = solution.get_as_const() {
            if q.is_zero() {
                new_eqs.push(NewEq::EqZero(x));
            }
            // If x is known to be integer-typed but the solution is a
            // non-integer rational (e.g., 5/2 from 2x=5), the path is Unsat.
            // Cross-ref: OCaml PulseFormulaTerm.ml IsInt term evaluation
            // detects this when normalizing `IsInt(5/2)` → 0 ≠ 1.
            if !q.is_integer() && self.is_marked_int(x) {
                return SatUnsat::Unsat;
            }
        }

        // Check if solution is just a variable (x = y → merge classes)
        if let Some(y) = solution.get_as_var() {
            match self.and_var_equal(x, y) {
                SatUnsat::Unsat => return SatUnsat::Unsat,
                SatUnsat::Sat(eqs) => new_eqs.extend(eqs),
            }
            return SatUnsat::Sat(new_eqs);
        }

        // Substitute x in all existing equations, then re-solve them through
        // add_linear_eq so non-integer constants and newly exposed equalities
        // are handled consistently.
        let mut updated_eqs = Vec::new();
        for (&v, lin) in &self.linear_eqs {
            if lin.get_coefficient(x).is_some() {
                let substed = lin.subst_var(x, &solution);
                let normalized = self.normalize_linear(&substed);
                updated_eqs.push((v, normalized));
            }
        }
        for (v, new_lin) in updated_eqs {
            self.linear_eqs.remove(&v);
            match self.add_linear_eq(v, new_lin) {
                SatUnsat::Unsat => return SatUnsat::Unsat,
                SatUnsat::Sat(eqs) => new_eqs.extend(eqs),
            }
        }

        // Substitute x in atoms
        let replacement = lin_to_term(&solution);
        let old_atoms: Vec<_> = self.atoms.iter().cloned().collect();
        self.atoms.clear();
        for atom in old_atoms {
            let subst = atom.subst_var(x, &replacement);
            if let Some(false) = subst.is_trivially_true() {
                return SatUnsat::Unsat;
            }
            if subst.is_trivially_true() != Some(true) {
                self.atoms.insert(subst);
            }
        }

        // Store the equation
        self.linear_eqs.insert(x, solution);

        SatUnsat::Sat(new_eqs)
    }

    /// Propagate an equality `merged = kept` through linear_eqs and atoms.
    fn propagate_equality(
        &mut self,
        merged: AbstractValue,
        kept: AbstractValue,
        new_eqs: &mut Vec<NewEq>,
    ) -> SatUnsat<Vec<NewEq>> {
        // If merged had a linear eq, substitute merged → kept in it
        if let Some(lin) = self.linear_eqs.remove(&merged) {
            match self.and_linear_eq(kept, lin) {
                SatUnsat::Unsat => return SatUnsat::Unsat,
                SatUnsat::Sat(eqs) => new_eqs.extend(eqs),
            }
        }

        // Substitute merged → kept in all other linear eqs, then
        // re-normalize to propagate known constants.
        let replacement = LinArith::of_var(kept);
        let mut updated = Vec::new();
        for (&v, lin) in &self.linear_eqs {
            if lin.get_coefficient(merged).is_some() {
                let substed = lin.subst_var(merged, &replacement);
                let normalized = self.normalize_linear(&substed);
                updated.push((v, normalized));
            }
        }
        for (v, new_lin) in updated {
            // Re-solve through add_linear_eq so constant solutions
            // trigger the is_int check and other normalization.
            self.linear_eqs.remove(&v);
            match self.add_linear_eq(v, new_lin) {
                SatUnsat::Unsat => return SatUnsat::Unsat,
                SatUnsat::Sat(eqs) => new_eqs.extend(eqs),
            }
        }

        // Check is_int consistency: if any is_int variable now has
        // a non-integer constant solution, the path is Unsat.
        // This catches cases like 2x=5 → x=5/2 where x is int-typed.
        // Cross-ref: OCaml PulseFormulaTerm.ml IsInt normalization.
        for &v in &self.is_int_vars {
            let repr = self.get_repr(v);
            if let Some(lin) = self.linear_eqs.get(&repr) {
                let normalized = self.normalize_linear(lin);
                if let Some(q) = normalized.get_as_const() {
                    if !q.is_integer() {
                        return SatUnsat::Unsat;
                    }
                }
            }
        }

        // Substitute in atoms
        let term_replacement = Term::Var(kept);
        let old_atoms: Vec<_> = self.atoms.iter().cloned().collect();
        self.atoms.clear();
        for atom in old_atoms {
            let subst = atom.subst_var(merged, &term_replacement);
            if let Some(false) = subst.is_trivially_true() {
                return SatUnsat::Unsat;
            }
            if subst.is_trivially_true() != Some(true) {
                self.atoms.insert(subst);
            }
        }

        // Check if kept is now known zero
        if let Some(q) = self.get_known_const(kept) {
            if q.is_zero() {
                new_eqs.push(NewEq::EqZero(kept));
            }
        }

        SatUnsat::Sat(std::mem::take(new_eqs))
    }

    /// Normalize a linear expression by replacing variables with their
    /// canonical representatives and substituting known linear equations.
    fn normalize_linear(&self, lin: &LinArith) -> LinArith {
        let mut result = LinArith::of_q(lin.constant);
        for (&v, coeff) in &lin.vars {
            let repr = self.get_repr(v);
            if let Some(eq) = self.linear_eqs.get(&repr) {
                // v is known to equal eq, substitute
                result = result.add(&eq.mult_scalar(coeff));
            } else {
                let mut term = LinArith::of_var(repr);
                term = term.mult_scalar(coeff);
                result = result.add(&term);
            }
        }
        result
    }

    /// Resolve an atom by substituting known constants and representatives.
    fn resolve_atom(&self, atom: &Atom) -> Atom {
        let resolve_term = |t: &Term| -> Term { self.resolve_term(t) };
        match atom {
            Atom::Equal(a, b) => Atom::Equal(resolve_term(a), resolve_term(b)),
            Atom::NotEqual(a, b) => Atom::NotEqual(resolve_term(a), resolve_term(b)),
            Atom::LessEqual(a, b) => Atom::LessEqual(resolve_term(a), resolve_term(b)),
            Atom::LessThan(a, b) => Atom::LessThan(resolve_term(a), resolve_term(b)),
        }
    }

    fn resolve_condition_atom(&self, atom: &Atom) -> Atom {
        let resolve_term = |t: &Term| -> Term {
            self.resolve_condition_term(t, &mut std::collections::HashSet::new())
        };
        match atom {
            Atom::Equal(a, b) => Atom::Equal(resolve_term(a), resolve_term(b)),
            Atom::NotEqual(a, b) => Atom::NotEqual(resolve_term(a), resolve_term(b)),
            Atom::LessEqual(a, b) => Atom::LessEqual(resolve_term(a), resolve_term(b)),
            Atom::LessThan(a, b) => Atom::LessThan(resolve_term(a), resolve_term(b)),
        }
    }

    /// Resolve a term by substituting canonical reps and known constants.
    fn resolve_term(&self, t: &Term) -> Term {
        match t {
            Term::Var(v) => {
                let repr = self.get_repr(*v);
                if let Some(lin) = self.linear_eqs.get(&repr) {
                    if let Some(q) = lin.get_as_const() {
                        // Only resolve to Const if value is an integer.
                        // Non-integer rationals (e.g., 5/2 from float division)
                        // can't be represented in Term::Const(i64).
                        if q.is_integer() {
                            return Term::Const(*q.numer() / *q.denom());
                        }
                    }
                }
                Term::Var(repr)
            }
            Term::Const(_) => t.clone(),
            Term::Add(a, b) => {
                let ra = self.resolve_term(a);
                let rb = self.resolve_term(b);
                match (ra.as_const(), rb.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x + y),
                    _ => Term::Add(Box::new(ra), Box::new(rb)),
                }
            }
            Term::Sub(a, b) => {
                let ra = self.resolve_term(a);
                let rb = self.resolve_term(b);
                match (ra.as_const(), rb.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x - y),
                    _ => Term::Sub(Box::new(ra), Box::new(rb)),
                }
            }
            Term::Mult(a, b) => {
                let ra = self.resolve_term(a);
                let rb = self.resolve_term(b);
                match (ra.as_const(), rb.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x * y),
                    _ => Term::Mult(Box::new(ra), Box::new(rb)),
                }
            }
            Term::Neg(a) => {
                let ra = self.resolve_term(a);
                if let Some(x) = ra.as_const() {
                    Term::Const(-x)
                } else {
                    Term::Neg(Box::new(ra))
                }
            }
            Term::Not(a) => Term::Not(Box::new(self.resolve_term(a))),
            Term::IsZero(a) => {
                let ra = self.resolve_term(a);
                if let Some(x) = ra.as_const() {
                    Term::Const(if x == 0 { 1 } else { 0 })
                } else {
                    Term::IsZero(Box::new(ra))
                }
            }
        }
    }

    fn resolve_condition_term(
        &self,
        t: &Term,
        visited: &mut std::collections::HashSet<AbstractValue>,
    ) -> Term {
        fn coeff_term(coeff: &Q, term: Term) -> Term {
            if *coeff == Q::from_integer(1) {
                term
            } else if *coeff == Q::from_integer(-1) {
                Term::Neg(Box::new(term))
            } else {
                Term::Mult(
                    Box::new(Term::Const(*coeff.numer() / *coeff.denom())),
                    Box::new(term),
                )
            }
        }

        fn add_term(lhs: Option<Term>, rhs: Term) -> Term {
            match lhs {
                Some(lhs) => match (lhs.as_const(), rhs.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x + y),
                    _ => Term::Add(Box::new(lhs), Box::new(rhs)),
                },
                None => rhs,
            }
        }

        fn lin_to_condition_term(phi: &Phi, lin: &LinArith) -> Term {
            let mut result = (!lin.constant.is_zero())
                .then_some(Term::Const(*lin.constant.numer() / *lin.constant.denom()));
            for (&dep, coeff) in &lin.vars {
                let dep_term = Term::Var(phi.get_repr(dep));
                result = Some(add_term(result, coeff_term(coeff, dep_term)));
            }
            result.unwrap_or(Term::Const(0))
        }

        match t {
            Term::Var(v) => {
                let repr = self.get_repr(*v);
                if !visited.insert(repr) {
                    return Term::Var(repr);
                }

                let resolved = if let Some(lin) = self.linear_eqs.get(&repr) {
                    if let Some(q) = lin.get_as_const() {
                        if q.is_integer() {
                            Term::Const(*q.numer() / *q.denom())
                        } else {
                            Term::Var(repr)
                        }
                    } else {
                        Term::Var(repr)
                    }
                } else if let Some(solution) = self.linear_eqs.iter().find_map(|(&lhs, lin)| {
                    let coeff = lin.get_coefficient(repr)?;
                    let mut equation = LinArith::of_var(lhs).sub(lin);
                    equation.vars.remove(&repr)?;
                    let scale = Q::from_integer(1) / *coeff;
                    Some(equation.mult_scalar(&scale))
                }) {
                    lin_to_condition_term(self, &solution)
                } else {
                    Term::Var(repr)
                };

                visited.remove(&repr);
                resolved
            }
            Term::Const(_) => t.clone(),
            Term::Add(a, b) => {
                let ra = self.resolve_condition_term(a, visited);
                let rb = self.resolve_condition_term(b, visited);
                match (ra.as_const(), rb.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x + y),
                    _ => Term::Add(Box::new(ra), Box::new(rb)),
                }
            }
            Term::Sub(a, b) => {
                let ra = self.resolve_condition_term(a, visited);
                let rb = self.resolve_condition_term(b, visited);
                match (ra.as_const(), rb.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x - y),
                    _ => Term::Sub(Box::new(ra), Box::new(rb)),
                }
            }
            Term::Mult(a, b) => {
                let ra = self.resolve_condition_term(a, visited);
                let rb = self.resolve_condition_term(b, visited);
                match (ra.as_const(), rb.as_const()) {
                    (Some(x), Some(y)) => Term::Const(x * y),
                    _ => Term::Mult(Box::new(ra), Box::new(rb)),
                }
            }
            Term::Neg(a) => {
                let ra = self.resolve_condition_term(a, visited);
                if let Some(x) = ra.as_const() {
                    Term::Const(-x)
                } else {
                    Term::Neg(Box::new(ra))
                }
            }
            Term::Not(a) => {
                let ra = self.resolve_condition_term(a, visited);
                if let Some(x) = ra.as_const() {
                    Term::Const(if x == 0 { 1 } else { 0 })
                } else {
                    Term::Not(Box::new(ra))
                }
            }
            Term::IsZero(a) => {
                let ra = self.resolve_condition_term(a, visited);
                if let Some(x) = ra.as_const() {
                    Term::Const(if x == 0 { 1 } else { 0 })
                } else {
                    Term::IsZero(Box::new(ra))
                }
            }
        }
    }
}

/// Convert a LinArith to a Term (for atom substitution).
fn lin_to_term(lin: &LinArith) -> Term {
    let mut result: Option<Term> = None;

    for (&v, coeff) in &lin.vars {
        let var_term = if *coeff == Q::from_integer(1) {
            Term::Var(v)
        } else {
            Term::Mult(
                Box::new(Term::Const(*coeff.numer() / *coeff.denom())),
                Box::new(Term::Var(v)),
            )
        };
        result = Some(match result {
            None => var_term,
            Some(acc) => Term::Add(Box::new(acc), Box::new(var_term)),
        });
    }

    if !lin.constant.is_zero() {
        let const_term = Term::Const(*lin.constant.numer() / *lin.constant.denom());
        result = Some(match result {
            None => const_term,
            Some(acc) => Term::Add(Box::new(acc), Box::new(const_term)),
        });
    }

    result.unwrap_or(Term::Const(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_var_equal() {
        let mut phi = Phi::ttrue();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);

        let result = phi.and_var_equal(v1, v2);
        assert!(result.is_sat());
        assert_eq!(phi.get_repr(v1), phi.get_repr(v2));
    }

    #[test]
    fn test_const_eq() {
        let mut phi = Phi::ttrue();
        let v = AbstractValue::of_raw(1);

        let result = phi.and_const_eq(v, 42);
        assert!(result.is_sat());
        assert_eq!(phi.get_known_const(v), Some(Q::from_integer(42)));
    }

    #[test]
    fn test_const_eq_zero() {
        let mut phi = Phi::ttrue();
        let v = AbstractValue::of_raw(1);

        let result = phi.and_const_eq(v, 0);
        assert!(result.is_sat());
        assert!(phi.is_known_zero(v));
    }

    #[test]
    fn test_contradiction_different_constants() {
        let mut phi = Phi::ttrue();
        let v = AbstractValue::of_raw(1);

        phi.and_const_eq(v, 0);
        let result = phi.and_const_eq(v, 42);
        assert!(result.is_unsat());
    }

    #[test]
    fn test_propagation_via_equality() {
        let mut phi = Phi::ttrue();
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);

        // v1 = 0
        phi.and_const_eq(v1, 0);
        // v1 = v2 → v2 = 0
        phi.and_var_equal(v1, v2);
        assert!(phi.is_known_zero(v2));
    }

    #[test]
    fn test_linear_eq_propagation() {
        let mut phi = Phi::ttrue();
        let x = AbstractValue::of_raw(1);
        let y = AbstractValue::of_raw(2);

        // x = y + 1
        let lin = LinArith::of_var(y).add(&LinArith::of_int(1));
        phi.and_linear_eq(x, lin);

        // y = 0 → x should be 1
        phi.and_const_eq(y, 0);
        assert_eq!(phi.get_known_const(x), Some(Q::from_integer(1)));
    }

    #[test]
    fn test_add_linear_eq_rejects_non_integer_solution_from_substitution() {
        let mut phi = Phi::ttrue();
        let x = AbstractValue::of_raw(1);
        let sum = AbstractValue::of_raw(3);

        phi.mark_is_int(x);
        assert!(phi
            .add_linear_eq(x, LinArith::of_var(sum).mult_scalar(&Q::new(1, 2)))
            .is_sat());

        let result = phi.add_linear_eq(sum, LinArith::of_int(1));
        assert!(
            result.is_unsat(),
            "substituting sum=1 into x=sum/2 should reject x=1/2 when x is integer-typed"
        );
    }

    #[test]
    fn test_atom_not_equal() {
        let mut phi = Phi::ttrue();
        let v = AbstractValue::of_raw(1);

        // v ≠ 0
        let result = phi.and_atom(Atom::NotEqual(Term::Var(v), Term::Const(0)));
        assert!(result.is_sat());
    }

    #[test]
    fn test_atom_contradiction() {
        let mut phi = Phi::ttrue();
        let v = AbstractValue::of_raw(1);

        // v = 0
        phi.and_const_eq(v, 0);
        // v ≠ 0 → contradiction
        let result = phi.and_atom(Atom::NotEqual(Term::Var(v), Term::Const(0)));
        assert!(result.is_unsat());
    }

    #[test]
    fn test_less_than_contradiction() {
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let t1 = Term::Var(v1);
        let t2 = Term::Var(v2);

        // x < y ∧ y < x → Unsat
        let mut phi = Phi::ttrue();
        assert!(phi
            .and_atom(Atom::LessThan(t1.clone(), t2.clone()))
            .is_sat());
        assert!(phi
            .and_atom(Atom::LessThan(t2.clone(), t1.clone()))
            .is_unsat());

        // x < y ∧ x = y → Unsat
        let mut phi = Phi::ttrue();
        assert!(phi
            .and_atom(Atom::LessThan(t1.clone(), t2.clone()))
            .is_sat());
        assert!(phi.and_atom(Atom::Equal(t1.clone(), t2.clone())).is_unsat());
    }

    #[test]
    fn test_simplify_drops_unreachable_term_fn_app_interval_and_is_int_facts() {
        let mut phi = Phi::ttrue();
        let keep = AbstractValue::of_raw(1);
        let dead_term = AbstractValue::of_raw(2);
        let dead_actual = AbstractValue::of_raw(3);
        let dead_fn_ret = AbstractValue::of_raw(4);
        let dead_interval = AbstractValue::of_raw(5);
        let dead_is_int = AbstractValue::of_raw(6);

        assert!(phi.and_const_eq(keep, 7).is_sat());
        assert!(phi.and_const_eq(dead_actual, 0).is_sat());
        phi.term_eqs.insert(
            dead_term,
            TermEq {
                op: sil::binop::Binop::PlusA(None),
                lhs: super::super::Operand::ConstOperand(1),
                rhs: super::super::Operand::ConstOperand(2),
            },
        );
        assert!(phi
            .and_fn_app(dead_fn_ret, "__infer_skip", &[dead_actual])
            .is_sat());
        assert!(phi
            .add_interval(dead_interval, super::super::citv::CItv::equal_to(42))
            .is_sat());
        phi.mark_is_int(dead_is_int);

        phi.simplify(&HashSet::from([keep]));

        assert!(
            phi.term_eqs.is_empty(),
            "dead term equalities should be dropped during simplification"
        );
        assert!(
            phi.iter_fn_app_eqs().next().is_none(),
            "dead function-application facts should be dropped during simplification"
        );
        assert!(
            !phi.intervals.contains_key(&dead_interval),
            "dead intervals should be dropped during simplification"
        );
        assert!(
            !phi.is_int_vars.contains(&dead_is_int),
            "dead is_int facts should be dropped during simplification"
        );
        assert_eq!(
            phi.get_known_const(keep),
            Some(Q::from_integer(7)),
            "reachable constraints should survive simplification"
        );
    }

    #[test]
    fn test_simplify_keeps_reachable_term_fn_app_interval_and_is_int_facts() {
        let mut phi = Phi::ttrue();
        let term_ret = AbstractValue::of_raw(1);
        let fn_actual = AbstractValue::of_raw(2);
        let fn_ret = AbstractValue::of_raw(3);

        phi.term_eqs.insert(
            term_ret,
            TermEq {
                op: sil::binop::Binop::PlusA(None),
                lhs: super::super::Operand::AbstractValue(fn_actual),
                rhs: super::super::Operand::ConstOperand(1),
            },
        );
        assert!(phi.and_const_eq(fn_actual, 0).is_sat());
        assert!(phi
            .and_fn_app(fn_ret, "__infer_skip", &[fn_actual])
            .is_sat());
        assert!(phi
            .add_interval(fn_actual, super::super::citv::CItv::equal_to(0))
            .is_sat());
        phi.mark_is_int(fn_actual);

        phi.simplify(&HashSet::from([term_ret, fn_actual, fn_ret]));

        assert!(
            phi.term_eqs.contains_key(&term_ret),
            "reachable term equalities should survive simplification"
        );
        assert_eq!(
            phi.iter_fn_app_eqs().count(),
            1,
            "reachable function-application facts should survive simplification"
        );
        assert!(
            phi.intervals.contains_key(&fn_actual),
            "reachable intervals should survive simplification"
        );
        assert!(
            phi.is_int_vars.contains(&fn_actual),
            "reachable is_int facts should survive simplification"
        );
    }
}
