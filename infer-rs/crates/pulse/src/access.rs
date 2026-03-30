// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Heap access paths — how to reach a value from an address.
//!
//! Mirrors OCaml's `PulseAccess.ml`.

use std::fmt;

use serde::{Deserialize, Serialize};

use sil::fieldname::Fieldname;
use sil::typ::Typ;

use crate::abstract_value::AbstractValue;

/// How to access a value from a heap address.
///
/// Mirrors OCaml's `PulseAccess.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Access {
    /// Struct field access: `addr.field`.
    FieldAccess(Fieldname),
    /// Array element access: `addr[index]` with element type.
    ArrayAccess(Typ, AbstractValue),
    /// Pointer dereference: `*addr`.
    Dereference,
}

impl Access {
    /// Canonicalize by replacing abstract values with their representatives.
    pub fn canonicalize(&self, get_var_repr: impl Fn(AbstractValue) -> AbstractValue) -> Self {
        match self {
            Access::ArrayAccess(typ, idx) => {
                let new_idx = get_var_repr(*idx);
                Access::ArrayAccess(typ.clone(), new_idx)
            }
            Access::FieldAccess(_) | Access::Dereference => self.clone(),
        }
    }
}

impl PartialOrd for Access {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Access {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Access::FieldAccess(a), Access::FieldAccess(b)) => a.field_name.cmp(&b.field_name),
            (Access::FieldAccess(_), _) => std::cmp::Ordering::Less,
            (_, Access::FieldAccess(_)) => std::cmp::Ordering::Greater,
            (Access::ArrayAccess(_, a), Access::ArrayAccess(_, b)) => a.cmp(b),
            (Access::ArrayAccess(_, _), _) => std::cmp::Ordering::Less,
            (_, Access::ArrayAccess(_, _)) => std::cmp::Ordering::Greater,
            (Access::Dereference, Access::Dereference) => std::cmp::Ordering::Equal,
        }
    }
}

impl fmt::Display for Access {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Access::FieldAccess(field) => write!(f, ".{field}"),
            Access::ArrayAccess(_, idx) => write!(f, "[{idx}]"),
            Access::Dereference => write!(f, "*"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::fieldname::Fieldname;
    use sil::typ::TypeName;

    #[test]
    fn test_access_display() {
        let field = Access::FieldAccess(Fieldname::make(
            TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string("S")),
            "x",
        ));
        assert!(format!("{field}").contains("x"));
        assert_eq!(format!("{}", Access::Dereference), "*");
    }

    #[test]
    fn test_canonicalize_field_unchanged() {
        let field = Access::FieldAccess(Fieldname::make(
            TypeName::CStruct(sil::qualified_cpp_name::QualifiedCppName::from_string("S")),
            "x",
        ));
        let canon = field.canonicalize(|v| v);
        assert_eq!(field, canon);
    }

    #[test]
    fn test_canonicalize_array_replaces_index() {
        let v1 = AbstractValue::of_raw(1);
        let v2 = AbstractValue::of_raw(2);
        let arr = Access::ArrayAccess(Typ::void(), v1);
        let canon = arr.canonicalize(|_| v2);
        assert_eq!(canon, Access::ArrayAccess(Typ::void(), v2));
    }
}
