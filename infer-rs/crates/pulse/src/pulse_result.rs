// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Tri-state result type for Pulse analysis operations.
//!
//! Mirrors OCaml's `PulseResult.ml`.
//!
//! Analysis operations can:
//! - Succeed (`Ok`)
//! - Succeed but accumulate recoverable errors (`Recoverable`)
//! - Fail fatally (`FatalError`)

use std::fmt;

/// Result of a Pulse operation.
///
/// Unlike `std::result::Result`, this has a third state: `Recoverable` carries
/// both a successful value AND accumulated errors (e.g. potential bugs found
/// on this path that may or may not be real depending on path feasibility).
#[derive(Clone, Debug)]
pub enum PulseResult<T, E> {
    /// Success, no errors.
    Ok(T),
    /// Success, but with accumulated recoverable errors.
    Recoverable(T, Vec<E>),
    /// Fatal error — analysis of this path aborts.
    FatalError(E, Vec<E>),
}

impl<T, E> PulseResult<T, E> {
    /// Map over the success value.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> PulseResult<U, E> {
        match self {
            PulseResult::Ok(x) => PulseResult::Ok(f(x)),
            PulseResult::Recoverable(x, errs) => PulseResult::Recoverable(f(x), errs),
            PulseResult::FatalError(e, errs) => PulseResult::FatalError(e, errs),
        }
    }

    /// Flat-map (bind) over the success value.
    pub fn and_then<U>(self, f: impl FnOnce(T) -> PulseResult<U, E>) -> PulseResult<U, E> {
        match self {
            PulseResult::Ok(x) => f(x),
            PulseResult::FatalError(e, errs) => PulseResult::FatalError(e, errs),
            PulseResult::Recoverable(x, errors) => match f(x) {
                PulseResult::Ok(y) => PulseResult::Recoverable(y, errors),
                PulseResult::Recoverable(y, more_errors) => {
                    let mut all = more_errors;
                    all.extend(errors);
                    PulseResult::Recoverable(y, all)
                }
                PulseResult::FatalError(fatal, more_errors) => {
                    let mut all = more_errors;
                    all.extend(errors);
                    PulseResult::FatalError(fatal, all)
                }
            },
        }
    }

    /// Append errors to any result variant.
    pub fn append_errors(self, errors: Vec<E>) -> Self {
        if errors.is_empty() {
            return self;
        }
        match self {
            PulseResult::Ok(x) => PulseResult::Recoverable(x, errors),
            PulseResult::Recoverable(x, mut existing) => {
                existing.extend(errors);
                PulseResult::Recoverable(x, existing)
            }
            PulseResult::FatalError(fatal, mut existing) => {
                existing.extend(errors);
                PulseResult::FatalError(fatal, existing)
            }
        }
    }

    /// Extract the Ok value, if present.
    pub fn ok(self) -> Option<T> {
        match self {
            PulseResult::Ok(x) => Some(x),
            _ => None,
        }
    }

    /// Is this an Ok result (no errors at all)?
    pub fn is_ok(&self) -> bool {
        matches!(self, PulseResult::Ok(_))
    }

    /// Is this a fatal error?
    pub fn is_fatal(&self) -> bool {
        matches!(self, PulseResult::FatalError(_, _))
    }

    /// Create a fatal error from a single error.
    pub fn fatal(err: E) -> Self {
        PulseResult::FatalError(err, Vec::new())
    }
}

impl<T: fmt::Display, E: fmt::Display> fmt::Display for PulseResult<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PulseResult::Ok(x) => write!(f, "Ok({x})"),
            PulseResult::Recoverable(x, errs) => {
                write!(f, "Recoverable({x}, {} errors)", errs.len())
            }
            PulseResult::FatalError(e, errs) => {
                write!(f, "FatalError({e}, {} more)", errs.len())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ok() {
        let r: PulseResult<i32, String> = PulseResult::Ok(42);
        assert!(r.is_ok());
        assert_eq!(r.ok(), Some(42));
    }

    #[test]
    fn test_map() {
        let r: PulseResult<i32, String> = PulseResult::Ok(10);
        let mapped = r.map(|x| x * 2);
        assert_eq!(mapped.ok(), Some(20));
    }

    #[test]
    fn test_and_then_ok() {
        let r: PulseResult<i32, String> = PulseResult::Ok(10);
        let result = r.and_then(|x| PulseResult::Ok(x + 1));
        assert_eq!(result.ok(), Some(11));
    }

    #[test]
    fn test_and_then_recoverable_accumulates() {
        let r: PulseResult<i32, String> = PulseResult::Recoverable(10, vec!["err1".into()]);
        let result = r.and_then(|x| PulseResult::Recoverable(x + 1, vec!["err2".into()]));
        match result {
            PulseResult::Recoverable(val, errs) => {
                assert_eq!(val, 11);
                assert_eq!(errs.len(), 2);
            }
            other => panic!("expected Recoverable, got {other:?}"),
        }
    }

    #[test]
    fn test_fatal() {
        let r: PulseResult<i32, String> = PulseResult::fatal("boom".into());
        assert!(r.is_fatal());
        assert!(r.ok().is_none());
    }

    #[test]
    fn test_append_errors() {
        let r: PulseResult<i32, String> = PulseResult::Ok(42);
        let r2 = r.append_errors(vec!["warning".into()]);
        assert!(!r2.is_ok()); // now Recoverable
    }
}
