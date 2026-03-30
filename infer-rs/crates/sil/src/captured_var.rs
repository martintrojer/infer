// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use serde::{Deserialize, Serialize};

use crate::location::Location;
use crate::procname::Procname;
use crate::pvar::Pvar;
use crate::typ::Typ;

/// Capture mode for variables captured by closures/blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureMode {
    ByReference,
    ByValue,
}

/// Information about how a variable was captured.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapturedInfo {
    pub loc: Location,
    /// If the captured variable is a formal parameter, the procname of the enclosing function.
    pub is_formal: Option<Procname>,
}

/// Context information for captured variables (ObjC blocks).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContextInfo {
    pub is_checked_for_null: bool,
    pub is_internal_pointer_of: Option<Typ>,
}

/// A captured variable in a closure/block.
///
/// Mirrors OCaml's `CapturedVar.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapturedVar {
    pub pvar: Pvar,
    pub typ: Typ,
    pub capture_mode: CaptureMode,
    /// Only set for captured variables in ObjC blocks.
    pub captured_from: Option<CapturedInfo>,
    /// Only set for captured variables in ObjC blocks.
    pub context_info: Option<ContextInfo>,
}
