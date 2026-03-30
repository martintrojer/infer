// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Test harness for running analyses on Textual source strings.
//!
//! Provides utilities to parse `.sil` text, convert to SIL, and run analyses
//! with assertions keyed by Textual label names. This lets tests be written
//! as readable Textual programs rather than hand-constructed SIL CFGs.

use std::collections::HashMap;

use sil::cfg::Cfg;
use sil::procdesc::{NodeId, Procdesc};
use sil::tenv::Tenv;

/// Result of parsing and converting a Textual source string.
pub struct TestModule {
    pub cfg: Cfg,
    pub tenv: Tenv,
    /// Maps (proc_name, label_name) → node_id in the SIL CFG.
    pub label_to_node: HashMap<String, Vec<(String, NodeId)>>,
}

/// Parse a Textual source string and convert to SIL.
///
/// Panics on parse or conversion errors (intended for tests).
pub fn parse_and_convert(src: &str) -> TestModule {
    let module =
        textual::parse_module(src, "test.sil").unwrap_or_else(|e| panic!("parse error: {e}"));
    module_to_test_module(module)
}

/// Parse a Textual source file from disk and convert to SIL.
///
/// Panics on I/O, parse, or conversion errors (intended for tests).
pub fn parse_file_and_convert(path: &std::path::Path) -> TestModule {
    let src = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("test.sil");

    let module =
        textual::parse_module(&src, filename).unwrap_or_else(|e| panic!("parse error: {e}"));
    module_to_test_module(module)
}

/// Convert a parsed Textual module to a TestModule (SIL + label map).
///
/// Runs the Textual transform pipeline before conversion, matching
/// OCaml's behavior (let_propagation inlines __sil_* builtins).
fn module_to_test_module(module: textual::ast::Module) -> TestModule {
    module_to_test_module_inner(module)
}

fn module_to_test_module_inner(mut module: textual::ast::Module) -> TestModule {
    let (decls, decl_errors) = textual::decls::DeclEnv::from_module(&module);
    assert!(
        decl_errors.is_empty(),
        "declaration errors: {decl_errors:?}"
    );

    textual::transform::run(&mut module, &decls);

    let (cfg, tenv) = textual::to_sil::module_to_sil(&module, &decls)
        .unwrap_or_else(|e| panic!("conversion errors: {e:?}"));

    // Build label→node_id mapping from the Textual AST.
    // The Textual nodes map 1:1 to SIL nodes created by to_sil.
    // Node IDs: 0=start, 1=exit, 2..=textual nodes in order.
    let mut label_to_node: HashMap<String, Vec<(String, NodeId)>> = HashMap::new();
    for decl in &module.decls {
        if let textual::ast::Decl::Proc(pdesc) = decl {
            let proc_name = format!("{}", pdesc.procdecl.qualified_name);
            for (i, node) in pdesc.nodes.iter().enumerate() {
                let node_id = (i as NodeId) + 2; // 0=start, 1=exit
                label_to_node
                    .entry(node.label.value.clone())
                    .or_default()
                    .push((proc_name.clone(), node_id));
            }
        }
    }

    TestModule {
        cfg,
        tenv,
        label_to_node,
    }
}

impl TestModule {
    /// Get the first procedure's Procdesc.
    ///
    /// Panics if there are no procedures.
    pub fn first_proc(&self) -> &Procdesc {
        self.cfg
            .iter_proc_descs()
            .next()
            .expect("no procedures in module")
    }

    /// Get a procedure's Procdesc by name.
    ///
    /// Panics if no procedure with the given name exists.
    pub fn proc_by_name(&self, name: &str) -> &Procdesc {
        self.cfg
            .iter_proc_descs()
            .find(|pd| format!("{}", pd.proc_name) == name)
            .unwrap_or_else(|| panic!("procedure '{name}' not found"))
    }

    /// Get the node ID for a label in the first (or only) procedure.
    ///
    /// Panics if the label is not found.
    pub fn node_id(&self, label: &str) -> NodeId {
        self.label_to_node
            .get(label)
            .and_then(|v| v.first())
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("label '{label}' not found"))
    }

    /// Get the node ID for a label in a specific procedure.
    ///
    /// Panics if the label is not found for the given procedure.
    pub fn node_id_in(&self, proc_name: &str, label: &str) -> NodeId {
        self.label_to_node
            .get(label)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|(pn, _)| pn == proc_name)
                    .map(|(_, id)| *id)
            })
            .unwrap_or_else(|| panic!("label '{label}' not found in procedure '{proc_name}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_and_convert() {
        let tm = parse_and_convert(
            r#"
            .source_language = "java"
            define f(x: int) : int {
              #entry:
                n0 : int = load &x
                ret n0
            }
        "#,
        );
        assert_eq!(tm.cfg.num_procs(), 1);
        assert!(tm.label_to_node.contains_key("entry"));
    }

    #[test]
    fn test_node_id_lookup() {
        let tm = parse_and_convert(
            r#"
            .source_language = "java"
            define f(x: int) : void {
              #entry:
                n0 : int = load &x
                jmp done
              #done:
                ret null
            }
        "#,
        );
        let entry_id = tm.node_id("entry");
        let done_id = tm.node_id("done");
        assert_ne!(entry_id, done_id);
    }
}
