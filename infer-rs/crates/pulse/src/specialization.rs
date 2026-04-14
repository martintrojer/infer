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
use sil::typ::{Typ, TypeDesc, TypeName};
use sil::var::Var;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::pulse_result::PulseResult;

/// Apply a specialization to an AbductiveDomain.
///
/// For each dynamic type binding in the specialization, walk the heap path
/// to find or create the abstract value, then add the corresponding dynamic
/// type constraint to the specialized state.
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
        state.add_dynamic_type_unsafe(val, Typ::mk_struct(type_name.clone()));
    }
}

fn procname_to_dynamic_type_name(pname: &Procname) -> Option<TypeName> {
    match pname {
        Procname::C(sig) => Some(TypeName::CFunction(sig.clone())),
        Procname::Block(sig) => Some(TypeName::ObjcBlock(sig.clone())),
        _ => None,
    }
}

fn procname_from_dynamic_type_name(type_name: &TypeName) -> Option<Procname> {
    match type_name {
        TypeName::CFunction(sig) => Some(Procname::C(sig.clone())),
        TypeName::ObjcBlock(sig) => Some(Procname::Block(sig.clone())),
        _ => None,
    }
}

fn dynamic_type_name_for_value(state: &AbductiveDomain, value: AbstractValue) -> Option<TypeName> {
    if let Some(typ) = state.get_dynamic_type(value) {
        if let TypeDesc::Tstruct(type_name) = typ.desc.as_ref() {
            return Some(type_name.clone());
        }
    }
    state
        .get_closure_proc_name(value)
        .and_then(procname_to_dynamic_type_name)
}

pub(crate) fn resolve_procname_for_value(
    state: &AbductiveDomain,
    value: AbstractValue,
) -> Option<Procname> {
    dynamic_type_name_for_value(state, value)
        .and_then(|type_name| procname_from_dynamic_type_name(&type_name))
        .or_else(|| state.get_closure_proc_name(value).cloned())
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
        let _ = state.and_equal(head, value);
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

        // Cross-ref: OCaml specialization is keyed by the full callee heap
        // path, not just by the root actual. For example, callbacks require
        // looking through `formal->field` to find the closure stored there.
        let Some(needed_val) =
            actual_value_for_callee_heap_path(heap_path, actual_val, &eval_state)
        else {
            continue;
        };

        // OCaml specialization keys use dynamic type information, not
        // exported `Closure(...)` attrs. Fall back to Closure only for direct
        // constants/closures where the caller has not materialized a separate
        // dynamic-type fact.
        if let Some(type_name) = dynamic_type_name_for_value(&eval_state, needed_val) {
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

/// Translate a callee heap path rooted at a formal into the corresponding
/// caller-side value reached from the actual argument.
///
/// Actual arguments represent the value passed to the callee, not the callee's
/// stack slot for the formal. Therefore a leading `Dereference(Pvar(formal))`
/// in the callee heap path corresponds to the actual value itself.
///
/// Cross-ref: OCaml `PulseInterproc` tracks dynamic-type needs by heap path,
/// then resolves those paths in the caller state before requesting
/// specialization.
fn actual_value_for_callee_heap_path(
    heap_path: &HeapPath,
    actual_val: AbstractValue,
    caller_state: &AbductiveDomain,
) -> Option<AbstractValue> {
    match heap_path {
        HeapPath::Pvar(_) => Some(actual_val),
        HeapPath::Dereference(inner) => {
            if matches!(inner.as_ref(), HeapPath::Pvar(_)) {
                Some(actual_val)
            } else {
                let base = actual_value_for_callee_heap_path(inner, actual_val, caller_state)?;
                caller_state.post.heap.find_edge(base, &Access::Dereference)
            }
        }
        HeapPath::FieldAccess(field, inner) => {
            let base = actual_value_for_callee_heap_path(inner, actual_val, caller_state)?;
            caller_state
                .post
                .heap
                .find_edge(base, &Access::FieldAccess(field.clone()))
        }
    }
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
    use crate::attribute::Attribute;
    use sil::exp::Exp;
    use sil::fieldname::Fieldname;
    use sil::ident::{Ident, IdentName};
    use sil::location::Location;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::qualified_cpp_name::QualifiedCppName;
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
        assert!(
            state.path_condition.conditions().is_empty(),
            "specialization equalities should be baked into phi, not exported as caller conditions"
        );
    }

    #[test]
    fn test_apply_dynamic_type_specialization_sets_dynamic_type_without_closure_attr() {
        let pdesc = make_pdesc("callee", &["f"]);
        let mut state = AbductiveDomain::mk_initial(&pdesc);
        let formal = Pvar::mk(Mangled::from_string("f"), pdesc.proc_name.clone());
        let funptr_val = state.read_heap(
            state
                .post
                .stack
                .find(&Var::ProgramVar(Box::new(formal.clone())))
                .expect("formal should be bound"),
            Access::Dereference,
        );
        let target = Procname::c_from_string("assign_NULL");
        let spec = PulseSpecialization {
            aliases: None,
            dynamic_types: HashMap::from([(
                formal_value_heap_path(&formal),
                TypeName::CFunction(match target {
                    Procname::C(sig) => sig,
                    _ => unreachable!("c procname expected"),
                }),
            )]),
        };

        apply(&spec, &mut state);

        assert_eq!(
            resolve_procname_for_value(&state, funptr_val),
            Some(Procname::c_from_string("assign_NULL"))
        );
        assert!(
            state.get_closure_proc_name(funptr_val).is_none(),
            "specialization should seed dynamic type constraints, not exported Closure attrs"
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

    #[test]
    fn test_make_specialization_from_caller_follows_field_access_heap_path() {
        let callback_struct =
            sil::typ::TypeName::CStruct(QualifiedCppName::from_string("Callback"));
        let field = Fieldname::make(callback_struct.clone(), "f");

        let callee_pdesc = make_pdesc("apply_callback", &["callback"]);
        let callback_formal = Pvar::mk(
            Mangled::from_string("callback"),
            callee_pdesc.proc_name.clone(),
        );
        let needed_path = HeapPath::FieldAccess(
            field.clone(),
            Box::new(formal_value_heap_path(&callback_formal)),
        );

        let caller_pdesc = Procdesc::new(
            Procname::c_from_string("caller"),
            Typ::void(),
            Location::dummy(),
        );
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let callback_pvar = Pvar::mk(Mangled::from_string("cb"), caller_pdesc.proc_name.clone());
        let callback_addr = AbstractValue::mk_fresh();
        let funptr_val = AbstractValue::mk_fresh();
        caller_state.post.stack.add(
            Var::ProgramVar(Box::new(callback_pvar.clone())),
            callback_addr,
        );
        caller_state.post.heap.add_edge(
            callback_addr,
            Access::FieldAccess(field.clone()),
            funptr_val,
        );
        caller_state.post.attrs.add_one(
            funptr_val,
            Attribute::Closure(Procname::c_from_string("assign_NULL")),
        );

        let mut callee_needs = HashMap::new();
        callee_needs.insert(needed_path.clone(), AbstractValue::mk_fresh());

        let spec = make_specialization_from_caller(
            &callee_needs,
            &caller_state,
            &[(callback_formal, AbstractValue::mk_fresh())],
            &[Typ::mk_ptr(Typ::void())],
            &[(Exp::Lvar(callback_pvar), Typ::mk_ptr(Typ::void()))],
        )
        .expect("expected dynamic-type specialization for callback field");

        assert_eq!(
            spec.dynamic_types.get(&needed_path),
            Some(&sil::typ::TypeName::CFunction(
                match Procname::c_from_string("assign_NULL") {
                    Procname::C(sig) => sig,
                    _ => unreachable!("c procname expected"),
                }
            ))
        );
    }

    #[test]
    fn test_make_specialization_from_caller_uses_dynamic_type_without_closure_attr() {
        let callee_pdesc = make_pdesc("apply_callback", &["callback"]);
        let callback_formal = Pvar::mk(
            Mangled::from_string("callback"),
            callee_pdesc.proc_name.clone(),
        );
        let needed_path = formal_value_heap_path(&callback_formal);

        let caller_pdesc = Procdesc::new(
            Procname::c_from_string("caller"),
            Typ::void(),
            Location::dummy(),
        );
        let mut caller_state = AbductiveDomain::mk_initial(&caller_pdesc);
        let callback_pvar = Pvar::mk(Mangled::from_string("cb"), caller_pdesc.proc_name.clone());
        let callback_addr = AbstractValue::mk_fresh();
        caller_state.post.stack.add(
            Var::ProgramVar(Box::new(callback_pvar.clone())),
            callback_addr,
        );
        caller_state.add_dynamic_type_unsafe(
            callback_addr,
            Typ::mk_struct(TypeName::CFunction(
                match Procname::c_from_string("assign_NULL") {
                    Procname::C(sig) => sig,
                    _ => unreachable!("c procname expected"),
                },
            )),
        );

        let mut callee_needs = HashMap::new();
        callee_needs.insert(needed_path.clone(), AbstractValue::mk_fresh());

        let spec = make_specialization_from_caller(
            &callee_needs,
            &caller_state,
            &[(callback_formal, AbstractValue::mk_fresh())],
            &[Typ::mk_ptr(Typ::void())],
            &[(Exp::Lvar(callback_pvar), Typ::mk_ptr(Typ::void()))],
        )
        .expect("expected dynamic-type specialization from caller state");

        assert_eq!(
            spec.dynamic_types.get(&needed_path),
            Some(&TypeName::CFunction(
                match Procname::c_from_string("assign_NULL") {
                    Procname::C(sig) => sig,
                    _ => unreachable!("c procname expected"),
                }
            ))
        );
    }
}
