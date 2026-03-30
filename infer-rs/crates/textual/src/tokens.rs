// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Token types shared between lexer and parser.
//!
//! The token set mirrors `TextualMenhir` tokens. Compound tokens like
//! `ProcAndLParen` are produced by the adapter layer in `lexer.rs`,
//! not by the raw logos lexer.

use num_bigint::BigInt;

/// Source span (byte offsets).
pub type Loc = (usize, usize);

/// Tokens for the Textual grammar.
///
/// Atomic tokens are produced by logos. Compound tokens (marked below)
/// are produced by the adapter layer that sits between logos and lalrpop.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    // ---- Keywords ----
    Declare,
    Define,
    Else,
    Extends,
    Equals,
    False,
    FloatKw,
    Global,
    Handlers,
    If,
    IntKw,
    Jmp,
    Load,
    LocalKw,
    Null,
    Prune,
    Ret,
    Store,
    Then,
    Throw,
    True,
    Type,
    Unreachable,
    Void,
    Wildcard,

    // ---- Punctuation ----
    Ampersand, // &
    And,       // &&
    Arrow,     // ->
    Assign,    // <-
    Colon,     // :
    Comma,     // ,
    Dot,       // .
    Ellipsis,  // ...
    Eq,        // =
    LAngle,    // <
    LBrace,    // {
    LParen,    // (
    LBracket,  // [
    Not,       // !
    Or,        // ||
    RAngle,    // >
    RBrace,    // }
    RParen,    // )
    RBracket,  // ]
    Semicolon, // ;
    Star,      // *
    Question,  // ?

    // ---- Literals & identifiers ----
    Ident(String),
    /// Local SSA variable `nN`.
    Local(i32),
    Integer(BigInt),
    FloatingPoint(f64),
    /// Label `#name`.
    Label(String),
    StringLit(String),

    // ---- Compound tokens (produced by adapter, not logos) ----
    /// `name(` or `class.name(` → (Option<class>, name).
    /// Mirrors OCaml's `PROC_AND_LPAREN`.
    ProcAndLParen(Option<String>, String),
    /// `if(` → mirrors OCaml's `IF_AND_LPAREN`.
    IfAndLParen,
    /// `fun(` → mirrors OCaml's `FUN`.
    Fun,
    /// `(fun _ -> _)` → mirrors OCaml's `FUNTYPE`.
    FunType,

    /// End of input sentinel.
    Eof,
}

impl std::fmt::Display for Tok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "{s}"),
            Tok::Local(n) => write!(f, "n{n}"),
            Tok::Integer(i) => write!(f, "{i}"),
            Tok::FloatingPoint(v) => write!(f, "{v}"),
            Tok::Label(s) => write!(f, "#{s}"),
            Tok::StringLit(s) => write!(f, "\"{s}\""),
            Tok::ProcAndLParen(None, name) => write!(f, "{name}("),
            Tok::ProcAndLParen(Some(cls), name) => write!(f, "{cls}.{name}("),
            other => write!(f, "{other:?}"),
        }
    }
}
