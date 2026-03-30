// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Execution domain: the possible states of a Pulse analysis path.
//!
//! Mirrors OCaml's `PulseExecutionDomain.t`.

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
