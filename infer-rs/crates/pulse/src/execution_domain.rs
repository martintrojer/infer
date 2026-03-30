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
/// We use structural equality here because the disjunctive abstract interpreter
/// relies on reflexive equality for subset checks and deduplication. OCaml can
/// get away with pointer identity (`equal_fast`) because unchanged states often
/// keep the same heap object; in Rust we clone states freely, so pointer-style
/// equality would make `d <= d` fail and break fixpoint convergence.
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
            | ExecutionDomain::LatentAbortProgram { state, .. } => state,
        }
    }
}

impl Comparable for ExecutionDomain {
    fn leq(&self, rhs: &Self) -> bool {
        use ExecutionDomain::{
            AbortProgram, ContinueProgram, ExceptionRaised, ExitProgram, LatentAbortProgram,
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
                invalidation_location: lhs_invalidation_location,
                ..
            },
            Diagnostic::AccessToInvalidAddress {
                invalidation: rhs_invalidation,
                access_location: rhs_access_location,
                invalidation_location: rhs_invalidation_location,
                ..
            },
        ) => {
            lhs_invalidation == rhs_invalidation
                && lhs_access_location == rhs_access_location
                && lhs_invalidation_location == rhs_invalidation_location
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
