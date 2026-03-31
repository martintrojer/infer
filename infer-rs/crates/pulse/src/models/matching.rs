// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Shared procname matching helpers for config-driven models.

use regex::Regex;
use sil::procname::Procname;

/// Match a procname against an OCaml-configured regex pattern.
///
/// OCaml uses `Str.string_match r s 0`, so the pattern must match at the
/// start of the procname string. Shared `.inferconfig` files also use OCaml
/// `Str.regexp` syntax such as `\\(my\\|a\\)_malloc`, so translate the subset
/// of grouping/alternation escapes used in those configs before compiling with
/// Rust's regex engine.
pub(crate) fn matches_procname_pattern(callee: &Procname, pattern: Option<&str>) -> bool {
    let Some(pattern) = pattern else {
        return false;
    };

    let translated = translate_ocaml_regex(pattern);
    let Ok(regex) = Regex::new(&translated) else {
        return false;
    };
    let proc_name = callee.to_string();
    regex.find(&proc_name).is_some_and(|m| m.start() == 0)
}

fn translate_ocaml_regex(pattern: &str) -> String {
    let mut translated = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some('(' | ')' | '|') = chars.peek().copied() {
                translated.push(chars.next().expect("peeked character should exist"));
                continue;
            }
        }
        translated.push(ch);
    }
    translated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_procname_pattern_uses_ocaml_str_syntax() {
        let callee = Procname::c_from_string("a_malloc");
        assert!(matches_procname_pattern(
            &callee,
            Some("\\(my\\|a\\)_malloc")
        ));
        assert!(!matches_procname_pattern(&callee, Some("^my_malloc$")));
    }
}
