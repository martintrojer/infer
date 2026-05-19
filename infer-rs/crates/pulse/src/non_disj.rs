// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Non-disjunctive Pulse side domain.
//!
//! This is the Phase-1 scaffold for the Rust port of OCaml's
//! `PulseNonDisjunctiveDomain` over-approximate `astate` sideband. It is kept
//! isolated from transfer/summary wiring for now: subsequent phases will feed
//! dropped `ContinueProgram` disjuncts into this domain, execute the hidden
//! state after each instruction, and export/apply the hidden summary pre/post.
//!
//! Deliberately not ported here: OCaml's `intra` copy/const-ref/lifetime maps
//! and `inter` transitive-info bookkeeping. The arithmetic-focused port only
//! needs the sticky dropped-disjunct bit and a bounded over-approximate
//! `AbductiveDomain` slot.

#![allow(dead_code)]

use absint::domain::{AbstractDomain, Comparable, WithBottom};

use crate::abductive::AbductiveDomain;
use crate::execution_domain::ExecutionDomain;

/// Over-approximate hidden Pulse state carried outside the ordinary
/// disjunctive list.
///
/// Cross-ref: OCaml `PulseNonDisjunctiveDomain.OverApproxDomain` is a
/// bottom-lifted `(AbductiveDomain.t * PathContext.t)` joined with
/// `PulseJoin.join`. Rust does not yet have a Pulse join for two abductive
/// states, so Phase 1 intentionally uses a deterministic single-state
/// retention policy with a coarse "any non-bottom hidden state subsumes any
/// other" ordering behind the same API. Phase 2+ can replace only
/// `join_over_approx`/`leq` when a proper bounded join/list representation
/// lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonDisjDomain {
    /// Sticky bit set once the disjunctive interpreter drops any disjunct.
    ///
    /// Mirrors OCaml `has_dropped_disjuncts: AbstractDomain.BooleanOr.t` and
    /// Rust's existing summary-level `has_dropped_disjuncts` flag.
    has_dropped_disjuncts: bool,
    /// Hidden over-approximate continuing state. `None` is bottom.
    over_approx: Option<AbductiveDomain>,
}

impl NonDisjDomain {
    /// Bottom non-disjunctive domain: no disjuncts dropped, no hidden state.
    pub fn bottom() -> Self {
        Self {
            has_dropped_disjuncts: false,
            over_approx: None,
        }
    }

    /// Top placeholder for the OCaml `WithBottomTop` surface.
    ///
    /// OCaml's over-approximate domain deliberately defines `top = bottom` for
    /// the hidden astate, because the abstract interpreter uses top as a hack
    /// for no executable disjuncts. Keep that shape here without exposing any
    /// semantic effect in Phase 1.
    pub fn top() -> Self {
        Self {
            has_dropped_disjuncts: true,
            over_approx: None,
        }
    }

    pub fn is_bottom(&self) -> bool {
        !self.has_dropped_disjuncts && self.over_approx.is_none()
    }

    pub fn is_top(&self) -> bool {
        self.has_dropped_disjuncts && self.over_approx.is_none()
    }

    pub fn has_dropped_disjuncts(&self) -> bool {
        self.has_dropped_disjuncts
    }

    pub fn is_over_approx_bottom(&self) -> bool {
        self.over_approx.is_none()
    }

    pub fn over_approx(&self) -> Option<&AbductiveDomain> {
        self.over_approx.as_ref()
    }

    /// Join two non-disjunctive domains.
    ///
    /// The dropped-disjunct bit is BooleanOr. The hidden state currently uses
    /// deterministic single-slot retention: if both sides have states, keep
    /// the left-hand state. The corresponding `leq` treats any non-bottom
    /// hidden state as subsuming any other so this scaffold remains a stable
    /// bounded lattice without claiming OCaml `PulseJoin.join` precision.
    pub fn join(&self, other: &Self) -> Self {
        Self {
            has_dropped_disjuncts: self.has_dropped_disjuncts || other.has_dropped_disjuncts,
            over_approx: join_over_approx(self.over_approx.as_ref(), other.over_approx.as_ref()),
        }
    }

    pub fn widen(&self, next: &Self, _num_iters: usize) -> Self {
        self.join(next)
    }

    /// Record disjuncts dropped by the disjunctive domain.
    ///
    /// Any dropped state sets the sticky bit. Only dropped
    /// `ContinueProgram` payloads enter the hidden over-approximate state;
    /// stopped states are intentionally ignored for the arithmetic-focused
    /// minimal port, matching the day-plan scope.
    pub fn remember_dropped_disjuncts<I>(&self, dropped: I) -> Self
    where
        I: IntoIterator<Item = ExecutionDomain>,
    {
        let mut result = self.clone();
        let mut saw_dropped = false;
        for exec in dropped {
            saw_dropped = true;
            if let ExecutionDomain::ContinueProgram(astate) = exec {
                result = result.join_to_astate(astate);
            }
        }
        if saw_dropped {
            result.has_dropped_disjuncts = true;
        }
        result
    }

    /// Join one hidden over-approximate state into the domain.
    pub fn join_to_astate(&self, astate: AbductiveDomain) -> Self {
        Self {
            has_dropped_disjuncts: self.has_dropped_disjuncts,
            over_approx: join_over_approx(self.over_approx.as_ref(), Some(&astate)),
        }
    }

    /// Prepare the non-disjunctive domain for ordinary disjunct execution.
    ///
    /// Cross-ref: OCaml `for_disjunct_exec_instr` clears only `astate` so the
    /// normal disjunct transfer cannot recursively consume the hidden
    /// over-approximate state. The sticky dropped-disjunct bit is preserved.
    pub fn for_disjunct_exec_instr(&self) -> Self {
        Self {
            has_dropped_disjuncts: self.has_dropped_disjuncts,
            over_approx: None,
        }
    }
}

fn join_over_approx(
    lhs: Option<&AbductiveDomain>,
    rhs: Option<&AbductiveDomain>,
) -> Option<AbductiveDomain> {
    match (lhs, rhs) {
        (None, None) => None,
        (Some(astate), None) | (None, Some(astate)) => Some(astate.clone()),
        (Some(lhs), Some(_rhs)) => {
            // TODO(nondisj_phase2+): replace this with a real over-approximate
            // Pulse join or a tiny bounded sideband. Keeping the left slot is
            // deterministic and avoids pretending this scaffold has full OCaml
            // `PulseJoin.join` semantics.
            Some(lhs.clone())
        }
    }
}

impl Default for NonDisjDomain {
    fn default() -> Self {
        Self::bottom()
    }
}

impl Comparable for NonDisjDomain {
    fn leq(&self, rhs: &Self) -> bool {
        let dropped_leq = !self.has_dropped_disjuncts || rhs.has_dropped_disjuncts;
        let over_approx_leq = match (&self.over_approx, &rhs.over_approx) {
            (None, _) => true,
            (Some(_), None) => false,
            // Phase-1 single-slot abstraction: any non-bottom hidden state is
            // an upper bound for any other non-bottom hidden state. This keeps
            // `join` lawful enough for future product-domain plumbing while
            // the actual retained payload remains deterministic.
            (Some(_), Some(_)) => true,
        };
        dropped_leq && over_approx_leq
    }

    fn equal_fast(&self, rhs: &Self) -> bool {
        self == rhs
    }
}

impl AbstractDomain for NonDisjDomain {
    fn join(&self, other: &Self) -> Self {
        Self::join(self, other)
    }

    fn widen(&self, next: &Self, num_iters: usize) -> Self {
        Self::widen(self, next, num_iters)
    }
}

impl WithBottom for NonDisjDomain {
    fn bottom() -> Self {
        Self::bottom()
    }

    fn is_bottom(&self) -> bool {
        Self::is_bottom(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sil::location::Location;
    use sil::procdesc::Procdesc;
    use sil::procname::Procname;
    use sil::typ::Typ;

    fn test_astate(name: &str) -> AbductiveDomain {
        let pdesc = Procdesc::new(
            Procname::c_from_string(name),
            Typ::void(),
            Location::dummy(),
        );
        AbductiveDomain::mk_initial(&pdesc)
    }

    #[test]
    fn bottom_has_no_dropped_disjuncts_or_over_approx_state() {
        let domain = NonDisjDomain::bottom();
        assert!(domain.is_bottom());
        assert!(!domain.has_dropped_disjuncts());
        assert!(domain.is_over_approx_bottom());
    }

    #[test]
    fn remember_dropped_continue_sets_sticky_bit_and_hidden_state() {
        let astate = test_astate("dropped_continue");
        let domain = NonDisjDomain::bottom()
            .remember_dropped_disjuncts([ExecutionDomain::ContinueProgram(astate.clone())]);

        assert!(domain.has_dropped_disjuncts());
        assert_eq!(domain.over_approx(), Some(&astate));
        assert!(!domain.is_over_approx_bottom());
    }

    #[test]
    fn remember_dropped_stopped_state_sets_bit_without_hidden_continue() {
        let astate = test_astate("dropped_exit");
        let domain = NonDisjDomain::bottom()
            .remember_dropped_disjuncts([ExecutionDomain::ExitProgram(astate)]);

        assert!(domain.has_dropped_disjuncts());
        assert!(domain.is_over_approx_bottom());
    }

    #[test]
    fn join_or_combines_dropped_bit_and_keeps_available_hidden_state() {
        let astate = test_astate("join_rhs");
        let lhs =
            NonDisjDomain::bottom().remember_dropped_disjuncts([ExecutionDomain::ExitProgram(
                test_astate("join_lhs_stopped"),
            )]);
        let rhs = NonDisjDomain::bottom().join_to_astate(astate.clone());

        let joined = lhs.join(&rhs);

        assert!(joined.has_dropped_disjuncts());
        assert_eq!(joined.over_approx(), Some(&astate));
    }

    #[test]
    fn for_disjunct_exec_instr_preserves_dropped_bit_and_clears_hidden_state() {
        let domain =
            NonDisjDomain::bottom().remember_dropped_disjuncts([ExecutionDomain::ContinueProgram(
                test_astate("for_disjunct"),
            )]);

        let ordinary = domain.for_disjunct_exec_instr();

        assert!(ordinary.has_dropped_disjuncts());
        assert!(ordinary.is_over_approx_bottom());
    }
}
