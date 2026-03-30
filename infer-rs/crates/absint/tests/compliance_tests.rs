// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Compliance tests ported from OCaml's `abstractInterpreterTests.ml`.
//!
//! Uses a path counting domain: counts the number of distinct paths reaching
//! each program point. This is a stress test for the fixpoint engine — join
//! adds counts, widen goes to Top, and loops must converge via widening.

use absint::domain::{AbstractDomain, Comparable, WithBottom};
use absint::interp::{compute_fixpoint_rpo, extract_post, InvariantMap};
use absint::transfer::TransferFunctions;
use sil::instr::Instr;
use sil::location::Location;
use sil::procdesc::{NodeId, NodeKind, Procdesc, StmtNodeKind};
use sil::procname::Procname;
use sil::typ::Typ;

// ---------------------------------------------------------------------------
// PathCountDomain — mirrors the OCaml PathCountDomain
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum PathCount {
    Count(i64),
    Top,
}

impl PathCount {
    fn make(c: i64) -> Self {
        if c < 0 {
            PathCount::Top
        } else {
            PathCount::Count(c)
        }
    }

    fn initial() -> Self {
        PathCount::Count(1)
    }
}

impl Comparable for PathCount {
    fn leq(&self, rhs: &Self) -> bool {
        match (self, rhs) {
            (PathCount::Count(c1), PathCount::Count(c2)) => c1 <= c2,
            (_, PathCount::Top) => true,
            (PathCount::Top, PathCount::Count(_)) => false,
        }
    }
}

impl AbstractDomain for PathCount {
    fn join(&self, other: &Self) -> Self {
        match (self, other) {
            (PathCount::Count(c1), PathCount::Count(c2)) => PathCount::make(c1 + c2),
            (PathCount::Top, _) | (_, PathCount::Top) => PathCount::Top,
        }
    }

    fn widen(&self, _next: &Self, _num_iters: usize) -> Self {
        PathCount::Top
    }
}

impl WithBottom for PathCount {
    fn bottom() -> Self {
        PathCount::Count(0)
    }

    fn is_bottom(&self) -> bool {
        matches!(self, PathCount::Count(0))
    }
}

impl std::fmt::Display for PathCount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathCount::Count(c) => write!(f, "{c}"),
            PathCount::Top => write!(f, "T"),
        }
    }
}

// ---------------------------------------------------------------------------
// PathCount transfer functions — just propagate (identity)
// ---------------------------------------------------------------------------

struct PathCountTransfer;

impl TransferFunctions for PathCountTransfer {
    type Domain = PathCount;
    type AnalysisData = ();

    fn exec_instr(
        &self,
        state: &PathCount,
        _data: &(),
        _node_id: NodeId,
        _instr_idx: usize,
        _instr: &Instr,
    ) -> PathCount {
        state.clone()
    }
}

// ---------------------------------------------------------------------------
// CFG builder helpers
// ---------------------------------------------------------------------------

fn loc() -> Location {
    Location::dummy()
}

fn make_node(pdesc: &mut Procdesc) -> NodeId {
    pdesc.add_node(
        NodeKind::StmtNode(StmtNodeKind::MethodBody),
        vec![Instr::skip()],
        loc(),
    )
}

fn run_analysis(pdesc: &Procdesc) -> InvariantMap<PathCount> {
    compute_fixpoint_rpo(&PathCountTransfer, &(), pdesc, PathCount::initial())
}

fn assert_post(inv_map: &InvariantMap<PathCount>, node_id: NodeId, expected: &str) {
    let post = extract_post(node_id, inv_map);
    let actual = post.map(|p| format!("{p}")).unwrap_or("_|_".to_string());
    assert_eq!(
        actual, expected,
        "node {node_id}: expected {expected}, got {actual}"
    );
}

// ---------------------------------------------------------------------------
// Tests from abstractInterpreterTests.ml
// ---------------------------------------------------------------------------

/// OCaml: ("straightline", [invariant "1"; invariant "1"])
/// A straight-line program: start -> n1 -> n2 -> exit.
/// One path reaches each node.
#[test]
fn test_straightline() {
    let pname = Procname::c_from_string("straightline");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let n1 = make_node(&mut pdesc);
    let n2 = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![n1]); // start -> n1
    pdesc.set_succs(n1, vec![n2]); // n1 -> n2
    pdesc.set_succs(n2, vec![1]); // n2 -> exit

    let inv = run_analysis(&pdesc);
    assert_post(&inv, n1, "1");
    assert_post(&inv, n2, "1");
}

/// OCaml: ("if", [invariant "1"; If (unknown_exp, [], []); invariant "2"])
/// An if-diamond: start -> n1 -> {left, right} -> n2 -> exit.
/// Two paths reach n2.
#[test]
fn test_if_diamond() {
    let pname = Procname::c_from_string("if_diamond");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let n1 = make_node(&mut pdesc);
    let left = make_node(&mut pdesc);
    let right = make_node(&mut pdesc);
    let n2 = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![n1]);
    pdesc.set_succs(n1, vec![left, right]);
    pdesc.set_succs(left, vec![n2]);
    pdesc.set_succs(right, vec![n2]);
    pdesc.set_succs(n2, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, n1, "1");
    assert_post(&inv, n2, "2");
}

/// OCaml: ("if_then", [If (unknown_exp, [invariant "1"], []); invariant "2"])
/// An if with only a then branch containing a node.
/// start -> {then_node, skip} -> join -> exit
#[test]
fn test_if_then() {
    let pname = Procname::c_from_string("if_then");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let then_node = make_node(&mut pdesc);
    let skip_node = make_node(&mut pdesc);
    let join = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![then_node, skip_node]);
    pdesc.set_succs(then_node, vec![join]);
    pdesc.set_succs(skip_node, vec![join]);
    pdesc.set_succs(join, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, then_node, "1");
    assert_post(&inv, join, "2");
}

/// OCaml: ("nested_if_then", ...)
/// start -> {if1_then -> {if2_left, if2_right} -> n2, skip} -> n3 -> exit
/// n2 gets 2 paths from if2, n3 gets 3 (2 from if1_then path + 1 from skip)
#[test]
fn test_nested_if() {
    // outer: start -> {inner_entry, skip}
    // inner: inner_entry -> {if2_left, if2_right} -> n2
    // join: {n2, skip} -> n3
    let pname = Procname::c_from_string("nested_if");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let inner_entry = make_node(&mut pdesc); // branch point for inner if
    let if2_left = make_node(&mut pdesc);
    let if2_right = make_node(&mut pdesc);
    let n2 = make_node(&mut pdesc);
    let skip = make_node(&mut pdesc);
    let n3 = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![inner_entry, skip]); // outer if
    pdesc.set_succs(inner_entry, vec![if2_left, if2_right]); // inner if
    pdesc.set_succs(if2_left, vec![n2]);
    pdesc.set_succs(if2_right, vec![n2]);
    pdesc.set_succs(n2, vec![n3]);
    pdesc.set_succs(skip, vec![n3]);
    pdesc.set_succs(n3, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, n2, "2"); // two paths through inner if
    assert_post(&inv, n3, "3"); // 2 from inner + 1 from skip
}

/// OCaml: ("if_diamond", [inv "1"; If; inv "2"; If; inv "4"])
/// Two sequential diamonds: paths multiply.
#[test]
fn test_double_diamond() {
    let pname = Procname::c_from_string("double_diamond");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let n1 = make_node(&mut pdesc);
    let l1 = make_node(&mut pdesc);
    let r1 = make_node(&mut pdesc);
    let n2 = make_node(&mut pdesc);
    let l2 = make_node(&mut pdesc);
    let r2 = make_node(&mut pdesc);
    let n3 = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![n1]);
    pdesc.set_succs(n1, vec![l1, r1]);
    pdesc.set_succs(l1, vec![n2]);
    pdesc.set_succs(r1, vec![n2]);
    pdesc.set_succs(n2, vec![l2, r2]);
    pdesc.set_succs(l2, vec![n3]);
    pdesc.set_succs(r2, vec![n3]);
    pdesc.set_succs(n3, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, n1, "1");
    assert_post(&inv, n2, "2");
    assert_post(&inv, n3, "4"); // 2*2 = 4 paths
}

/// OCaml: ("if_else", [If (unknown, [], [inv "1"]); inv "2"])
/// An if with only an else branch containing a node.
#[test]
fn test_if_else() {
    let pname = Procname::c_from_string("if_else");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let skip_node = make_node(&mut pdesc);
    let else_node = make_node(&mut pdesc);
    let join = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![skip_node, else_node]);
    pdesc.set_succs(skip_node, vec![join]);
    pdesc.set_succs(else_node, vec![join]);
    pdesc.set_succs(join, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, else_node, "1");
    assert_post(&inv, join, "2");
}

/// OCaml: ("if_then_else", [If (unknown, [inv "1"], [inv "1"]); inv "2"])
/// Both branches have a node.
#[test]
fn test_if_then_else() {
    let pname = Procname::c_from_string("if_then_else");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let then_node = make_node(&mut pdesc);
    let else_node = make_node(&mut pdesc);
    let join = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![then_node, else_node]);
    pdesc.set_succs(then_node, vec![join]);
    pdesc.set_succs(else_node, vec![join]);
    pdesc.set_succs(join, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, then_node, "1");
    assert_post(&inv, else_node, "1");
    assert_post(&inv, join, "2");
}

/// OCaml: ("nested_if_else", [If(unknown, [], [If(unknown,[],[]);inv "2"]); inv "3"])
/// Nested if in the else branch.
#[test]
fn test_nested_if_else() {
    let pname = Procname::c_from_string("nested_if_else");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let skip = make_node(&mut pdesc);
    let inner_entry = make_node(&mut pdesc);
    let if2_left = make_node(&mut pdesc);
    let if2_right = make_node(&mut pdesc);
    let n2 = make_node(&mut pdesc);
    let n3 = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![skip, inner_entry]);
    pdesc.set_succs(inner_entry, vec![if2_left, if2_right]);
    pdesc.set_succs(if2_left, vec![n2]);
    pdesc.set_succs(if2_right, vec![n2]);
    pdesc.set_succs(n2, vec![n3]);
    pdesc.set_succs(skip, vec![n3]);
    pdesc.set_succs(n3, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, n2, "2");
    assert_post(&inv, n3, "3");
}

/// OCaml: ("nested_if_then_else", ...)
/// Both branches have nested ifs.
/// start -> {then_if, else_if}
/// then_if -> {tl, tr} -> n_t -> join
/// else_if -> {el, er} -> n_e -> join
#[test]
fn test_nested_if_then_else() {
    let pname = Procname::c_from_string("nested_if_then_else");
    let mut pdesc2 = Procdesc::new(pname, Typ::void(), loc());

    let then_if = make_node(&mut pdesc2);
    let else_if = make_node(&mut pdesc2);
    let tl = make_node(&mut pdesc2);
    let tr = make_node(&mut pdesc2);
    let n_t = make_node(&mut pdesc2);
    let el = make_node(&mut pdesc2);
    let er = make_node(&mut pdesc2);
    let n_e = make_node(&mut pdesc2);
    let join2 = make_node(&mut pdesc2);

    pdesc2.set_succs(0, vec![then_if, else_if]);
    pdesc2.set_succs(then_if, vec![tl, tr]);
    pdesc2.set_succs(tl, vec![n_t]);
    pdesc2.set_succs(tr, vec![n_t]);
    pdesc2.set_succs(n_t, vec![join2]);
    pdesc2.set_succs(else_if, vec![el, er]);
    pdesc2.set_succs(el, vec![n_e]);
    pdesc2.set_succs(er, vec![n_e]);
    pdesc2.set_succs(n_e, vec![join2]);
    pdesc2.set_succs(join2, vec![1]);

    let inv = run_analysis(&pdesc2);
    assert_post(&inv, n_t, "2");
    assert_post(&inv, n_e, "2");
    assert_post(&inv, join2, "4"); // 2 + 2
}

/// OCaml: ("if_in_loop", [While(unknown, [If(unknown,[],[]); inv "T"]); inv "T"])
/// An if inside a loop — everything widens to Top.
#[test]
fn test_if_in_loop() {
    let pname = Procname::c_from_string("if_in_loop");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let header = make_node(&mut pdesc);
    let if_left = make_node(&mut pdesc);
    let if_right = make_node(&mut pdesc);
    let body_end = make_node(&mut pdesc);
    let after = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![header]);
    pdesc.set_succs(header, vec![if_left, if_right, after]); // branch into loop or exit
    pdesc.set_succs(if_left, vec![body_end]);
    pdesc.set_succs(if_right, vec![body_end]);
    pdesc.set_succs(body_end, vec![header]); // back edge
    pdesc.set_succs(after, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, body_end, "T");
    assert_post(&inv, after, "T");
}

/// OCaml: ("nested_loop_visit", ...)
/// Nested loops: everything widens to Top.
#[test]
fn test_nested_loop() {
    let pname = Procname::c_from_string("nested_loop");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let outer_header = make_node(&mut pdesc);
    let inner_header = make_node(&mut pdesc);
    let inner_body = make_node(&mut pdesc);
    let between = make_node(&mut pdesc);
    let after = make_node(&mut pdesc);

    pdesc.set_succs(0, vec![outer_header]);
    pdesc.set_succs(outer_header, vec![inner_header, after]);
    pdesc.set_succs(inner_header, vec![inner_body, between]);
    pdesc.set_succs(inner_body, vec![inner_header]); // inner back edge
    pdesc.set_succs(between, vec![outer_header]); // outer back edge
    pdesc.set_succs(after, vec![1]);

    let inv = run_analysis(&pdesc);
    assert_post(&inv, outer_header, "T");
    assert_post(&inv, inner_header, "T");
    assert_post(&inv, inner_body, "T");
    assert_post(&inv, after, "T");
}

/// OCaml: ("loop", [inv "1"; While (unknown, [inv "T"]); inv "T"])
/// A loop should widen to Top.
#[test]
fn test_loop_widens_to_top() {
    let pname = Procname::c_from_string("loop");
    let mut pdesc = Procdesc::new(pname, Typ::void(), loc());

    let header = make_node(&mut pdesc); // loop header
    let body = make_node(&mut pdesc); // loop body
    let after = make_node(&mut pdesc); // after loop

    pdesc.set_succs(0, vec![header]);
    pdesc.set_succs(header, vec![body, after]); // branch: body or exit loop
    pdesc.set_succs(body, vec![header]); // back edge
    pdesc.set_succs(after, vec![1]);

    let inv = run_analysis(&pdesc);
    // After enough iterations, widening should kick in and make it Top.
    assert_post(&inv, header, "T");
    assert_post(&inv, after, "T");
}
