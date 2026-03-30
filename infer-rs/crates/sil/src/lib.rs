// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! SIL (Smallfoot Intermediate Language) - Core types for the Infer static analyzer.
//!
//! This crate provides Rust representations of the core SIL types that are the
//! lingua franca of the Infer analysis framework. All source languages are
//! translated into SIL before analysis.

pub mod annot;
pub mod binop;
pub mod builtin_decl;
pub mod call_flags;
pub mod captured_var;
pub mod cfg;
pub mod const_val;
pub mod exp;
pub mod fieldname;
pub mod ident;
pub mod instr;
pub mod int_lit;
pub mod location;
pub mod mangled;
pub mod procdesc;
pub mod procname;
pub mod pvar;
pub mod qualified_cpp_name;
pub mod source_file;
pub mod specialization;
pub mod strukt;
pub mod tenv;
pub mod typ;
pub mod unop;
pub mod var;
