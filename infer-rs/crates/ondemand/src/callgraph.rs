// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Call graph construction and scheduling.
//!
//! Builds a call graph from a `Cfg` and computes a bottom-up analysis order
//! (callees before callers). This mirrors OCaml's `SyntacticCallGraph.ml`.

use std::collections::{HashMap, HashSet, VecDeque};

use sil::cfg::Cfg;
use sil::const_val::Const;
use sil::exp::Exp;
use sil::instr::Instr;
use sil::procname::Procname;

/// A call graph: maps each procedure to the set of procedures it calls.
#[derive(Debug)]
pub struct CallGraph {
    /// caller -> set of callees
    pub edges: HashMap<Procname, HashSet<Procname>>,
    /// All known procedure names (including those without bodies)
    pub all_procs: HashSet<Procname>,
}

impl CallGraph {
    /// Build a call graph from a Cfg by scanning all Cfun references.
    ///
    /// Scans not just Call.fun_exp but ALL Cfun constants in every instruction
    /// (Store values, Call args, etc.). This captures function pointer targets
    /// from `__sil_cfun("name")` which appear as Const(Cfun) in stores/args.
    pub fn from_cfg(cfg: &Cfg) -> Self {
        let mut edges: HashMap<Procname, HashSet<Procname>> = HashMap::new();
        let mut all_procs: HashSet<Procname> = HashSet::new();

        for pdesc in cfg.iter_proc_descs() {
            let caller = &pdesc.proc_name;
            all_procs.insert(caller.clone());
            let callees = edges.entry(caller.clone()).or_default();

            for (_node_id, instr) in pdesc.iter_instrs() {
                collect_cfun_from_instr(instr, callees, &mut all_procs);
            }
        }

        Self { edges, all_procs }
    }

    /// Get the callees of a procedure.
    pub fn callees(&self, proc: &Procname) -> impl Iterator<Item = &Procname> {
        self.edges.get(proc).into_iter().flatten()
    }

    /// Count the number of defined callees each defined procedure still depends on.
    ///
    /// External callees are treated as already analyzed, matching OCaml's
    /// syntactic callgraph scheduler.
    pub fn defined_dependency_counts(
        &self,
        defined: &HashSet<Procname>,
    ) -> HashMap<Procname, usize> {
        let mut dep_counts = HashMap::with_capacity(defined.len());
        for proc in defined {
            let num_defined_callees = self.callees(proc).filter(|c| defined.contains(*c)).count();
            dep_counts.insert(proc.clone(), num_defined_callees);
        }
        dep_counts
    }

    /// Build the reverse defined-callgraph: callee -> sorted callers.
    pub fn callers_of_defined(
        &self,
        defined: &HashSet<Procname>,
    ) -> HashMap<Procname, Vec<Procname>> {
        let mut callers_of: HashMap<Procname, Vec<Procname>> = HashMap::new();
        for caller in defined {
            for callee in self.callees(caller) {
                if defined.contains(callee) {
                    callers_of
                        .entry(callee.clone())
                        .or_default()
                        .push(caller.clone());
                }
            }
        }
        for callers in callers_of.values_mut() {
            callers.sort_by(|a, b| format!("{a}").cmp(&format!("{b}")));
        }
        callers_of
    }

    /// Pick one deterministic cycle cut from the remaining defined procedures.
    ///
    /// Cross-ref: OCaml `CallGraphScheduler.bottom_up` eventually reaches a
    /// state where no more leaves are available and only cycles remain. The
    /// Rust runner uses this helper to cut one SCC at a time while preserving
    /// dynamic leaf-driven scheduling elsewhere.
    pub fn cycle_cut(&self, remaining: &HashSet<Procname>) -> Vec<Procname> {
        let successors = |proc: &Procname| {
            self.callees(proc)
                .filter(|c| remaining.contains(*c))
                .cloned()
        };
        let mut remaining_sorted: Vec<_> = remaining.iter().cloned().collect();
        remaining_sorted.sort_by(|a, b| format!("{a}").cmp(&format!("{b}")));
        for start in &remaining_sorted {
            let scc = find_one_cycle_from(start, remaining, &successors);
            let has_self_edge = self.callees(start).any(|callee| callee == start);
            if scc.len() > 1 || has_self_edge {
                let mut cut = scc;
                cut.sort_by_cached_key(|p| format!("{p}"));
                return cut;
            }
        }

        vec![remaining_sorted.into_iter().next().unwrap()]
    }

    /// Compute a bottom-up schedule: procedures with no unanalyzed callees first.
    ///
    /// Returns a list of "waves" — each wave contains procedures that can be
    /// analyzed in parallel (all their callees are in earlier waves).
    /// Procedures in cycles are placed in the final wave.
    pub fn bottom_up_schedule(&self, defined: &HashSet<Procname>) -> Vec<Vec<Procname>> {
        let mut waves = Vec::new();
        let mut analyzed: HashSet<Procname> = HashSet::new();
        let mut remaining: HashSet<Procname> = defined.clone();

        // External callees (not defined in this Cfg) are considered already analyzed
        for proc in &self.all_procs {
            if !defined.contains(proc) {
                analyzed.insert(proc.clone());
            }
        }

        while !remaining.is_empty() {
            // Find all procedures whose callees are all analyzed
            let mut ready: Vec<Procname> = remaining
                .iter()
                .filter(|proc| self.callees(proc).all(|callee| analyzed.contains(callee)))
                .cloned()
                .collect();
            // Sort for deterministic wave ordering regardless of HashSet iteration order.
            ready.sort_by(|a, b| format!("{a}").cmp(&format!("{b}")));

            if ready.is_empty() {
                let wave_procs = self.cycle_cut(&remaining);
                for proc in &wave_procs {
                    remaining.remove(proc);
                    analyzed.insert(proc.clone());
                }
                let mut wave = wave_procs;
                wave.sort_by_cached_key(|p| format!("{p}"));
                waves.push(wave);
                continue;
            }

            for proc in &ready {
                remaining.remove(proc);
                analyzed.insert(proc.clone());
            }

            let mut wave = ready;
            wave.sort_by_cached_key(|p| format!("{p}"));
            waves.push(wave);
        }

        waves
    }

    /// Compute a simple topological order (one procedure at a time).
    /// Falls back to arbitrary order for cycles.
    pub fn topological_order(&self, defined: &HashSet<Procname>) -> Vec<Procname> {
        // Kahn's algorithm
        let mut in_degree: HashMap<&Procname, usize> = HashMap::new();
        for proc in defined {
            in_degree.entry(proc).or_insert(0);
            for callee in self.callees(proc) {
                if defined.contains(callee) {
                    *in_degree.entry(callee).or_insert(0) += 1;
                }
            }
        }

        // Bottom-up: count how many defined callees each proc has.
        let mut dep_count: HashMap<&Procname, usize> = HashMap::new();
        for proc in defined {
            let num_defined_callees = self.callees(proc).filter(|c| defined.contains(*c)).count();
            dep_count.insert(proc, num_defined_callees);
        }

        // Build reverse edge map: callee → Vec<caller> (O(|E|) once, avoids O(|V|²) scan)
        let mut callers_of: HashMap<&Procname, Vec<&Procname>> = HashMap::new();
        for proc in defined {
            for callee in self.callees(proc) {
                if defined.contains(callee) {
                    callers_of.entry(callee).or_default().push(proc);
                }
            }
        }

        let mut queue: VecDeque<Procname> = dep_count
            .iter()
            .filter(|(_, &count)| count == 0)
            .map(|(&proc, _)| proc.clone())
            .collect();

        let mut order = Vec::new();
        let mut visited = HashSet::new();

        while let Some(proc) = queue.pop_front() {
            if !visited.insert(proc.clone()) {
                continue;
            }
            order.push(proc.clone());

            // For each caller of `proc`, decrement their dependency count
            if let Some(callers) = callers_of.get(&proc) {
                for caller in callers {
                    if visited.contains(*caller) {
                        continue;
                    }
                    if let Some(count) = dep_count.get_mut(*caller) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            queue.push_back((*caller).clone());
                        }
                    }
                }
            }
        }

        // Add any remaining (cyclic) procedures
        for proc in defined {
            if !visited.contains(proc) {
                order.push(proc.clone());
            }
        }

        order
    }
}

/// Collect all Cfun references from a SIL instruction.
fn collect_cfun_from_instr(
    instr: &Instr,
    callees: &mut HashSet<Procname>,
    all_procs: &mut HashSet<Procname>,
) {
    match instr {
        Instr::Call { fun_exp, args, .. } => {
            collect_cfun_from_exp(fun_exp, callees, all_procs);
            for (arg_exp, _) in args {
                collect_cfun_from_exp(arg_exp, callees, all_procs);
            }
        }
        Instr::Store { e1, e2, .. } => {
            collect_cfun_from_exp(e1, callees, all_procs);
            collect_cfun_from_exp(e2, callees, all_procs);
        }
        Instr::Load { e, .. } => {
            collect_cfun_from_exp(e, callees, all_procs);
        }
        Instr::Prune { exp, .. } => {
            collect_cfun_from_exp(exp, callees, all_procs);
        }
        Instr::Metadata(_) => {}
    }
}

/// Recursively collect all Cfun references from a SIL expression.
fn collect_cfun_from_exp(
    exp: &Exp,
    callees: &mut HashSet<Procname>,
    all_procs: &mut HashSet<Procname>,
) {
    match exp {
        Exp::Const(Const::Cfun(pname)) => {
            callees.insert(pname.clone());
            all_procs.insert(pname.clone());
        }
        Exp::BinOp(_, l, r) | Exp::Lindex(l, r) => {
            collect_cfun_from_exp(l, callees, all_procs);
            collect_cfun_from_exp(r, callees, all_procs);
        }
        Exp::UnOp(_, inner, _) | Exp::Cast(_, inner) | Exp::Exn(inner) => {
            collect_cfun_from_exp(inner, callees, all_procs);
        }
        Exp::Lfield(data, _, _) => {
            collect_cfun_from_exp(&data.exp, callees, all_procs);
        }
        _ => {}
    }
}

/// Find the SCC containing `start` in the remaining set.
///
/// Strategy: find all nodes reachable from start (forward DFS),
/// then find all nodes that can reach start (backward DFS on reverse edges).
/// The intersection is the SCC containing start.
fn find_one_cycle_from<F, I>(
    start: &Procname,
    remaining: &HashSet<Procname>,
    successors: &F,
) -> Vec<Procname>
where
    F: Fn(&Procname) -> I,
    I: Iterator<Item = Procname>,
{
    let start = start.clone();

    // Forward reachability from start
    let mut forward = HashSet::new();
    let mut stack = vec![start.clone()];
    while let Some(node) = stack.pop() {
        if !forward.insert(node.clone()) {
            continue;
        }
        for succ in successors(&node) {
            if remaining.contains(&succ) && !forward.contains(&succ) {
                stack.push(succ);
            }
        }
    }

    // Build reverse edges for backward reachability
    let mut reverse: HashMap<Procname, Vec<Procname>> = HashMap::new();
    for node in &forward {
        for succ in successors(node) {
            if forward.contains(&succ) {
                reverse.entry(succ).or_default().push(node.clone());
            }
        }
    }

    // Backward reachability from start
    let mut backward = HashSet::new();
    let mut stack = vec![start];
    while let Some(node) = stack.pop() {
        if !backward.insert(node.clone()) {
            continue;
        }
        if let Some(preds) = reverse.get(&node) {
            for pred in preds {
                if !backward.contains(pred) {
                    stack.push(pred.clone());
                }
            }
        }
    }

    // SCC = forward ∩ backward
    let scc: Vec<_> = forward.intersection(&backward).cloned().collect();

    // A single-node "SCC" with no self-edge is not a real cycle — it's just
    // a node whose dependencies aren't resolved. Return it anyway so the
    // scheduler makes progress.
    scc
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::call_flags::CallFlags;
    use sil::const_val::Const;
    use sil::exp::Exp;
    use sil::ident::{Ident, IdentName};
    use sil::instr::Instr;
    use sil::location::Location;
    use sil::procdesc::{NodeKind, Procdesc, StmtNodeKind};
    use sil::typ::Typ;

    fn mk_proc_with_calls(name: &str, callees: &[&str]) -> Procdesc {
        let pname = Procname::c_from_string(name);
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());

        let instrs: Vec<Instr> = callees
            .iter()
            .enumerate()
            .map(|(i, callee)| Instr::Call {
                ret: (
                    Ident::create_normal(IdentName::from_string("n"), i as i32),
                    Typ::void(),
                ),
                fun_exp: Exp::Const(Const::Cfun(Procname::c_from_string(callee))),
                args: vec![],
                loc: Location::dummy(),
                flags: CallFlags::default(),
            })
            .collect();

        let node = pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            instrs,
            Location::dummy(),
        );
        pdesc.set_succs(0, vec![node]);
        pdesc.set_succs(node, vec![1]);
        pdesc
    }

    #[test]
    fn test_callgraph_construction() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_proc_with_calls("main", &["foo", "bar"]));
        cfg.add_proc_desc(mk_proc_with_calls("foo", &["bar"]));
        cfg.add_proc_desc(mk_proc_with_calls("bar", &[]));

        let cg = CallGraph::from_cfg(&cfg);
        let main_callees: Vec<_> = cg.callees(&Procname::c_from_string("main")).collect();
        assert_eq!(main_callees.len(), 2);
        let bar_callees: Vec<_> = cg.callees(&Procname::c_from_string("bar")).collect();
        assert!(bar_callees.is_empty());
    }

    #[test]
    fn test_bottom_up_schedule() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_proc_with_calls("main", &["foo", "bar"]));
        cfg.add_proc_desc(mk_proc_with_calls("foo", &["bar"]));
        cfg.add_proc_desc(mk_proc_with_calls("bar", &[]));

        let cg = CallGraph::from_cfg(&cfg);
        let defined: HashSet<_> = cfg.proc_descs.keys().cloned().collect();
        let waves = cg.bottom_up_schedule(&defined);

        // Wave 0: bar (no callees)
        // Wave 1: foo (calls bar, which is in wave 0)
        // Wave 2: main (calls foo and bar, both in earlier waves)
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0].len(), 1);
        assert_eq!(waves[0][0], Procname::c_from_string("bar"));
        assert_eq!(waves[1].len(), 1);
        assert_eq!(waves[1][0], Procname::c_from_string("foo"));
        assert_eq!(waves[2].len(), 1);
        assert_eq!(waves[2][0], Procname::c_from_string("main"));
    }

    #[test]
    fn test_schedule_with_cycle() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_proc_with_calls("a", &["b"]));
        cfg.add_proc_desc(mk_proc_with_calls("b", &["a"]));
        cfg.add_proc_desc(mk_proc_with_calls("main", &["a"]));

        let cg = CallGraph::from_cfg(&cfg);
        let defined: HashSet<_> = cfg.proc_descs.keys().cloned().collect();
        let waves = cg.bottom_up_schedule(&defined);

        // All 3 procedures should be scheduled
        let total: usize = waves.iter().map(|w| w.len()).sum();
        assert_eq!(total, 3);

        // a and b should be in the same wave (they form a cycle)
        let a = Procname::c_from_string("a");
        let b = Procname::c_from_string("b");
        let a_wave = waves.iter().position(|w| w.contains(&a)).unwrap();
        let b_wave = waves.iter().position(|w| w.contains(&b)).unwrap();
        assert_eq!(a_wave, b_wave, "a and b should be in the same wave (cycle)");

        // main should be in a later wave than the cycle (or same wave if
        // the SCC finder happened to start from main)
        let main = Procname::c_from_string("main");
        let main_wave = waves.iter().position(|w| w.contains(&main)).unwrap();
        assert!(
            main_wave >= a_wave,
            "main should be in same or later wave than the cycle"
        );
    }

    #[test]
    fn test_schedule_self_recursive_callee_before_caller() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_proc_with_calls("self_rec", &["self_rec"]));
        cfg.add_proc_desc(mk_proc_with_calls("caller", &["self_rec"]));

        let cg = CallGraph::from_cfg(&cfg);
        let defined: HashSet<_> = cfg.proc_descs.keys().cloned().collect();
        let waves = cg.bottom_up_schedule(&defined);

        assert_eq!(waves.len(), 2);
        assert_eq!(waves[0], vec![Procname::c_from_string("self_rec")]);
        assert_eq!(waves[1], vec![Procname::c_from_string("caller")]);
    }

    #[test]
    fn test_schedule_independent_procs() {
        let mut cfg = sil::cfg::Cfg::new();
        cfg.add_proc_desc(mk_proc_with_calls("a", &[]));
        cfg.add_proc_desc(mk_proc_with_calls("b", &[]));
        cfg.add_proc_desc(mk_proc_with_calls("c", &[]));

        let cg = CallGraph::from_cfg(&cfg);
        let defined: HashSet<_> = cfg.proc_descs.keys().cloned().collect();
        let waves = cg.bottom_up_schedule(&defined);

        // All independent — one wave with all three
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].len(), 3);
    }
}
