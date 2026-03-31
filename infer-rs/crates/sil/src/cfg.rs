// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::collections::HashMap;

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
        self.proc_descs.extend(other.proc_descs);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::location::Location;
    use crate::procname::Procname;
    use crate::typ::Typ;

    fn mk_proc(name: &str) -> Procdesc {
        Procdesc::new(
            Procname::c_from_string(name),
            Typ::void(),
            Location::dummy(),
        )
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
    fn test_merge_prefers_other_on_duplicate_procname() {
        let pname = Procname::c_from_string("dup");
        let mut lhs = Cfg::new();
        let mut left = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        left.is_no_return = false;
        lhs.add_proc_desc(left);

        let mut rhs = Cfg::new();
        let mut right = Procdesc::new(pname.clone(), Typ::void(), Location::dummy());
        right.is_no_return = true;
        rhs.add_proc_desc(right);

        lhs.merge(rhs);

        assert!(
            lhs.get_proc_desc(&pname)
                .expect("merged proc should exist")
                .is_no_return
        );
    }
}
