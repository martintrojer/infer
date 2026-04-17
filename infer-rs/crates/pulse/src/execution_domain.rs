// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Execution domain: the possible states of a Pulse analysis path.
//!
//! Mirrors OCaml's `PulseExecutionDomain.t`.

use absint::domain::Comparable;

use crate::abductive::AbductiveDomain;
use crate::diagnostic::Diagnostic;

/// The state of an analysis path after executing an instruction.
///
/// `PartialEq` gives us a cheap structural approximation of OCaml's
/// `equal_fast` for disjunctive dedup. Semantic subset/fixpoint checks still
/// live in `Comparable::leq` below and fall back to alpha-equivalence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionDomain {
    /// Normal execution continues.
    ContinueProgram(AbductiveDomain),
    /// A definite (manifest) error was found — analysis of this path aborts.
    AbortProgram {
        state: Box<AbductiveDomain>,
        diagnostic: Box<Diagnostic>,
    },
    /// A latent error: the path condition depends on caller-provided values.
    /// Kept in summaries so callers can check if the error manifests.
    /// Cross-ref: OCaml PulseExecutionDomain.ml LatentAbortProgram.
    LatentAbortProgram {
        state: Box<AbductiveDomain>,
        diagnostic: Box<Diagnostic>,
    },
    /// A latent invalid access carried interprocedurally until a caller can
    /// prove the accessed address is invalid in its own context.
    ///
    /// Cross-ref: OCaml PulseExecutionDomain.ml LatentInvalidAccess.
    LatentInvalidAccess {
        state: Box<AbductiveDomain>,
        diagnostic: Box<Diagnostic>,
    },
    /// The procedure exits normally (return).
    ExitProgram(AbductiveDomain),
    /// An exception was raised.
    ExceptionRaised(AbductiveDomain),
}

impl ExecutionDomain {
    /// Is this a continuing (non-error, non-exit) state?
    pub fn is_continue(&self) -> bool {
        matches!(self, ExecutionDomain::ContinueProgram(_))
    }

    /// Extract the abstract state, regardless of variant.
    pub fn get_astate(&self) -> &AbductiveDomain {
        match self {
            ExecutionDomain::ContinueProgram(s)
            | ExecutionDomain::ExitProgram(s)
            | ExecutionDomain::ExceptionRaised(s) => s,
            ExecutionDomain::AbortProgram { state, .. }
            | ExecutionDomain::LatentAbortProgram { state, .. }
            | ExecutionDomain::LatentInvalidAccess { state, .. } => state,
        }
    }
}

impl Comparable for ExecutionDomain {
    fn equal_fast(&self, rhs: &Self) -> bool {
        use ExecutionDomain::{
            AbortProgram, ContinueProgram, ExceptionRaised, ExitProgram, LatentAbortProgram,
            LatentInvalidAccess,
        };

        match (self, rhs) {
            (ContinueProgram(lhs), ContinueProgram(rhs))
            | (ExceptionRaised(lhs), ExceptionRaised(rhs))
            | (ExitProgram(lhs), ExitProgram(rhs)) => lhs == rhs,
            (
                AbortProgram {
                    state: lhs_state,
                    diagnostic: lhs_diag,
                },
                AbortProgram {
                    state: rhs_state,
                    diagnostic: rhs_diag,
                },
            )
            | (
                LatentAbortProgram {
                    state: lhs_state,
                    diagnostic: lhs_diag,
                },
                LatentAbortProgram {
                    state: rhs_state,
                    diagnostic: rhs_diag,
                },
            ) => lhs_state == rhs_state && diagnostics_compatible(lhs_diag, rhs_diag),
            (
                LatentInvalidAccess {
                    state: lhs_state,
                    diagnostic: lhs_diag,
                },
                LatentInvalidAccess {
                    state: rhs_state,
                    diagnostic: rhs_diag,
                },
            ) => {
                lhs_state == rhs_state
                    && latent_invalid_access_diagnostics_compatible(lhs_diag, rhs_diag)
            }
            _ => false,
        }
    }

    fn leq(&self, rhs: &Self) -> bool {
        if self.equal_fast(rhs) {
            return true;
        }

        use ExecutionDomain::{
            AbortProgram, ContinueProgram, ExceptionRaised, ExitProgram, LatentAbortProgram,
            LatentInvalidAccess,
        };

        match (self, rhs) {
            (ContinueProgram(lhs), ContinueProgram(rhs))
            | (ExceptionRaised(lhs), ExceptionRaised(rhs))
            | (ExitProgram(lhs), ExitProgram(rhs)) => crate::state_cmp::alpha_equivalent(lhs, rhs),
            (
                AbortProgram {
                    state: lhs_state,
                    diagnostic: lhs_diag,
                },
                AbortProgram {
                    state: rhs_state,
                    diagnostic: rhs_diag,
                },
            )
            | (
                LatentAbortProgram {
                    state: lhs_state,
                    diagnostic: lhs_diag,
                },
                LatentAbortProgram {
                    state: rhs_state,
                    diagnostic: rhs_diag,
                },
            ) => {
                crate::state_cmp::alpha_equivalent(lhs_state, rhs_state)
                    && diagnostics_compatible(lhs_diag, rhs_diag)
            }
            (
                LatentInvalidAccess {
                    state: lhs_state,
                    diagnostic: lhs_diag,
                },
                LatentInvalidAccess {
                    state: rhs_state,
                    diagnostic: rhs_diag,
                },
            ) => diagnostics_compatible_semantic(lhs_state, lhs_diag, rhs_state, rhs_diag),
            _ => false,
        }
    }
}

fn diagnostics_compatible(lhs: &Diagnostic, rhs: &Diagnostic) -> bool {
    match (lhs, rhs) {
        (
            Diagnostic::AccessToInvalidAddress {
                invalidation: lhs_invalidation,
                access_location: lhs_access_location,
                access_history: lhs_access_history,
                invalidation_history: lhs_invalidation_history,
                ..
            },
            Diagnostic::AccessToInvalidAddress {
                invalidation: rhs_invalidation,
                access_location: rhs_access_location,
                access_history: rhs_access_history,
                invalidation_history: rhs_invalidation_history,
                ..
            },
        ) => {
            lhs_invalidation == rhs_invalidation
                && lhs_access_location == rhs_access_location
                && lhs_access_history == rhs_access_history
                && lhs_invalidation_history == rhs_invalidation_history
        }
        (
            Diagnostic::MemoryLeak {
                allocator: lhs_allocator,
                allocation_location: lhs_allocation_location,
                ..
            },
            Diagnostic::MemoryLeak {
                allocator: rhs_allocator,
                allocation_location: rhs_allocation_location,
                ..
            },
        ) => lhs_allocator == rhs_allocator && lhs_allocation_location == rhs_allocation_location,
        (Diagnostic::RetainCycle { location: lhs }, Diagnostic::RetainCycle { location: rhs }) => {
            lhs == rhs
        }
        _ => false,
    }
}

fn latent_invalid_access_diagnostics_compatible(lhs: &Diagnostic, rhs: &Diagnostic) -> bool {
    match (lhs, rhs) {
        (
            Diagnostic::AccessToInvalidAddress { addr: lhs_addr, .. },
            Diagnostic::AccessToInvalidAddress { addr: rhs_addr, .. },
        ) => lhs_addr == rhs_addr && diagnostics_compatible(lhs, rhs),
        _ => diagnostics_compatible(lhs, rhs),
    }
}

fn diagnostics_compatible_semantic(
    lhs_state: &AbductiveDomain,
    lhs: &Diagnostic,
    rhs_state: &AbductiveDomain,
    rhs: &Diagnostic,
) -> bool {
    match (lhs, rhs) {
        (
            Diagnostic::AccessToInvalidAddress {
                addr: lhs_addr,
                invalidation: lhs_invalidation,
                access_location: lhs_access_location,
                access_history: lhs_access_history,
                invalidation_history: lhs_invalidation_history,
                ..
            },
            Diagnostic::AccessToInvalidAddress {
                addr: rhs_addr,
                invalidation: rhs_invalidation,
                access_location: rhs_access_location,
                access_history: rhs_access_history,
                invalidation_history: rhs_invalidation_history,
                ..
            },
        ) => {
            crate::state_cmp::alpha_equivalent_value(lhs_state, *lhs_addr, rhs_state, *rhs_addr)
                && lhs_invalidation == rhs_invalidation
                && lhs_access_location == rhs_access_location
                && lhs_access_history == rhs_access_history
                && lhs_invalidation_history == rhs_invalidation_history
        }
        _ => diagnostics_compatible(lhs, rhs),
    }
}

#[cfg(test)]
mod tests {
    use absint::domain::Comparable;
    use sil::fieldname::Fieldname;
    use sil::int_lit::IntLit;
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::qualified_cpp_name::QualifiedCppName;
    use sil::typ::{Typ, TypeName};
    use sil::var::Var;

    use super::*;
    use crate::abstract_value::AbstractValue;
    use crate::access::Access;
    use crate::invalidation::Invalidation;
    use crate::value_history::ValueHistory;

    fn make_state_with_nested_value(
        dummy_fresh_values: usize,
    ) -> (AbductiveDomain, AbstractValue, AbstractValue) {
        let pname = Procname::c_from_string("execution_domain_test");
        let mut pdesc = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        pdesc.formals = vec![(Mangled::from_string("x"), Typ::void(), Default::default())];
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let pvar = Pvar::mk(Mangled::from_string("x"), pname);
        let formal_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(pvar)))
            .expect("formal should exist");

        for _ in 0..dummy_fresh_values {
            let _ = AbstractValue::mk_fresh();
        }

        let pointee = state.read_heap(formal_addr, Access::Dereference);
        let nested = AbstractValue::mk_fresh();
        let field = Access::FieldAccess(Fieldname::make(
            TypeName::CStruct(QualifiedCppName::from_string("Node")),
            "next",
        ));
        state.write_heap(pointee, field, nested);
        (state, pointee, nested)
    }

    fn invalid_access_diagnostic(addr: AbstractValue) -> Diagnostic {
        Diagnostic::AccessToInvalidAddress {
            addr,
            invalidation: Invalidation::ConstantDereference(IntLit::zero()),
            access_location: Location::dummy(),
            access_history: ValueHistory::epoch(),
            invalidation_history: ValueHistory::epoch(),
        }
    }

    #[test]
    fn latent_invalid_access_leq_tracks_address_under_alpha_renaming() {
        AbstractValue::reset_counters();
        let (state1, _root1, nested1) = make_state_with_nested_value(0);
        AbstractValue::reset_counters();
        let (state2, _root2, nested2) = make_state_with_nested_value(3);

        let exec1 = ExecutionDomain::LatentInvalidAccess {
            state: Box::new(state1),
            diagnostic: Box::new(invalid_access_diagnostic(nested1)),
        };
        let exec2 = ExecutionDomain::LatentInvalidAccess {
            state: Box::new(state2),
            diagnostic: Box::new(invalid_access_diagnostic(nested2)),
        };

        assert!(exec1.leq(&exec2));
        assert!(exec2.leq(&exec1));
    }

    #[test]
    fn latent_invalid_access_keeps_distinct_addresses_distinct() {
        AbstractValue::reset_counters();
        let (state, root, nested) = make_state_with_nested_value(0);

        let exec1 = ExecutionDomain::LatentInvalidAccess {
            state: Box::new(state.clone()),
            diagnostic: Box::new(invalid_access_diagnostic(root)),
        };
        let exec2 = ExecutionDomain::LatentInvalidAccess {
            state: Box::new(state),
            diagnostic: Box::new(invalid_access_diagnostic(nested)),
        };

        assert!(!exec1.equal_fast(&exec2));
        assert!(!exec1.leq(&exec2));
        assert!(!exec2.leq(&exec1));
    }
}
