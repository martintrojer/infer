// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Summary specialization types.
//!
//! Mirrors OCaml's `IR/Specialization.ml`.
//!
//! Summary specialization increases precision by re-analyzing a callee
//! in a calling context. For example, when a function pointer argument
//! has a known target in the caller, the callee can be re-analyzed with
//! that knowledge to produce a more precise specialized summary.

use std::collections::HashMap;

use crate::fieldname::Fieldname;
use crate::pvar::Pvar;
use crate::typ::TypeName;

/// Heap symbolic path in a precondition context.
///
/// Mirrors OCaml's `Specialization.HeapPath.t`.
/// Describes a path through the heap from a program variable,
/// following field accesses and dereferences.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HeapPath {
    /// A program variable (the root of the path).
    Pvar(Pvar),
    /// A field access: `path.field`.
    FieldAccess(Fieldname, Box<HeapPath>),
    /// A pointer dereference: `*path`.
    Dereference(Box<HeapPath>),
}

impl std::fmt::Display for HeapPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeapPath::Pvar(pvar) => write!(f, "{pvar}"),
            HeapPath::FieldAccess(field, path) => write!(f, "{path}->{field}"),
            HeapPath::Dereference(path) => write!(f, "*{path}"),
        }
    }
}

/// Pulse specialization: dynamic types for heap paths.
///
/// Mirrors OCaml's `Specialization.Pulse.t`.
///
/// Currently focused on `dynamic_types` for function pointer dispatch.
/// OCaml also has `aliases` for aliasing-based specialization which
/// we may add later.
/// Pulse specialization: dynamic types for heap paths.
///
/// Mirrors OCaml's `Specialization.Pulse.t`.
///
/// Currently focused on `dynamic_types` for function pointer dispatch.
/// OCaml also has `aliases` for aliasing-based specialization which
/// we may add later.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PulseSpecialization {
    /// Dynamic type bindings: maps heap paths to their known dynamic type.
    /// For C function pointers, the type name encodes the target procedure.
    /// Cross-ref: OCaml `Specialization.Pulse.dynamic_types`.
    pub dynamic_types: HashMap<HeapPath, TypeName>,
}

impl PulseSpecialization {
    pub fn bottom() -> Self {
        Self::default()
    }

    pub fn is_bottom(&self) -> bool {
        self.dynamic_types.is_empty()
    }
}

impl std::fmt::Display for PulseSpecialization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.dynamic_types.is_empty() {
            write!(f, "⊥")
        } else {
            let parts: Vec<String> = self
                .dynamic_types
                .iter()
                .map(|(path, ty)| format!("{path}: {ty}"))
                .collect();
            write!(f, "dynamic_types: {{{}}}", parts.join(", "))
        }
    }
}
