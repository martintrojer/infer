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
    matches_exact_procname(callee, &cfg.pulse_model_abort)
        || matches_exact_procname(callee, &cfg.pulse_model_unreachable)
        || matches_procname_pattern(callee, cfg.pulse_model_return_nonnull.as_deref())
        || matches_procname_pattern(callee, cfg.pulse_model_return_this.as_deref())
        || matches_procname_pattern(callee, cfg.pulse_model_return_first_arg.as_deref())
        || matches_procname_pattern(callee, cfg.pulse_model_return_nullable.as_deref())
        || matches_procname_pattern(callee, cfg.pulse_model_skip_pattern.as_deref())
        || matches_procname_patterns(callee, &cfg.pulse_model_unknown_pure)
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
    if matches_exact_procname(callee, &cfg.pulse_model_abort) {
        return Some(noreturn(state));
    }
    if matches_exact_procname(callee, &cfg.pulse_model_unreachable) {
        return Some(unreachable());
    }
    if matches_procname_pattern(callee, cfg.pulse_model_return_nonnull.as_deref()) {
        return Some(return_nonnull(ret_id, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_return_this.as_deref()) {
        return Some(return_this(callee, ret_id, args, loc, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_return_first_arg.as_deref()) {
        return Some(return_first_arg(callee, ret_id, args, loc, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_return_nullable.as_deref()) {
        return Some(super::c::fresh_or_null(ret_id, loc, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_skip_pattern.as_deref()) {
        return Some(unknown_call(callee, ret_id, args, loc, state, false));
    }
    if matches_procname_patterns(callee, &cfg.pulse_model_unknown_pure) {
        return Some(unknown_call(callee, ret_id, args, loc, state, true));
    }
    None
}

fn matches_exact_procname(callee: &Procname, configured: &[String]) -> bool {
    let proc_name = callee.to_string();
    configured.iter().any(|entry| entry == &proc_name)
}

fn matches_procname_patterns(callee: &Procname, configured: &[String]) -> bool {
    configured
        .iter()
        .any(|pattern| matches_procname_pattern(callee, Some(pattern.as_str())))
}

fn noreturn(state: AbductiveDomain) -> Vec<ExecutionDomain> {
    vec![ExecutionDomain::ExitProgram(state)]
}

fn unreachable() -> Vec<ExecutionDomain> {
    vec![]
}

fn return_nonnull(ret_id: &Ident, mut state: AbductiveDomain) -> Vec<ExecutionDomain> {
    let ret_val = AbstractValue::mk_fresh();
    let _ = state.and_positive(ret_val);
    operations::write_id(ret_id, ret_val, &mut state);
    vec![ExecutionDomain::ContinueProgram(state)]
}

fn return_this(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    if callee.is_java() || callee.is_objc_instance_method() {
        return return_alias_of_arg(ret_id, args, loc, state, 0);
    }
    vec![ExecutionDomain::ContinueProgram(state)]
}

fn return_first_arg(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let source_index = if callee.is_java() || callee.is_objc_instance_method() {
        Some(1)
    } else if callee.is_c() || callee.is_objc_class_method() {
        Some(0)
    } else {
        None
    };

    match source_index {
        Some(index) => return_alias_of_arg(ret_id, args, loc, state, index),
        None => vec![ExecutionDomain::ContinueProgram(state)],
    }
}

fn return_alias_of_arg(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
    source_index: usize,
) -> Vec<ExecutionDomain> {
    if let Some((arg_exp, _)) = args.get(source_index) {
        let arg_val = operations::eval_or_fresh(arg_exp, loc, &mut state);
        operations::write_id(ret_id, arg_val, &mut state);
    }
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Treat the call as an unknown function, mirroring the existing empty-body
/// fallback: havoc pointer actuals, keep pure calls stable via FunctionApplication.
fn unknown_call(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
    force_pure: bool,
) -> Vec<ExecutionDomain> {
    let ret_val = AbstractValue::mk_fresh();
    operations::write_id(ret_id, ret_val, &mut state);

    let mut is_pure = true;
    let mut actual_vals = Vec::with_capacity(args.len());
    for (arg_exp, arg_typ) in args {
        let arg_val = operations::eval_or_fresh(arg_exp, loc, &mut state);
        actual_vals.push(arg_val);
        if arg_typ.is_pointer() && !force_pure {
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
    use sil::procname::{JavaKind, JavaProcname};
    use sil::typ::JavaClassName;
    use sil::var::Var;

    fn mk_state() -> AbductiveDomain {
        let pname = Procname::c_from_string("test");
        let pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        AbductiveDomain::mk_initial(&pdesc)
    }

    fn extract_continue_state(results: Vec<ExecutionDomain>) -> AbductiveDomain {
        results
            .into_iter()
            .find_map(|exec| match exec {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("expected a continuing state")
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

        let continue_state = extract_continue_state(result);
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
    fn test_dispatch_routes_configured_unreachable() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("handle_failure");
        let cfg = config::InferConfig {
            pulse_model_unreachable: vec!["handle_failure".to_string()],
            ..config::InferConfig::default()
        };

        let result = dispatch_with_config(&callee, &ret_id, &[], &Location::dummy(), state, &cfg)
            .expect("configured unreachable should dispatch");

        assert!(
            result.is_empty(),
            "configured unreachable should terminate the path"
        );
    }

    #[test]
    fn test_dispatch_routes_configured_return_this_for_java() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        let self_id = Ident::create_normal(IdentName::from_string("self"), 1);
        let callee = Procname::Java(JavaProcname {
            class_name: JavaClassName("pkg.Model".to_string()),
            method_name: "duplicate".to_string(),
            parameters: vec![Typ::int(sil::typ::IKind::IInt)],
            return_type: Some(Typ::int(sil::typ::IKind::IInt)),
            kind: JavaKind::NonStatic,
        });
        let cfg = config::InferConfig {
            pulse_model_return_this: Some("pkg.Model.duplicate".to_string()),
            ..config::InferConfig::default()
        };

        let self_val = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::LogicalVar(self_id.clone()), self_val);

        let result = dispatch_with_config(
            &callee,
            &ret_id,
            &[(
                Exp::Var(self_id),
                Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)),
            )],
            &Location::dummy(),
            state,
            &cfg,
        )
        .expect("configured return-this should dispatch");

        let continue_state = extract_continue_state(result);
        let ret_addr = continue_state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return value should be bound");
        assert_eq!(
            continue_state.get_var_repr(ret_addr),
            continue_state.get_var_repr(self_val),
            "configured return-this should alias the receiver"
        );
    }

    #[test]
    fn test_dispatch_routes_configured_return_first_arg_for_c() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 1);
        let callee = Procname::c_from_string("release_fst");
        let cfg = config::InferConfig {
            pulse_model_return_first_arg: Some("release_fst".to_string()),
            ..config::InferConfig::default()
        };

        let arg_val = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::LogicalVar(arg_id.clone()), arg_val);

        let result = dispatch_with_config(
            &callee,
            &ret_id,
            &[(
                Exp::Var(arg_id),
                Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)),
            )],
            &Location::dummy(),
            state,
            &cfg,
        )
        .expect("configured return-first-arg should dispatch");

        let continue_state = extract_continue_state(result);
        let ret_addr = continue_state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return value should be bound");
        assert_eq!(
            continue_state.get_var_repr(ret_addr),
            continue_state.get_var_repr(arg_val),
            "configured return-first-arg should alias the first C actual"
        );
    }

    #[test]
    fn test_dispatch_routes_configured_return_first_arg_for_java_uses_second_actual() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        let self_id = Ident::create_normal(IdentName::from_string("self"), 1);
        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 2);
        let callee = Procname::Java(JavaProcname {
            class_name: JavaClassName("pkg.Model".to_string()),
            method_name: "release".to_string(),
            parameters: vec![Typ::int(sil::typ::IKind::IInt)],
            return_type: Some(Typ::int(sil::typ::IKind::IInt)),
            kind: JavaKind::NonStatic,
        });
        let cfg = config::InferConfig {
            pulse_model_return_first_arg: Some("pkg.Model.release".to_string()),
            ..config::InferConfig::default()
        };

        let self_val = AbstractValue::mk_fresh();
        let arg_val = AbstractValue::mk_fresh();
        state
            .post
            .stack
            .add(Var::LogicalVar(self_id.clone()), self_val);
        state
            .post
            .stack
            .add(Var::LogicalVar(arg_id.clone()), arg_val);

        let result = dispatch_with_config(
            &callee,
            &ret_id,
            &[
                (
                    Exp::Var(self_id),
                    Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)),
                ),
                (
                    Exp::Var(arg_id),
                    Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)),
                ),
            ],
            &Location::dummy(),
            state,
            &cfg,
        )
        .expect("configured return-first-arg should dispatch");

        let continue_state = extract_continue_state(result);
        let ret_addr = continue_state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id))
            .expect("return value should be bound");
        assert_eq!(
            continue_state.get_var_repr(ret_addr),
            continue_state.get_var_repr(arg_val),
            "configured return-first-arg should skip the Java receiver"
        );
        assert_ne!(
            continue_state.get_var_repr(ret_addr),
            continue_state.get_var_repr(self_val),
            "configured return-first-arg should not alias the Java receiver"
        );
    }

    #[test]
    fn test_dispatch_routes_configured_return_nullable() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("ret"), 0);
        let callee = Procname::c_from_string("dangerous");
        let cfg = config::InferConfig {
            pulse_model_return_nullable: Some("dangerous".to_string()),
            ..config::InferConfig::default()
        };

        let result = dispatch_with_config(&callee, &ret_id, &[], &Location::dummy(), state, &cfg)
            .expect("configured return-nullable should dispatch");

        let mut saw_null = false;
        let mut saw_nonnull = false;
        for exec in result {
            let ExecutionDomain::ContinueProgram(state) = exec else {
                panic!("return-nullable should only produce continuing states");
            };
            let ret_addr = state
                .post
                .stack
                .find(&Var::LogicalVar(ret_id.clone()))
                .expect("return value should be bound");
            if state.is_known_zero(ret_addr) {
                saw_null = true;
                assert!(
                    state
                        .post
                        .attrs
                        .get(&ret_addr)
                        .and_then(|attrs| attrs.get_invalid())
                        .is_some(),
                    "null branch should carry a null invalidation"
                );
            }
            if state.path_condition.is_known_nonzero(ret_addr) {
                saw_nonnull = true;
            }
        }

        assert!(saw_null, "return-nullable should include a null branch");
        assert!(
            saw_nonnull,
            "return-nullable should include a non-null branch"
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

        let continue_state = extract_continue_state(result);

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
        let first_state = extract_continue_state(first);
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
        let second_state = extract_continue_state(second);
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

    #[test]
    fn test_dispatch_routes_configured_unknown_pure_preserves_pointer_arg() {
        let mut state = mk_state();
        let callee = Procname::c_from_string("get_value_pure");
        let cfg = config::InferConfig {
            pulse_model_unknown_pure: vec!["get_value_pure".to_string()],
            ..config::InferConfig::default()
        };

        let arg_id = Ident::create_normal(IdentName::from_string("arg"), 0);
        let ret0 = Ident::create_normal(IdentName::from_string("ret"), 1);
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

        let first = dispatch_with_config(
            &callee,
            &ret0,
            &[(
                Exp::Var(arg_id.clone()),
                Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)),
            )],
            &Location::dummy(),
            state,
            &cfg,
        )
        .expect("configured unknown-pure should dispatch");
        let first_state = extract_continue_state(first);
        let preserved_pointee = first_state
            .post
            .heap
            .find_edge(arg_val, &Access::Dereference)
            .expect("unknown-pure should preserve the dereference edge");
        assert_eq!(
            preserved_pointee, old_pointee,
            "unknown-pure should not havoc pointer actuals"
        );
        assert!(
            first_state
                .post
                .attrs
                .get(&old_pointee)
                .and_then(|attrs| attrs.get_allocated())
                .is_some(),
            "unknown-pure should preserve allocation tracking on reachable pointees"
        );

        let ret0_addr = first_state
            .post
            .stack
            .find(&Var::LogicalVar(ret0))
            .expect("first return value should be bound");
        let ret1 = Ident::create_normal(IdentName::from_string("ret"), 2);
        let second = dispatch_with_config(
            &callee,
            &ret1,
            &[(
                Exp::Var(arg_id),
                Typ::mk_ptr(Typ::int(sil::typ::IKind::IInt)),
            )],
            &Location::dummy(),
            first_state,
            &cfg,
        )
        .expect("configured unknown-pure should dispatch");
        let second_state = extract_continue_state(second);
        let ret1_addr = second_state
            .post
            .stack
            .find(&Var::LogicalVar(ret1))
            .expect("second return value should be bound");
        assert_eq!(
            second_state.get_var_repr(ret0_addr),
            second_state.get_var_repr(ret1_addr),
            "unknown-pure calls with identical actuals should share FunctionApplication results"
        );
    }
}
