// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use serde::{Deserialize, Serialize};

/// Flags for a procedure call.
///
/// Mirrors OCaml's `CallFlags.t`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CallFlags {
    pub cf_assign_last_arg: bool,
    /// True if this is an implicit C++ destructor call injected by the clang frontend.
    pub cf_injected_destructor: bool,
    pub cf_interface: bool,
    pub cf_is_objc_block: bool,
    pub cf_is_objc_getter_setter: bool,
    pub cf_virtual: bool,
}
