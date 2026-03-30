// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Textual IR parser and printer for the Infer static analyzer.
//!
//! Textual is a human-readable, text-based intermediate representation for SIL.
//! It serves as the stable interface between language frontends and the analysis
//! engine.
//!
//! Architecture:
//! - `tokens`  — token types shared between lexer and parser
//! - `lexer`   — logos for atomic tokens + adapter for compound tokens
//! - `parser`  — recursive-descent parser (methods mirror `TextualMenhir.mly` rules)
//! - `ast`     — AST types mirroring `Textual.mli`
//! - `printer` — pretty printer for roundtrip testing

pub mod ast;
pub mod decls;
pub mod lexer;
pub mod parser;
pub mod printer;
pub mod to_sil;
pub mod tokens;
pub mod transform;
pub mod type_check;
pub mod verification;

pub use ast::*;
pub use parser::parse_module;
