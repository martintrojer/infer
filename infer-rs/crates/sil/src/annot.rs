// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use serde::{Deserialize, Serialize};

/// A single annotation parameter.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AnnotParam {
    Str(String),
    Bool(bool),
    Int(i64),
    Null,
    Enum { class_name: String, value: String },
    Array(Vec<AnnotParam>),
    Class(String),
    Annot(Annot),
}

/// A single annotation (e.g., `@Nullable`).
///
/// Mirrors OCaml's `Annot.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Annot {
    pub class_name: String,
    pub parameters: Vec<AnnotParam>,
}

impl Annot {
    /// Annotation that marks a class/field as `final`.
    ///
    /// Mirrors OCaml's `Annot.final`.
    pub fn final_() -> Self {
        Self {
            class_name: "final".to_string(),
            parameters: Vec::new(),
        }
    }

    pub fn is_final(&self) -> bool {
        self.class_name == "final" && self.parameters.is_empty()
    }
}

/// A list of annotations, representing an annotation item.
///
/// Mirrors OCaml's `Annot.Item.t`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct AnnotItem(pub Vec<Annot>);

impl AnnotItem {
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Mirrors OCaml's `Annot.Item.is_final`.
    pub fn is_final(&self) -> bool {
        self.0.iter().any(Annot::is_final)
    }
}
