// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::collections::{hash_map::Entry, HashMap};

use serde::{Deserialize, Serialize};

use crate::procdesc::Procdesc;
use crate::procname::Procname;

/// Control flow graph: a collection of procedure descriptions.
///
/// Mirrors OCaml's `Cfg.t` which is `Procdesc.t Procname.Hash.t`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cfg {
    pub proc_descs: HashMap<Procname, Procdesc>,
}

impl Cfg {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn merge(&mut self, other: Cfg) {
        for (pname, incoming) in other.proc_descs {
            match self.proc_descs.entry(pname) {
                Entry::Vacant(slot) => {
                    slot.insert(incoming);
                }
                Entry::Occupied(mut slot) => {
                    if should_replace_procdesc(slot.get(), &incoming) {
                        slot.insert(incoming);
                    }
                }
            }
        }
    }

    pub fn add_proc_desc(&mut self, pdesc: Procdesc) {
        self.proc_descs.insert(pdesc.proc_name.clone(), pdesc);
    }

    pub fn get_proc_desc(&self, name: &Procname) -> Option<&Procdesc> {
        self.proc_descs.get(name)
    }

    pub fn iter_proc_descs(&self) -> impl Iterator<Item = &Procdesc> {
        self.proc_descs.values()
    }

    pub fn iter_proc_descs_mut(&mut self) -> impl Iterator<Item = &mut Procdesc> {
        self.proc_descs.values_mut()
    }

    pub fn num_procs(&self) -> usize {
        self.proc_descs.len()
    }
}

fn should_replace_procdesc(current: &Procdesc, incoming: &Procdesc) -> bool {
    // OCaml stores one procdesc per proc UID in capture.db. Exported Textual
    // can still contain duplicate plain proc names, most often as a real body
    // plus one or more empty `@?` stubs. Never let an empty stub overwrite a
    // real body during merged direct-Textual analysis.
    match (
        current.is_defined && !current.is_empty_body(),
        incoming.is_defined && !incoming.is_empty_body(),
    ) {
        (false, true) => true,
        (true, false) => false,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instr::Instr;
    use crate::location::Location;
    use crate::procdesc::{NodeKind, StmtNodeKind};
    use crate::procname::Procname;
    use crate::typ::Typ;

    fn mk_proc(name: &str) -> Procdesc {
        Procdesc::new(
            Procname::c_from_string(name),
            Typ::void(),
            Location::dummy(),
        )
    }

    fn mk_real_proc(name: &str) -> Procdesc {
        let mut pdesc = mk_proc(name);
        pdesc.add_node(
            NodeKind::StmtNode(StmtNodeKind::MethodBody),
            vec![Instr::skip()],
            Location::dummy(),
        );
        pdesc
    }

    fn mk_stub_proc(name: &str) -> Procdesc {
        let mut pdesc = mk_proc(name);
        pdesc.is_defined = false;
        pdesc
    }

    #[test]
    fn test_merge_extends_proc_descs() {
        let mut lhs = Cfg::new();
        lhs.add_proc_desc(mk_proc("left"));

        let mut rhs = Cfg::new();
        rhs.add_proc_desc(mk_proc("right"));

        lhs.merge(rhs);

        assert!(lhs
            .get_proc_desc(&Procname::c_from_string("left"))
            .is_some());
        assert!(lhs
            .get_proc_desc(&Procname::c_from_string("right"))
            .is_some());
        assert_eq!(lhs.num_procs(), 2);
    }

    #[test]
    fn test_merge_replaces_stub_with_real_duplicate_procname() {
        let pname = Procname::c_from_string("dup");
        let mut lhs = Cfg::new();
        lhs.add_proc_desc(mk_stub_proc("dup"));

        let mut rhs = Cfg::new();
        let mut right = mk_real_proc("dup");
        right.is_no_return = true;
        rhs.add_proc_desc(right);

        lhs.merge(rhs);

        let merged = lhs.get_proc_desc(&pname).expect("merged proc should exist");
        assert!(merged.is_defined);
        assert!(merged.is_no_return);
        assert!(!merged.is_empty_body());
    }

    #[test]
    fn test_merge_keeps_existing_real_over_incoming_stub() {
        let pname = Procname::c_from_string("dup");
        let mut lhs = Cfg::new();
        let mut left = mk_real_proc("dup");
        left.is_no_return = true;
        lhs.add_proc_desc(left);

        let mut rhs = Cfg::new();
        rhs.add_proc_desc(mk_stub_proc("dup"));

        lhs.merge(rhs);

        let merged = lhs.get_proc_desc(&pname).expect("merged proc should exist");
        assert!(merged.is_defined);
        assert!(merged.is_no_return);
        assert!(!merged.is_empty_body());
    }
}
