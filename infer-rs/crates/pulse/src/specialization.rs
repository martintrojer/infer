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

use sil::procname::Procname;
use sil::pvar::Pvar;
use sil::specialization::{HeapPath, PulseSpecialization};
use sil::var::Var;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::access::Access;
use crate::attribute::Attribute;

/// Apply a specialization to an AbductiveDomain.
///
/// For each dynamic type binding in the specialization, walk the heap path
/// to find or create the abstract value, then set its Closure attribute
/// to the specified procedure name.
///
/// Cross-ref: OCaml `PulseSpecialization.apply`.
pub fn apply(spec: &PulseSpecialization, state: &mut AbductiveDomain) {
    for (heap_path, type_name) in &spec.dynamic_types {
        let val = initialize_heap_path(heap_path, state);
        // For C function pointers, the TypeName encodes the procedure name.
        // Set the Closure attribute so __call_c_function_ptr can resolve it.
        let pname = Procname::c_from_string(&format!("{type_name}"));
        state.post.attrs.add_one(val, Attribute::Closure(pname));
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
    callee_needs: &std::collections::HashMap<HeapPath, AbstractValue>,
    caller_state: &AbductiveDomain,
    formals: &[(Pvar, AbstractValue)],
    actuals: &[(sil::exp::Exp, sil::typ::Typ)],
) -> Option<PulseSpecialization> {
    if callee_needs.is_empty() {
        return None;
    }

    let mut dynamic_types = std::collections::HashMap::new();

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
        let actual_val = match crate::operations::eval(
            actual_exp,
            &sil::location::Location::dummy(),
            &mut eval_state,
        ) {
            crate::pulse_result::PulseResult::Ok(v) => v,
            _ => continue,
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

    if dynamic_types.is_empty() {
        None
    } else {
        Some(PulseSpecialization { dynamic_types })
    }
}

/// Extract the root Pvar from a HeapPath.
fn extract_root_pvar(path: &HeapPath) -> Option<&Pvar> {
    match path {
        HeapPath::Pvar(pv) => Some(pv),
        HeapPath::Dereference(inner) | HeapPath::FieldAccess(_, inner) => extract_root_pvar(inner),
    }
}
