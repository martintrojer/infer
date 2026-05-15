// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Base domain: the composite abstract state (stack + heap + attributes).
//!
//! Mirrors OCaml's `PulseBaseDomain.ml`.

use std::fmt;

use crate::abstract_value::AbstractValue;
use crate::base_attrs::BaseAddressAttributes;
use crate::base_memory::BaseMemory;
use crate::base_stack::BaseStack;

/// The base abstract state: stack, heap, and address attributes.
///
/// This is the raw state before abductive reasoning wraps it with
/// pre/post separation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseDomain {
    pub stack: BaseStack,
    pub heap: BaseMemory,
    pub attrs: BaseAddressAttributes,
}

impl BaseDomain {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Substitute an abstract value: replace `old` with `new` everywhere.
    pub fn subst_var(&mut self, old: AbstractValue, new: AbstractValue) {
        self.stack.subst_var(old, new);
        self.heap.subst_var(old, new);
        self.attrs.subst_var(old, new);
    }

    /// Substitution with OCaml's heap aliasing-contradiction check. See
    /// [`BaseMemory::subst_var_or_unsat`].
    pub fn subst_var_or_unsat(
        &mut self,
        old: AbstractValue,
        new: AbstractValue,
    ) -> crate::sat_unsat::SatUnsat<()> {
        self.stack.subst_var(old, new);
        match self.heap.subst_var_or_unsat(old, new) {
            crate::sat_unsat::SatUnsat::Sat(()) => {
                self.attrs.subst_var(old, new);
                crate::sat_unsat::SatUnsat::Sat(())
            }
            crate::sat_unsat::SatUnsat::Unsat => crate::sat_unsat::SatUnsat::Unsat,
        }
    }
}

impl fmt::Display for BaseDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{{ stack=[{stack}]; heap=[{heap}]; attrs=[{attrs}] }}",
            stack = self.stack,
            heap = self.heap,
            attrs = self.attrs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Access;
    use crate::value_history::ValueHistory;
    use sil::ident::{Ident, IdentName};
    use sil::int_lit::IntLit;
    use sil::location::Location;
    use sil::var::Var;

    #[test]
    fn test_base_domain_empty() {
        let dom = BaseDomain::empty();
        assert!(dom.stack.is_empty());
        assert!(dom.heap.is_empty());
        assert!(dom.attrs.is_empty());
    }

    #[test]
    fn test_base_domain_stack_heap_attrs() {
        let mut dom = BaseDomain::empty();

        let v1 = AbstractValue::mk_fresh();
        let v2 = AbstractValue::mk_fresh();

        // Stack: variable x → v1
        let x = Var::LogicalVar(Ident::create_normal(IdentName::from_string("x"), 0));
        dom.stack.add(x.clone(), v1);

        // Heap: v1 --*--> v2
        dom.heap.add_edge(v1, Access::Dereference, v2);

        // Attrs: v2 is invalid (null)
        dom.attrs.invalidate(
            v2,
            crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
            ValueHistory::invalidated(
                crate::invalidation::Invalidation::ConstantDereference(IntLit::zero()),
                Location::dummy(),
            ),
        );

        // Verify: following x → v1 --*--> v2, and v2 is invalid
        let addr = dom.stack.find(&x).unwrap();
        let target = dom.heap.find_edge(addr, &Access::Dereference).unwrap();
        assert!(dom.attrs.check_valid(target).is_err());
    }

    #[test]
    fn test_subst_var() {
        let mut dom = BaseDomain::empty();
        let v1 = AbstractValue::of_raw(10);
        let v2 = AbstractValue::of_raw(20);
        let v3 = AbstractValue::of_raw(30);

        let x = Var::LogicalVar(Ident::create_normal(IdentName::from_string("x"), 0));
        dom.stack.add(x.clone(), v1);
        dom.heap.add_edge(v1, Access::Dereference, v2);

        // Replace v1 with v3
        dom.subst_var(v1, v3);

        // Stack should now map x → v3
        assert_eq!(dom.stack.find(&x), Some(v3));
        // Heap should have v3 --*--> v2
        assert_eq!(dom.heap.find_edge(v3, &Access::Dereference), Some(v2));
    }
}
