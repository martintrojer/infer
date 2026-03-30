// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Weak Topological Order (Bourdoncle's algorithm).
//!
//! Mirrors OCaml's `WeakTopologicalOrder.ml`. Computes a hierarchical
//! decomposition of a graph into components (loops) and vertices.
//! Used by the WTO-based fixpoint engine for precise widening at loop heads.
//!
//! Reference: F. Bourdoncle, "Efficient chaotic iteration strategies with
//! widenings", FMPA 1993.

use std::collections::HashMap;

use sil::procdesc::NodeId;

/// A weak topological order: a recursive partition of graph nodes.
///
/// The partition is a sequence of elements, each either a plain `Vertex`
/// (not a loop head) or a `Component` (a loop with a head and a nested WTO
/// for its body).
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Partition {
    #[default]
    Empty,
    /// A plain vertex (not a loop head).
    Vertex { node: NodeId, next: Box<Partition> },
    /// A loop component: `head` is the loop header, `rest` is the WTO of
    /// the loop body (excluding the head), `next` continues after this component.
    Component {
        head: NodeId,
        rest: Box<Partition>,
        next: Box<Partition>,
    },
}

impl Partition {
    /// Visit all nodes in the partition in order.
    pub fn for_each_node(&self, f: &mut impl FnMut(NodeId)) {
        match self {
            Partition::Empty => {}
            Partition::Vertex { node, next } => {
                f(*node);
                next.for_each_node(f);
            }
            Partition::Component { head, rest, next } => {
                f(*head);
                rest.for_each_node(f);
                next.for_each_node(f);
            }
        }
    }

    /// Visit only the component heads (loop headers).
    pub fn for_each_head(&self, f: &mut impl FnMut(NodeId)) {
        match self {
            Partition::Empty => {}
            Partition::Vertex { next, .. } => {
                next.for_each_head(f);
            }
            Partition::Component { head, rest, next } => {
                f(*head);
                rest.for_each_head(f);
                next.for_each_head(f);
            }
        }
    }

    /// Prepend a vertex to this partition.
    fn prepend_vertex(self, node: NodeId) -> Self {
        Partition::Vertex {
            node,
            next: Box::new(self),
        }
    }

    /// Prepend a component to this partition.
    fn prepend_component(self, head: NodeId, rest: Partition) -> Self {
        Partition::Component {
            head,
            rest: Box::new(rest),
            next: Box::new(self),
        }
    }
}

impl std::fmt::Display for Partition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Partition::Empty => Ok(()),
            Partition::Vertex { node, next } => {
                write!(f, "{node}")?;
                if !matches!(**next, Partition::Empty) {
                    write!(f, " {next}")?;
                }
                Ok(())
            }
            Partition::Component { head, rest, next } => {
                write!(f, "({head}")?;
                if !matches!(**rest, Partition::Empty) {
                    write!(f, " {rest}")?;
                }
                write!(f, ")")?;
                if !matches!(**next, Partition::Empty) {
                    write!(f, " {next}")?;
                }
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Bourdoncle's algorithm
// ---------------------------------------------------------------------------

const DFN_VISITED: usize = usize::MAX;

/// State for one DFS stack frame.
struct StackFrame {
    node: NodeId,
    dfn: usize,
    succs: Vec<NodeId>,
    succ_idx: usize,
    head: usize,
    component: Vec<NodeId>,
    building_component: bool,
}

/// Compute the Weak Topological Order of a graph.
///
/// `start` is the entry node. `succs_fn` returns the successors of a node.
///
/// Mirrors OCaml's `Bourdoncle_SCC.make`.
pub fn compute_wto(start: NodeId, succs_fn: impl Fn(NodeId) -> Vec<NodeId>) -> Partition {
    let mut num: usize = 0;
    let mut dfn: HashMap<NodeId, usize> = HashMap::new();
    let mut stack: Vec<StackFrame> = Vec::new();
    let mut partition = Partition::Empty;

    // Push start node
    push_node(start, &succs_fn, &mut num, &mut dfn, &mut stack);

    // Process the stack
    process_stack(
        &succs_fn,
        &mut num,
        &mut dfn,
        &mut stack,
        &mut partition,
        false,
    );

    partition
}

fn push_node(
    node: NodeId,
    succs_fn: &impl Fn(NodeId) -> Vec<NodeId>,
    num: &mut usize,
    dfn: &mut HashMap<NodeId, usize>,
    stack: &mut Vec<StackFrame>,
) {
    *num += 1;
    let node_dfn = *num;
    dfn.insert(node, node_dfn);
    let succs = succs_fn(node);
    stack.push(StackFrame {
        node,
        dfn: node_dfn,
        succs: succs.clone(),
        succ_idx: 0,
        head: usize::MAX,
        component: Vec::new(),
        building_component: false,
    });
}

fn process_stack(
    succs_fn: &impl Fn(NodeId) -> Vec<NodeId>,
    num: &mut usize,
    dfn: &mut HashMap<NodeId, usize>,
    stack: &mut Vec<StackFrame>,
    partition: &mut Partition,
    stop_on_building: bool,
) {
    loop {
        let Some(frame) = stack.last_mut() else {
            return;
        };

        // Case A: unvisited successors remain
        if frame.succ_idx < frame.succs.len() {
            let succ = frame.succs[frame.succ_idx];
            frame.succ_idx += 1;

            if let Some(&succ_dfn) = dfn.get(&succ) {
                // Already visited — record if it could be a loop head
                if succ_dfn < frame.head {
                    frame.head = succ_dfn;
                }
            } else {
                // Not visited — push onto stack
                push_node(succ, succs_fn, num, dfn, stack);
            }
            continue;
        }

        // No more successors to visit
        let frame = stack.last().unwrap();
        let node = frame.node;
        let node_dfn = frame.dfn;
        let head = frame.head;
        let building = frame.building_component;

        // Case B: building a sub-component and done with this level
        if building && stop_on_building {
            return;
        }

        if head < node_dfn {
            // Case C: inside an SCC but not the head
            let component = frame.component.clone();
            stack.pop();

            if let Some(parent) = stack.last_mut() {
                if head < parent.head {
                    parent.head = head;
                }
                parent.component.push(node);
                parent.component.extend(component);
            }
        } else {
            // Case D: head >= node_dfn — this node is done or is an SCC head
            dfn.insert(node, DFN_VISITED);

            if head > node_dfn {
                // D1: plain vertex, not part of any SCC
                stack.pop();
                *partition = std::mem::take(partition).prepend_vertex(node);
            } else {
                // D2: head == node_dfn — this is the head of an SCC
                let component = frame.component.clone();

                // Remove component members from dfn (make them unvisited)
                for member in &component {
                    dfn.remove(member);
                }

                // Re-traverse to build the sub-partition
                {
                    let frame = stack.last_mut().unwrap();
                    frame.building_component = true;
                    frame.succ_idx = 0;
                    frame.head = usize::MAX;
                    frame.component.clear();
                }

                let mut component_partition = Partition::Empty;
                process_stack(succs_fn, num, dfn, stack, &mut component_partition, true);

                stack.pop();
                *partition = std::mem::take(partition).prepend_component(node, component_partition);
            }
        }

        if stack.is_empty() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn succs_from_edges(edges: &[(NodeId, Vec<NodeId>)]) -> impl Fn(NodeId) -> Vec<NodeId> + '_ {
        move |node| {
            edges
                .iter()
                .find(|(n, _)| *n == node)
                .map(|(_, s)| s.clone())
                .unwrap_or_default()
        }
    }

    #[test]
    fn test_linear() {
        // 0 → 1 → 2
        let edges = vec![(0, vec![1]), (1, vec![2]), (2, vec![])];
        let wto = compute_wto(0, succs_from_edges(&edges));
        assert_eq!(format!("{wto}"), "0 1 2");
    }

    #[test]
    fn test_diamond() {
        // 0 → {1, 2} → 3
        let edges = vec![(0, vec![1, 2]), (1, vec![3]), (2, vec![3]), (3, vec![])];
        let wto = compute_wto(0, succs_from_edges(&edges));
        let mut nodes = Vec::new();
        wto.for_each_node(&mut |n| nodes.push(n));
        assert_eq!(nodes.len(), 4);
        // WTO ordering: entry first, exit last
        assert_eq!(nodes[0], 0, "entry should be first");
        assert_eq!(nodes[3], 3, "exit should be last");
        // 1 and 2 must appear between 0 and 3
        let pos1 = nodes.iter().position(|&n| n == 1).unwrap();
        let pos2 = nodes.iter().position(|&n| n == 2).unwrap();
        assert!(pos1 > 0 && pos1 < 3, "node 1 between entry and exit");
        assert!(pos2 > 0 && pos2 < 3, "node 2 between entry and exit");
    }

    #[test]
    fn test_simple_loop() {
        // 0 → 1 → 2 → 1 (loop), 2 → 3
        let edges = vec![(0, vec![1]), (1, vec![2]), (2, vec![1, 3]), (3, vec![])];
        let wto = compute_wto(0, succs_from_edges(&edges));
        let s = format!("{wto}");

        // Should have a component with head 1
        assert!(s.contains('('), "should have a component: {s}");

        // Verify heads
        let mut heads = Vec::new();
        wto.for_each_head(&mut |n| heads.push(n));
        assert!(heads.contains(&1), "node 1 should be a loop head: {s}");
    }

    #[test]
    fn test_nested_loops() {
        // 0 → 1 → 2 → 3 → 2 (inner loop), 3 → 1 (outer loop), 3 → 4
        let edges = vec![
            (0, vec![1]),
            (1, vec![2]),
            (2, vec![3]),
            (3, vec![2, 1, 4]),
            (4, vec![]),
        ];
        let wto = compute_wto(0, succs_from_edges(&edges));
        let s = format!("{wto}");

        // Should have nested components
        let mut heads = Vec::new();
        wto.for_each_head(&mut |n| heads.push(n));
        assert!(
            heads.len() >= 2,
            "should have at least 2 loop heads for nested loops: {s}"
        );
    }

    #[test]
    fn test_self_loop() {
        // 0 → 1 → 1 (self-loop), 1 → 2
        let edges = vec![(0, vec![1]), (1, vec![1, 2]), (2, vec![])];
        let wto = compute_wto(0, succs_from_edges(&edges));
        let s = format!("{wto}");

        assert!(s.contains('('), "self-loop should create a component: {s}");
        let mut heads = Vec::new();
        wto.for_each_head(&mut |n| heads.push(n));
        assert!(heads.contains(&1), "node 1 should be a loop head: {s}");
    }

    #[test]
    fn test_no_loops() {
        // 0 → 1, 0 → 2, 1 → 3, 2 → 3
        let edges = vec![(0, vec![1, 2]), (1, vec![3]), (2, vec![3]), (3, vec![])];
        let wto = compute_wto(0, succs_from_edges(&edges));

        let mut heads = Vec::new();
        wto.for_each_head(&mut |n| heads.push(n));
        assert!(heads.is_empty(), "no loops means no component heads");
    }
}
