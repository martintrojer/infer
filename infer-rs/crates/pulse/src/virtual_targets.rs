// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Shared helpers for virtual-call target collection.
//!
//! Cross-ref: review note #344, follow-up task `sil_virtual_target_summary_cache`.
//!
//! Without an index, every virtual call site that the CLI / end-to-end test
//! harnesses see triggers an O(N) walk over `cfg.iter_proc_descs()` to find
//! same-name candidate methods, and re-runs `analyze_with_spec_loop` for each
//! candidate without consulting the global summary cache. On a non-trivial
//! Cfg with many same-name methods this is O(callers * vcalls * candidates *
//! spec_depth).
//!
//! This module centralises:
//!   1. The language-aware "is this procname a virtual target of that one?"
//!      predicate (`virtual_target_name_matches`), shared between the CLI
//!      driver and the end-to-end test harness.
//!   2. A `VirtualTargetIndex` that buckets candidate procnames once per Cfg
//!      so each virtual call site does an O(1) candidate lookup instead of an
//!      O(N) scan.
//!
//! The index intentionally only buckets the language flavours where we
//! currently match by `(name, arity)` / `(name, parameters)`: Hack, Java,
//! Python. Other procname kinds fall through to "no candidates" because the
//! callers above all return `false` for cross-language matches.

use std::collections::HashMap;

use sil::cfg::Cfg;
use sil::procname::Procname;

/// Bucket key for the candidate index.
///
/// Each enum variant captures only the fields that
/// `virtual_target_name_matches` actually compares for that procname kind.
/// Two procnames hash/equal under the same key iff they would match under
/// `virtual_target_name_matches`.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum CandidateKey {
    Hack {
        function_name: String,
        arity: Option<i32>,
    },
    Java {
        method_name: String,
        parameters: Vec<sil::typ::Typ>,
    },
    Python {
        function_name: String,
        arity: Option<i32>,
    },
}

fn candidate_key(name: &Procname) -> Option<CandidateKey> {
    match name {
        Procname::Hack(p) => Some(CandidateKey::Hack {
            function_name: p.function_name.clone(),
            arity: p.arity,
        }),
        Procname::Java(p) => Some(CandidateKey::Java {
            method_name: p.method_name.clone(),
            parameters: p.parameters.clone(),
        }),
        Procname::Python(p) => Some(CandidateKey::Python {
            function_name: p.function_name.clone(),
            arity: p.arity,
        }),
        // Languages we do not currently devirtualise this way (C, ObjcCpp,
        // CSharp, Erlang, Rust, Swift, Block) match nothing in
        // `virtual_target_name_matches`, so we deliberately do not index them.
        _ => None,
    }
}

/// True when `target` is a candidate implementation for the virtual `callee`.
///
/// Mirrors the predicate that previously lived (twice) in
/// `crates/cli/src/main.rs` and `crates/pulse/tests/end_to_end.rs`. Both copies
/// matched purely by name + arity / parameters; this matches that legacy
/// behaviour exactly so the cache change is purely additive.
pub fn virtual_target_name_matches(callee: &Procname, target: &Procname) -> bool {
    candidate_key(callee)
        .as_ref()
        .zip(candidate_key(target).as_ref())
        .is_some_and(|(c, t)| c == t)
}

/// Pre-built map of `(name, arity)` → procnames that share that signature.
///
/// Construction walks the Cfg exactly once. Lookup is O(1) in the number of
/// procedures and yields the candidate procnames for a given virtual call.
#[derive(Debug, Default, Clone)]
pub struct VirtualTargetIndex {
    by_key: HashMap<CandidateKey, Vec<Procname>>,
}

impl VirtualTargetIndex {
    /// Empty index. Mostly useful for tests that want to call lookup-only
    /// helpers without scanning a real Cfg.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build the index from the procedures in `cfg`.
    ///
    /// Cost: O(num_procs) hash inserts + per-procname clones, paid once.
    pub fn build(cfg: &Cfg) -> Self {
        let mut by_key: HashMap<CandidateKey, Vec<Procname>> = HashMap::new();
        for pdesc in cfg.iter_proc_descs() {
            if let Some(key) = candidate_key(&pdesc.proc_name) {
                by_key.entry(key).or_default().push(pdesc.proc_name.clone());
            }
        }
        Self { by_key }
    }

    /// Return the candidate procnames that match `callee` under
    /// `virtual_target_name_matches`. Empty slice if `callee` is a
    /// language we do not index.
    pub fn candidates_for(&self, callee: &Procname) -> &[Procname] {
        candidate_key(callee)
            .and_then(|k| self.by_key.get(&k))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use sil::cfg::Cfg;
    use sil::location::Location;
    use sil::procdesc::Procdesc;
    use sil::procname::{HackProcname, JavaKind, JavaProcname, Procname, PythonProcname};
    use sil::typ::{HackClassName, JavaClassName, Typ};

    fn hack(name: &str, arity: Option<i32>) -> Procname {
        Procname::Hack(HackProcname {
            class_name: None,
            function_name: name.to_string(),
            arity,
        })
    }

    fn python(name: &str, arity: Option<i32>) -> Procname {
        Procname::Python(PythonProcname {
            class_name: None,
            function_name: name.to_string(),
            arity,
        })
    }

    fn java(name: &str, params: Vec<Typ>) -> Procname {
        Procname::Java(JavaProcname {
            class_name: JavaClassName("Foo".to_string()),
            method_name: name.to_string(),
            parameters: params,
            return_type: None,
            kind: JavaKind::NonStatic,
        })
    }

    fn empty_pdesc(name: Procname) -> Procdesc {
        Procdesc::new(name, Typ::void(), Location::dummy())
    }

    fn cfg_with(names: Vec<Procname>) -> Cfg {
        let mut cfg = Cfg::new();
        for n in names {
            cfg.add_proc_desc(empty_pdesc(n));
        }
        cfg
    }

    #[test]
    fn matches_same_hack_signature() {
        assert!(virtual_target_name_matches(
            &hack("foo", Some(1)),
            &hack("foo", Some(1))
        ));
        assert!(!virtual_target_name_matches(
            &hack("foo", Some(1)),
            &hack("foo", Some(2))
        ));
        assert!(!virtual_target_name_matches(
            &hack("foo", Some(1)),
            &python("foo", Some(1))
        ));
    }

    #[test]
    fn index_returns_only_matching_candidates() {
        let cfg = cfg_with(vec![
            hack("foo", Some(1)),
            hack("foo", Some(2)),
            hack("bar", Some(1)),
            python("foo", Some(1)),
            java("baz", vec![]),
        ]);
        let idx = VirtualTargetIndex::build(&cfg);

        let cands_foo1 = idx.candidates_for(&hack("foo", Some(1)));
        assert_eq!(cands_foo1.len(), 1);
        assert_eq!(&cands_foo1[0], &hack("foo", Some(1)));

        let cands_py = idx.candidates_for(&python("foo", Some(1)));
        assert_eq!(cands_py.len(), 1);
        assert_eq!(&cands_py[0], &python("foo", Some(1)));

        let cands_baz = idx.candidates_for(&java("baz", vec![]));
        assert_eq!(cands_baz.len(), 1);

        // Unindexed language → no candidates.
        let cands_c = idx.candidates_for(&Procname::c_from_string("foo"));
        assert!(cands_c.is_empty());
    }

    #[test]
    fn index_groups_overrides() {
        // Two Hack methods with the same (name, arity) but different class —
        // legacy behaviour ignores class, so both must be returned.
        let foo_a = Procname::Hack(HackProcname {
            class_name: Some(HackClassName("A".to_string())),
            function_name: "foo".into(),
            arity: Some(1),
        });
        let foo_b = Procname::Hack(HackProcname {
            class_name: Some(HackClassName("B".to_string())),
            function_name: "foo".into(),
            arity: Some(1),
        });
        let cfg = cfg_with(vec![foo_a.clone(), foo_b.clone()]);
        let idx = VirtualTargetIndex::build(&cfg);

        // Lookup by either concrete callee returns both.
        let cands = idx.candidates_for(&foo_a);
        assert_eq!(cands.len(), 2);
        let names: Vec<_> = cands.iter().collect();
        assert!(names.contains(&&foo_a));
        assert!(names.contains(&&foo_b));
    }
}
