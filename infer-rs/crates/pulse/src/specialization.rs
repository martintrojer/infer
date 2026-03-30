// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Apply specialization to an AbductiveDomain before re-analysis.
//!
//! Mirrors OCaml's `PulseSpecialization.ml`.
//!
//! When a callee needs dynamic type information (e.g., for function pointer
//! dispatch), the caller creates a `PulseSpecialization` binding heap paths
//! to known types. This module applies those bindings to the callee's
//! initial state before re-analysis.

use std::collections::{BTreeMap, HashMap};

use sil::location::Location;
use sil::procname::Procname;
use sil::pvar::Pvar;
use sil::specialization::{HeapPath, PulseSpecialization};
use sil::typ::Typ;
use sil::var::Var;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::Attribute;
use crate::formula::atom::Atom;
use crate::formula::term::Term;
use crate::pulse_result::PulseResult;

/// Apply a specialization to an AbductiveDomain.
///
/// For each dynamic type binding in the specialization, walk the heap path
/// to find or create the abstract value, then set its Closure attribute
/// to the specified procedure name.
///
/// Cross-ref: OCaml `PulseSpecialization.apply`.
pub fn apply(spec: &PulseSpecialization, state: &mut AbductiveDomain) {
    if let Some(alias_groups) = &spec.aliases {
        for alias_group in alias_groups {
            prune_eq_list_values(state, alias_group);
        }
    }
    for (heap_path, type_name) in &spec.dynamic_types {
        let val = initialize_heap_path(heap_path, state);
        // For C function pointers, the TypeName encodes the procedure name.
        // Set the Closure attribute so __call_c_function_ptr can resolve it.
        let pname = Procname::c_from_string(&format!("{type_name}"));
        state.post.attrs.add_one(val, Attribute::Closure(pname));
    }
}

fn prune_eq_list_values(state: &mut AbductiveDomain, alias_group: &[HeapPath]) {
    let values: Vec<_> = alias_group
        .iter()
        .map(|heap_path| initialize_heap_path(heap_path, state))
        .collect();
    let Some((&head, tail)) = values.split_first() else {
        return;
    };
    for &value in tail {
        let _ = state.and_condition_direct(Atom::Equal(Term::Var(head), Term::Var(value)), 1);
    }
}

/// Walk a heap path to find or create the abstract value at that position.
///
/// Cross-ref: OCaml `PulseSpecialization.initialize_heap_path`.
fn initialize_heap_path(heap_path: &HeapPath, state: &mut AbductiveDomain) -> AbstractValue {
    match heap_path {
        HeapPath::Pvar(pvar) => {
            let var = Var::ProgramVar(Box::new(pvar.clone()));
            state.eval_var(&var)
        }
        HeapPath::FieldAccess(fieldname, inner) => {
            let src = initialize_heap_path(inner, state);
            let access = Access::FieldAccess(fieldname.clone());
            state.read_heap(src, access)
        }
        HeapPath::Dereference(inner) => {
            let src = initialize_heap_path(inner, state);
            state.read_heap(src, Access::Dereference)
        }
    }
}

/// Create a specialization from known Closure attributes on actual arguments.
///
/// When a caller has Closure attributes on values that correspond to
/// heap paths the callee needs for specialization, create a
/// PulseSpecialization binding those paths to the known proc names.
///
/// Returns None if no useful specialization can be created.
pub fn make_specialization_from_caller(
    callee_needs: &HashMap<HeapPath, AbstractValue>,
    caller_state: &AbductiveDomain,
    formals: &[(Pvar, AbstractValue)],
    formal_types: &[Typ],
    actuals: &[(sil::exp::Exp, sil::typ::Typ)],
) -> Option<PulseSpecialization> {
    let aliases =
        make_alias_specialization_from_caller(caller_state, formals, formal_types, actuals);
    let mut dynamic_types = HashMap::new();

    for heap_path in callee_needs.keys() {
        // Extract the root Pvar from the heap path and find the formal index
        let root_pvar = extract_root_pvar(heap_path);
        let Some(root_pvar) = root_pvar else {
            continue;
        };

        // Find which formal this path's root Pvar corresponds to
        let formal_idx = formals.iter().position(|(pv, _)| pv == root_pvar);
        let Some(idx) = formal_idx else {
            continue;
        };

        // Evaluate the actual at that formal position.
        // We eval into a clone because eval may create new attributes
        // (e.g., Closure for Cfun constants). We check the clone for Closure.
        let Some((actual_exp, _)) = actuals.get(idx) else {
            continue;
        };
        let mut eval_state = caller_state.clone();
        let actual_val =
            match crate::operations::eval(actual_exp, &Location::dummy(), &mut eval_state) {
                PulseResult::Ok(v) | PulseResult::Recoverable(v, _) => v,
                PulseResult::FatalError(_, _) => continue,
            };

        // Check if the actual has a Closure attribute (checking the eval state
        // where the Closure was created by eval_const for Cfun).
        if let Some(pname) = eval_state.get_closure_proc_name(actual_val) {
            let type_name = sil::typ::TypeName::CStruct(
                sil::qualified_cpp_name::QualifiedCppName::from_string(format!("{pname}")),
            );
            dynamic_types.insert(heap_path.clone(), type_name);
        }
    }

    let spec = PulseSpecialization {
        aliases,
        dynamic_types,
    };
    if spec.is_bottom() {
        None
    } else {
        Some(spec)
    }
}

fn make_alias_specialization_from_caller(
    caller_state: &AbductiveDomain,
    formals: &[(Pvar, AbstractValue)],
    formal_types: &[Typ],
    actuals: &[(sil::exp::Exp, sil::typ::Typ)],
) -> Option<Vec<Vec<HeapPath>>> {
    let mut by_actual: BTreeMap<AbstractValue, Vec<HeapPath>> = BTreeMap::new();

    for (idx, (formal_pvar, _formal_addr)) in formals.iter().enumerate() {
        if !formal_types.get(idx).is_some_and(Typ::is_pointer) {
            continue;
        }
        let Some((actual_exp, _)) = actuals.get(idx) else {
            continue;
        };
        let mut eval_state = caller_state.clone();
        let actual_val =
            match crate::operations::eval(actual_exp, &Location::dummy(), &mut eval_state) {
                PulseResult::Ok(v) | PulseResult::Recoverable(v, _) => v,
                PulseResult::FatalError(_, _) => continue,
            };
        let actual_repr = eval_state.get_var_repr(actual_val);
        by_actual
            .entry(actual_repr)
            .or_default()
            .push(formal_value_heap_path(formal_pvar));
    }

    let aliases: Vec<Vec<HeapPath>> = by_actual
        .into_values()
        .filter(|paths| paths.len() > 1)
        .collect();
    if aliases.is_empty() {
        None
    } else {
        Some(aliases)
    }
}

fn formal_value_heap_path(formal_pvar: &Pvar) -> HeapPath {
    HeapPath::Dereference(Box::new(HeapPath::Pvar(formal_pvar.clone())))
}

/// Extract the root Pvar from a HeapPath.
fn extract_root_pvar(path: &HeapPath) -> Option<&Pvar> {
    match path {
        HeapPath::Pvar(pv) => Some(pv),
        HeapPath::Dereference(inner) | HeapPath::FieldAccess(_, inner) => extract_root_pvar(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::exp::Exp;
    use sil::ident::{Ident, IdentName};
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::typ::Typ;

    fn make_pdesc(name: &str, formals: &[&str]) -> Procdesc {
        let pname = Procname::c_from_string(name);
        let mut pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        pdesc.formals = formals
            .iter()
            .map(|formal| {
                (
                    Mangled::from_string(*formal),
                    Typ::mk_ptr(Typ::void()),
                    Default::default(),
                )
            })
            .collect();
        pdesc
    }

    #[test]
    fn test_apply_alias_specialization_equates_formal_values() {
        let pdesc = make_pdesc("callee", &["p", "q"]);
        let mut state = AbductiveDomain::mk_initial(&pdesc);

        let p_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(Pvar::mk(
                Mangled::from_string("p"),
                pdesc.proc_name.clone(),
            ))))
            .unwrap();
        let q_addr = state
            .post
            .stack
            .find(&Var::ProgramVar(Box::new(Pvar::mk(
                Mangled::from_string("q"),
                pdesc.proc_name.clone(),
            ))))
            .unwrap();
        let p_val = state.read_heap(p_addr, Access::Dereference);
        let q_val = state.read_heap(q_addr, Access::Dereference);
        state.add_attr(q_val, Attribute::Initialized);

        let spec = PulseSpecialization {
            aliases: Some(vec![vec![
                formal_value_heap_path(&Pvar::mk(
                    Mangled::from_string("p"),
                    pdesc.proc_name.clone(),
                )),
                formal_value_heap_path(&Pvar::mk(
                    Mangled::from_string("q"),
                    pdesc.proc_name.clone(),
                )),
            ]]),
            dynamic_types: HashMap::new(),
        };
        apply(&spec, &mut state);

        assert_eq!(state.get_var_repr(p_val), state.get_var_repr(q_val));
        assert!(state
            .post
            .attrs
            .get(&state.get_var_repr(p_val))
            .is_some_and(|attrs| attrs.contains(&Attribute::Initialized)));
        assert_eq!(
            state
                .path_condition
                .conditions()
                .get(&Atom::Equal(Term::Var(p_val), Term::Var(q_val))),
            Some(&1)
        );
    }

    #[test]
    fn test_make_specialization_from_caller_creates_alias_groups() {
        let callee_pdesc = make_pdesc("callee", &["p", "q"]);
        let caller_pdesc = Procdesc::new(
            Procname::c_from_string("caller"),
            Typ::void(),
            Location::dummy(),
        );
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let shared = AbstractValue::mk_fresh();
        let id = Ident::create_normal(IdentName::from_string("arg"), 0);
        crate::operations::write_id(&id, shared, &mut caller_state);

        let formals = vec![
            (
                Pvar::mk(Mangled::from_string("p"), callee_pdesc.proc_name.clone()),
                AbstractValue::mk_fresh(),
            ),
            (
                Pvar::mk(Mangled::from_string("q"), callee_pdesc.proc_name.clone()),
                AbstractValue::mk_fresh(),
            ),
        ];
        let formal_types = vec![Typ::mk_ptr(Typ::void()), Typ::mk_ptr(Typ::void())];
        let actuals = vec![
            (Exp::Var(id.clone()), Typ::mk_ptr(Typ::void())),
            (Exp::Var(id), Typ::mk_ptr(Typ::void())),
        ];

        let spec = make_specialization_from_caller(
            &HashMap::new(),
            &caller_state,
            &formals,
            &formal_types,
            &actuals,
        )
        .expect("expected alias specialization");

        assert_eq!(
            spec.aliases,
            Some(vec![vec![
                formal_value_heap_path(&formals[0].0),
                formal_value_heap_path(&formals[1].0)
            ]])
        );
    }
}
