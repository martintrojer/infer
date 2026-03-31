// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! C models: malloc, free, realloc, etc.
//!
//! Mirrors OCaml's `PulseModelsC.ml`.

use sil::builtin_decl;
use sil::exp::Exp;
use sil::ident::Ident;
use sil::location::Location;
use sil::procname::Procname;
use sil::typ::Typ;

use crate::abductive::AbductiveDomain;
use crate::abstract_value::AbstractValue;
use crate::attribute::Allocator;
use crate::execution_domain::ExecutionDomain;
use crate::invalidation::Invalidation;
use crate::models::matching::matches_procname_pattern;
use crate::operations;
use crate::pulse_result::PulseResult;
use crate::value_history::ValueHistory;

/// Method names that have C models. Single source of truth —
/// adding a model means adding the name here AND a dispatch arm below.
pub const MODELED_NAMES: &[&str] = &[
    "malloc",
    "free",
    "__new",
    "__new_array",
    "__delete",
    "__delete_array",
    "memcpy",
    "memmove",
    "realloc",
    "exit",
    "_exit",
    "abort",
    "__assert_fail",
    "__assert_rtn",
    "__infer_fail",
    "__builtin_expect",
    "random",
    "fopen",
    "getcwd",
    // stdio functions that dereference their FILE* argument
    "fclose",
    "fgetc",
    "fgets",
    "fputc",
    "fputs",
    "fprintf",
    "fseek",
    "ftell",
    "feof",
    "ferror",
    "clearerr",
    "rewind",
    "fgetpos",
    "fsetpos",
    "fileno",
    "getc",
    "putc",
    "ungetc",
];

/// Dispatch a call to the matching C model.
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
    if builtin_decl::match_builtin(&builtin_decl::malloc(), callee) {
        return Some(malloc(ret_id, loc, state));
    }
    if builtin_decl::match_builtin(&builtin_decl::free(), callee) {
        return Some(free(ret_id, args, loc, state));
    }
    if builtin_decl::match_builtin(&builtin_decl::__new(), callee)
        || builtin_decl::match_builtin(&builtin_decl::__new_array(), callee)
    {
        return Some(cpp_new(ret_id, loc, state));
    }
    if builtin_decl::match_builtin(&builtin_decl::__delete(), callee)
        || builtin_decl::match_builtin(&builtin_decl::__delete_array(), callee)
    {
        return Some(cpp_delete(ret_id, args, loc, state));
    }

    // Non-builtin models: match by name
    let name = callee.get_method_name();
    if matches!(
        name,
        "exit" | "_exit" | "abort" | "__assert_fail" | "__infer_fail" | "__assert_rtn"
    ) {
        return Some(noreturn(state));
    }
    // random(): non-deterministic — each call returns an independent fresh value.
    // Must be a model (not empty-body) to bypass FunctionApplication tracking.
    // Cross-ref: OCaml PulseModelsImport.ml L437: random <>$$--> nondet.
    if name == "random" {
        return Some(nondet(ret_id, state));
    }
    // __builtin_expect(x, y): returns x (branch prediction hint, no-op).
    // Without this, the return value is fresh/unknown, losing the condition.
    if name == "__builtin_expect" {
        return Some(builtin_expect(ret_id, args, loc, state));
    }
    // memcpy/memmove: check validity of both dest and src.
    // Cross-ref: OCaml PulseModelsC.ml memcpy: check_valid dest, check_valid src.
    if matches!(name, "memcpy" | "memmove") {
        return Some(memcpy(ret_id, args, loc, state));
    }
    // realloc(ptr, size): free old ptr, then allocate-or-null.
    // Cross-ref: OCaml PulseModelsC.ml realloc_common.
    if name == "realloc" {
        return Some(realloc(ret_id, args, loc, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_free_pattern.as_deref()) {
        return Some(free(ret_id, args, loc, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_realloc_pattern.as_deref()) {
        return Some(custom_realloc(callee, ret_id, args, loc, state));
    }
    if matches_procname_pattern(callee, cfg.pulse_model_malloc_pattern.as_deref()) {
        return Some(custom_malloc(callee, ret_id, loc, state));
    }
    // fopen: return null-or-non-null disjuncts for null-deref checking,
    // but do NOT mark as Allocated (no MEMORY_LEAK_C tracking for file
    // handles). Cross-ref: OCaml tracks fopen via CFile allocator which
    // only reports resource leaks, not MEMORY_LEAK_C.
    if name == "fopen" {
        return Some(fresh_or_null(ret_id, loc, state));
    }
    // getcwd(buf, size): when buf=NULL, getcwd mallocs internally (POSIX).
    // Cross-ref: OCaml PulseModelsC.ml getcwd: prune_eq_zero buf → ret_alloc_or_null CMalloc.
    if name == "getcwd" {
        return Some(getcwd_model(ret_id, args, loc, state));
    }
    if matches!(
        name,
        "fclose"
            | "fgetc"
            | "fgets"
            | "fputc"
            | "fputs"
            | "fprintf"
            | "fseek"
            | "ftell"
            | "feof"
            | "ferror"
            | "clearerr"
            | "rewind"
            | "fgetpos"
            | "fsetpos"
            | "fileno"
            | "getc"
            | "putc"
            | "ungetc"
    ) {
        return Some(check_file_arg(ret_id, args, loc, state));
    }

    None
}

/// Dynamic-model pre-check for regex-configured wrappers.
///
/// Cross-ref: OCaml PulseModelsC.ml `match_regexp_opt Config.pulse_model_*_pattern`.
pub(crate) fn matches_configured_wrapper(callee: &Procname, cfg: &config::InferConfig) -> bool {
    matches_procname_pattern(callee, cfg.pulse_model_free_pattern.as_deref())
        || matches_procname_pattern(callee, cfg.pulse_model_malloc_pattern.as_deref())
        || matches_procname_pattern(callee, cfg.pulse_model_realloc_pattern.as_deref())
}

/// Returns two disjuncts: fresh non-null value + null value.
/// Used for functions like fopen/getcwd that may return NULL on failure
/// but whose return values are NOT tracked as heap allocations.
pub(crate) fn fresh_or_null(
    ret_id: &Ident,
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let mut ok_state = state.clone();
    let addr = AbstractValue::mk_fresh();
    // Constrain non-null return to be positive so prune(addr = 0) is Unsat.
    // Cross-ref: OCaml PulseModelsImport.ml alloc_not_null_common calls and_positive.
    let _ = ok_state.and_positive(addr);
    operations::write_id_with_history(
        ret_id,
        crate::value_history::ValueWithHistory::new(addr, ValueHistory::assignment(loc.clone())),
        &mut ok_state,
    );

    let mut fail_state = state;
    let null_val = AbstractValue::mk_fresh();
    operations::write_id_with_history(
        ret_id,
        crate::value_history::ValueWithHistory::new(
            null_val,
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(sil::int_lit::IntLit::zero()),
                loc.clone(),
            ),
        ),
        &mut fail_state,
    );
    let _ = fail_state.and_equal_const(null_val, 0);
    fail_state.post.attrs.add_one(
        null_val,
        crate::attribute::Attribute::Invalid(
            Invalidation::ConstantDereference(sil::int_lit::IntLit::zero()),
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(sil::int_lit::IntLit::zero()),
                loc.clone(),
            ),
        ),
    );

    vec![
        ExecutionDomain::ContinueProgram(ok_state),
        ExecutionDomain::ContinueProgram(fail_state),
    ]
}

/// Shared: allocate-or-null model. Returns two disjuncts: allocated + null.
/// Used by malloc and similar functions that may fail.
fn allocate_or_null(
    ret_id: &Ident,
    allocator: Allocator,
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    let mut ok_state = state.clone();
    let addr = AbstractValue::mk_fresh();
    // Constrain non-null return to be positive so prune(addr = 0) is Unsat.
    // Cross-ref: OCaml PulseModelsImport.ml alloc_not_null_common calls and_positive.
    let _ = ok_state.and_positive(addr);
    operations::allocate(addr, allocator, loc.clone(), &mut ok_state);
    operations::write_id_with_history(
        ret_id,
        crate::value_history::ValueWithHistory::new(addr, ValueHistory::assignment(loc.clone())),
        &mut ok_state,
    );

    let mut fail_state = state;
    let null_val = AbstractValue::mk_fresh();
    operations::write_id_with_history(
        ret_id,
        crate::value_history::ValueWithHistory::new(
            null_val,
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(sil::int_lit::IntLit::zero()),
                loc.clone(),
            ),
        ),
        &mut fail_state,
    );
    // Constrain null_val = 0 in the formula so prune(val ≠ 0) can eliminate it.
    // Matches OCaml's `and_eq_int ret_addr IntLit.zero` in alloc_common_dsl.
    let _ = fail_state.and_equal_const(null_val, 0);
    fail_state.post.attrs.add_one(
        null_val,
        crate::attribute::Attribute::Invalid(
            Invalidation::ConstantDereference(sil::int_lit::IntLit::zero()),
            ValueHistory::invalidated(
                Invalidation::ConstantDereference(sil::int_lit::IntLit::zero()),
                loc.clone(),
            ),
        ),
    );

    vec![
        ExecutionDomain::ContinueProgram(ok_state),
        ExecutionDomain::ContinueProgram(fail_state),
    ]
}

/// Model: `ret = malloc(size)` — allocate or null.
fn malloc(ret_id: &Ident, loc: &Location, state: AbductiveDomain) -> Vec<ExecutionDomain> {
    allocate_or_null(ret_id, Allocator::CMalloc, loc, state)
}

/// Model: configured wrapper to `malloc(size)` — allocate or null with a
/// custom allocator tag.
fn custom_malloc(
    callee: &Procname,
    ret_id: &Ident,
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    allocate_or_null(ret_id, Allocator::CustomMalloc(callee.clone()), loc, state)
}

/// Shared: invalidate first argument with given invalidation kind.
/// Used by free, delete, delete[].
fn invalidate_first_arg(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    inv: Invalidation,
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    if let Some((arg_exp, _)) = args.first() {
        let addr = operations::eval_or_fresh_with_history(arg_exp, loc, &mut state);

        match operations::check_addr_access_with_history(addr.clone(), loc, &mut state) {
            PulseResult::FatalError(diag, _) => {
                return vec![ExecutionDomain::AbortProgram {
                    state: Box::new(state),
                    diagnostic: Box::new(diag),
                }];
            }
            PulseResult::Recoverable((), errors) => {
                state.invalidate(
                    addr.addr,
                    inv.clone(),
                    addr.history
                        .append_event(crate::value_history::HistoryEvent::Invalidated {
                            invalidation: inv.clone(),
                            location: loc.clone(),
                        }),
                );
                let ret_val = AbstractValue::mk_fresh();
                operations::write_id_with_history(
                    ret_id,
                    crate::value_history::ValueWithHistory::new(
                        ret_val,
                        ValueHistory::assignment(loc.clone()),
                    ),
                    &mut state,
                );
                let mut results = vec![ExecutionDomain::ContinueProgram(state.clone())];
                for diag in errors {
                    results.push(ExecutionDomain::AbortProgram {
                        state: Box::new(state.clone()),
                        diagnostic: Box::new(diag),
                    });
                }
                return results;
            }
            PulseResult::Ok(()) => {
                state.invalidate(
                    addr.addr,
                    inv.clone(),
                    addr.history
                        .append_event(crate::value_history::HistoryEvent::Invalidated {
                            invalidation: inv,
                            location: loc.clone(),
                        }),
                );
            }
        }
    }

    let ret_val = AbstractValue::mk_fresh();
    operations::write_id_with_history(
        ret_id,
        crate::value_history::ValueWithHistory::new(ret_val, ValueHistory::assignment(loc.clone())),
        &mut state,
    );
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Model: `free(ptr)` — invalidate with CFree.
///
/// In C, `free(NULL)` is a valid no-op. Mirror OCaml's
/// `Basic.free_or_delete`: try both `ptr == 0` and `ptr > 0`, but
/// keep only the satisfiable branches.
fn free(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    if let Some((arg_exp, _)) = args.first() {
        let addr = operations::eval_or_fresh_with_history(arg_exp, loc, &mut state);

        let mut results = Vec::new();

        let mut null_state = state.clone();
        if null_state.and_equal_const(addr.addr, 0).is_sat() {
            let ret_val = AbstractValue::mk_fresh();
            operations::write_id_with_history(
                ret_id,
                crate::value_history::ValueWithHistory::new(
                    ret_val,
                    ValueHistory::assignment(loc.clone()),
                ),
                &mut null_state,
            );
            results.push(ExecutionDomain::ContinueProgram(null_state));
        }

        if state.and_positive(addr.addr).is_sat() {
            results.extend(invalidate_first_arg(
                ret_id,
                args,
                Invalidation::CFree,
                loc,
                state,
            ));
        }

        return results;
    }
    invalidate_first_arg(ret_id, args, Invalidation::CFree, loc, state)
}

/// Model: `ret = new T` / `ret = new T[]` — allocate.
fn cpp_new(ret_id: &Ident, loc: &Location, mut state: AbductiveDomain) -> Vec<ExecutionDomain> {
    let addr = AbstractValue::mk_fresh();
    operations::allocate(addr, Allocator::CppNew, loc.clone(), &mut state);
    operations::write_id_with_history(
        ret_id,
        crate::value_history::ValueWithHistory::new(addr, ValueHistory::assignment(loc.clone())),
        &mut state,
    );
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Model: `getcwd(buf, size)` — conditional allocation.
///
/// When buf=NULL (per POSIX), getcwd mallocs a buffer internally → track as
/// Allocated(CMalloc). When buf!=NULL, returns buf or NULL (no allocation).
/// Cross-ref: OCaml PulseModelsC.ml getcwd: prune_eq_zero buf → ret_alloc_or_null CMalloc.
fn getcwd_model(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // Check if first arg (buf) is a zero/null constant syntactically.
    // When buf=NULL, getcwd mallocs internally (POSIX).
    // When buf!=NULL, it writes to the provided buffer (no allocation).
    // Use is_zero() not is_null_literal() because textual converts NULL to
    // integer 0 (is_pointer=false).
    let buf_is_null = args.first().map(|(exp, _)| exp.is_zero()).unwrap_or(true);
    if buf_is_null {
        allocate_or_null(ret_id, Allocator::CMalloc, loc, state)
    } else {
        fresh_or_null(ret_id, loc, state)
    }
}

/// Model: `random()` — non-deterministic return value.
///
/// Each call returns an independent fresh value (no FunctionApplication
/// tracking). Cross-ref: OCaml PulseModelsImport.ml `nondet`.
fn nondet(ret_id: &Ident, mut state: AbductiveDomain) -> Vec<ExecutionDomain> {
    let ret_val = AbstractValue::mk_fresh();
    operations::write_id(ret_id, ret_val, &mut state);
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Model: `exit()`, `abort()`, `__assert_fail()` — non-returning.
///
/// Transitions to ExitProgram, stopping execution on this path.
/// Prevents false positives from unreachable code after these calls.
fn noreturn(state: AbductiveDomain) -> Vec<ExecutionDomain> {
    vec![ExecutionDomain::ExitProgram(state)]
}

/// Model for stdio functions that dereference their FILE* argument.
///
/// Checks the first argument for validity (FILE* is the first arg for most
/// stdio functions). Reports NULL_DEREFERENCE if null, then returns fresh value.
fn check_file_arg(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    if let Some((arg_exp, _)) = args.first() {
        let addr = operations::eval_or_fresh(arg_exp, loc, &mut state);
        match operations::check_addr_access(addr, loc, &mut state) {
            PulseResult::FatalError(diag, _) => {
                return vec![ExecutionDomain::AbortProgram {
                    state: Box::new(state),
                    diagnostic: Box::new(diag),
                }];
            }
            PulseResult::Recoverable((), errors) => {
                let ret_val = AbstractValue::mk_fresh();
                operations::write_id(ret_id, ret_val, &mut state);
                let mut results = vec![ExecutionDomain::ContinueProgram(state.clone())];
                for diag in errors {
                    results.push(ExecutionDomain::AbortProgram {
                        state: Box::new(state.clone()),
                        diagnostic: Box::new(diag),
                    });
                }
                return results;
            }
            PulseResult::Ok(()) => {}
        }
    }

    let ret_val = AbstractValue::mk_fresh();
    operations::write_id(ret_id, ret_val, &mut state);
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Model: `__builtin_expect(x, y)` — returns x (branch prediction hint).
///
/// This is a GCC/Clang builtin for branch prediction. It returns its
/// first argument unchanged. Without modeling, the return is a fresh
/// unknown value, losing any constraints on the condition.
fn builtin_expect(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // Return the first argument's value
    let ret_val = if let Some((arg_exp, _)) = args.first() {
        operations::eval_or_fresh(arg_exp, loc, &mut state)
    } else {
        AbstractValue::mk_fresh()
    };
    operations::write_id(ret_id, ret_val, &mut state);
    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Model: `realloc(ptr, size)` — free old pointer, then allocate or null.
///
/// Cross-ref: OCaml PulseModelsC.ml realloc_common.
/// Steps: 1) Free the old pointer. 2) Return allocate-or-null.
fn realloc(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    realloc_with_allocator(ret_id, args, loc, state, Allocator::CRealloc)
}

/// Model: configured wrapper to `realloc(ptr, size)`.
fn custom_realloc(
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    realloc_with_allocator(
        ret_id,
        args,
        loc,
        state,
        Allocator::CustomRealloc(callee.clone()),
    )
}

fn realloc_with_allocator(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
    allocator: Allocator,
) -> Vec<ExecutionDomain> {
    // Cross-ref: OCaml PulseModelsC.ml realloc_common = free pointer >>= alloc_common.
    let free_results = if !args.is_empty() {
        free(&Ident::create_none(), args, loc, state)
    } else {
        vec![ExecutionDomain::ContinueProgram(state)]
    };

    let mut results = Vec::new();
    for result in free_results {
        match result {
            ExecutionDomain::ContinueProgram(state) => {
                results.extend(allocate_or_null(ret_id, allocator.clone(), loc, state));
            }
            other => results.push(other),
        }
    }

    results
}

/// Model: `memcpy(dest, src, size)` / `memmove(dest, src, size)`.
///
/// Checks validity of both dest (first arg) and src (second arg).
/// Returns dest. Reports NULL_DEREFERENCE if either is null.
///
/// Cross-ref: OCaml PulseModelsC.ml memcpy.
fn memcpy(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    // Check dest (first arg)
    if let Some((dest_exp, _)) = args.first() {
        let dest_addr = operations::eval_or_fresh(dest_exp, loc, &mut state);
        match operations::check_addr_access(dest_addr, loc, &mut state) {
            PulseResult::FatalError(diag, _) => {
                return vec![ExecutionDomain::AbortProgram {
                    state: Box::new(state),
                    diagnostic: Box::new(diag),
                }];
            }
            PulseResult::Recoverable((), errors) => {
                operations::write_id(ret_id, dest_addr, &mut state);
                let mut results = vec![ExecutionDomain::ContinueProgram(state.clone())];
                for diag in errors {
                    results.push(ExecutionDomain::AbortProgram {
                        state: Box::new(state.clone()),
                        diagnostic: Box::new(diag),
                    });
                }
                return results;
            }
            PulseResult::Ok(()) => {
                // dest is valid, continue to check src
                operations::write_id(ret_id, dest_addr, &mut state);
            }
        }
    }

    // Check src (second arg)
    if let Some((src_exp, _)) = args.get(1) {
        let src_addr = operations::eval_or_fresh(src_exp, loc, &mut state);
        match operations::check_addr_access(src_addr, loc, &mut state) {
            PulseResult::FatalError(diag, _) => {
                return vec![ExecutionDomain::AbortProgram {
                    state: Box::new(state),
                    diagnostic: Box::new(diag),
                }];
            }
            PulseResult::Recoverable((), errors) => {
                let mut results = vec![ExecutionDomain::ContinueProgram(state.clone())];
                for diag in errors {
                    results.push(ExecutionDomain::AbortProgram {
                        state: Box::new(state.clone()),
                        diagnostic: Box::new(diag),
                    });
                }
                return results;
            }
            PulseResult::Ok(()) => {}
        }
    }

    vec![ExecutionDomain::ContinueProgram(state)]
}

/// Model: `delete ptr` / `delete[] ptr` — invalidate with CppDelete.
/// Model: `delete ptr` / `delete[] ptr` — invalidate with CppDelete.
///
/// In C++, `delete nullptr` is a valid no-op (like free(NULL) in C).
fn cpp_delete(
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    mut state: AbductiveDomain,
) -> Vec<ExecutionDomain> {
    if let Some((arg_exp, _)) = args.first() {
        let addr = operations::eval_or_fresh(arg_exp, loc, &mut state);
        if state.is_known_zero(addr) {
            let ret_val = AbstractValue::mk_fresh();
            operations::write_id(ret_id, ret_val, &mut state);
            return vec![ExecutionDomain::ContinueProgram(state)];
        }
    }
    invalidate_first_arg(ret_id, args, Invalidation::CppDelete, loc, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::Operand;
    use sil::ident::IdentName;
    use sil::mangled::Mangled;
    use sil::procdesc::Procdesc;
    use sil::pvar::Pvar;
    use sil::var::Var;

    fn mk_state() -> AbductiveDomain {
        let pname = Procname::c_from_string("test");
        let pdesc = Procdesc::new(pname, Typ::void(), Location::dummy());
        AbductiveDomain::mk_initial(&pdesc)
    }

    #[test]
    fn test_malloc_returns_two_disjuncts() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let results = malloc(&ret_id, &Location::dummy(), state);
        assert_eq!(results.len(), 2, "malloc should return ok + null disjuncts");
        assert!(results.iter().all(|r| r.is_continue()));

        // First disjunct: allocated (valid)
        if let ExecutionDomain::ContinueProgram(s) = &results[0] {
            let var = Var::LogicalVar(ret_id.clone());
            let addr = s.post.stack.find(&var).unwrap();
            assert!(s.check_valid(addr).is_ok(), "success path should be valid");
        }

        // Second disjunct: null (invalid)
        if let ExecutionDomain::ContinueProgram(s) = &results[1] {
            let var = Var::LogicalVar(ret_id);
            let addr = s.post.stack.find(&var).unwrap();
            assert!(
                s.check_valid(addr).is_err(),
                "failure path should be invalid"
            );
        }
    }

    #[test]
    fn test_free_invalidates_pointer() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let ptr = AbstractValue::mk_fresh();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), ptr);
        state.allocate(ptr, Allocator::CMalloc, Location::dummy());

        let args = vec![(Exp::Lvar(pvar), Typ::void())];
        let results = free(&ret_id, &args, &Location::dummy(), state);
        assert!(results.iter().any(|r| r.is_continue()));

        // The non-null disjunct should have ptr invalidated (freed)
        let has_freed = results.iter().any(|r| {
            if let ExecutionDomain::ContinueProgram(s) = r {
                s.check_valid(ptr).is_err()
            } else {
                false
            }
        });
        assert!(
            has_freed,
            "some disjunct should have freed pointer as invalid"
        );
    }

    #[test]
    fn test_free_on_known_nonnull_path_does_not_keep_null_disjunct() {
        let mut state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let ptr = AbstractValue::mk_fresh();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), ptr);
        state.allocate(ptr, Allocator::CMalloc, Location::dummy());
        let _ = state.and_not_equal(&Operand::AbstractValue(ptr), &Operand::ConstOperand(0));

        let args = vec![(Exp::Lvar(pvar), Typ::void())];
        let results = free(&ret_id, &args, &Location::dummy(), state);

        assert_eq!(
            results.len(),
            1,
            "free should discard the impossible NULL branch on a known non-null path"
        );
        assert!(results.iter().all(|r| r.is_continue()));
    }

    #[test]
    fn test_dispatch_routes_malloc() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = builtin_decl::malloc();
        let result = dispatch(&callee, &ret_id, &[], &Location::dummy(), state);
        assert!(result.is_some(), "malloc should be dispatched");
    }

    #[test]
    fn test_dispatch_routes_free() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = builtin_decl::free();
        let result = dispatch(&callee, &ret_id, &[], &Location::dummy(), state);
        assert!(result.is_some(), "free should be dispatched");
    }

    #[test]
    fn test_dispatch_unknown_returns_none() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("unknown_func");
        let result = dispatch(&callee, &ret_id, &[], &Location::dummy(), state);
        assert!(result.is_none(), "unknown function should not dispatch");
    }

    #[test]
    fn test_dispatch_routes_custom_malloc_from_config() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("a_malloc");
        let expected_allocator = Allocator::CustomMalloc(callee.clone());
        let cfg = config::InferConfig {
            pulse_model_malloc_pattern: Some("\\(my\\|a\\)_malloc".to_string()),
            ..config::InferConfig::default()
        };

        let result = dispatch_with_config(&callee, &ret_id, &[], &Location::dummy(), state, &cfg)
            .expect("configured malloc wrapper should dispatch");

        let continue_state = result
            .into_iter()
            .find_map(|exec| match exec {
                ExecutionDomain::ContinueProgram(state) => Some(state),
                _ => None,
            })
            .expect("malloc wrapper should produce a ContinueProgram state");
        let ret_addr = continue_state
            .post
            .stack
            .find(&Var::LogicalVar(ret_id.clone()))
            .expect("wrapper should bind return value");
        let (allocator, _) = continue_state
            .post
            .attrs
            .get(&ret_addr)
            .and_then(|attrs| attrs.get_allocated())
            .expect("wrapper return should be tracked as allocated");
        assert_eq!(allocator, &expected_allocator);
    }

    #[test]
    fn test_dispatch_routes_custom_realloc_from_config() {
        let state = mk_state();
        let ret_id = Ident::create_normal(IdentName::from_string("n"), 0);
        let callee = Procname::c_from_string("my_realloc");
        let cfg = config::InferConfig {
            pulse_model_realloc_pattern: Some("my_realloc".to_string()),
            ..config::InferConfig::default()
        };

        let result = dispatch_with_config(&callee, &ret_id, &[], &Location::dummy(), state, &cfg);
        assert!(
            result.is_some(),
            "configured realloc wrapper should dispatch"
        );
    }

    #[test]
    fn test_realloc_splits_on_nullable_input() {
        let mut state = mk_state();
        let ptr_id = Ident::create_normal(IdentName::from_string("p"), 0);
        state.post.stack.add(
            Var::LogicalVar(ptr_id.clone()),
            crate::abstract_value::AbstractValue::mk_fresh(),
        );

        let results = realloc(
            &Ident::create_normal(IdentName::from_string("n"), 0),
            &[(Exp::Var(ptr_id), Typ::void())],
            &Location::dummy(),
            state,
        );

        let continue_count = results.iter().filter(|result| result.is_continue()).count();
        assert_eq!(
            continue_count, 4,
            "realloc should split through free(NULL/non-null) and allocate-or-null on each branch"
        );
    }

    #[test]
    fn test_matches_configured_wrapper_for_free_pattern() {
        let callee = Procname::c_from_string("my_free");
        let cfg = config::InferConfig {
            pulse_model_free_pattern: Some("^my_free$".to_string()),
            ..config::InferConfig::default()
        };
        assert!(matches_configured_wrapper(&callee, &cfg));
    }

    #[test]
    fn test_use_after_free_via_model() {
        let mut state = mk_state();
        let pvar = Pvar::mk(Mangled::from_string("p"), Procname::c_from_string("test"));

        // malloc
        let n0 = Ident::create_normal(IdentName::from_string("n"), 0);
        let results = malloc(&n0, &Location::dummy(), state);
        state = match results.into_iter().next().unwrap() {
            ExecutionDomain::ContinueProgram(s) => s,
            _ => panic!("malloc should continue"),
        };

        let ptr_val = state.post.stack.find(&Var::LogicalVar(n0.clone())).unwrap();
        state
            .post
            .stack
            .add(Var::ProgramVar(Box::new(pvar.clone())), ptr_val);

        // free
        let n1 = Ident::create_normal(IdentName::from_string("n"), 1);
        let results = free(
            &n1,
            &[(Exp::Lvar(pvar.clone()), Typ::void())],
            &Location::dummy(),
            state,
        );
        state = match results.into_iter().find(|r| r.is_continue()).unwrap() {
            ExecutionDomain::ContinueProgram(s) => s,
            _ => panic!("free should continue"),
        };

        // Dereference after free → should detect UAF
        let deref_result = operations::eval_deref(&Exp::Lvar(pvar), &Location::dummy(), &mut state);
        assert!(
            matches!(deref_result, PulseResult::FatalError(_, _)),
            "use after free should be detected"
        );
    }
}
