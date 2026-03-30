// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Built-in function declarations recognized by the analysis.
//!
//! Mirrors OCaml's `BuiltinDecl.ml` / `BuiltinDecl.mli`.
//!
//! Each builtin is a pre-registered `Procname` that frontends emit and
//! the analysis engine can match on. The registry enables identity-based
//! matching (faster, handles qualified/mangled names) rather than ad-hoc
//! string comparisons.

use std::collections::HashSet;
use std::sync::LazyLock;

use crate::procname::Procname;

/// Create a C-function builtin procname.
fn c(name: &str) -> Procname {
    Procname::c_from_string(name)
}

/// All registered builtin procnames.
static BUILTINS: LazyLock<HashSet<Procname>> = LazyLock::new(|| {
    let builders: Vec<fn() -> Procname> = vec![
        malloc,
        free,
        __new,
        __new_array,
        __delete,
        __delete_array,
        __placement_new,
        __placement_delete,
        abort,
        exit,
        __assert_fail,
        __cast,
        __instanceof,
        __infer_assume,
        __infer_fail,
        __infer_skip,
        __throw,
        __get_array_length,
        __set_array_length,
        __objc_alloc_no_fail,
        __get_lazy_class,
        __lazy_class_initialize,
    ];
    builders.into_iter().map(|f| f()).collect()
});

/// Check if a procname is a declared builtin.
pub fn is_declared(pname: &Procname) -> bool {
    BUILTINS.contains(pname)
}

/// Match a builtin by comparing method names.
///
/// Mirrors OCaml's `BuiltinDecl.match_builtin`.
pub fn match_builtin(builtin: &Procname, candidate: &Procname) -> bool {
    builtin.get_method_name() == candidate.get_method_name()
}

// ---- Memory management ----

pub fn malloc() -> Procname {
    c("malloc")
}
pub fn free() -> Procname {
    c("free")
}

// ---- C++ new/delete ----

pub fn __new() -> Procname {
    c("__new")
}
pub fn __new_array() -> Procname {
    c("__new_array")
}
pub fn __delete() -> Procname {
    c("__delete")
}
pub fn __delete_array() -> Procname {
    c("__delete_array")
}
pub fn __placement_new() -> Procname {
    c("__placement_new")
}
pub fn __placement_delete() -> Procname {
    c("__placement_delete")
}

// ---- Control flow / assertions ----

pub fn abort() -> Procname {
    c("abort")
}
pub fn exit() -> Procname {
    c("exit")
}
pub fn __assert_fail() -> Procname {
    c("__assert_fail")
}

// ---- Type / casting ----

pub fn __cast() -> Procname {
    c("__cast")
}
pub fn __instanceof() -> Procname {
    c("__instanceof")
}

// ---- Infer internals ----

pub fn __infer_assume() -> Procname {
    c("__infer_assume")
}
pub fn __infer_fail() -> Procname {
    c("__infer_fail")
}
pub fn __infer_skip() -> Procname {
    c("__infer_skip")
}
pub fn __throw() -> Procname {
    c("__throw")
}

// ---- Arrays ----

pub fn __get_array_length() -> Procname {
    c("__get_array_length")
}
pub fn __set_array_length() -> Procname {
    c("__set_array_length")
}

// ---- ObjC ----

pub fn __objc_alloc_no_fail() -> Procname {
    c("__objc_alloc_no_fail")
}

// ---- Lazy classes (Hack) ----

pub fn __get_lazy_class() -> Procname {
    c("__get_lazy_class")
}
pub fn __lazy_class_initialize() -> Procname {
    c("__lazy_class_initialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_declared() {
        assert!(is_declared(&malloc()));
        assert!(is_declared(&free()));
        assert!(is_declared(&__new()));
        assert!(!is_declared(&Procname::c_from_string("unknown_func")));
    }

    #[test]
    fn test_match_builtin() {
        let candidate = Procname::c_from_string("free");
        assert!(match_builtin(&free(), &candidate));
        assert!(!match_builtin(&malloc(), &candidate));
    }
}
