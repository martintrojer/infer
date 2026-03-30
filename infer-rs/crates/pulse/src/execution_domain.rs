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
/// NOTE: PartialEq always returns false — each disjunct is unique.
/// This matches OCaml's use of pointer equality (equal_fast) which
/// is false for independently constructed states.
#[derive(Clone, Debug)]
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

impl PartialEq for ExecutionDomain {
    fn eq(&self, _other: &Self) -> bool {
        // Each disjunct is unique — no structural equality.
        // Matches OCaml's pointer equality (equal_fast).
        false
    }
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
