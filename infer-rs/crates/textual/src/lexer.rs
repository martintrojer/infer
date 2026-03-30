// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Lexer for Textual IR.
//!
//! Two layers:
//! 1. `RawToken` — atomic tokens produced by logos (no lookahead).
//! 2. `Adapter` — merges raw tokens into compound tokens like `ProcAndLParen`
//!    that the lalrpop grammar expects. This mirrors the context-sensitive
//!    patterns in the OCaml sedlex lexer (`TextualLexer.ml`).

use logos::Logos;
use num_bigint::BigInt;
use std::collections::VecDeque;
use std::fmt;

use crate::tokens::Tok;

// ---------------------------------------------------------------------------
// Layer 1: logos raw tokens
// ---------------------------------------------------------------------------

/// Raw tokens produced by logos. These are strictly atomic — no lookahead.
#[derive(Logos, Clone, Debug, PartialEq)]
#[logos(skip r"[ \t\r\n]+")] // skip whitespace
#[logos(skip r"//[^\n]*")] // skip line comments
#[logos(skip r"/\*([^*]|\*[^/])*\*/")] // skip block comments
#[logos(skip r"@\[[0-9]+:[0-9]+\]")] // skip @[line:col] location annotations
#[logos(skip r"@[0-9]+")] // skip @line location annotations
#[logos(skip r"@\?")] // skip @? unknown location annotations
pub(crate) enum RawToken {
    // -- Punctuation (multi-char first for priority) --
    #[token("&&")]
    And,
    #[token("||")]
    Or,
    #[token("->")]
    Arrow,
    #[token("<-")]
    Assign,
    #[token("...")]
    Ellipsis,
    #[token("&")]
    Ampersand,
    #[token(":")]
    Colon,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,
    #[token("=")]
    Eq,
    #[token("<")]
    LAngle,
    #[token(">")]
    RAngle,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(";")]
    Semicolon,
    #[token("*")]
    Star,
    #[token("!")]
    Not,
    #[token("?")]
    Question,

    // -- Keywords --
    #[token("declare")]
    Declare,
    #[token("define")]
    Define,
    #[token("else")]
    Else,
    #[token("extends")]
    Extends,
    #[token("equals")]
    Equals,
    #[token("false")]
    False,
    #[token("float")]
    FloatKw,
    #[token("global")]
    Global,
    #[token("if")]
    If,
    #[token("int")]
    IntKw,
    #[token("jmp")]
    Jmp,
    #[token("load")]
    Load,
    #[token("local")]
    LocalKw,
    #[token("null")]
    Null,
    #[token("prune")]
    Prune,
    #[token("ret")]
    Ret,
    #[token("store")]
    Store,
    #[token("then")]
    Then,
    #[token("throw")]
    Throw,
    #[token("true")]
    True,
    #[token("type")]
    Type,
    #[token("unreachable")]
    Unreachable,
    #[token("void")]
    Void,

    // -- Compound keyword-like --
    #[token("fun")]
    FunKw,

    // -- Labels: #identifier --
    #[regex(r"#[a-zA-Z_$][a-zA-Z0-9_$]*", |lex| lex.slice()[1..].to_string())]
    Label(String),

    // -- Local variables: nN --
    #[regex(r"n[0-9]+", |lex| lex.slice()[1..].parse::<i32>().ok())]
    Local(i32),

    // -- Float literals (must come before integer to win priority) --
    #[regex(r"[+-]?[0-9]+\.[0-9]*([eE][+-]?[0-9]+)?", |lex| lex.slice().parse::<f64>().ok())]
    #[regex(r"[+-]?[0-9]+[eE][+-]?[0-9]+", |lex| lex.slice().parse::<f64>().ok())]
    FloatingPoint(f64),

    // -- Integer literals --
    #[regex(r"-?0[xX][0-9a-fA-F_]+[lL]?", lex_integer)]
    #[regex(r"-?0[bB][01_]+[lL]?", lex_integer)]
    #[regex(r"-?[0-9][0-9_]*[lL]?", lex_integer)]
    Integer(BigInt),

    // -- String literals --
    #[regex(r#""([^"\\]|\\.)*""#, lex_string)]
    StringLit(String),

    // -- Wildcard `_` (must be before Ident so it wins priority) --
    #[token("_")]
    Wildcard,

    // -- Identifiers (must be after keywords for logos priority) --
    // Supports `::` and `:::` in identifiers (e.g., `A::foo`, `mixed:::bar`).
    #[regex(r"[a-zA-Z_$][a-zA-Z0-9_$]*", |lex| lex.slice().to_string(), priority = 1)]
    Ident(String),
}

fn lex_integer(lex: &mut logos::Lexer<'_, RawToken>) -> Option<BigInt> {
    let mut s = lex.slice().to_string();
    // Strip trailing L/l suffix
    if s.ends_with('l') || s.ends_with('L') {
        s.pop();
    }
    // Remove underscores
    s.retain(|c| c != '_');
    s.parse::<BigInt>().ok()
}

fn lex_string(lex: &mut logos::Lexer<'_, RawToken>) -> String {
    let s = lex.slice();
    // Strip surrounding quotes, handle basic escapes
    let inner = &s[1..s.len() - 1];
    let mut result = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(escaped) = chars.next() {
                result.push(escaped);
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Convert a raw token to the corresponding atomic `Tok`.
/// Returns `None` for tokens that need compound handling (handled by adapter).
fn raw_to_tok(raw: &RawToken) -> Tok {
    match raw {
        RawToken::And => Tok::And,
        RawToken::Or => Tok::Or,
        RawToken::Arrow => Tok::Arrow,
        RawToken::Assign => Tok::Assign,
        RawToken::Ellipsis => Tok::Ellipsis,
        RawToken::Ampersand => Tok::Ampersand,
        RawToken::Colon => Tok::Colon,
        RawToken::Comma => Tok::Comma,
        RawToken::Dot => Tok::Dot,
        RawToken::Eq => Tok::Eq,
        RawToken::LAngle => Tok::LAngle,
        RawToken::RAngle => Tok::RAngle,
        RawToken::LBrace => Tok::LBrace,
        RawToken::RBrace => Tok::RBrace,
        RawToken::LParen => Tok::LParen,
        RawToken::RParen => Tok::RParen,
        RawToken::LBracket => Tok::LBracket,
        RawToken::RBracket => Tok::RBracket,
        RawToken::Semicolon => Tok::Semicolon,
        RawToken::Star => Tok::Star,
        RawToken::Not => Tok::Not,
        RawToken::Question => Tok::Question,
        RawToken::Declare => Tok::Declare,
        RawToken::Define => Tok::Define,
        RawToken::Else => Tok::Else,
        RawToken::Extends => Tok::Extends,
        RawToken::Equals => Tok::Equals,
        RawToken::False => Tok::False,
        RawToken::FloatKw => Tok::FloatKw,
        RawToken::Global => Tok::Global,
        RawToken::If => Tok::If,
        RawToken::IntKw => Tok::IntKw,
        RawToken::Jmp => Tok::Jmp,
        RawToken::Load => Tok::Load,
        RawToken::LocalKw => Tok::LocalKw,
        RawToken::Null => Tok::Null,
        RawToken::Prune => Tok::Prune,
        RawToken::Ret => Tok::Ret,
        RawToken::Store => Tok::Store,
        RawToken::Then => Tok::Then,
        RawToken::Throw => Tok::Throw,
        RawToken::True => Tok::True,
        RawToken::Type => Tok::Type,
        RawToken::Unreachable => Tok::Unreachable,
        RawToken::Void => Tok::Void,
        RawToken::Wildcard => Tok::Wildcard,
        RawToken::FunKw => Tok::Ident("fun".to_string()),
        RawToken::Label(s) => Tok::Label(s.clone()),
        RawToken::Local(n) => Tok::Local(*n),
        RawToken::FloatingPoint(f) => Tok::FloatingPoint(*f),
        RawToken::Integer(i) => Tok::Integer(i.clone()),
        RawToken::StringLit(s) => Tok::StringLit(s.clone()),
        RawToken::Ident(s) => Tok::Ident(s.clone()),
    }
}

/// Check if a raw token is an identifier-like token that can start a proc name.
fn is_ident_like(raw: &RawToken) -> bool {
    matches!(
        raw,
        RawToken::Ident(_)
            | RawToken::FunKw
            | RawToken::Declare
            | RawToken::Define
            | RawToken::Extends
            | RawToken::Equals
            | RawToken::Global
            | RawToken::Jmp
            | RawToken::Load
            | RawToken::LocalKw
            | RawToken::Prune
            | RawToken::Ret
            | RawToken::Store
            | RawToken::Throw
            | RawToken::Type
            | RawToken::Unreachable
    )
}

/// Extract the string value of an identifier-like token.
fn ident_str(raw: &RawToken) -> &str {
    match raw {
        RawToken::Ident(s) => s,
        RawToken::FunKw => "fun",
        RawToken::Declare => "declare",
        RawToken::Define => "define",
        RawToken::Extends => "extends",
        RawToken::Equals => "equals",
        RawToken::Global => "global",
        RawToken::Jmp => "jmp",
        RawToken::Load => "load",
        RawToken::LocalKw => "local",
        RawToken::Prune => "prune",
        RawToken::Ret => "ret",
        RawToken::Store => "store",
        RawToken::Throw => "throw",
        RawToken::Type => "type",
        RawToken::Unreachable => "unreachable",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Layer 2: adapter — merges raw tokens into compound tokens
// ---------------------------------------------------------------------------

/// Spanned raw token: (token, byte_start, byte_end).
type RawSpanned = (RawToken, usize, usize);

/// Lexer error.
#[derive(Clone, Debug)]
pub struct LexError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lex error at byte {}: {}", self.offset, self.message)
    }
}

/// Merge `ident :: ident` and `ident ::: ident` sequences into single Ident tokens.
///
/// Scans a raw token slice and greedily joins sequences like `A :: foo` into
/// `Ident("A::foo")`. Must run before compound-token rules so that e.g.
/// `A::foo(` merges into `ProcAndLParen(None, "A::foo")`.
fn merge_double_colon_idents(raw_tokens: VecDeque<RawSpanned>) -> VecDeque<RawSpanned> {
    let raw_vec: Vec<RawSpanned> = raw_tokens.into_iter().collect();
    let mut out: VecDeque<RawSpanned> = VecDeque::new();
    let mut i = 0;
    while i < raw_vec.len() {
        if is_ident_like(&raw_vec[i].0) {
            let start = raw_vec[i].1;
            let mut combined = ident_str(&raw_vec[i].0).to_string();
            let mut last_end = raw_vec[i].2;
            let mut j = i + 1;
            while j + 2 < raw_vec.len()
                && raw_vec[j].0 == RawToken::Colon
                && raw_vec[j + 1].0 == RawToken::Colon
            {
                // `:::ident` (triple colon)
                if j + 3 < raw_vec.len()
                    && raw_vec[j + 2].0 == RawToken::Colon
                    && is_ident_like(&raw_vec[j + 3].0)
                {
                    combined.push_str(":::");
                    combined.push_str(ident_str(&raw_vec[j + 3].0));
                    last_end = raw_vec[j + 3].2;
                    j += 4;
                    continue;
                }
                // `::ident` (double colon)
                if is_ident_like(&raw_vec[j + 2].0) {
                    combined.push_str("::");
                    combined.push_str(ident_str(&raw_vec[j + 2].0));
                    last_end = raw_vec[j + 2].2;
                    j += 3;
                    continue;
                }
                break;
            }
            if j > i + 1 {
                out.push_back((RawToken::Ident(combined), start, last_end));
            } else {
                out.push_back(raw_vec[i].clone());
            }
            i = j;
        } else {
            out.push_back(raw_vec[i].clone());
            i += 1;
        }
    }
    out
}

/// Merge compound tokens: `ident.ident(` → `ProcAndLParen`, etc.
///
/// Processes a queue of raw tokens and produces the final `Tok` stream.
fn merge_compound_tokens(mut q: VecDeque<RawSpanned>) -> Vec<(usize, Tok, usize)> {
    let mut out: Vec<(usize, Tok, usize)> = Vec::new();

    while let Some((tok, start, end)) = q.pop_front() {
        match &tok {
            // `.handlers` — dot followed by ident "handlers"
            RawToken::Dot if matches!(q.front(), Some((RawToken::Ident(s), _, _)) if s == "handlers") =>
            {
                let (_, _, end2) = q.pop_front().unwrap();
                out.push((start, Tok::Handlers, end2));
            }

            // `?` `.` ident `(` → ProcAndLParen(Some("?"), ident)
            RawToken::Question
                if matches_seq(
                    &q,
                    &[
                        |t| *t == RawToken::Dot,
                        is_ident_like,
                        |t| *t == RawToken::LParen,
                    ],
                ) =>
            {
                q.pop_front(); // dot
                let (id_tok, _, _) = q.pop_front().unwrap(); // ident
                let (_, _, end3) = q.pop_front().unwrap(); // lparen
                out.push((
                    start,
                    Tok::ProcAndLParen(Some("?".to_string()), ident_str(&id_tok).to_string()),
                    end3,
                ));
            }

            // ident `.` ident `(` → ProcAndLParen(Some(class), method)
            _ if is_ident_like(&tok)
                && matches_seq(
                    &q,
                    &[
                        |t| *t == RawToken::Dot,
                        is_ident_like,
                        |t| *t == RawToken::LParen,
                    ],
                ) =>
            {
                let class = ident_str(&tok).to_string();
                q.pop_front(); // dot
                let (id_tok, _, _) = q.pop_front().unwrap(); // method
                let (_, _, end3) = q.pop_front().unwrap(); // lparen
                let method = ident_str(&id_tok).to_string();
                out.push((start, Tok::ProcAndLParen(Some(class), method), end3));
            }

            // `if` `(` → IfAndLParen
            RawToken::If if matches!(q.front(), Some((RawToken::LParen, _, _))) => {
                let (_, _, end2) = q.pop_front().unwrap();
                out.push((start, Tok::IfAndLParen, end2));
            }

            // `fun` `(` → Fun
            RawToken::FunKw if matches!(q.front(), Some((RawToken::LParen, _, _))) => {
                let (_, _, end2) = q.pop_front().unwrap();
                out.push((start, Tok::Fun, end2));
            }

            // ident `(` → ProcAndLParen(None, ident)
            _ if is_ident_like(&tok) && matches!(q.front(), Some((RawToken::LParen, _, _))) => {
                let name = ident_str(&tok).to_string();
                let (_, _, end2) = q.pop_front().unwrap();
                out.push((start, Tok::ProcAndLParen(None, name), end2));
            }

            // `(` `fun` `_` `->` `_` `)` → FunType
            RawToken::LParen if matches_funtype(&q) => {
                for _ in 0..5 {
                    q.pop_front();
                }
                let end_ft = q.front().map_or(end, |t| t.1);
                out.push((start, Tok::FunType, end_ft));
            }

            // Default: emit as-is.
            _ => {
                out.push((start, raw_to_tok(&tok), end));
            }
        }
    }

    out
}

/// Lex input and produce the token stream.
///
/// Pipeline:
/// 1. logos produces raw tokens
/// 2. `merge_double_colon_idents` joins `ident :: ident` sequences
/// 3. `merge_compound_tokens` creates `ProcAndLParen`, `Handlers`, etc.
pub fn lex(input: &str) -> Result<Vec<(usize, Tok, usize)>, LexError> {
    let mut raw_tokens: VecDeque<RawSpanned> = VecDeque::new();
    let mut lex = RawToken::lexer(input);
    while let Some(result) = lex.next() {
        let span = lex.span();
        match result {
            Ok(tok) => raw_tokens.push_back((tok, span.start, span.end)),
            Err(()) => {
                return Err(LexError {
                    offset: span.start,
                    message: format!("unexpected token: {:?}", &input[span.start..span.end]),
                });
            }
        }
    }

    let merged = merge_double_colon_idents(raw_tokens);
    Ok(merge_compound_tokens(merged))
}

/// Check if the front of the queue matches a sequence of predicates.
fn matches_seq(q: &VecDeque<RawSpanned>, preds: &[fn(&RawToken) -> bool]) -> bool {
    if q.len() < preds.len() {
        return false;
    }
    preds
        .iter()
        .zip(q.iter())
        .all(|(pred, (tok, _, _))| pred(tok))
}

/// Check if queue starts with `fun _ -> _ )` (for FUNTYPE).
fn matches_funtype(q: &VecDeque<RawSpanned>) -> bool {
    if q.len() < 5 {
        return false;
    }
    let items: Vec<&RawToken> = q.iter().take(5).map(|(t, _, _)| t).collect();
    matches!(
        items.as_slice(),
        [
            RawToken::FunKw,
            RawToken::Wildcard,
            RawToken::Arrow,
            RawToken::Wildcard,
            RawToken::RParen
        ]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Tok> {
        lex(input).unwrap().into_iter().map(|(_, t, _)| t).collect()
    }

    #[test]
    fn test_punctuation() {
        let toks = tokens("{ } ( ) [ ] < > : , ; . * = ! & -> <- ...");
        assert_eq!(
            toks,
            vec![
                Tok::LBrace,
                Tok::RBrace,
                Tok::LParen,
                Tok::RParen,
                Tok::LBracket,
                Tok::RBracket,
                Tok::LAngle,
                Tok::RAngle,
                Tok::Colon,
                Tok::Comma,
                Tok::Semicolon,
                Tok::Dot,
                Tok::Star,
                Tok::Eq,
                Tok::Not,
                Tok::Ampersand,
                Tok::Arrow,
                Tok::Assign,
                Tok::Ellipsis,
            ]
        );
    }

    #[test]
    fn test_keywords() {
        let toks = tokens("define declare type global int float void ret jmp if then else");
        assert_eq!(
            toks,
            vec![
                Tok::Define,
                Tok::Declare,
                Tok::Type,
                Tok::Global,
                Tok::IntKw,
                Tok::FloatKw,
                Tok::Void,
                Tok::Ret,
                Tok::Jmp,
                Tok::If,
                Tok::Then,
                Tok::Else,
            ]
        );
    }

    #[test]
    fn test_ident_and_local() {
        let toks = tokens("foo n0 n123 bar_baz");
        assert_eq!(
            toks,
            vec![
                Tok::Ident("foo".into()),
                Tok::Local(0),
                Tok::Local(123),
                Tok::Ident("bar_baz".into()),
            ]
        );
    }

    #[test]
    fn test_proc_and_lparen() {
        let toks = tokens("f(x) A.f(x)");
        assert_eq!(
            toks,
            vec![
                Tok::ProcAndLParen(None, "f".into()),
                Tok::Ident("x".into()),
                Tok::RParen,
                Tok::ProcAndLParen(Some("A".into()), "f".into()),
                Tok::Ident("x".into()),
                Tok::RParen,
            ]
        );
    }

    #[test]
    fn test_if_and_lparen() {
        assert_eq!(
            tokens("if(x)"),
            vec![Tok::IfAndLParen, Tok::Ident("x".into()), Tok::RParen]
        );
        // `if x` without `(` stays as separate tokens
        assert_eq!(tokens("if x"), vec![Tok::If, Tok::Ident("x".into())]);
    }

    #[test]
    fn test_fun_and_lparen() {
        assert_eq!(
            tokens("fun(int) -> void)"),
            vec![
                Tok::Fun,
                Tok::IntKw,
                Tok::RParen,
                Tok::Arrow,
                Tok::Void,
                Tok::RParen,
            ]
        );
    }

    #[test]
    fn test_funtype() {
        assert_eq!(tokens("(fun _ -> _)"), vec![Tok::FunType]);
    }

    #[test]
    fn test_numbers() {
        let toks = tokens("42 3.14 0 -1");
        assert_eq!(
            toks,
            vec![
                Tok::Integer(BigInt::from(42)),
                Tok::FloatingPoint(3.14),
                Tok::Integer(BigInt::from(0)),
                Tok::Integer(BigInt::from(-1)),
            ]
        );
    }

    #[test]
    fn test_strings() {
        let toks = tokens(r#""hello" "world""#);
        assert_eq!(
            toks,
            vec![
                Tok::StringLit("hello".into()),
                Tok::StringLit("world".into())
            ]
        );
    }

    #[test]
    fn test_labels() {
        let toks = tokens("#entry #lab1");
        assert_eq!(
            toks,
            vec![Tok::Label("entry".into()), Tok::Label("lab1".into())]
        );
    }

    #[test]
    fn test_comments() {
        let toks = tokens("foo // comment\nbar /* block */ baz");
        assert_eq!(
            toks,
            vec![
                Tok::Ident("foo".into()),
                Tok::Ident("bar".into()),
                Tok::Ident("baz".into())
            ]
        );
    }

    #[test]
    fn test_handlers() {
        let toks = tokens(".handlers lab1, lab2");
        assert_eq!(
            toks,
            vec![
                Tok::Handlers,
                Tok::Ident("lab1".into()),
                Tok::Comma,
                Tok::Ident("lab2".into())
            ]
        );
    }

    #[test]
    fn test_source_language() {
        let toks = tokens(r#".source_language = "java""#);
        assert_eq!(
            toks,
            vec![
                Tok::Dot,
                Tok::Ident("source_language".into()),
                Tok::Eq,
                Tok::StringLit("java".into()),
            ]
        );
    }

    #[test]
    fn test_mangled_names() {
        let toks = tokens("A::foo mixed:::bar");
        assert_eq!(
            toks,
            vec![
                Tok::Ident("A::foo".into()),
                Tok::Ident("mixed:::bar".into())
            ]
        );
    }

    #[test]
    fn test_mangled_name_call() {
        // `A::foo(` should merge into ProcAndLParen
        let toks = tokens("A::foo()");
        assert_eq!(
            toks,
            vec![Tok::ProcAndLParen(None, "A::foo".into()), Tok::RParen,]
        );
        // `A::mixed:::zoo(` — chained double+triple colon
        let toks = tokens("A::mixed:::zoo()");
        assert_eq!(
            toks,
            vec![
                Tok::ProcAndLParen(None, "A::mixed:::zoo".into()),
                Tok::RParen,
            ]
        );
    }

    /// Regression test: logos with `::` in the Ident regex misassigns
    /// priority when a keyword token is immediately followed by `:`,
    /// producing `Ident("null")` instead of `Null`. The fix removes `::` from
    /// the regex and merges double-colon identifiers in the adapter.
    #[test]
    fn test_keywords_before_colon() {
        // null: should be Null + Colon, not Ident("null") + Colon
        assert_eq!(tokens("null:"), vec![Tok::Null, Tok::Colon]);
        assert_eq!(tokens("true:"), vec![Tok::True, Tok::Colon]);
        assert_eq!(tokens("false:"), vec![Tok::False, Tok::Colon]);
        assert_eq!(tokens("ret:"), vec![Tok::Ret, Tok::Colon]);
        assert_eq!(tokens("store:"), vec![Tok::Store, Tok::Colon]);
        // Store instruction with null value
        assert_eq!(
            tokens("<- null: *list"),
            vec![
                Tok::Assign,
                Tok::Null,
                Tok::Colon,
                Tok::Star,
                Tok::Ident("list".into()),
            ]
        );
    }

    #[test]
    fn test_question_dot_ident_lparen() {
        let toks = tokens("?.cons(1)");
        assert_eq!(
            toks,
            vec![
                Tok::ProcAndLParen(Some("?".into()), "cons".into()),
                Tok::Integer(BigInt::from(1)),
                Tok::RParen,
            ]
        );
    }

    /// Ensure `::` between non-ident tokens is NOT merged.
    #[test]
    fn test_double_colon_not_merged_for_non_idents() {
        // `null :: null` — keywords aren't ident-like, so no merge
        assert_eq!(
            tokens("null :: null"),
            vec![Tok::Null, Tok::Colon, Tok::Colon, Tok::Null]
        );
        // Single colon after ident stays separate
        assert_eq!(
            tokens("x : int"),
            vec![Tok::Ident("x".into()), Tok::Colon, Tok::IntKw]
        );
    }

    /// The store instruction pattern from real SIL files.
    #[test]
    fn test_store_instruction_tokens() {
        let toks = tokens("store &l <- null: *list");
        assert_eq!(
            toks,
            vec![
                Tok::Store,
                Tok::Ampersand,
                Tok::Ident("l".into()),
                Tok::Assign,
                Tok::Null,
                Tok::Colon,
                Tok::Star,
                Tok::Ident("list".into()),
            ]
        );
    }

    /// Load instruction with type annotation.
    #[test]
    fn test_load_instruction_tokens() {
        let toks = tokens("n0 : *list = load &l");
        assert_eq!(
            toks,
            vec![
                Tok::Local(0),
                Tok::Colon,
                Tok::Star,
                Tok::Ident("list".into()),
                Tok::Eq,
                Tok::Load,
                Tok::Ampersand,
                Tok::Ident("l".into()),
            ]
        );
    }

    /// Chained double-colon identifiers with class.method() call.
    #[test]
    fn test_double_colon_with_class_method() {
        let toks = tokens("A::B.method()");
        assert_eq!(
            toks,
            vec![
                Tok::ProcAndLParen(Some("A::B".into()), "method".into()),
                Tok::RParen,
            ]
        );
    }

    /// Location annotations are skipped directly by the lexer.
    /// Matches OCaml's TextualLexer.ml which skips @[line:col], @line, @?.
    #[test]
    fn test_location_annotations_skipped() {
        // @[line:col] — skipped
        assert_eq!(tokens("ret n0 @[9:1]"), vec![Tok::Ret, Tok::Local(0)]);
        // @line — skipped
        assert_eq!(tokens("ret n0 @42"), vec![Tok::Ret, Tok::Local(0)]);
        // @? — skipped
        assert_eq!(tokens("ret n0 @?"), vec![Tok::Ret, Tok::Local(0)]);
        // Mixed with other tokens
        assert_eq!(
            tokens("#node_0: @[9:1]"),
            vec![Tok::Label("node_0".into()), Tok::Colon]
        );
    }

    /// Wildcard `_` gets its own token, not Ident("_").
    #[test]
    fn test_wildcard_token() {
        assert_eq!(tokens("_"), vec![Tok::Wildcard]);
        assert_eq!(
            tokens("_ = foo()"),
            vec![
                Tok::Wildcard,
                Tok::Eq,
                Tok::ProcAndLParen(None, "foo".into()),
                Tok::RParen,
            ]
        );
        // `_foo` is still an ident, not wildcard + ident
        assert_eq!(tokens("_foo"), vec![Tok::Ident("_foo".into())]);
        // `__sil_allocate` is still an ident
        assert_eq!(
            tokens("__sil_allocate"),
            vec![Tok::Ident("__sil_allocate".into())]
        );
    }
}
