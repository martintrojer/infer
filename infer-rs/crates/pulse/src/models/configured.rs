// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Generic config-driven Pulse models.
//!
//! Mirrors the configurable matcher surface from OCaml's `PulseModelsImport.ml`.

use sil::exp::Exp;
use sil::ident::Ident;
use sil::location::Location;
use sil::procname::Procname;
use sil::typ::Typ;

use super::matching::matches_procname_pattern;
use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::execution_domain::ExecutionDomain;
use crate::operations;

/// Check whether any generic config-driven model applies.
pub fn has_model(callee: &Procname, cfg: &config::InferConfig) -> bool {
    matches_abort_procname(callee, &cfg.pulse_model_abort)
        || matches_procname_pattern(callee, cfg.pulse_model_return_nonnull.as_deref())
        || matches_procname_pattern(callee, cfg.pulse_model_skip_pattern.as_deref())
}

/// Try to dispatch a call to a generic config-driven model.
pub fn dispatch(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Option<Vec<ExecutionDomain>> {
    dispatch_with_config(callee, ret_id, args, loc, state, config::get())
}

fn dispatch_with_config(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
    cfg: &config::InferConfig,
) -> Option<Vec<ExecutionDomain>> {
    if matches_abort_procname(callee, &cfg.pulse_model_abort) {
        return Some(noreturn(state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_return_nonnull.as_deref()) {
        return Some(return_nonnull(ret_id, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_skip_pattern.as_deref()) {
        return Some(skip_unknown(callee, ret_id, args, loc, state));
    }
    None
}

fn matches_abort_procname(callee: &Procname, configured: &[String]) -> bool {
    let proc_name = callee.to_string();
    configured.iter().any(|entry| entry == &proc_name)
}

fn noreturn(state: AbductiveDomain) -> Vec<ExecutionDomain> {
    vec![ExecutionDomain::ExitProgram(state)]
}

fn return_nonnull(ret_id: &Ident, mut state: AbductiveDomain) -> Vec<ExecutionDomain> {
    let ret_val = AbstractValue::mk_fresh();
    let _ = state.and_positive(ret_val);
    operations::write_id(ret_id, ret_val, &mut state);
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Treat the call as an unknown function, mirroring the existing empty-body
/// fallback: havoc pointer actuals, keep pure calls stable via FunctionApplication.
fn skip_unknown(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let ret_val = AbstractValue::mk_fresh();
    operations::write_id(ret_id, ret_val, &mut state);

    let mut is_pure = true;
    let mut actual_vals = Vec::with_capacity(args.len());
    for (arg_exp, arg_typ) in args {
        let arg_val = operations::eval_or_fresh(arg_exp, loc, &mut state);
        actual_vals.push(arg_val);
        if arg_typ.is_pointer() {
            is_pure = false;
            state.apply_unknown_effect(arg_val);
            operations::refresh_unknown_lvalue_root(arg_exp, arg_val, &mut state);
        }
    }

    if is_pure {
        let callee_name = format!("{callee}");
        if state
            .path_condition
            .and_fn_app(ret_val, &callee_name, &actual_vals)
            .is_unsat()
        {
            return vec![];
        }
    }

    vec![ExecutionDomain::ContinueProgram(state)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Access;
    use crate::attribute::Allocator;
    use sil::const_val::Const;
    use sil::ident::IdentName;
    use sil::int_lit::IntLit;
    use sil::procdesc::Procdesc;
    use sil::var::Var;

    fn mk_state() -> AbductiveDomain {
        let pname = Procname::c_from_string("test");
        let pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        AbductiveDomain::mk_initial(&pdesc)
    }

    #[test]
    fn test_dispatch_routes_configured_abort() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("ns1::ns2::fun_abort");
        let cfg = config::InferConfig {
            pulse_model_abort: vec!["ns1::ns2::fun_abort".to_string()],
            ..config::InferConfig::default()
        };

        let result = dispatch_with_config(&callee, &ret_id, &[], &Location::dummy(), state, &cfg)
            .expect("configured abort should dispatch");

        assert!(matches!(
            result.as_slice(),
            [ExecutionDomain::ExitProgram(_)]
        ));
    }

    #[test]
    fn test_dispatch_routes_configured_return_nonnull() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("Handle::get");
        let cfg = config::InferConfig {
            pulse_model_return_nonnull: Some("Handle::get".to_string()),
            ..config::InferConfig::default()
        };

        let result = dispatch_with_config(&callee, &ret_id, &[], &Location::dummy(), state, &cfg)
            .expect("configured return-nonnull should dispatch");

        let continue_state = result
            .into_iter()
            .find_map(|exec| match exec {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("return-nonnull model should continue");
        let ret_addr = continue_state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return value should be bound");
        assert!(
            continue_state.path_condition.is_known_nonzero(ret_addr),
            "configured return-nonnull model should constrain the return value to be non-zero"
        );
    }

    #[test]
    fn test_dispatch_routes_configured_skip_pattern_and_havocs_pointer_arg() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 1);
        let callee = Procname::c_from_string("skip_model::SkipAll::goo");
        let cfg = config::InferConfig {
            pulse_model_skip_pattern: Some(
                "skip_model::SkipAll::.*\\|.*SkipSome<.*>::skip_me".to_string(),
            ),
            ..config::InferConfig::default()
        };

        let arg_val = AbstractValue::mk_fresh();
        let old_pointee = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::LogicalVar(arg_id.clone()), arg_val);
        state
            .post
            .heap
            .add_edge(arg_val, Access::Dereference, old_pointee);
        state.allocate(old_pointee, Allocator::CMalloc, Location::dummy());

        let result = dispatch_with_config(
            &callee,
            &ret_id,
            &[(Exp::Var(arg_id), Typ::mk_ptr(Typ::void()))],
            &Location::dummy(),
            state,
            &cfg,
        )
        .expect("configured skip-pattern should dispatch");

        let continue_state = result
            .into_iter()
            .find_map(|exec| match exec {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("skip-pattern model should continue");

        let new_pointee = continue_state
            .post
            .heap
            .find_edge(arg_val, &Access::Dereference)
            .expect("skip-pattern should preserve the dereference edge");
        assert_ne!(
            new_pointee, old_pointee,
            "skip-pattern should havoc the pointed-to memory"
        );
        let old_allocated = continue_state
            .post
            .attrs
            .get(&old_pointee)
            .and_then(|attrs| attrs.get_allocated());
        assert!(
            old_allocated.is_none(),
            "skip-pattern should remove allocation tracking from havoced memory"
        );
    }

    #[test]
    fn test_dispatch_routes_configured_skip_pattern_as_pure_fn_app() {
        let state = mk_state();
        let callee = Procname::c_from_string("skip_model::SkipSome<int>::skip_me");
        let cfg = config::InferConfig {
            pulse_model_skip_pattern: Some(
                "skip_model::SkipAll::.*\\|.*SkipSome<.*>::skip_me".to_string(),
            ),
            ..config::InferConfig::default()
        };

        let ret0 = Ident::create_normal(IdentName::from_string("ret"), 0);
        let first = dispatch_with_config(
            &callee,
            &ret0,
            &[(
                Exp::Const(Const::Cint(IntLit::of_int(7))),
                Typ::int(sil::typ::IKind::IInt),
            )],
            &Location::dummy(),
            state,
            &cfg,
        )
        .expect("configured skip-pattern should dispatch");
        let first_state = first
            .into_iter()
            .find_map(|exec| match exec {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("first skip-pattern call should continue");
        let ret0_addr = first_state
            .post
            .stack
            .find(&Var::LogicalVar(ret0))
            .expect("first return value should be bound");

        let ret1 = Ident::create_normal(IdentName::from_string("ret"), 1);
        let second = dispatch_with_config(
            &callee,
            &ret1,
            &[(
                Exp::Const(Const::Cint(IntLit::of_int(7))),
                Typ::int(sil::typ::IKind::IInt),
            )],
            &Location::dummy(),
            first_state,
            &cfg,
        )
        .expect("configured skip-pattern should dispatch");
        let second_state = second
            .into_iter()
            .find_map(|exec| match exec {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("second skip-pattern call should continue");
        let ret1_addr = second_state
            .post
            .stack
            .find(&Var::LogicalVar(ret1))
            .expect("second return value should be bound");

        assert_eq!(
            second_state.get_var_repr(ret0_addr),
            second_state.get_var_repr(ret1_addr),
            "pure skip-pattern calls with identical actuals should share FunctionApplication results"
        );
    }
}
