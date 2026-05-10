// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Hack-specific Pulse models.
//!
//! Mirrors a small subset of OCaml's `PulseModelsHack.ml` — currently only
//! the `$builtins.hack_get_static_class` modelling needed to exercise SIL
//! `$static`-class virtual dispatch (see `infer/tests/codetoanalyze/sil/pulse/virt.sil`'s
//! `Int.zero` / `devirtualize_with_static_call_*`).

use sil::exp::Exp;
use sil::ident::Ident;
use sil::location::Location;
use sil::procname::Procname;
use sil::typ::{HackClassName, Typ, TypeName};

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::execution_domain::ExecutionDomain;
use crate::operations;
use crate::value_history::{ValueHistory, ValueWithHistory};

/// Method names that have Hack models. Single source of truth — adding a
/// model here must also add a dispatch arm below.
pub const MODELED_NAMES: &[&str] = &["hack_get_static_class"];

/// Dispatch a call to the matching Hack model.
pub fn dispatch(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Option<Vec<ExecutionDomain>> {
    let Procname::Hack(hack) = callee else {
        return None;
    };
    if hack.class_name.as_ref().map(|c| c.0.as_str()) != Some("$builtins") {
        return None;
    }
    match hack.function_name.as_str() {
        "hack_get_static_class" => Some(get_static_class(ret_id, args, loc, state)),
        _ => None,
    }
}

/// Model: `n = $builtins.hack_get_static_class(receiver)`.
///
/// Mirrors `PulseModelsHack.get_static_class`: when the receiver has a known
/// dynamic Hack-class type `T`, return a value tagged with the static
/// companion type `T$static`. When the dynamic type is unknown, return a
/// fresh non-null value (matching OCaml's `unknown_class_object` branch),
/// which keeps callers analyzable but loses static-companion devirtualization.
fn get_static_class(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let class_object = AbstractValue::mk_fresh();
    let _ = state.and_positive(class_object);

    if let Some((receiver_exp, _)) = args.first() {
        let receiver = operations::eval_or_fresh(receiver_exp, loc, &mut state);
        match static_companion_typ(state.get_dynamic_type(receiver)) {
            Some(static_typ) => state.add_dynamic_type_unsafe(class_object, static_typ),
            None => {
                // Mirror OCaml `get_dynamic_type ~ask_specialization:true`:
                // when the receiver's dynamic type isn't known yet, request
                // caller-driven specialization so this proc gets re-analyzed
                // once the actual receiver type is bound. Without this the
                // returned class object stays untyped and the downstream
                // virtual call on `T$static.method` falls back to an unknown
                // call.
                state.add_need_dynamic_type_specialization(receiver);
            }
        }
    }

    operations::write_id_with_history(
        ret_id,
        ValueWithHistory::new(class_object, ValueHistory::assignment(loc.clone())),
        &mut state,
    );
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Convert a dynamic receiver type into its Hack `$static` companion type.
///
/// Mirrors `Typ.Name.Hack.static_companion`: an instance type
/// `Foo` (`HackClass("Foo")`) maps to `Foo$static`; a value already typed
/// as a static companion (suffix `$static`) is returned unchanged.
fn static_companion_typ(typ: Option<&Typ>) -> Option<Typ> {
    let typ = typ?;
    let sil::typ::TypeDesc::Tstruct(TypeName::HackClass(HackClassName(name))) = typ.desc.as_ref()
    else {
        return None;
    };
    let static_name = if name.ends_with("$static") {
        name.clone()
    } else {
        format!("{name}$static")
    };
    Some(Typ::mk_struct(TypeName::HackClass(HackClassName(
        static_name,
    ))))
}
