// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::annot::AnnotItem;
use crate::instr::{IfKind, Instr};
use crate::location::Location;
use crate::mangled::Mangled;
use crate::procname::Procname;
use crate::typ::Typ;

/// Unique node identifier within a procedure.
pub type NodeId = u32;

// ---- Node kinds ----

/// Kind of destruction in a CFG node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DestructionKind {
    DestrBreakStmt,
    DestrContinueStmt,
    DestrFields,
    DestrReturnStmt,
    DestrScope,
    DestrTemporariesCleanup,
    DestrVirtualBase,
}

/// Kind of statement node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StmtNodeKind {
    AssertionFailure,
    AtomicCompareExchangeBranch,
    AtomicExpr,
    BetweenJoinAndExit,
    BinaryConditionalStmtInit,
    BinaryOperatorStmt(String),
    Call(String),
    CallObjCNew,
    CaseStmt,
    ClassCastException,
    CompoundStmt,
    ConditionalStmtBranch,
    ConstructorInit,
    CXXDynamicCast,
    CXXNewExpr,
    CXXStdInitializerListExpr,
    CXXTemporaryMarkerSet,
    CXXTry,
    CXXTypeidExpr,
    DeclStmt,
    DefineBody,
    Destruction(DestructionKind),
    Erlang,
    ExceptionHandler,
    ExceptionsSink,
    ExprWithCleanups,
    FinallyBranch,
    GCCAsmStmt,
    GenericSelectionExpr,
    IfStmtBranch,
    InitializeDynamicArrayLength,
    InitListExp,
    LoopBody,
    LoopIterIncr,
    LoopIterInit,
    MessageCall(String),
    MethodBody,
    MonitorEnter,
    MonitorExit,
    ObjCCPPThrow,
    ObjCIndirectCopyRestoreExpr,
    OutOfBound,
    ReturnStmt,
    Scope(String),
    Skip,
    SwitchStmt,
    ThisNotNull,
    Throw,
    ThrowNPE,
    UnaryOperator,
}

/// Kind of prune node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PruneNodeKind {
    ExceptionHandler,
    FalseBranch,
    InBound,
    IsInstance,
    MethodBody,
    NotNull,
    TrueBranch,
}

/// Kind of CFG node.
///
/// Mirrors OCaml's `Procdesc.Node.nodekind`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    StartNode,
    ExitNode,
    StmtNode(StmtNodeKind),
    JoinNode,
    /// (true/false branch, if_kind, prune kind)
    PruneNode(bool, IfKind, PruneNodeKind),
    SkipNode(String),
}

// ---- Node ----

/// A node in the control flow graph.
///
/// Unlike OCaml's mutable `Procdesc.Node.t`, this uses index-based references
/// for successors and predecessors, stored externally in the `Procdesc`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub instrs: Vec<Instr>,
    pub loc: Location,
}

// ---- Procdesc ----

/// Local variable data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarData {
    pub name: Mangled,
    pub typ: Typ,
    pub modify_in_block: bool,
    pub is_constexpr: bool,
    pub is_declared_unused: bool,
    pub is_structured_binding: bool,
    #[serde(default)]
    pub has_cleanup_attribute: bool,
}

/// Procedure description -- a single procedure's control flow graph.
///
/// Mirrors OCaml's `Procdesc.t`, but uses an index-based graph representation
/// instead of mutable linked nodes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Procdesc {
    /// Procedure name.
    pub proc_name: Procname,
    /// Formal parameters: (name, type, annotations).
    pub formals: Vec<(Mangled, Typ, AnnotItem)>,
    /// Return type.
    pub ret_type: Typ,
    /// Local variables.
    pub locals: Vec<VarData>,
    /// Source location of the procedure.
    pub loc: Location,
    /// All nodes in the CFG.
    pub nodes: Vec<Node>,
    /// Start node ID.
    pub start_node: NodeId,
    /// Exit node ID.
    pub exit_node: NodeId,
    /// Successor edges: node_id -> set of successor node_ids.
    /// Set semantics: no duplicate edges, `contains()` is a core operation.
    pub succs: HashMap<NodeId, BTreeSet<NodeId>>,
    /// Predecessor edges: node_id -> set of predecessor node_ids.
    pub preds: HashMap<NodeId, BTreeSet<NodeId>>,
    /// Exception handler edges: node_id -> set of exception handler node_ids.
    pub exn_succs: HashMap<NodeId, BTreeSet<NodeId>>,
    /// Whether the procedure is defined (has a body) vs just declared.
    pub is_defined: bool,
    /// Whether the procedure is known not to return.
    ///
    /// Mirrors OCaml ProcAttributes.is_no_return when that metadata is
    /// available from the capture pipeline.
    #[serde(default)]
    pub is_no_return: bool,
}

impl Procdesc {
    pub fn new(proc_name: Procname, ret_type: Typ, loc: Location) -> Self {
        // Create start and exit nodes.
        let start_node = Node {
            id: 0,
            kind: NodeKind::StartNode,
            instrs: Vec::new(),
            loc: loc.clone(),
        };
        let exit_node = Node {
            id: 1,
            kind: NodeKind::ExitNode,
            instrs: Vec::new(),
            loc: loc.clone(),
        };
        Self {
            proc_name,
            formals: Vec::new(),
            ret_type,
            locals: Vec::new(),
            loc,
            nodes: vec![start_node, exit_node],
            start_node: 0,
            exit_node: 1,
            succs: HashMap::new(),
            preds: HashMap::new(),
            exn_succs: HashMap::new(),
            is_defined: true,
            is_no_return: false,
        }
    }

    /// Add a node to the CFG and return its ID.
    pub fn add_node(&mut self, kind: NodeKind, instrs: Vec<Instr>, loc: Location) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(Node {
            id,
            kind,
            instrs,
            loc,
        });
        id
    }

    /// Set successor edges from `from` to `to_nodes`.
    pub fn set_succs(&mut self, from: NodeId, to_nodes: impl IntoIterator<Item = NodeId>) {
        let set: BTreeSet<NodeId> = to_nodes.into_iter().collect();
        for &to in &set {
            self.preds.entry(to).or_default().insert(from);
        }
        self.succs.insert(from, set);
    }

    /// Set exception handler edges.
    pub fn set_exn_succs(&mut self, from: NodeId, to_nodes: impl IntoIterator<Item = NodeId>) {
        self.exn_succs.insert(from, to_nodes.into_iter().collect());
    }

    /// Get a node by its ID.
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id as usize)
    }

    /// Get successor node IDs.
    pub fn get_succs(&self, id: NodeId) -> impl Iterator<Item = &NodeId> {
        self.succs.get(&id).into_iter().flatten()
    }

    /// Get predecessor node IDs.
    pub fn get_preds(&self, id: NodeId) -> impl Iterator<Item = &NodeId> {
        self.preds.get(&id).into_iter().flatten()
    }

    /// Check if the procedure has an empty body (stub for extern declarations).
    ///
    /// Empty stubs have no real instructions in any node — they're generated
    /// by dump-textual for extern declarations and should be treated as
    /// unknown calls rather than analyzed.
    pub fn is_empty_body(&self) -> bool {
        self.nodes.iter().all(|n| n.instrs.is_empty())
    }

    /// Iterate over all instructions in the procedure.
    pub fn iter_instrs(&self) -> impl Iterator<Item = (NodeId, &Instr)> {
        self.nodes
            .iter()
            .flat_map(|node| node.instrs.iter().map(move |instr| (node.id, instr)))
    }

    /// Coarse CFG size metric used to skip pathologically large procedures.
    ///
    /// Cross-ref: OCaml `Procdesc.size` counts one unit per node, one per
    /// successor edge, and one per SIL instruction.
    pub fn size(&self) -> usize {
        self.nodes
            .iter()
            .map(|node| 1 + self.get_succs(node.id).count() + node.instrs.len())
            .sum()
    }
}

impl fmt::Display for Procdesc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "procdesc {} ({} nodes)",
            self.proc_name,
            self.nodes.len()
        )
    }
}
