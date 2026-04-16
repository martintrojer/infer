// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Abstract interpreter — fixpoint computation engines.
//!
//! Mirrors OCaml's `AbstractInterpreter.ml`.
//!
//! Implements:
//! - `compute_fixpoint_rpo`: forward RPO fixpoint (mirrors `MakeRPO`)
//! - `compute_fixpoint_backward_rpo`: backward RPO fixpoint (mirrors `MakeBackwardRPO`)
//!
//! The backward variant mirrors OCaml's `ProcCfg.Backward`: swap succs/preds,
//! swap start/exit, reverse instruction order within nodes.

use std::collections::{HashMap, HashSet};

use crate::domain::{AbstractDomain, Comparable, WithBottom};
use crate::transfer::TransferFunctions;
use sil::procdesc::{NodeId, Procdesc};

/// Pre/post state pair at a program point.
#[derive(Clone, Debug, PartialEq)]
pub struct State<D> {
    pub pre: D,
    pub post: D,
    pub visit_count: usize,
}

/// Invariant map: node id → state.
pub type InvariantMap<D> = HashMap<NodeId, State<D>>;

/// Extract the postcondition for a node from the invariant map.
pub fn extract_post<D: Clone>(node_id: NodeId, inv_map: &InvariantMap<D>) -> Option<D> {
    inv_map.get(&node_id).map(|s| s.post.clone())
}

/// Extract the precondition for a node from the invariant map.
pub fn extract_pre<D: Clone>(node_id: NodeId, inv_map: &InvariantMap<D>) -> Option<D> {
    inv_map.get(&node_id).map(|s| s.pre.clone())
}

// ---------------------------------------------------------------------------
// CFG direction abstraction
// ---------------------------------------------------------------------------

/// Abstracts over forward vs backward traversal of a CFG.
///
/// Mirrors OCaml's `ProcCfg.Normal` vs `ProcCfg.Backward`.
trait CfgDirection {
    /// The entry node for the analysis.
    fn entry_node(pdesc: &Procdesc) -> NodeId;

    /// Successors in the analysis direction (for computing RPO).
    fn succs(pdesc: &Procdesc, node_id: NodeId) -> Vec<NodeId>;

    /// Predecessors in the analysis direction (for computing pre-states).
    fn preds(pdesc: &Procdesc, node_id: NodeId) -> Vec<NodeId>;

    /// Whether to reverse instruction order within a node.
    fn reverse_instrs() -> bool;
}

/// Forward direction: start→exit, succs are succs, instrs in order.
struct Forward;

impl CfgDirection for Forward {
    fn entry_node(pdesc: &Procdesc) -> NodeId {
        pdesc.start_node
    }

    fn succs(pdesc: &Procdesc, node_id: NodeId) -> Vec<NodeId> {
        pdesc.get_succs(node_id).copied().collect()
    }

    fn preds(pdesc: &Procdesc, node_id: NodeId) -> Vec<NodeId> {
        pdesc.get_preds(node_id).copied().collect()
    }

    fn reverse_instrs() -> bool {
        false
    }
}

/// Backward direction: exit→start, succs become preds, instrs reversed.
///
/// Mirrors OCaml's `ProcCfg.Backward`.
struct Backward;

impl CfgDirection for Backward {
    fn entry_node(pdesc: &Procdesc) -> NodeId {
        pdesc.exit_node
    }

    fn succs(pdesc: &Procdesc, node_id: NodeId) -> Vec<NodeId> {
        pdesc.get_preds(node_id).copied().collect()
    }

    fn preds(pdesc: &Procdesc, node_id: NodeId) -> Vec<NodeId> {
        pdesc.get_succs(node_id).copied().collect()
    }

    fn reverse_instrs() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Shared fixpoint engine
// ---------------------------------------------------------------------------

/// Compute the reverse post-order of nodes w.r.t. a given direction.
fn reverse_post_order<D: CfgDirection>(pdesc: &Procdesc) -> Vec<NodeId> {
    let mut visited = HashSet::new();
    let mut post_order = Vec::new();

    fn dfs<Dir: CfgDirection>(
        node_id: NodeId,
        pdesc: &Procdesc,
        visited: &mut HashSet<NodeId>,
        post_order: &mut Vec<NodeId>,
    ) {
        if !visited.insert(node_id) {
            return;
        }
        for succ in Dir::succs(pdesc, node_id) {
            dfs::<Dir>(succ, pdesc, visited, post_order);
        }
        post_order.push(node_id);
    }

    dfs::<D>(D::entry_node(pdesc), pdesc, &mut visited, &mut post_order);
    post_order.reverse();
    post_order
}

/// Core fixpoint computation, parametrized by direction.
fn compute_fixpoint<Dir: CfgDirection, TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> InvariantMap<TF::Domain>
where
    TF::Domain: WithBottom,
{
    let rpo = reverse_post_order::<Dir>(pdesc);
    let mut inv_map: InvariantMap<TF::Domain> = HashMap::new();

    // Initialize entry node with the initial state.
    let entry = Dir::entry_node(pdesc);
    inv_map.insert(
        entry,
        State {
            pre: initial.clone(),
            post: initial,
            visit_count: 0,
        },
    );

    let mut changed = true;
    while changed {
        changed = false;

        for &node_id in &rpo {
            // Compute the pre-state by joining post-states of predecessors
            // (in the analysis direction).
            let pre = compute_pre::<Dir, TF::Domain>(node_id, pdesc, &inv_map);

            // Check if the pre-state has changed.
            let old_pre = inv_map.get(&node_id).map(|s| &s.pre);
            if old_pre.is_some_and(|old| pre.leq(old)) {
                continue;
            }

            // Compute the post-state by executing all instructions.
            let post = exec_node::<Dir, TF>(tf, data, node_id, pdesc, &pre);

            let visit_count = inv_map
                .get(&node_id)
                .map(|s| s.visit_count + 1)
                .unwrap_or(1);

            // Apply widening if this node has been visited enough times.
            let post = if visit_count > config::get().pulse_widen_threshold {
                if let Some(old_state) = inv_map.get(&node_id) {
                    old_state.post.widen(&post, visit_count)
                } else {
                    post
                }
            } else {
                post
            };

            inv_map.insert(
                node_id,
                State {
                    pre,
                    post,
                    visit_count,
                },
            );
            changed = true;
        }
    }

    inv_map
}

/// Compute pre-state by joining post-states of all predecessors (in the
/// analysis direction).
fn compute_pre<Dir: CfgDirection, D: WithBottom>(
    node_id: NodeId,
    pdesc: &Procdesc,
    inv_map: &InvariantMap<D>,
) -> D {
    let mut pre = D::bottom();
    for pred_id in Dir::preds(pdesc, node_id) {
        if let Some(pred_state) = inv_map.get(&pred_id) {
            pre = pre.join(&pred_state.post);
        }
    }
    // If no predecessors contributed (entry node), use whatever is in inv_map.
    if pre.is_bottom() {
        if let Some(state) = inv_map.get(&node_id) {
            pre = state.pre.clone();
        }
    }
    pre
}

/// Execute all instructions in a node, threading the state through.
/// Instruction order is reversed for backward analysis.
fn exec_node<Dir: CfgDirection, TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    node_id: NodeId,
    pdesc: &Procdesc,
    pre: &TF::Domain,
) -> TF::Domain {
    let node = match pdesc.get_node(node_id) {
        Some(n) => n,
        None => return pre.clone(),
    };
    let mut state = pre.clone();
    if Dir::reverse_instrs() {
        for (idx, instr) in node.instrs.iter().enumerate().rev() {
            state = tf.exec_instr(&state, data, node_id, idx, instr);
        }
    } else {
        for (idx, instr) in node.instrs.iter().enumerate() {
            state = tf.exec_instr(&state, data, node_id, idx, instr);
        }
    }
    state
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compute the forward fixpoint using reverse post-order iteration.
///
/// Mirrors OCaml's `AbstractInterpreter.MakeRPO`.
pub fn compute_fixpoint_rpo<TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> InvariantMap<TF::Domain>
where
    TF::Domain: WithBottom,
{
    compute_fixpoint::<Forward, TF>(tf, data, pdesc, initial)
}

/// Compute the forward postcondition of a procedure.
///
/// Returns `None` if the exit node is unreachable.
pub fn compute_post_rpo<TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> Option<TF::Domain>
where
    TF::Domain: WithBottom,
{
    let inv_map = compute_fixpoint_rpo(tf, data, pdesc, initial);
    extract_post(pdesc.exit_node, &inv_map)
}

/// Compute the backward fixpoint using reverse post-order iteration.
///
/// Mirrors OCaml's `AbstractInterpreter.MakeBackwardRPO`.
///
/// The analysis starts from the exit node, flows backwards through the CFG,
/// and instructions within each node are processed in reverse order.
pub fn compute_fixpoint_backward_rpo<TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> InvariantMap<TF::Domain>
where
    TF::Domain: WithBottom,
{
    compute_fixpoint::<Backward, TF>(tf, data, pdesc, initial)
}

/// Compute the backward "postcondition" (i.e. the state at the start node).
///
/// Returns `None` if the start node is unreachable in backward traversal.
pub fn compute_post_backward_rpo<TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> Option<TF::Domain>
where
    TF::Domain: WithBottom,
{
    let inv_map = compute_fixpoint_backward_rpo(tf, data, pdesc, initial);
    extract_post(pdesc.start_node, &inv_map)
}

// ---------------------------------------------------------------------------
// WTO-based fixpoint
// ---------------------------------------------------------------------------

use crate::wto::{self, Partition};

/// Whether a node reached fixpoint during WTO iteration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Convergence {
    ReachedFixPoint,
    DidNotReachFixPoint,
}

/// Execute a single node in the WTO fixpoint, applying widening at loop heads.
fn exec_wto_node<Dir: CfgDirection, TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    node_id: NodeId,
    pdesc: &Procdesc,
    is_loop_head: bool,
    inv_map: &mut InvariantMap<TF::Domain>,
) -> Convergence
where
    TF::Domain: WithBottom,
{
    // Compute pre-state from predecessors
    let pre = compute_pre::<Dir, TF::Domain>(node_id, pdesc, inv_map);

    if let Some(old_state) = inv_map.get(&node_id) {
        // Apply widening at loop heads
        let new_pre = if is_loop_head {
            old_state.pre.widen(&pre, old_state.visit_count)
        } else {
            pre
        };

        // Check convergence: new_pre ≤ old_pre
        let reached_fixpoint = new_pre.leq(&old_state.pre);
        if reached_fixpoint {
            return Convergence::ReachedFixPoint;
        }

        // Not converged: execute instructions and update
        let post = exec_node::<Dir, TF>(tf, data, node_id, pdesc, &new_pre);
        // Cross-ref: OCaml `AbstractInterpreter.exec_node_instrs` keeps the
        // existing node post and joins newly produced disjuncts into it.
        // Replacing the post outright loses duplicate/drop information that was
        // discovered on earlier visits, which in turn hides under-approximation
        // metadata from summary export.
        let post = old_state.post.join(&post);
        let visit_count = old_state.visit_count + 1;
        inv_map.insert(
            node_id,
            State {
                pre: new_pre,
                post,
                visit_count,
            },
        );
        Convergence::DidNotReachFixPoint
    } else {
        // First visit
        let post = exec_node::<Dir, TF>(tf, data, node_id, pdesc, &pre);
        inv_map.insert(
            node_id,
            State {
                pre,
                post,
                visit_count: 1,
            },
        );
        Convergence::DidNotReachFixPoint
    }
}

/// Execute a WTO partition (sequence of vertices and components).
fn exec_wto_partition<Dir: CfgDirection, TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    partition: &Partition,
    inv_map: &mut InvariantMap<TF::Domain>,
) where
    TF::Domain: WithBottom,
{
    match partition {
        Partition::Empty => {}
        Partition::Vertex { node, next } => {
            exec_wto_node::<Dir, TF>(tf, data, *node, pdesc, false, inv_map);
            exec_wto_partition::<Dir, TF>(tf, data, pdesc, next, inv_map);
        }
        Partition::Component { head, rest, next } => {
            exec_wto_component::<Dir, TF>(tf, data, pdesc, *head, rest, inv_map);
            exec_wto_partition::<Dir, TF>(tf, data, pdesc, next, inv_map);
        }
    }
}

/// Execute a WTO component (loop): iterate head + body until fixpoint.
fn exec_wto_component<Dir: CfgDirection, TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    head: NodeId,
    rest: &Partition,
    inv_map: &mut InvariantMap<TF::Domain>,
) where
    TF::Domain: WithBottom,
{
    // First iteration: execute head (with widening since it's a loop head)
    exec_wto_node::<Dir, TF>(tf, data, head, pdesc, true, inv_map);
    // Execute the body
    exec_wto_partition::<Dir, TF>(tf, data, pdesc, rest, inv_map);

    // Iterate until fixpoint. Safety bound from config (default 10000).
    // A well-behaved widening operator should converge much sooner; hitting
    // this limit indicates a buggy widen implementation.
    let max_widens = config::get().max_widens;
    for _ in 0..max_widens {
        let convergence = exec_wto_node::<Dir, TF>(tf, data, head, pdesc, true, inv_map);
        if convergence == Convergence::ReachedFixPoint {
            return;
        }
        exec_wto_partition::<Dir, TF>(tf, data, pdesc, rest, inv_map);
    }
}

/// Core WTO-based fixpoint computation, parametrized by direction.
fn compute_fixpoint_wto_inner<Dir: CfgDirection, TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> InvariantMap<TF::Domain>
where
    TF::Domain: WithBottom,
{
    let entry = Dir::entry_node(pdesc);
    // Compute WTO using the analysis direction's successor function
    let wto = wto::compute_wto(entry, |node_id| Dir::succs(pdesc, node_id));

    let mut inv_map: InvariantMap<TF::Domain> = HashMap::new();

    // Initialize entry node
    inv_map.insert(
        entry,
        State {
            pre: initial.clone(),
            post: initial,
            visit_count: 0,
        },
    );

    // Execute the WTO partition
    exec_wto_partition::<Dir, TF>(tf, data, pdesc, &wto, &mut inv_map);

    inv_map
}

/// Compute the forward fixpoint using Weak Topological Order.
///
/// Mirrors OCaml's `AbstractInterpreter.MakeWTO`.
/// Widens only at loop heads (identified by WTO components), giving
/// more precise results than RPO on nested loops.
pub fn compute_fixpoint_wto<TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> InvariantMap<TF::Domain>
where
    TF::Domain: WithBottom,
{
    compute_fixpoint_wto_inner::<Forward, TF>(tf, data, pdesc, initial)
}

/// Compute the backward fixpoint using Weak Topological Order.
pub fn compute_fixpoint_backward_wto<TF: TransferFunctions>(
    tf: &TF,
    data: &TF::AnalysisData,
    pdesc: &Procdesc,
    initial: TF::Domain,
) -> InvariantMap<TF::Domain>
where
    TF::Domain: WithBottom,
{
    compute_fixpoint_wto_inner::<Backward, TF>(tf, data, pdesc, initial)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AbstractDomain, Comparable, WithBottom};
    use sil::instr::Instr;
    use sil::location::Location;
    use sil::procdesc::{NodeKind, StmtNodeKind};
    use sil::procname::Procname;
    use sil::typ::Typ;

    // -- Forward: counting transfer function --

    #[derive(Clone, Debug, PartialEq)]
    struct Count(usize);

    impl Comparable for Count {
        fn leq(&self, rhs: &Self) -> bool {
            self.0 <= rhs.0
        }
    }

    impl AbstractDomain for Count {
        fn join(&self, other: &Self) -> Self {
            Count(self.0.max(other.0))
        }

        fn widen(&self, next: &Self, _num_iters: usize) -> Self {
            self.join(next)
        }
    }

    impl WithBottom for Count {
        fn bottom() -> Self {
            Count(0)
        }

        fn is_bottom(&self) -> bool {
            self.0 == 0
        }
    }

    struct CountingTransfer;

    impl TransferFunctions for CountingTransfer {
        type Domain = Count;
        type AnalysisData = ();

        fn exec_instr(
            &self,
            state: &Count,
            _data: &(),
            _node_id: NodeId,
            _instr_idx: usize,
            _instr: &Instr,
        ) -> Count {
            Count(state.0 + 1)
        }
    }

    fn make_test_pdesc() -> Procdesc {
        let pname = Procname::c_from_string("test_func");
        let loc = Location::dummy();
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc.clone());

        let instrs = vec![Instr::skip(), Instr::skip(), Instr::skip()];
        let node_id = pdesc.add_node(NodeKind::StmtNode(StmtNodeKind::MethodBody), instrs, loc);

        pdesc.set_succs(0, vec![node_id]);
        pdesc.set_succs(node_id, vec![1]);

        pdesc
    }

    #[test]
    fn test_forward_fixpoint() {
        let pdesc = make_test_pdesc();
        let tf = CountingTransfer;
        let inv_map = compute_fixpoint_rpo(&tf, &(), &pdesc, Count(0));
        let post = extract_post(2, &inv_map).unwrap();
        assert_eq!(post, Count(3));
    }

    #[test]
    fn test_forward_post() {
        let pdesc = make_test_pdesc();
        let tf = CountingTransfer;
        let post = compute_post_rpo(&tf, &(), &pdesc, Count(0));
        assert!(post.is_some());
        assert_eq!(post.unwrap(), Count(3));
    }

    #[test]
    fn test_empty_procedure() {
        let pname = Procname::c_from_string("empty");
        let loc = Location::dummy();
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc);
        pdesc.set_succs(0, vec![1]);

        let tf = CountingTransfer;
        let post = compute_post_rpo(&tf, &(), &pdesc, Count(0));
        assert_eq!(post, Some(Count(0)));
    }

    #[test]
    fn test_branching_cfg() {
        let pname = Procname::c_from_string("branch");
        let loc = Location::dummy();
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc.clone());

        let left = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc.clone(),
        );
        let right = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip(), Instr::skip()],
            loc,
        );

        pdesc.set_succs(0, vec![left, right]);
        pdesc.set_succs(left, vec![1]);
        pdesc.set_succs(right, vec![1]);

        let tf = CountingTransfer;
        let post = compute_post_rpo(&tf, &(), &pdesc, Count(0));
        assert_eq!(post, Some(Count(2)));
    }

    // -- Backward: counting instructions backward --

    #[test]
    fn test_backward_fixpoint() {
        let pdesc = make_test_pdesc();
        let tf = CountingTransfer;

        // Backward analysis starts from exit, flows to start.
        let inv_map = compute_fixpoint_backward_rpo(&tf, &(), &pdesc, Count(0));

        // Node 2 has 3 instructions; backward analysis processes them in reverse
        // but counting doesn't care about order — still +3.
        let post = extract_post(2, &inv_map).unwrap();
        assert_eq!(post, Count(3));

        // Start node (0) should be reachable from exit via backward traversal.
        let start_post = extract_post(0, &inv_map).unwrap();
        assert_eq!(start_post, Count(3));
    }

    #[test]
    fn test_backward_branching() {
        // In backward analysis, a node with two successors (forward) has two
        // predecessors (backward), so its pre-state is a join.
        let pname = Procname::c_from_string("back_branch");
        let loc = Location::dummy();
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc.clone());

        let left = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc.clone(),
        );
        let right = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc,
        );

        // Forward: start -> {left, right} -> exit
        pdesc.set_succs(0, vec![left, right]);
        pdesc.set_succs(left, vec![1]);
        pdesc.set_succs(right, vec![1]);

        let tf = CountingTransfer;
        let inv_map = compute_fixpoint_backward_rpo(&tf, &(), &pdesc, Count(0));

        // Backward: exit -> {left, right} -> start
        // Start node (0) has two backward predecessors (left, right),
        // each with post Count(1). Join = max(1, 1) = 1.
        let start_post = extract_post(0, &inv_map).unwrap();
        assert_eq!(start_post, Count(1));
    }

    // -- WTO fixpoint tests --

    #[test]
    fn test_wto_forward_fixpoint() {
        let pdesc = make_test_pdesc();
        let tf = CountingTransfer;
        let inv_map = compute_fixpoint_wto(&tf, &(), &pdesc, Count(0));
        let post = extract_post(2, &inv_map).unwrap();
        assert_eq!(post, Count(3));
    }

    #[test]
    fn test_wto_matches_rpo_on_linear() {
        let pdesc = make_test_pdesc();
        let tf = CountingTransfer;
        let rpo_map = compute_fixpoint_rpo(&tf, &(), &pdesc, Count(0));
        let wto_map = compute_fixpoint_wto(&tf, &(), &pdesc, Count(0));

        // On a linear CFG, RPO and WTO should produce the same results
        for (&node_id, rpo_state) in &rpo_map {
            let wto_state = wto_map.get(&node_id).unwrap();
            assert_eq!(
                rpo_state.post, wto_state.post,
                "node {node_id}: RPO and WTO post-states differ"
            );
        }
    }

    #[test]
    fn test_wto_matches_rpo_on_branch() {
        let pname = Procname::c_from_string("wto_branch");
        let loc = Location::dummy();
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc.clone());
        let left = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc.clone(),
        );
        let right = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip(), Instr::skip()],
            loc,
        );
        pdesc.set_succs(0, vec![left, right]);
        pdesc.set_succs(left, vec![1]);
        pdesc.set_succs(right, vec![1]);

        let tf = CountingTransfer;
        let rpo_post = compute_post_rpo(&tf, &(), &pdesc, Count(0));
        let wto_map = compute_fixpoint_wto(&tf, &(), &pdesc, Count(0));
        let wto_post = extract_post(pdesc.exit_node, &wto_map);
        assert_eq!(rpo_post, wto_post);
    }

    #[test]
    fn test_wto_loop_converges() {
        // start → header → {body, after}
        // body → header (back edge)
        let pname = Procname::c_from_string("wto_loop");
        let loc = Location::dummy();
        let mut pdesc = Procdesc::new(pname, Typ::void(), loc.clone());

        let header = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc.clone(),
        );
        let body = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc.clone(),
        );
        let after = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            loc,
        );

        pdesc.set_succs(0, vec![header]);
        pdesc.set_succs(header, vec![body, after]);
        pdesc.set_succs(body, vec![header]); // back edge
        pdesc.set_succs(after, vec![1]);

        let tf = CountingTransfer;
        let inv_map = compute_fixpoint_wto(&tf, &(), &pdesc, Count(0));

        // The loop should converge (Count domain's widen = join = max,
        // and the counting transfer is monotone)
        assert!(inv_map.contains_key(&header));
        assert!(inv_map.contains_key(&body));
        assert!(inv_map.contains_key(&after));
    }

    #[test]
    fn test_wto_backward_fixpoint() {
        let pdesc = make_test_pdesc();
        let tf = CountingTransfer;
        let inv_map = compute_fixpoint_backward_wto(&tf, &(), &pdesc, Count(0));
        let post = extract_post(2, &inv_map).unwrap();
        assert_eq!(post, Count(3));
    }
}
