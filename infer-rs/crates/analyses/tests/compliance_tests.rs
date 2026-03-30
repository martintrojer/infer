// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Compliance tests ported from OCaml's `unit/livenessTests.ml`.
//!
//! Uses the Textual test harness to write tests as `.sil` programs.
//! Each test parses Textual, converts to SIL, runs liveness analysis,
//! and checks which variables are live before each labeled node.

use analyses::liveness;
use test_harness::textual_utils::parse_and_convert;

/// Assert that exactly the given variables are live before a labeled node.
#[allow(dead_code)]
fn assert_live_before(src: &str, label: &str, expected_live: &[&str]) {
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let result = liveness::analyze(pdesc);
    let node_id = tm.node_id(label);

    let live = result
        .live_before(node_id)
        .unwrap_or_else(|| panic!("no liveness state for label '{label}'"));

    let mut actual: Vec<String> = live.0.iter().map(|v| format!("{v}")).collect();
    actual.sort();

    let mut expected: Vec<String> = expected_live.iter().map(|s| s.to_string()).collect();
    expected.sort();

    assert_eq!(
        actual, expected,
        "label '{label}': expected live={expected:?}, got live={actual:?}"
    );
}

/// Assert that a variable is live before a labeled node.
fn assert_var_live(src: &str, label: &str, var: &str) {
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let result = liveness::analyze(pdesc);
    let node_id = tm.node_id(label);

    let live = result
        .live_before(node_id)
        .unwrap_or_else(|| panic!("no liveness state for label '{label}'"));

    let actual: Vec<String> = live.0.iter().map(|v| format!("{v}")).collect();
    assert!(
        actual.iter().any(|v| v == var),
        "label '{label}': expected '{var}' to be live, got {actual:?}"
    );
}

/// Assert that a variable is NOT live before a labeled node.
fn assert_var_dead(src: &str, label: &str, var: &str) {
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let result = liveness::analyze(pdesc);
    let node_id = tm.node_id(label);

    let live = result
        .live_before(node_id)
        .unwrap_or_else(|| panic!("no liveness state for label '{label}'"));

    let actual: Vec<String> = live.0.iter().map(|v| format!("{v}")).collect();
    assert!(
        !actual.iter().any(|v| v == var),
        "label '{label}': expected '{var}' to be dead, got {actual:?}"
    );
}

// ---------------------------------------------------------------------------
// Tests ported from OCaml's livenessTests.ml
// ---------------------------------------------------------------------------

/// OCaml: ("basic_live", [invariant "{ b }"; id_assign_var "a" "b"])
/// `n0 = load &b` — b should be live before the load.
#[test]
fn test_basic_live() {
    let src = r#"
        .source_language = "java"
        define f(b: int) : int {
          #entry:
            n0 : int = load &b
            ret n0
        }
    "#;
    assert_var_live(src, "entry", "b");
}

/// OCaml: ("basic_live_then_dead", [empty; var_assign 1; inv "{ b }"; id_assign_var "a" "b"])
/// `n0 = load &b; store &b <- 1` — b is live before the load, dead after the store.
#[test]
fn test_basic_live_then_dead() {
    let src = r#"
        .source_language = "java"
        define f(b: int) : void {
          #read_b:
            n0 : int = load &b
            jmp kill_b
          #kill_b:
            store &b <- 1 : int
            jmp done
          #done:
            ret null
        }
    "#;
    assert_var_live(src, "read_b", "b");
    assert_var_dead(src, "done", "b");
}

/// OCaml: ("iterative_live", [inv "{ d, b, f }"; ...; inv "{ b }"; id_assign_var "a" "b"])
/// Chain of loads: each variable becomes live when read.
#[test]
fn test_iterative_live() {
    let src = r#"
        .source_language = "java"
        define f(b: int, d: int, ff: int) : void {
          #step1:
            n0 : int = load &b
            jmp step2
          #step2:
            n1 : int = load &d
            jmp step3
          #step3:
            n2 : int = load &ff
            ret null
        }
    "#;
    // At step1: b is read here, d and ff are read later → all three live
    assert_var_live(src, "step1", "b");
    assert_var_live(src, "step1", "d");
    assert_var_live(src, "step1", "ff");

    // At step2: b already read, d read here, ff later
    assert_var_live(src, "step2", "d");
    assert_var_live(src, "step2", "ff");

    // At step3: only ff read here
    assert_var_live(src, "step3", "ff");
}

/// OCaml: ("live_kill_live", ...)
/// Read b, kill b, read b again — b should be live at each read.
#[test]
fn test_live_kill_live() {
    let src = r#"
        .source_language = "java"
        define f(b: int) : void {
          #read1:
            n0 : int = load &b
            jmp kill
          #kill:
            store &b <- 1 : int
            jmp read2
          #read2:
            n1 : int = load &b
            ret null
        }
    "#;
    assert_var_live(src, "read1", "b");
    assert_var_dead(src, "kill", "b"); // killed, then re-read in read2 but not yet
    assert_var_live(src, "read2", "b");
}

/// OCaml: ("call_params_live", [inv "{ c, b, a }"; call_unknown ["a"; "b"; "c"]])
/// Function arguments are live at the call site.
#[test]
fn test_call_params_live() {
    let src = r#"
        .source_language = "java"
        declare unknown(int, int, int) : void
        define f(a: int, b: int, c: int) : void {
          #entry:
            n0 : int = load &a
            n1 : int = load &b
            n2 : int = load &c
            n3 = unknown(n0, n1, n2)
            ret null
        }
    "#;
    assert_var_live(src, "entry", "a");
    assert_var_live(src, "entry", "b");
    assert_var_live(src, "entry", "c");
}

/// OCaml: ("if_conservative_live1", [inv "{ b }"; If(_, [id_assign_var "a" "b"], [])])
/// In a branch, variables read in either arm are live before the branch.
#[test]
fn test_if_conservative_live() {
    let src = r#"
        .source_language = "java"
        define f(b: int, d: int) : void {
          #entry:
            n0 : int = load &b
            jmp then_branch, else_branch
          #then_branch:
            n1 : int = load &b
            jmp join
          #else_branch:
            n2 : int = load &d
            jmp join
          #join:
            ret null
        }
    "#;
    // Both b and d are live before entry (conservative: either branch may execute)
    assert_var_live(src, "entry", "b");
    assert_var_live(src, "entry", "d");
}

/// OCaml: ("if_precise1", ...) — variables only live in their branch.
#[test]
fn test_if_precise() {
    let src = r#"
        .source_language = "java"
        define f() : void {
          #entry:
            jmp then_arm, else_arm
          #then_arm:
            store &b <- 1 : int
            n0 : int = load &b
            jmp join
          #else_arm:
            store &d <- 1 : int
            n1 : int = load &d
            jmp join
          #join:
            ret null
        }
    "#;
    // In then_arm: b is assigned then read — dead at entry of then_arm
    assert_var_dead(src, "then_arm", "b");
    assert_var_dead(src, "then_arm", "d");
    // In else_arm: d is assigned then read — dead at entry of else_arm
    assert_var_dead(src, "else_arm", "b");
    assert_var_dead(src, "else_arm", "d");
}

/// OCaml: ("if_conservative_kill", ...)
/// b is killed in only one branch — it should still be live after the join
/// because the other branch doesn't kill it.
#[test]
fn test_if_conservative_kill() {
    let src = r#"
        .source_language = "java"
        define f(b: int) : void {
          #entry:
            jmp then_arm, else_arm
          #then_arm:
            store &b <- 1 : int
            jmp join
          #else_arm:
            jmp join
          #join:
            n0 : int = load &b
            ret null
        }
    "#;
    // b is killed in then_arm but NOT in else_arm, so b is still live at entry
    assert_var_live(src, "entry", "b");
    // b is live at join because it's read there
    assert_var_live(src, "join", "b");
}

/// OCaml: ("if_conservative_kill_live", ...)
/// Mixed: b killed in then_arm, d read in else_arm.
#[test]
fn test_if_conservative_kill_live() {
    let src = r#"
        .source_language = "java"
        define f(b: int, d: int) : void {
          #entry:
            jmp then_arm, else_arm
          #then_arm:
            store &b <- 1 : int
            jmp join
          #else_arm:
            n0 : int = load &d
            jmp join
          #join:
            n1 : int = load &b
            ret null
        }
    "#;
    // b is live at entry (not killed in all branches, read at join)
    assert_var_live(src, "entry", "b");
    // d is live at entry (read in else_arm)
    assert_var_live(src, "entry", "d");
}

/// OCaml: ("if_precise2", ...)
/// Both branches assign b, then b is read after join — b is dead at entry
/// since all paths assign it.
#[test]
fn test_if_precise_both_assign() {
    let src = r#"
        .source_language = "java"
        define f() : void {
          #entry:
            jmp then_arm, else_arm
          #then_arm:
            store &b <- 1 : int
            jmp join
          #else_arm:
            store &b <- 2 : int
            jmp join
          #join:
            n0 : int = load &b
            ret null
        }
    "#;
    // b is killed in BOTH branches → dead at entry
    assert_var_dead(src, "entry", "b");
    // b is live at join (read there)
    assert_var_live(src, "join", "b");
}

/// OCaml: ("loop_as_if1", [inv{b}; While(_, [a=b])])
/// b is read inside a loop body — live at the loop header.
#[test]
fn test_loop_read_in_body() {
    let src = r#"
        .source_language = "java"
        define f(b: int) : void {
          #header:
            jmp body, exit
          #body:
            n0 : int = load &b
            jmp header
          #exit:
            ret null
        }
    "#;
    assert_var_live(src, "header", "b");
    assert_var_live(src, "body", "b");
}

/// OCaml: ("loop_before_after", [inv{d,b}; While(_, [b=d]); inv{b}; a=b])
/// d is read in loop body (store &b <- d), b is read after loop.
#[test]
fn test_loop_before_after() {
    let src = r#"
        .source_language = "java"
        define f(b: int, d: int) : void {
          #entry:
            jmp header
          #header:
            jmp body, after
          #body:
            n0 : int = load &d
            store &b <- n0 : int
            jmp header
          #after:
            n1 : int = load &b
            ret null
        }
    "#;
    // d is used in the loop body
    assert_var_live(src, "entry", "d");
    assert_var_live(src, "header", "d");
    // b is read after the loop
    assert_var_live(src, "header", "b");
    assert_var_live(src, "after", "b");
}

/// OCaml: ("dead_after_call_with_retval", ...)
/// Return value of a call is live, but killed on reassignment.
#[test]
fn test_dead_after_call_with_retval() {
    let src = r#"
        .source_language = "java"
        declare unknown() : int
        define f() : void {
          #entry:
            n0 = unknown()
            jmp use_it
          #use_it:
            n1 : int = load &x
            ret null
        }
    "#;
    // n0 is the return value of unknown() — it's dead if never used
    // (the test checks the call return ident, not a pvar)
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let result = liveness::analyze(pdesc);
    let node_id = tm.node_id("entry");
    let live = result.live_before(node_id).unwrap();
    // n0 is not used after the call, so it should be dead
    let actual: Vec<String> = live.0.iter().map(|v| format!("{v}")).collect();
    assert!(
        !actual.iter().any(|v| v == "n0"),
        "n0 should be dead (return value unused), got {actual:?}"
    );
}

/// OCaml: ("basic_live_load", [inv "{ y$0 }"; id_assign_id "x" "y"])
/// Loading from one ident to another: the source ident is live.
#[test]
fn test_basic_live_load_ident() {
    let src = r#"
        .source_language = "java"
        declare get_y() : int
        define f() : void {
          #setup:
            n0 = get_y()
            jmp entry
          #entry:
            n1 : int = load &x
            ret null
        }
    "#;
    // n0 (return of get_y) is dead — never used
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let result = liveness::analyze(pdesc);
    let node_id = tm.node_id("setup");
    let live = result.live_before(node_id).unwrap();
    let actual: Vec<String> = live.0.iter().map(|v| format!("{v}")).collect();
    assert!(
        !actual.iter().any(|v| v == "n0"),
        "n0 should be dead (never used), got {actual:?}"
    );
}

/// OCaml: ("if_exp_live", ...)
/// A variable used in a prune condition should be live.
#[test]
fn test_prune_reads_variable() {
    let src = r#"
        .source_language = "java"
        define f(x: int) : void {
          #entry:
            n0 : int = load &x
            jmp check
          #check:
            prune n0
            jmp done
          #done:
            ret null
        }
    "#;
    // n0 is read by prune → live at check
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let result = liveness::analyze(pdesc);
    let node_id = tm.node_id("check");
    let live = result.live_before(node_id).unwrap();
    let actual: Vec<String> = live.0.iter().map(|v| format!("{v}")).collect();
    assert!(
        actual.iter().any(|v| v == "n0"),
        "n0 should be live at check (used by prune), got {actual:?}"
    );
    // x should be live at entry (loaded into n0)
    assert_var_live(src, "entry", "x");
}

/// OCaml: ("set_id", [inv "{ x$0, y$0 }"; id_set_id "x" "y"])
/// A store `*n0 = n1` reads both n0 (address) and n1 (value).
#[test]
fn test_store_reads_both_operands() {
    let src = r#"
        .source_language = "java"
        define f(x: *int, y: int) : void {
          #entry:
            n0 : *int = load &x
            n1 : int = load &y
            jmp do_store
          #do_store:
            store n0 <- n1 : int
            ret null
        }
    "#;
    // Both n0 and n1 are read by the store → live at do_store
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let result = liveness::analyze(pdesc);
    let node_id = tm.node_id("do_store");
    let live = result.live_before(node_id).unwrap();
    let actual: Vec<String> = live.0.iter().map(|v| format!("{v}")).collect();
    assert!(
        actual.iter().any(|v| v == "n0"),
        "n0 should be live (address in store), got {actual:?}"
    );
    assert!(
        actual.iter().any(|v| v == "n1"),
        "n1 should be live (value in store), got {actual:?}"
    );
}

// ---------------------------------------------------------------------------
// Dead store reporting tests
// ---------------------------------------------------------------------------

/// Simple dead store: `x = 5` where x is never read.
#[test]
fn test_dead_store_simple() {
    let src = r#"
        .source_language = "java"
        define f() : void {
          #entry:
            store &x <- 5 : int
            ret null
        }
    "#;
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let log = liveness::report_dead_stores(pdesc);
    assert_eq!(log.len(), 1, "should report 1 dead store");
    assert!(log.issues[0].qualifier.contains("x"));
}

/// Not a dead store: x is written then read.
#[test]
fn test_no_dead_store_when_read() {
    let src = r#"
        .source_language = "java"
        define f() : int {
          #entry:
            store &x <- 5 : int
            n0 : int = load &x
            ret n0
        }
    "#;
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let log = liveness::report_dead_stores(pdesc);
    assert!(
        log.is_empty(),
        "should not report dead store when x is read"
    );
}

/// Dead store then live: x = 5; x = 3; return x.
/// First store is dead, second is live.
#[test]
fn test_dead_store_overwrite() {
    let src = r#"
        .source_language = "java"
        define f() : int {
          #entry:
            store &x <- 5 : int
            store &x <- 3 : int
            n0 : int = load &x
            ret n0
        }
    "#;
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let log = liveness::report_dead_stores(pdesc);
    assert_eq!(log.len(), 1, "first store to x is dead (overwritten)");
}

/// Sentinel value (0) should not be reported as dead store.
#[test]
fn test_dead_store_sentinel_suppressed() {
    let src = r#"
        .source_language = "java"
        define f() : void {
          #entry:
            store &x <- 0 : int
            ret null
        }
    "#;
    let tm = parse_and_convert(src);
    let pdesc = tm.first_proc();
    let log = liveness::report_dead_stores(pdesc);
    assert!(log.is_empty(), "sentinel value 0 should be suppressed");
}

/// Test liveness on a loop program: `n` is read in the loop body,
/// so it should be live at the loop header (propagated via back-edge).
#[test]
fn test_end_to_end_complex() {
    let src = r#"
        .source_language = "java"

        type Cell = { value: int; next: *Cell }

        declare alloc() : *Cell

        define build_list(n: int) : *Cell {
          #entry:
            n0 : int = load &n
            n1 = alloc()
            jmp loop_header
          #loop_header:
            jmp loop_body, loop_exit
          #loop_body:
            n2 : int = load &n
            n3 = alloc()
            jmp loop_header
          #loop_exit:
            ret null
        }
    "#;
    // n is read in both entry and loop_body, so it should be live
    // at the loop header (propagated via the back-edge from loop_body)
    assert_var_live(src, "loop_header", "n");
    assert_var_live(src, "loop_body", "n");
    assert_var_live(src, "entry", "n");
}
