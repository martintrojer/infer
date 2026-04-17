// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Transfer function traits.
//!
//! Mirrors OCaml's `TransferFunctions.mli`.

use crate::domain::AbstractDomain;
use crate::interp::InvariantMap;
use sil::instr::Instr;
use sil::procdesc::NodeId;

/// Transfer functions that push abstract states across instructions.
///
/// Mirrors OCaml's `TransferFunctions.SIL`.
///
/// A typical checker implements this trait to define how executing an
/// instruction transforms the abstract state.
pub trait TransferFunctions {
    /// The abstract domain.
    type Domain: AbstractDomain;

    /// Read-only analysis data (results of previous analyses, globals, etc.).
    type AnalysisData;

    /// Execute one instruction.
    ///
    /// `state` is the abstract state before the instruction.
    /// Returns the abstract state after the instruction.
    ///
    /// `node_id` is the CFG node containing the instruction.
    /// `instr_idx` is the index of the instruction within the node.
    fn exec_instr(
        &self,
        state: &Self::Domain,
        data: &Self::AnalysisData,
        node_id: NodeId,
        instr_idx: usize,
        instr: &Instr,
    ) -> Self::Domain;

    /// Observe the current fixpoint invariant map after a node update.
    ///
    /// Default: no-op. Checkers can override this to emit coarse progress
    /// snapshots for long-running procedures.
    fn observe_fixpoint(&self, _node_id: NodeId, _inv_map: &InvariantMap<Self::Domain>) {}
}
