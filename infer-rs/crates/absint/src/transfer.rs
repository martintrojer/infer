// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Transfer function traits.
//!
//! Mirrors OCaml's `TransferFunctions.mli`.

use crate::domain::AbstractDomain;
use crate::interp::{InvariantMap, State};
use sil::instr::Instr;
use sil::procdesc::{NodeId, Procdesc};

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

    /// Execute all instructions in a node.
    ///
    /// Default: thread the whole input state through the node instruction list.
    /// Disjunctive analyses can override this to mirror OCaml's
    /// `exec_node_instrs` behavior, for example by re-executing only the new
    /// pre disjuncts and joining them into the retained node post.
    fn exec_node(
        &self,
        old_state: Option<&State<Self::Domain>>,
        pre: &Self::Domain,
        data: &Self::AnalysisData,
        node_id: NodeId,
        pdesc: &Procdesc,
        reverse_instrs: bool,
    ) -> Self::Domain {
        let _ = old_state;
        let node = match pdesc.get_node(node_id) {
            Some(node) => node,
            None => return pre.clone(),
        };

        let mut state = pre.clone();
        if reverse_instrs {
            for (idx, instr) in node.instrs.iter().enumerate().rev() {
                state = self.exec_instr(&state, data, node_id, idx, instr);
            }
        } else {
            for (idx, instr) in node.instrs.iter().enumerate() {
                state = self.exec_instr(&state, data, node_id, idx, instr);
            }
        }
        state
    }

    /// Observe the current fixpoint invariant map after a node update.
    ///
    /// Default: no-op. Checkers can override this to emit coarse progress
    /// snapshots for long-running procedures.
    fn observe_fixpoint(&self, _node_id: NodeId, _inv_map: &InvariantMap<Self::Domain>) {}
}
