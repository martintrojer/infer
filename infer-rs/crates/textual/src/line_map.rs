// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Line map: maps textual `.sil` file line numbers to original source locations.
//!
//! Supports two mechanisms:
//! - `@[line:col]` annotations (C/C++ frontend) — per-instruction, parsed from each line
//! - `// .line N` / `// .column N` directives (Rust frontend) — cumulative, like C's `#line`
//!
//! Mirrors OCaml's `LineMap.ml`.

/// Original source location for a textual line.
#[derive(Clone, Debug)]
pub struct OrigLoc {
    pub line: usize,
    pub col: usize,
}

/// Maps 1-based textual line numbers to original source locations.
pub struct LineMap {
    /// Indexed by 0-based line number. `None` = no mapping for this line.
    entries: Vec<Option<OrigLoc>>,
}

impl LineMap {
    /// Build a line map by scanning source text for location annotations.
    ///
    /// Scans for both:
    /// - `@[line:col]` annotations (last one on a line wins)
    /// - `// .line N` and `// .column N` directives (cumulative state)
    pub fn create(source: &str) -> Self {
        let lines: Vec<&str> = source.split('\n').collect();
        let mut entries: Vec<Option<OrigLoc>> = Vec::with_capacity(lines.len());

        // Cumulative state for // .line / // .column directives
        let mut directive_line: Option<usize> = None;
        let mut directive_col: Option<usize> = None;

        for line in &lines {
            let trimmed = line.trim();

            // Check for // .line N directive
            if let Some(rest) = trimmed.strip_prefix("// .line ") {
                if let Ok(n) = rest.trim().parse::<usize>() {
                    directive_line = Some(n);
                    directive_col = None; // reset column on new line directive
                }
                entries.push(None); // directive lines themselves don't map
                continue;
            }

            // Check for // .column N directive
            if let Some(rest) = trimmed.strip_prefix("// .column ") {
                if let Ok(n) = rest.trim().parse::<usize>() {
                    directive_col = Some(n);
                }
                entries.push(None);
                continue;
            }

            // Check for @[line:col] annotation (last one on the line wins)
            if let Some(loc) = parse_last_annot(line) {
                entries.push(Some(loc));
                continue;
            }

            // No annotation — use cumulative directive state if available
            if let Some(dl) = directive_line {
                entries.push(Some(OrigLoc {
                    line: dl,
                    col: directive_col.unwrap_or(0),
                }));
            } else {
                entries.push(None);
            }
        }

        Self { entries }
    }

    /// Look up the original location for a 1-based textual line number.
    pub fn lookup(&self, textual_line: usize) -> Option<&OrigLoc> {
        if textual_line == 0 {
            return None;
        }
        self.entries.get(textual_line - 1)?.as_ref()
    }

    /// Returns true if this line map has any mappings.
    pub fn is_empty(&self) -> bool {
        self.entries.iter().all(|e| e.is_none())
    }
}

/// Parse the last `@[line:col]` annotation on a line.
fn parse_last_annot(line: &str) -> Option<OrigLoc> {
    let mut result = None;
    let mut search_from = 0;
    while let Some(start) = line[search_from..].find("@[") {
        let abs_start = search_from + start + 2;
        if let Some(end) = line[abs_start..].find(']') {
            let inner = &line[abs_start..abs_start + end];
            if let Some((l, c)) = inner.split_once(':') {
                if let (Ok(line_num), Ok(col_num)) = (l.parse::<usize>(), c.parse::<usize>()) {
                    result = Some(OrigLoc {
                        line: line_num,
                        col: col_num,
                    });
                }
            }
            search_from = abs_start + end + 1;
        } else {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_annot_basic() {
        let source = "\
#node_0: @[3:1]
    jmp node_1 @[3:1]
";
        let map = LineMap::create(source);
        assert!(!map.is_empty());
        let loc = map.lookup(1).unwrap();
        assert_eq!(loc.line, 3);
        assert_eq!(loc.col, 1);
        let loc2 = map.lookup(2).unwrap();
        assert_eq!(loc2.line, 3);
        assert_eq!(loc2.col, 1);
    }

    #[test]
    fn test_annot_multiple_on_line() {
        // Last @[line:col] on the line wins
        let source = "    n0 = malloc(<int>) @[4:12]\n";
        let map = LineMap::create(source);
        let loc = map.lookup(1).unwrap();
        assert_eq!(loc.line, 4);
        assert_eq!(loc.col, 12);
    }

    #[test]
    fn test_directive_line() {
        let source = "\
// .line 8
    n0 = some_call()
// .line 9
// .column 3
    store &p <- n1:*int
";
        let map = LineMap::create(source);
        // Line 1 is the directive itself — no mapping
        assert!(map.lookup(1).is_none());
        // Line 2 inherits .line 8
        let loc = map.lookup(2).unwrap();
        assert_eq!(loc.line, 8);
        assert_eq!(loc.col, 0);
        // Line 3 is directive
        assert!(map.lookup(3).is_none());
        // Line 4 is directive
        assert!(map.lookup(4).is_none());
        // Line 5 inherits .line 9, .column 3
        let loc2 = map.lookup(5).unwrap();
        assert_eq!(loc2.line, 9);
        assert_eq!(loc2.col, 3);
    }

    #[test]
    fn test_annot_overrides_directive() {
        let source = "\
// .line 8
    n0 = foo() @[42:7]
";
        let map = LineMap::create(source);
        // @[42:7] takes precedence over // .line 8
        let loc = map.lookup(2).unwrap();
        assert_eq!(loc.line, 42);
        assert_eq!(loc.col, 7);
    }

    #[test]
    fn test_no_annotations() {
        let source = "define foo() : void {\n  ret\n}\n";
        let map = LineMap::create(source);
        assert!(map.is_empty());
    }

    #[test]
    fn test_unknown_annotation() {
        let source = "#node_0: @?\n    jmp @?\n";
        let map = LineMap::create(source);
        assert!(map.is_empty());
    }
}
