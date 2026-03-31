// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::typ::TypeName;

/// Names for fields of class/struct/union.
///
/// Mirrors OCaml's `Fieldname.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Fieldname {
    pub class_name: TypeName,
    pub field_name: String,
}

impl Fieldname {
    pub fn make(class_name: TypeName, field_name: impl Into<String>) -> Self {
        Self {
            class_name,
            field_name: field_name.into(),
        }
    }
}

impl fmt::Display for Fieldname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.class_name, self.field_name)
    }
}
