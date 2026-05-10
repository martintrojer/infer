// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::source_file::SourceFile;

/// Location in the original source file.
///
/// Mirrors OCaml's `Location.t`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Location {
    /// The name of the source file.
    pub file: SourceFile,
    /// The line number. -1 means "do not know".
    pub line: i32,
    /// The column number. -1 means "do not know".
    pub col: i32,
    /// If the location comes from macro expansion, the file the macro is defined in.
    pub macro_file_opt: Option<SourceFile>,
    /// If the location comes from macro expansion, the line number.
    pub macro_line: i32,
}

impl Location {
    pub const fn dummy() -> Self {
        Self {
            file: SourceFile::invalid(),
            line: -1,
            col: -1,
            macro_file_opt: None,
            macro_line: -1,
        }
    }

    pub fn is_dummy(&self) -> bool {
        self.line == -1 && self.col == -1
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}
