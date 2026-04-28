// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Backward liveness analysis.
//!
//! Mirrors OCaml's `checkers/liveness.ml`. Computes the set of maybe-live
//! variables at each program point using a backward dataflow analysis.
//!
//! Domain: sets of `Var` (logical vars + program vars).
//! Transfer: gen a variable when it is read, kill when it is assigned.
//! Direction: backward (from exit to entry).

use std::collections::BTreeSet;
use std::fmt;

use absint::domain::{AbstractDomain, Comparable, WithBottom};
use absint::interp::{compute_fixpoint_backward_rpo, InvariantMap};
use absint::transfer::TransferFunctions;
use sil::exp::Exp;
use sil::ident::Ident;
use sil::instr::Instr;
use sil::procdesc::{NodeId, Procdesc};
use sil::pvar::Pvar;
use sil::var::Var;

// ---------------------------------------------------------------------------
// Domain: set of live variables
// ---------------------------------------------------------------------------

/// Set of live variables.
///
/// Mirrors OCaml's `VarSet = AbstractDomain.FiniteSet(Var)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveVarSet(pub BTreeSet<LiveVar>);

/// A variable that can be live. Wraps enough info to identify it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LiveVar {
    /// Logical (temporary) variable, identified by kind + stamp.
    LogicalVar { stamp: i32 },
    /// Program variable, identified by name.
    ProgramVar { name: String },
}

impl LiveVar {
    pub fn of_ident(id: &Ident) -> Self {
        LiveVar::LogicalVar { stamp: id.stamp }
    }

    pub fn of_pvar(pv: &Pvar) -> Self {
        LiveVar::ProgramVar {
            name: pv.name.plain.clone(),
        }
    }

    pub fn of_var(var: &Var) -> Self {
        match var {
            Var::LogicalVar(id) => Self::of_ident(id),
            Var::ProgramVar(pv) => Self::of_pvar(pv),
        }
    }
}

impl fmt::Display for LiveVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiveVar::LogicalVar { stamp } => write!(f, "n{stamp}"),
            LiveVar::ProgramVar { name } => write!(f, "{name}"),
        }
    }
}

impl LiveVarSet {
    pub fn new() -> Self {
        Self(BTreeSet::new())
    }

    pub fn add(&self, var: LiveVar) -> Self {
        let mut s = self.0.clone();
        s.insert(var);
        Self(s)
    }

    pub fn remove(&self, var: &LiveVar) -> Self {
        let mut s = self.0.clone();
        s.remove(var);
        Self(s)
    }

    pub fn contains(&self, var: &LiveVar) -> bool {
        self.0.contains(var)
    }
}

impl Default for LiveVarSet {
    fn default() -> Self {
        Self::new()
    }
}

// Delegate domain traits to the inner BTreeSet<LiveVar>, which already
// implements Comparable, AbstractDomain, and WithBottom in absint::domain.
impl Comparable for LiveVarSet {
    fn leq(&self, rhs: &Self) -> bool {
        self.0.leq(&rhs.0)
    }
}

impl AbstractDomain for LiveVarSet {
    fn join(&self, other: &Self) -> Self {
        Self(self.0.join(&other.0))
    }

    fn widen(&self, next: &Self, num_iters: usize) -> Self {
        Self(self.0.widen(&next.0, num_iters))
    }
}

impl WithBottom for LiveVarSet {
    fn bottom() -> Self {
        Self(BTreeSet::bottom())
    }

    fn is_bottom(&self) -> bool {
        self.0.is_bottom()
    }
}

// ---------------------------------------------------------------------------
// Transfer functions
// ---------------------------------------------------------------------------

/// Liveness transfer functions.
///
/// Mirrors OCaml's `Liveness.TransferFunctions`.
///
/// Backward analysis: `exec_instr` receives the state *after* the instruction
/// and returns the state *before* it.
///
/// - Load `id = *e`: kill `id`, gen vars in `e`
/// - Store `*lvar = e`: kill `lvar` (if local), gen vars in `e`
/// - Prune `e`: gen vars in `e`
/// - Call `id = f(args)`: kill `id`, gen vars in `f` and `args`
/// - Metadata: pass through
pub struct LivenessTransfer;

impl TransferFunctions for LivenessTransfer {
    type Domain = LiveVarSet;
    type AnalysisData = ();

    fn exec_instr(
        &self,
        state: &LiveVarSet,
        _data: &(),
        _node_id: NodeId,
        _instr_idx: usize,
        instr: &Instr,
    ) -> LiveVarSet {
        match instr {
            Instr::Load { id, e, .. } => {
                // Kill the assigned variable, gen variables read in the expression.
                let state = if id.is_none() {
                    state.clone()
                } else {
                    state.remove(&LiveVar::of_ident(id))
                };
                exp_add_live(e, &state)
            }
            Instr::Store { e1, e2, .. } => {
                // Kill the lvar being stored to (if it's an Lvar), gen vars in both exprs.
                let state = match e1.as_ref() {
                    Exp::Lvar(pv) => {
                        // Only kill locals, not globals/returns.
                        if pv.is_global() || pv.is_return() {
                            state.clone()
                        } else {
                            state.remove(&LiveVar::of_pvar(pv))
                        }
                    }
                    _ => exp_add_live(e1, state),
                };
                exp_add_live(e2, &state)
            }
            Instr::Prune { exp, .. } => exp_add_live(exp, state),
            Instr::Call {
                ret: (ret_id, _),
                fun_exp,
                args,
                ..
            } => {
                // Kill the return variable, gen the function expression and all args.
                let state = state.remove(&LiveVar::of_ident(ret_id));
                let state = exp_add_live(fun_exp, &state);
                args.iter()
                    .fold(state, |acc, (arg_exp, _)| exp_add_live(arg_exp, &acc))
            }
            Instr::Metadata(_) => state.clone(),
        }
    }
}

/// Add all variables read in an expression to the live set.
///
/// Mirrors OCaml's `exp_add_live`.
fn exp_add_live(exp: &Exp, state: &LiveVarSet) -> LiveVarSet {
    match exp {
        Exp::Var(id) => state.add(LiveVar::of_ident(id)),
        Exp::Lvar(pv) => state.add(LiveVar::of_pvar(pv)),
        Exp::UnOp(_, e, _) | Exp::Cast(_, e) | Exp::Exn(e) => exp_add_live(e, state),
        Exp::BinOp(_, e1, e2) | Exp::Lindex(e1, e2) => {
            let s = exp_add_live(e1, state);
            exp_add_live(e2, &s)
        }
        Exp::Lfield(data, _, _) => exp_add_live(&data.exp, state),
        Exp::Closure(closure) => closure
            .captured_vars
            .iter()
            .fold(state.clone(), |acc, (e, _)| exp_add_live(e, &acc)),
        Exp::Const(_) | Exp::Sizeof(_) => state.clone(),
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Result of liveness analysis for a procedure.
#[derive(Clone)]
pub struct LivenessResult {
    pub inv_map: InvariantMap<LiveVarSet>,
}

impl LivenessResult {
    /// Get the set of live variables *before* a given node (in forward direction).
    ///
    /// In backward analysis, the "post" state at a node is the result of
    /// processing the node's instructions in reverse — this corresponds to
    /// the state *before* the node in the original forward direction.
    pub fn live_before(&self, node_id: NodeId) -> Option<LiveVarSet> {
        absint::interp::extract_post(node_id, &self.inv_map)
    }

    /// Check if a variable is live before a given node.
    pub fn is_live_before(&self, node_id: NodeId, var: &LiveVar) -> bool {
        self.live_before(node_id)
            .is_some_and(|set| set.contains(var))
    }
}

/// Run liveness analysis on a procedure.
///
/// This is a backward analysis: it starts from the exit node with an empty
/// live set and propagates backwards, gen-ing variables on reads and kill-ing
/// them on writes.
pub fn analyze(pdesc: &Procdesc) -> LivenessResult {
    let tf = LivenessTransfer;
    let inv_map = compute_fixpoint_backward_rpo(&tf, &(), pdesc, LiveVarSet::new());
    LivenessResult { inv_map }
}

// ---------------------------------------------------------------------------
// Dead store reporting
// ---------------------------------------------------------------------------

use diagnostics::issue::{Issue, IssueLog};
use diagnostics::issue_type::IssueType;

/// Report DEAD_STORE issues: stores to local variables whose values are
/// never subsequently read.
///
/// Mirrors OCaml's `Liveness.checker` / `report_dead_store`.
///
/// Runs backward liveness, then for each `Store` to a local variable, checks
/// if that variable is read after the store (by re-running the backward
/// transfer functions per-instruction within each node).
///
/// Gaps vs OCaml (all incremental, suppress false positives):
/// - No PassedByRefAnalyzer: variables whose addresses escape via calls
///   (e.g. `foo(&x)`) are not suppressed and may produce false positives.
/// - No frontend temp suppression (`is_frontend_tmp`).
/// - No scope guard / RAII type suppression (`is_scope_guard`).
/// - No `_` variable, `constexpr`, or `[[maybe_unused]]` suppression.
/// - No constructor dead store detection (`Call(constructor, Lvar pvar)`).
/// - No config-driven block list (`liveness_block_list_var_regex`).
pub fn report_dead_stores(pdesc: &Procdesc) -> IssueLog {
    let result = analyze(pdesc);
    let proc_name = format!("{}", pdesc.proc_name);
    let mut log = IssueLog::new();

    for node in &pdesc.nodes {
        // Get the live set at the EXIT of this node (= pre in backward analysis).
        // In backward analysis: pre = state at node exit, post = state at node entry.
        let live_at_exit = absint::interp::extract_pre(node.id, &result.inv_map);
        let Some(live_at_exit) = live_at_exit else {
            continue;
        };

        // Walk instructions in reverse (backward), maintaining the live set.
        // This gives us the live set *after* each instruction.
        let tf = LivenessTransfer;
        let mut live = live_at_exit;

        for (instr_idx, instr) in node.instrs.iter().enumerate().rev() {
            // Check for dead stores: if this is a Store to a local and the
            // variable is NOT live after the store, it's dead.
            if let Instr::Store { e1, e2, loc, .. } = instr {
                if let Exp::Lvar(pv) = e1.as_ref() {
                    if !pv.is_global() && !pv.is_return() {
                        let live_var = LiveVar::of_pvar(pv);
                        if !live.contains(&live_var) && !is_sentinel_exp(e2.as_ref()) {
                            let var_name = &pv.name.plain;
                            log.report(Issue {
                                issue_type: IssueType::dead_store(),
                                qualifier: format!(
                                    "The value written to `{var_name}` is never used"
                                ),
                                file: format!("{}", loc.file),
                                line: loc.line as u32,
                                column: loc.col as u32,
                                procedure: proc_name.clone(),
                                trace: "Write of unused value".to_string(),
                                bug_trace: None,
                                bug_trace_length: None,
                                bug_trace_max_depth: None,
                            });
                        }
                    }
                }
            }

            // Apply backward transfer function to get live set before this instruction
            live = tf.exec_instr(&live, &(), node.id, instr_idx, instr);
        }
    }

    log.sort();
    log
}

/// Check if an expression is a "sentinel" constant (e.g. `0`, `null`)
/// that's used for default initialization and shouldn't be reported.
fn is_sentinel_exp(exp: &Exp) -> bool {
    matches!(
        exp,
        Exp::Const(sil::const_val::Const::Cint(i)) if i.is_zero()
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sil::call_flags::CallFlags;
    use sil::const_val::Const;
    use sil::exp::Exp;
    use sil::ident::{Ident, IdentName};
    use sil::int_lit::IntLit;
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::{NodeKind, StmtNodeKind};
    use sil::procname::Procname;
    use sil::pvar::Pvar;
    use sil::typ::Typ;

    fn loc() -> Location {
        Location::dummy()
    }

    fn mk_ident(stamp: i32) -> Ident {
        Ident::create_normal(IdentName::from_string("n"), stamp)
    }

    fn mk_pvar(name: &str, pname: &Procname) -> Pvar {
        Pvar::mk(Mangled::from_string(name), pname.clone())
    }

    fn mk_load(id: Ident, exp: Exp) -> Instr {
        Instr::Load {
            id,
            e: exp,
            typ: Typ::int(sil::typ::IKind::IInt),
            loc: loc(),
        }
    }

    fn mk_store(pv: Pvar, exp: Exp) -> Instr {
        Instr::Store {
            e1: Box::new(Exp::Lvar(pv)),
            typ: Typ::int(sil::typ::IKind::IInt),
            e2: Box::new(exp),
            loc: loc(),
        }
    }

    fn mk_call(ret_id: Ident, callee: Procname, args: Vec<Exp>) -> Instr {
        Instr::Call {
            ret: (ret_id, Typ::void()),
            fun_exp: Exp::Const(Const::Cfun(callee)),
            args: args.into_iter().map(|e| (e, Typ::void())).collect(),
            loc: loc(),
            flags: CallFlags::default(),
        }
    }

    /// Simple test: `n0 = load &x; ret n0`
    /// Before the load: x is live (read), n0 is not yet live.
    /// After the load: n0 is live (used by ret).
    #[test]
    fn test_simple_load_liveness() {
        let pname = Procname::c_from_string("f");
        let x = mk_pvar("x", &pname);
        let n0 = mk_ident(0);

        let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

        let instrs = vec![
            mk_load(n0.clone(), Exp::Lvar(x.clone())),
            // Simulate `ret n0` as a second load for simplicity
            // (the terminator doesn't go through exec_instr)
        ];
        let node = pdesc.add_node(NodeKind::StmtNode(StmtNodeKind::MethodBody), instrs, loc());

        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);

        let result = analyze(&pdesc);

        // After backward analysis, at node entry, x should be live
        // (it's read by the load instruction).
        let live = result.live_before(node);
        assert!(live.is_some());
        let live = live.unwrap();
        assert!(
            live.contains(&LiveVar::of_pvar(&x)),
            "x should be live before the load"
        );
    }

    /// Test kill: `store &x <- 42; n0 = load &x`
    /// The store kills x, then the load gens it again.
    /// Before the store: x is NOT live (killed, then re-genned from the load
    /// but the store is first in backward order).
    #[test]
    fn test_store_kills_variable() {
        let pname = Procname::c_from_string("f");
        let x = mk_pvar("x", &pname);
        let n0 = mk_ident(0);

        let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

        // In forward order: store &x <- 42; n0 = load &x
        // In backward order: n0 = load &x (gen x, kill n0); store &x <- 42 (kill x)
        let instrs = vec![
            mk_store(x.clone(), Exp::Const(Const::Cint(IntLit::of_int(42)))),
            mk_load(n0.clone(), Exp::Lvar(x.clone())),
        ];
        let node = pdesc.add_node(NodeKind::StmtNode(StmtNodeKind::MethodBody), instrs, loc());

        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);

        let result = analyze(&pdesc);
        let live = result.live_before(node).unwrap();

        // x is killed by the store after being gen'd by the load.
        // In backward: load gens x, then store kills x. So x is NOT live at entry.
        assert!(
            !live.contains(&LiveVar::of_pvar(&x)),
            "x should be killed by the store"
        );
    }

    /// Test call: `n1 = foo(n0)` — n0 is live (read), n1 is killed (written).
    #[test]
    fn test_call_gen_kill() {
        let pname = Procname::c_from_string("f");
        let n0 = mk_ident(0);
        let n1 = mk_ident(1);
        let foo = Procname::c_from_string("foo");

        let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

        let instrs = vec![mk_call(n1.clone(), foo, vec![Exp::Var(n0.clone())])];
        let node = pdesc.add_node(NodeKind::StmtNode(StmtNodeKind::MethodBody), instrs, loc());

        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);

        let result = analyze(&pdesc);
        let live = result.live_before(node).unwrap();

        assert!(
            live.contains(&LiveVar::of_ident(&n0)),
            "n0 should be live (argument to call)"
        );
        assert!(
            !live.contains(&LiveVar::of_ident(&n1)),
            "n1 should be killed (return of call)"
        );
    }

    /// End-to-end: parse a .sil file → convert to SIL → run liveness.
    /// Verifies specific liveness facts: x is live (used in ret via n0),
    /// y is loaded into n1 but n1 is never used, so y should still be
    /// live (the load reads it) but n1 should be dead after the load.
    #[test]
    fn test_end_to_end_liveness() {
        let src = r#".source_language = "java"

define f(x: int, y: int) : int {
  #entry:
    n0 : int = load &x
    n1 : int = load &y
    ret n0
}"#;
        let module = textual::parse_module(src, "test.sil").unwrap();
        let (decls, _) = textual::decls::DeclEnv::from_module(&module);
        let (cfg, _tenv) = textual::to_sil::module_to_sil(&module, &decls).unwrap();

        let pdesc = cfg.iter_proc_descs().next().expect("should have one proc");
        let result = analyze(pdesc);

        // Node 2 is the first textual node (0=start, 1=exit, 2=entry)
        let live = result
            .live_before(2)
            .expect("should have liveness for entry");
        // x is live: it's loaded into n0 which is returned
        assert!(
            live.contains(&LiveVar::ProgramVar { name: "x".into() }),
            "x should be live (read by load, used in ret)"
        );
        // y is live: it's loaded into n1 (the load reads &y)
        assert!(
            live.contains(&LiveVar::ProgramVar { name: "y".into() }),
            "y should be live (read by load)"
        );
    }
}
