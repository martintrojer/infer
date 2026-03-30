// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::mangled::Mangled;
use crate::procname::Procname;
use crate::source_file::SourceFile;
use crate::typ::TemplateSpecInfo;

/// Program variable kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PvarKind {
    /// Local variable or formal parameter.
    Local {
        proc_name: Procname,
        is_syntactic: bool,
    },
    /// Callee program variable (for recursion handling).
    Callee(Procname),
    /// Global variable.
    Global {
        translation_unit: Option<SourceFile>,
        is_constexpr: bool,
        is_ice: bool,
        is_pod: bool,
        is_static_local: bool,
        is_static_global: bool,
        is_constant_array: bool,
        is_const: bool,
        template_args: TemplateSpecInfo,
    },
    /// Seed variable (initial value of formal parameters).
    Seed(Procname),
}

/// Program variable.
///
/// Mirrors OCaml's `Pvar.t`. There are 4 kinds: local, callee, global, and seed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pvar {
    pub name: Mangled,
    pub kind: PvarKind,
}

impl Pvar {
    pub fn mk(name: Mangled, proc_name: Procname) -> Self {
        Self {
            name,
            kind: PvarKind::Local {
                proc_name,
                is_syntactic: false,
            },
        }
    }

    pub fn mk_global(name: Mangled) -> Self {
        Self {
            name,
            kind: PvarKind::Global {
                translation_unit: None,
                is_constexpr: false,
                is_ice: false,
                is_pod: true,
                is_static_local: false,
                is_static_global: false,
                is_constant_array: false,
                is_const: false,
                template_args: TemplateSpecInfo::NoTemplate,
            },
        }
    }

    pub fn is_global(&self) -> bool {
        matches!(self.kind, PvarKind::Global { .. })
    }

    pub fn is_local(&self) -> bool {
        matches!(self.kind, PvarKind::Local { .. })
    }

    pub fn is_return(&self) -> bool {
        self.name.plain == "__return"
    }

    pub fn is_this(&self) -> bool {
        self.name.plain == "this"
    }

    pub fn is_self(&self) -> bool {
        self.name.plain == "self"
    }
}

impl fmt::Display for Pvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}
