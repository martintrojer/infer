// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Abstract values — symbolic addresses in the Pulse heap.
//!
//! Mirrors OCaml's `PulseAbstractValue.ml`.
//!
//! An abstract value is a fresh integer representing a symbolic heap address.
//! Positive values are "unrestricted" (can be anything), negative values are
//! "restricted" (non-negative in the concrete — used for arithmetic solving).
//!
//! Counters are thread-local so each procedure analysis gets deterministic
//! IDs regardless of parallelism. Call `reset_counters()` before each
//! procedure to ensure reproducible results.

use std::cell::Cell;
use std::fmt;

use serde::{Deserialize, Serialize};

/// A symbolic address in the Pulse abstract heap.
///
/// Just a newtype over `i64`. Positive = unrestricted, negative = restricted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AbstractValue(i64);

thread_local! {
    /// Thread-local counter for fresh unrestricted values (positive, counting up).
    static NEXT_FRESH: Cell<i64> = const { Cell::new(1) };
    /// Thread-local counter for fresh restricted values (negative, counting down).
    static NEXT_FRESH_RESTRICTED: Cell<i64> = const { Cell::new(-1) };
}

impl AbstractValue {
    /// Create a new fresh unrestricted abstract value.
    pub fn mk_fresh() -> Self {
        NEXT_FRESH.with(|c| {
            let v = c.get();
            c.set(v + 1);
            Self(v)
        })
    }

    /// Create a new fresh restricted abstract value (represents a non-negative concrete value).
    pub fn mk_fresh_restricted() -> Self {
        NEXT_FRESH_RESTRICTED.with(|c| {
            let v = c.get();
            c.set(v - 1);
            Self(v)
        })
    }

    /// Create a fresh value of the same kind (restricted or unrestricted).
    pub fn mk_fresh_same_kind(&self) -> Self {
        if self.is_restricted() {
            Self::mk_fresh_restricted()
        } else {
            Self::mk_fresh()
        }
    }

    /// Whether this is a restricted (non-negative) value.
    pub fn is_restricted(&self) -> bool {
        self.0 < 0
    }

    /// Whether this is an unrestricted value.
    pub fn is_unrestricted(&self) -> bool {
        self.0 > 0
    }

    /// Get the raw integer value.
    pub fn raw(&self) -> i64 {
        self.0
    }

    /// Create from a raw integer (for testing).
    pub fn of_raw(v: i64) -> Self {
        Self(v)
    }

    /// Reset the thread-local counters. Call before each procedure analysis
    /// to ensure deterministic abstract value IDs.
    pub fn reset_counters() {
        NEXT_FRESH.with(|c| c.set(1));
        NEXT_FRESH_RESTRICTED.with(|c| c.set(-1));
    }
}

impl fmt::Display for AbstractValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_restricted() {
            write!(f, "a{}", -self.0)
        } else {
            write!(f, "v{}", self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fresh_values_are_unique() {
        AbstractValue::reset_counters();
        let v1 = AbstractValue::mk_fresh();
        let v2 = AbstractValue::mk_fresh();
        let v3 = AbstractValue::mk_fresh();
        assert_ne!(v1, v2);
        assert_ne!(v2, v3);
        assert!(v1.is_unrestricted());
    }

    #[test]
    fn test_restricted_values() {
        AbstractValue::reset_counters();
        let r1 = AbstractValue::mk_fresh_restricted();
        let r2 = AbstractValue::mk_fresh_restricted();
        assert_ne!(r1, r2);
        assert!(r1.is_restricted());
        assert!(!r1.is_unrestricted());
    }

    #[test]
    fn test_display() {
        let v = AbstractValue::of_raw(42);
        assert_eq!(format!("{v}"), "v42");
        let r = AbstractValue::of_raw(-3);
        assert_eq!(format!("{r}"), "a3");
    }

    #[test]
    fn test_same_kind() {
        AbstractValue::reset_counters();
        let v = AbstractValue::mk_fresh();
        let v2 = v.mk_fresh_same_kind();
        assert!(v2.is_unrestricted());

        let r = AbstractValue::mk_fresh_restricted();
        let r2 = r.mk_fresh_same_kind();
        assert!(r2.is_restricted());
    }

    #[test]
    fn test_deterministic_after_reset() {
        AbstractValue::reset_counters();
        let a = AbstractValue::mk_fresh();
        let b = AbstractValue::mk_fresh();
        let c = AbstractValue::mk_fresh();

        AbstractValue::reset_counters();
        let a2 = AbstractValue::mk_fresh();
        let b2 = AbstractValue::mk_fresh();
        let c2 = AbstractValue::mk_fresh();

        assert_eq!(a, a2);
        assert_eq!(b, b2);
        assert_eq!(c, c2);
    }
}
