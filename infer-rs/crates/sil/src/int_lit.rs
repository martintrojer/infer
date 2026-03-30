// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

use std::fmt;

use num_bigint::BigInt;
use num_traits::{One, Signed, Zero};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Signed and unsigned integer literals.
///
/// Mirrors OCaml's `IntLit.t`. Uses arbitrary precision integers.
/// The OCaml implementation distinguishes between pointer and non-pointer
/// null values; we track this with the `is_pointer` flag.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IntLit {
    value: BigInt,
    is_pointer: bool,
}

// Custom serde: serialize BigInt as string.
impl Serialize for IntLit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut s = serializer.serialize_struct("IntLit", 2)?;
        s.serialize_field("value", &self.value.to_string())?;
        s.serialize_field("is_pointer", &self.is_pointer)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for IntLit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct IntLitHelper {
            value: String,
            is_pointer: bool,
        }
        let helper = IntLitHelper::deserialize(deserializer)?;
        let value = helper
            .value
            .parse::<BigInt>()
            .map_err(serde::de::Error::custom)?;
        Ok(IntLit {
            value,
            is_pointer: helper.is_pointer,
        })
    }
}

impl IntLit {
    pub fn of_int(v: i64) -> Self {
        Self {
            value: BigInt::from(v),
            is_pointer: false,
        }
    }

    pub fn of_big_int(v: BigInt) -> Self {
        Self {
            value: v,
            is_pointer: false,
        }
    }

    pub fn zero() -> Self {
        Self::of_int(0)
    }

    pub fn one() -> Self {
        Self::of_int(1)
    }

    pub fn two() -> Self {
        Self::of_int(2)
    }

    pub fn minus_one() -> Self {
        Self::of_int(-1)
    }

    /// Null pointer constant. Behaves like zero except for `is_null`.
    pub fn null() -> Self {
        Self {
            value: BigInt::zero(),
            is_pointer: true,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.value.is_zero()
    }

    pub fn is_one(&self) -> bool {
        self.value.is_one()
    }

    pub fn is_minus_one(&self) -> bool {
        self.value == BigInt::from(-1)
    }

    pub fn is_null(&self) -> bool {
        self.is_pointer && self.value.is_zero()
    }

    pub fn is_negative(&self) -> bool {
        self.value.is_negative()
    }

    pub fn value(&self) -> &BigInt {
        &self.value
    }

    pub fn to_i64(&self) -> Option<i64> {
        use num_traits::ToPrimitive;
        self.value.to_i64()
    }

    pub fn add(&self, other: &Self) -> Self {
        Self {
            value: &self.value + &other.value,
            is_pointer: false,
        }
    }

    pub fn sub(&self, other: &Self) -> Self {
        Self {
            value: &self.value - &other.value,
            is_pointer: false,
        }
    }

    pub fn mul(&self, other: &Self) -> Self {
        Self {
            value: &self.value * &other.value,
            is_pointer: false,
        }
    }

    pub fn neg(&self) -> Self {
        Self {
            value: -&self.value,
            is_pointer: false,
        }
    }
}

impl fmt::Display for IntLit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.value)
    }
}

impl PartialOrd for IntLit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IntLit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value.cmp(&other.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let zero = IntLit::zero();
        let one = IntLit::one();
        assert!(zero.is_zero());
        assert!(one.is_one());
        assert!(!zero.is_null());

        let null = IntLit::null();
        assert!(null.is_null());
        assert!(null.is_zero());
    }

    #[test]
    fn test_arithmetic() {
        let a = IntLit::of_int(3);
        let b = IntLit::of_int(4);
        let sum = a.add(&b);
        assert_eq!(sum.to_i64(), Some(7));
    }
}
