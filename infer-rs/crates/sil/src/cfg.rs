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

    pub fn add_proc_desc(&mut self, pdesc: Procdesc) {
        self.proc_descs.insert(pdesc.proc_name.clone(), pdesc);
    }

    pub fn get_proc_desc(&self, name: &Procname) -> Option<&Procdesc> {
        self.proc_descs.get(name)
    }

    pub fn iter_proc_descs(&self) -> impl Iterator<Item = &Procdesc> {
        self.proc_descs.values()
    }

    pub fn num_procs(&self) -> usize {
        self.proc_descs.len()
    }
}
