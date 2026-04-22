// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Pulse models: built-in semantics for known functions.
//!
//! Mirrors OCaml's `PulseModelsC.ml`, `PulseModelsJava.ml`, etc.
//!
//! Each language-specific module provides model implementations.
//! The `dispatch` function routes a call to the appropriate model
//! using `sil::builtin_decl` for identity-based matching.

pub mod c;
pub mod configured;
pub mod matching;

use std::sync::LazyLock;

use sil::exp::Exp;
use sil::ident::Ident;
use sil::location::Location;
use sil::procname::Procname;
use sil::typ::Typ;

use crate::abductive::AbductiveDomain;
use crate::execution_domain::ExecutionDomain;

/// All method names that have models.
///
/// This is the single source of truth for "do we have a model for this
/// function?". Each model module contributes its names here.
/// Used for cheap pre-checking before cloning state.
static MODELED_FUNCTIONS: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
    let mut set = std::collections::HashSet::new();
    // C models
    for name in c::MODELED_NAMES {
        set.insert(*name);
    }
    // Future: Java, Hack, ObjC models add their names here
    set
});

/// Check if a callee has any model (cheap, no cloning).
pub fn has_model(callee: &Procname) -> bool {
    MODELED_FUNCTIONS.contains(callee.get_method_name())
        || configured::has_model(callee, config::get())
        || c::matches_configured_wrapper(callee, config::get())
}

/// Try to dispatch a call to a built-in model.
///
/// Returns `Some(results)` if a model was found, `None` if the call
/// should be handled as an unknown function.
pub fn dispatch(
    caller: Option<&Procname>,
    callee: &Procname,
    ret_id: &Ident,
    args: &[(Exp, Typ)],
    loc: &Location,
    state: AbductiveDomain,
) -> Option<Vec<ExecutionDomain>> {
    // Cheap pre-check: avoid cloning state for unmodeled functions
    if !has_model(callee) {
        return None;
    }

    // Try each language module in order
    if let Some(results) = c::dispatch(caller, callee, ret_id, args, loc, state.clone()) {
        return Some(results);
    }

    if let Some(results) = configured::dispatch(callee, ret_id, args, loc, state) {
        return Some(results);
    }

    // Future: Java, Hack, ObjC, C++ models go here

    None
}
