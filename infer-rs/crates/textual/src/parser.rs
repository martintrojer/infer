// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Recursive-descent parser for Textual IR.
//!
//! Each parsing method references the corresponding rule in `TextualMenhir.mly`.
//! The parser consumes the token stream produced by the logos-based lexer
//! (with compound-token adapter).

use std::collections::BTreeSet;

use crate::ast::*;
use crate::lexer;
use crate::tokens::Tok;

/// Parse error with location info.
#[derive(Clone, Debug)]
pub struct ParseError {
    pub loc: Location,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "parse error at {}: {}", self.loc, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Token stream: a vec of (start, token, end) with a cursor.
struct TokenStream {
    tokens: Vec<(usize, Tok, usize)>,
    pos: usize,
    source: String,
}

/// Sentinel for end-of-input.
static EOF_TOK: Tok = Tok::Eof;

impl TokenStream {
    fn new(tokens: Vec<(usize, Tok, usize)>, source: &str) -> Self {
        Self {
            tokens,
            pos: 0,
            source: source.to_string(),
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn peek(&self) -> &Tok {
        self.tokens
            .get(self.pos)
            .map(|(_, t, _)| t)
            .unwrap_or(&EOF_TOK)
    }

    fn loc(&self) -> Location {
        let offset = self
            .tokens
            .get(self.pos)
            .map(|(s, _, _)| *s)
            .unwrap_or(self.source.len());
        offset_to_loc(&self.source, offset)
    }

    fn advance(&mut self) {
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, expected: &Tok) -> Result<(), ParseError> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(self.err(format!("expected `{expected:?}`, got `{:?}`", self.peek())))
        }
    }

    fn err(&self, message: String) -> ParseError {
        ParseError {
            loc: self.loc(),
            message,
        }
    }
}

/// Convert byte offset to line:col location.
fn offset_to_loc(source: &str, offset: usize) -> Location {
    let mut line = 1;
    let mut col = 0;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    Location::known(line, col)
}

// ===========================================================================
// Parser
// ===========================================================================

struct Parser {
    ts: TokenStream,
}

impl Parser {
    fn new(tokens: Vec<(usize, Tok, usize)>, source: &str) -> Self {
        Self {
            ts: TokenStream::new(tokens, source),
        }
    }

    fn peek(&self) -> &Tok {
        self.ts.peek()
    }

    fn loc(&self) -> Location {
        self.ts.loc()
    }

    fn eat(&mut self, expected: &Tok) -> Result<(), ParseError> {
        self.ts.eat(expected)
    }

    fn err(&self, msg: impl Into<String>) -> ParseError {
        self.ts.err(msg.into())
    }

    // -- Identifiers (Menhir: ident / basic_ident / ident_except_load) ------

    /// Menhir: `ident` — identifiers including keywords usable as names.
    fn ident(&mut self) -> Result<String, ParseError> {
        let s = match self.peek() {
            Tok::Ident(s) => s.clone(),
            Tok::Declare => "declare".into(),
            Tok::Define => "define".into(),
            Tok::Extends => "extends".into(),
            Tok::Equals => "equals".into(),
            Tok::Global => "global".into(),
            Tok::Jmp => "jmp".into(),
            Tok::Load => "load".into(),
            Tok::LocalKw => "local".into(),
            Tok::Prune => "prune".into(),
            Tok::Ret => "ret".into(),
            Tok::Store => "store".into(),
            Tok::Throw => "throw".into(),
            Tok::Type => "type".into(),
            Tok::Unreachable => "unreachable".into(),
            other => return Err(self.err(format!("expected identifier, got `{other:?}`"))),
        };
        self.ts.advance();
        Ok(s)
    }

    /// Name positions such as locals/fields can legitimately use `n0`/`n1`
    /// as source-level identifiers in exported Textual. The lexer tokenizes
    /// those as `Tok::Local(_)`, so declaration/name parsing must reinterpret
    /// them back to their textual spelling rather than rejecting them as SSA
    /// temporaries. Anonymous union fields also use `_`.
    fn name_ident(&mut self) -> Result<String, ParseError> {
        let s = match self.peek() {
            Tok::Ident(s) => s.clone(),
            Tok::Declare => "declare".into(),
            Tok::Define => "define".into(),
            Tok::Extends => "extends".into(),
            Tok::Equals => "equals".into(),
            Tok::Global => "global".into(),
            Tok::Jmp => "jmp".into(),
            Tok::Load => "load".into(),
            Tok::LocalKw => "local".into(),
            Tok::Prune => "prune".into(),
            Tok::Ret => "ret".into(),
            Tok::Store => "store".into(),
            Tok::Throw => "throw".into(),
            Tok::Type => "type".into(),
            Tok::Unreachable => "unreachable".into(),
            Tok::Local(n) => format!("n{n}"),
            Tok::Wildcard => "_".to_string(),
            other => return Err(self.err(format!("expected identifier, got `{other:?}`"))),
        };
        self.ts.advance();
        Ok(s)
    }

    // -- Names (Menhir: fname, nname, vname, tname) -------------------------

    fn fname(&mut self) -> Result<FieldName, ParseError> {
        let loc = self.loc();
        let id = self.name_ident()?;
        Ok(Name::new(id, loc))
    }

    fn nname(&mut self) -> Result<NodeName, ParseError> {
        let loc = self.loc();
        let id = self.ident()?;
        Ok(Name::new(id, loc))
    }

    fn vname(&mut self) -> Result<VarName, ParseError> {
        let loc = self.loc();
        let id = self.name_ident()?;
        Ok(Name::new(id, loc))
    }

    /// Menhir: `tname` — type name with optional generic args `T<A, B>`.
    /// Accepts type keywords (int, float, void) as type names for use in
    /// generic arguments like `Vec<int>`.
    fn tname(&mut self) -> Result<TypeName, ParseError> {
        let loc = self.loc();
        let id = match self.peek() {
            Tok::IntKw => {
                self.ts.advance();
                "int".to_string()
            }
            Tok::FloatKw => {
                self.ts.advance();
                "float".to_string()
            }
            Tok::Void => {
                self.ts.advance();
                "void".to_string()
            }
            _ => self.ident()?,
        };
        let name = Name::new(id, loc);
        if *self.peek() == Tok::LAngle {
            self.ts.advance();
            let args = self.sep_list(|p| p.tname(), &Tok::Comma)?;
            self.eat(&Tok::RAngle)?;
            Ok(TypeName::with_args(name, args))
        } else {
            Ok(TypeName {
                name,
                args: Vec::new(),
            })
        }
    }

    /// Menhir: `tname_or_void`
    fn tname_or_void(&mut self) -> Result<TypeName, ParseError> {
        if *self.peek() == Tok::Void {
            let loc = self.loc();
            self.ts.advance();
            Ok(TypeName::new("void", loc))
        } else {
            self.tname()
        }
    }

    /// Menhir: `opt_tname` — type name or `?`.
    fn opt_tname(&mut self) -> Result<TypeName, ParseError> {
        if *self.peek() == Tok::Question {
            let loc = self.loc();
            self.ts.advance();
            Ok(TypeName::new("?", loc))
        } else {
            self.tname()
        }
    }

    // -- Qualified proc names -----------------------------------------------

    /// Menhir: `qualified_pname_and_lparen` — consumes through the `(`.
    /// Returns the qualified name; the caller reads args and `)`.
    fn qualified_pname_and_lparen(&mut self) -> Result<QualifiedProcName, ParseError> {
        match self.peek().clone() {
            Tok::ProcAndLParen(class, name) => {
                let loc = self.loc();
                self.ts.advance();
                let pname = ProcName::new(name, loc.clone());
                Ok(match class {
                    Some(c) => QualifiedProcName::with_class(TypeName::new(c, loc), pname),
                    None => QualifiedProcName::top_level(pname),
                })
            }
            // Handle `TName<args>.method(` pattern
            Tok::Ident(_) => {
                // Try parsing TNameWithArgs . Ident (
                let tn = self.tname()?;
                if !tn.args.is_empty() && *self.peek() == Tok::Dot {
                    self.ts.advance(); // dot
                    let loc = self.loc();
                    let method = self.ident()?;
                    self.eat(&Tok::LParen)?;
                    Ok(QualifiedProcName::with_class(
                        tn,
                        ProcName::new(method, loc),
                    ))
                } else {
                    Err(self.err(format!("expected procedure name, got type `{tn}`")))
                }
            }
            other => Err(self.err(format!("expected procedure name, got `{other:?}`"))),
        }
    }

    // -- Attributes (Menhir: attribute, annot, annots) ----------------------

    /// Menhir: `attribute` — top-level `.name = "value"`.
    fn attribute(&mut self) -> Result<Attr, ParseError> {
        let loc = self.loc();
        self.eat(&Tok::Dot)?;
        let name = self.ident()?;
        self.eat(&Tok::Eq)?;
        let value = self.string_lit()?;
        Ok(Attr::new(name, vec![value], loc))
    }

    /// Menhir: `annot` — `.name` or `.name = "v1", "v2"`.
    fn annot(&mut self) -> Result<Attr, ParseError> {
        let loc = self.loc();
        self.eat(&Tok::Dot)?;
        let name = self.ident()?;
        let values = if *self.peek() == Tok::Eq {
            self.ts.advance();
            self.sep_list(|p| p.string_lit(), &Tok::Comma)?
        } else {
            Vec::new()
        };
        Ok(Attr::new(name, values, loc))
    }

    /// Menhir: `annots` — zero or more annotations.
    fn annots(&mut self) -> Result<Vec<Attr>, ParseError> {
        let mut attrs = Vec::new();
        while *self.peek() == Tok::Dot {
            attrs.push(self.annot()?);
        }
        Ok(attrs)
    }

    fn string_lit(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Tok::StringLit(s) => {
                self.ts.advance();
                Ok(s)
            }
            other => Err(self.err(format!("expected string literal, got `{other:?}`"))),
        }
    }

    // -- Types (Menhir: base_typ, typ, annotated_typ) -----------------------

    /// Menhir: `base_typ`
    fn base_typ(&mut self) -> Result<Typ, ParseError> {
        match self.peek().clone() {
            Tok::IntKw => {
                self.ts.advance();
                Ok(Typ::Int)
            }
            Tok::FloatKw => {
                self.ts.advance();
                Ok(Typ::Float)
            }
            Tok::Void => {
                self.ts.advance();
                Ok(Typ::Void)
            }
            Tok::FunType => {
                self.ts.advance();
                Ok(Typ::Fun(None))
            }
            Tok::Fun => {
                // `fun(` — params `)` `->` return_type `)`
                self.ts.advance(); // `fun(` already consumed `(`
                let params = self.comma_list(|p| p.typ())?;
                self.eat(&Tok::RParen)?;
                self.eat(&Tok::Arrow)?;
                let ret = self.typ()?;
                self.eat(&Tok::RParen)?;
                Ok(Typ::Fun(Some(FunctionPrototype {
                    params_type: params,
                    return_type: Box::new(ret),
                })))
            }
            Tok::LParen => {
                self.ts.advance();
                // Menhir: `LPAREN FUN params RPAREN ARROW ret RPAREN`
                // The `Fun` compound token already consumed `fun(`, so we
                // handle `( fun( params ) -> ret )` as a single production.
                if *self.peek() == Tok::Fun {
                    self.ts.advance();
                    let params = self.comma_list(|p| p.typ())?;
                    self.eat(&Tok::RParen)?;
                    self.eat(&Tok::Arrow)?;
                    let ret = self.typ()?;
                    self.eat(&Tok::RParen)?;
                    Ok(Typ::Fun(Some(FunctionPrototype {
                        params_type: params,
                        return_type: Box::new(ret),
                    })))
                } else if matches!(self.peek(), Tok::Ident(s) if s == "fun") {
                    // `(fun _ -> _)` — FunType that the adapter didn't merge
                    // (can happen when `fun` is not followed by `(`)
                    self.ts.advance(); // "fun"
                    if matches!(self.peek(), Tok::Wildcard) {
                        self.ts.advance(); // _
                        self.eat(&Tok::Arrow)?;
                        if matches!(self.peek(), Tok::Wildcard) {
                            self.ts.advance(); // _
                            self.eat(&Tok::RParen)?;
                            return Ok(Typ::Fun(None));
                        }
                    }
                    Err(self.err("malformed fun type".to_string()))
                } else {
                    let t = self.typ()?;
                    self.eat(&Tok::RParen)?;
                    Ok(t)
                }
            }
            _ => {
                let tn = self.tname()?;
                Ok(Typ::Struct(tn))
            }
        }
    }

    /// Menhir: `typ` — base_typ with optional `*` prefix and `[]` suffix.
    fn typ(&mut self) -> Result<Typ, ParseError> {
        if *self.peek() == Tok::Star {
            self.ts.advance();
            let inner = self.typ()?;
            Ok(Typ::Ptr(Box::new(inner), Vec::new()))
        } else {
            let mut t = self.base_typ()?;
            while *self.peek() == Tok::LBracket {
                self.ts.advance();
                self.eat(&Tok::RBracket)?;
                t = Typ::Array(Box::new(t));
            }
            Ok(t)
        }
    }

    /// Menhir: `annotated_typ`
    fn annotated_typ(&mut self) -> Result<AnnotatedTyp, ParseError> {
        let attrs = self.annots()?;
        let t = self.typ()?;
        Ok(AnnotatedTyp {
            typ: t,
            attributes: attrs,
        })
    }

    fn typed_var(&mut self) -> Result<(VarName, AnnotatedTyp), ParseError> {
        let name = self.vname()?;
        self.eat(&Tok::Colon)?;
        let at = self.annotated_typ()?;
        Ok((name, at))
    }

    fn typed_field(&mut self) -> Result<(FieldName, AnnotatedTyp), ParseError> {
        let name = self.fname()?;
        self.eat(&Tok::Colon)?;
        let at = self.annotated_typ()?;
        Ok((name, at))
    }

    fn typed_ident(&mut self) -> Result<(Ident, Typ), ParseError> {
        let id = self.local_var()?;
        self.eat(&Tok::Colon)?;
        let t = self.typ()?;
        Ok((id, t))
    }

    fn local_var(&mut self) -> Result<i32, ParseError> {
        match self.peek().clone() {
            Tok::Local(n) => {
                self.ts.advance();
                Ok(n)
            }
            other => Err(self.err(format!("expected local variable, got `{other:?}`"))),
        }
    }

    // -- Constants (Menhir: const) ------------------------------------------

    fn const_val(&mut self) -> Result<Const, ParseError> {
        match self.peek().clone() {
            Tok::Integer(i) => {
                self.ts.advance();
                Ok(Const::Int(i))
            }
            Tok::StringLit(s) => {
                self.ts.advance();
                Ok(Const::Str(s))
            }
            Tok::FloatingPoint(f) => {
                self.ts.advance();
                Ok(Const::Float(f))
            }
            Tok::True => {
                self.ts.advance();
                Ok(Const::Int(num_bigint::BigInt::from(1)))
            }
            Tok::False => {
                self.ts.advance();
                Ok(Const::Int(num_bigint::BigInt::from(0)))
            }
            Tok::Null => {
                self.ts.advance();
                Ok(Const::Null)
            }
            other => Err(self.err(format!("expected constant, got `{other:?}`"))),
        }
    }

    // -- Expressions (Menhir: expression) -----------------------------------

    /// Menhir: `expression` — primary + postfix.
    fn expression(&mut self) -> Result<Exp, ParseError> {
        let primary = self.primary_expression()?;
        self.postfix_expression(primary)
    }

    /// Primary expressions (no left-recursion).
    fn primary_expression(&mut self) -> Result<Exp, ParseError> {
        match self.peek().clone() {
            Tok::Local(id) => {
                self.ts.advance();
                Ok(Exp::Var(id))
            }
            Tok::LBracket => {
                self.ts.advance();
                let exp = self.expression()?;
                let typ = if *self.peek() == Tok::Colon {
                    self.ts.advance();
                    Some(self.typ()?)
                } else {
                    None
                };
                self.eat(&Tok::RBracket)?;
                Ok(Exp::Load {
                    exp: Box::new(exp),
                    typ,
                })
            }
            Tok::Ampersand => {
                self.ts.advance();
                let name = self.vname()?;
                Ok(Exp::Lvar(name))
            }
            Tok::LAngle => {
                self.ts.advance();
                let t = self.typ()?;
                self.eat(&Tok::RAngle)?;
                Ok(Exp::Typ(t))
            }
            Tok::LParen => {
                self.ts.advance();
                if *self.peek() == Tok::If {
                    // `(if cond then e1 else e2)`
                    self.ts.advance();
                    let cond = self.bool_expression()?;
                    self.eat(&Tok::Then)?;
                    let then_ = self.expression()?;
                    self.eat(&Tok::Else)?;
                    let else_ = self.expression()?;
                    self.eat(&Tok::RParen)?;
                    Ok(Exp::If {
                        cond,
                        then_: Box::new(then_),
                        else_: Box::new(else_),
                    })
                } else {
                    Err(self.err(format!(
                        "expected `if` after `(` in expression, got `{:?}`",
                        self.peek()
                    )))
                }
            }
            Tok::ProcAndLParen(class, name) => {
                let loc = self.loc();
                self.ts.advance();
                let pname = ProcName::new(name, loc.clone());
                let proc = match class {
                    Some(c) => QualifiedProcName::with_class(TypeName::new(c, loc), pname),
                    None => QualifiedProcName::top_level(pname),
                };
                let args = self.comma_list(|p| p.expression())?;
                self.eat(&Tok::RParen)?;
                Ok(Exp::Call {
                    proc,
                    args,
                    kind: CallKind::NonVirtual,
                })
            }
            // Constants
            Tok::Integer(_)
            | Tok::StringLit(_)
            | Tok::FloatingPoint(_)
            | Tok::True
            | Tok::False
            | Tok::Null => {
                let c = self.const_val()?;
                Ok(Exp::Const(c))
            }
            // Bare identifier → implicit load of variable
            _ if self.is_ident_like() => {
                let name = self.vname()?;
                Ok(Exp::Load {
                    exp: Box::new(Exp::Lvar(name)),
                    typ: None,
                })
            }
            other => Err(self.err(format!("expected expression, got `{other:?}`"))),
        }
    }

    /// Postfix: `.`, `->`, `[`, `(` after an expression.
    fn postfix_expression(&mut self, mut exp: Exp) -> Result<Exp, ParseError> {
        loop {
            match self.peek().clone() {
                // `exp.ProcAndLParen` → virtual call (lexer merged class.method()
                Tok::Dot if matches!(self.peek_at(1), Some(Tok::ProcAndLParen(..))) => {
                    self.ts.advance(); // dot
                    if let Tok::ProcAndLParen(class, name) = self.peek().clone() {
                        let loc = self.loc();
                        self.ts.advance();
                        let enclosing = match class {
                            Some(c) => TypeName::new(c, loc.clone()),
                            None => TypeName::new("?", loc.clone()),
                        };
                        let proc =
                            QualifiedProcName::with_class(enclosing, ProcName::new(name, loc));
                        let args = self.comma_list(|p| p.expression())?;
                        self.eat(&Tok::RParen)?;
                        exp = Exp::call_virtual(proc, exp, args);
                    }
                }
                // `exp.class.field` or `exp.class.method(args)`
                Tok::Dot => {
                    self.ts.advance();
                    let enclosing = self.opt_tname()?;
                    self.eat(&Tok::Dot)?;

                    match self.peek().clone() {
                        Tok::ProcAndLParen(class, name) => {
                            let loc = self.loc();
                            self.ts.advance();
                            let proc = if let Some(c) = class {
                                QualifiedProcName::with_class(
                                    TypeName::new(c, loc.clone()),
                                    ProcName::new(name, loc),
                                )
                            } else {
                                QualifiedProcName::with_class(enclosing, ProcName::new(name, loc))
                            };
                            let args = self.comma_list(|p| p.expression())?;
                            self.eat(&Tok::RParen)?;
                            exp = Exp::call_virtual(proc, exp, args);
                        }
                        _ => {
                            let name = self.fname()?;
                            let field = QualifiedFieldName {
                                enclosing_class: enclosing,
                                name,
                            };
                            exp = Exp::Field {
                                exp: Box::new(exp),
                                field,
                            };
                        }
                    }
                }
                // `exp->class.field` = Load { Field { exp, field } }
                // Matches OCaml: load through a field access.
                Tok::Arrow => {
                    self.ts.advance();
                    let enclosing = self.opt_tname()?;
                    self.eat(&Tok::Dot)?;
                    let name = self.fname()?;
                    let field = QualifiedFieldName {
                        enclosing_class: enclosing,
                        name,
                    };
                    exp = Exp::Load {
                        exp: Box::new(Exp::Field {
                            exp: Box::new(exp),
                            field,
                        }),
                        typ: None,
                    };
                }
                // `exp[exp2]`
                Tok::LBracket => {
                    self.ts.advance();
                    let idx = self.expression()?;
                    self.eat(&Tok::RBracket)?;
                    exp = Exp::Index(Box::new(exp), Box::new(idx));
                }
                // `exp(args)` — closure application
                Tok::LParen => {
                    self.ts.advance();
                    let args = self.comma_list(|p| p.expression())?;
                    self.eat(&Tok::RParen)?;
                    exp = Exp::Apply {
                        closure: Box::new(exp),
                        args,
                    };
                }
                _ => break,
            }
        }
        Ok(exp)
    }

    /// Menhir: `bool_expression`
    fn bool_expression(&mut self) -> Result<BoolExp, ParseError> {
        let mut bexp = BoolExp::Exp(Box::new(self.expression()?));
        loop {
            match self.peek() {
                Tok::And => {
                    self.ts.advance();
                    let right = BoolExp::Exp(Box::new(self.expression()?));
                    bexp = BoolExp::And(Box::new(bexp), Box::new(right));
                }
                Tok::Or => {
                    self.ts.advance();
                    let right = BoolExp::Exp(Box::new(self.expression()?));
                    bexp = BoolExp::Or(Box::new(bexp), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(bexp)
    }

    // -- Instructions (Menhir: instruction) ---------------------------------

    fn instruction(&mut self) -> Result<Instr, ParseError> {
        let loc = self.loc();
        match self.peek().clone() {
            Tok::Local(id) => {
                self.ts.advance();
                match self.peek() {
                    Tok::Colon => {
                        // `nN : typ = load exp`
                        self.ts.advance();
                        let t = self.typ()?;
                        self.eat(&Tok::Eq)?;
                        self.eat(&Tok::Load)?;
                        let exp = self.expression()?;
                        Ok(Instr::Load {
                            id,
                            exp,
                            typ: Some(t),
                            loc,
                        })
                    }
                    Tok::Eq => {
                        self.ts.advance();
                        if *self.peek() == Tok::Load {
                            // `nN = load exp`
                            self.ts.advance();
                            let exp = self.expression()?;
                            Ok(Instr::Load {
                                id,
                                exp,
                                typ: None,
                                loc,
                            })
                        } else {
                            // `nN = exp`
                            let exp = self.expression()?;
                            Ok(Instr::Let {
                                id: Some(id),
                                exp,
                                loc,
                            })
                        }
                    }
                    other => {
                        Err(self.err(format!("expected `:` or `=` after local, got `{other:?}`")))
                    }
                }
            }
            Tok::Store => {
                self.ts.advance();
                let e1 = self.expression()?;
                self.eat(&Tok::Assign)?;
                let e2 = self.expression()?;
                let typ = if *self.peek() == Tok::Colon {
                    self.ts.advance();
                    Some(self.typ()?)
                } else {
                    None
                };
                Ok(Instr::Store {
                    exp1: e1,
                    typ,
                    exp2: e2,
                    loc,
                })
            }
            Tok::Prune => {
                self.ts.advance();
                if *self.peek() == Tok::Not {
                    self.ts.advance();
                    let exp = self.expression()?;
                    Ok(Instr::Prune {
                        exp: Exp::logical_not(exp),
                        loc,
                    })
                } else {
                    let exp = self.expression()?;
                    Ok(Instr::Prune { exp, loc })
                }
            }
            // `_ = exp` (wildcard let-binding, printed by TextualOfSil for Let { id=None })
            Tok::Wildcard => {
                self.ts.advance();
                self.eat(&Tok::Eq)?;
                let exp = self.expression()?;
                Ok(Instr::Let { id: None, exp, loc })
            }
            other => Err(self.err(format!("expected instruction, got `{other:?}`"))),
        }
    }

    fn is_instruction_start(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Local(_) | Tok::Store | Tok::Prune | Tok::Wildcard
        )
    }

    // -- Terminators (Menhir: terminator) -----------------------------------

    fn terminator(&mut self) -> Result<Terminator, ParseError> {
        match self.peek().clone() {
            Tok::If => {
                self.ts.advance();
                let bexp = self.bool_expression()?;
                self.eat(&Tok::Then)?;
                let then_ = self.terminator()?;
                self.eat(&Tok::Else)?;
                let else_ = self.terminator()?;
                Ok(Terminator::If {
                    bexp,
                    then_: Box::new(then_),
                    else_: Box::new(else_),
                })
            }
            Tok::IfAndLParen => {
                self.ts.advance();
                let bexp = self.bool_expression()?;
                self.eat(&Tok::RParen)?;
                self.eat(&Tok::Then)?;
                let then_ = self.terminator()?;
                self.eat(&Tok::Else)?;
                let else_ = self.terminator()?;
                Ok(Terminator::If {
                    bexp,
                    then_: Box::new(then_),
                    else_: Box::new(else_),
                })
            }
            Tok::Ret => {
                self.ts.advance();
                let exp = self.expression()?;
                Ok(Terminator::Ret(exp))
            }
            Tok::Jmp => {
                self.ts.advance();
                let calls = self.node_call_list()?;
                Ok(Terminator::Jump(calls))
            }
            Tok::Unreachable => {
                self.ts.advance();
                Ok(Terminator::Unreachable)
            }
            Tok::Throw => {
                self.ts.advance();
                let exp = self.expression()?;
                Ok(Terminator::Throw(exp))
            }
            other => Err(self.err(format!("expected terminator, got `{other:?}`"))),
        }
    }

    fn node_call_list(&mut self) -> Result<Vec<NodeCall>, ParseError> {
        let mut calls = Vec::new();
        while self.is_node_call_start() {
            calls.push(self.node_call()?);
            if *self.peek() == Tok::Comma {
                self.ts.advance();
            } else {
                break;
            }
        }
        Ok(calls)
    }

    fn node_call(&mut self) -> Result<NodeCall, ParseError> {
        match self.peek().clone() {
            Tok::ProcAndLParen(_, name) => {
                let loc = self.loc();
                self.ts.advance();
                let ssa_args = self.comma_list(|p| p.expression())?;
                self.eat(&Tok::RParen)?;
                Ok(NodeCall {
                    label: NodeName::new(name, loc),
                    ssa_args,
                })
            }
            _ => {
                let label = self.nname()?;
                Ok(NodeCall {
                    label,
                    ssa_args: Vec::new(),
                })
            }
        }
    }

    fn is_node_call_start(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Ident(_)
                | Tok::ProcAndLParen(..)
                | Tok::Declare
                | Tok::Define
                | Tok::Extends
                | Tok::Equals
                | Tok::Global
                | Tok::Jmp
                | Tok::Load
                | Tok::LocalKw
                | Tok::Prune
                | Tok::Ret
                | Tok::Store
                | Tok::Throw
                | Tok::Type
                | Tok::Unreachable
        )
    }

    // -- Blocks (Menhir: block, label) --------------------------------------

    fn block(&mut self) -> Result<Node, ParseError> {
        let label_loc = self.loc();
        let (label, ssa_parameters) = self.block_label()?;

        let mut instrs = Vec::new();
        while self.is_instruction_start() {
            instrs.push(self.instruction()?);
        }

        let last_loc = self.loc();
        let last = self.terminator()?;

        let exn_succs = if *self.peek() == Tok::Handlers {
            self.ts.advance();
            self.sep_list(|p| p.nname(), &Tok::Comma)?
                .into_iter()
                .collect()
        } else {
            BTreeSet::new()
        };

        Ok(Node {
            label,
            ssa_parameters,
            exn_succs,
            last,
            instrs,
            last_loc,
            label_loc,
        })
    }

    fn block_label(&mut self) -> Result<(NodeName, Vec<(Ident, Typ)>), ParseError> {
        let label_str = match self.peek().clone() {
            Tok::Label(s) => s,
            other => return Err(self.err(format!("expected label, got `{other:?}`"))),
        };
        let loc = self.loc();
        self.ts.advance();

        let params = if *self.peek() == Tok::LParen {
            self.ts.advance();
            let params = self.sep_list(|p| p.typed_ident(), &Tok::Comma)?;
            self.eat(&Tok::RParen)?;
            params
        } else {
            Vec::new()
        };

        self.eat(&Tok::Colon)?;
        Ok((NodeName::new(label_str, loc), params))
    }

    // -- Declarations (Menhir: declaration) ---------------------------------

    fn declaration(&mut self) -> Result<Decl, ParseError> {
        match self.peek() {
            Tok::Global => self.decl_global(),
            Tok::Type => self.decl_type(),
            Tok::Declare => self.decl_declare(),
            Tok::Define => self.decl_define(),
            other => Err(self.err(format!("expected declaration, got `{other:?}`"))),
        }
    }

    fn decl_global(&mut self) -> Result<Decl, ParseError> {
        self.eat(&Tok::Global)?;
        let name = self.vname()?;
        self.eat(&Tok::Colon)?;
        let at = self.annotated_typ()?;
        Ok(Decl::Global(Global {
            name,
            typ: at.typ,
            attributes: at.attributes,
        }))
    }

    fn decl_type(&mut self) -> Result<Decl, ParseError> {
        self.eat(&Tok::Type)?;
        let tn = self.tname()?;

        // Check for `equals` (typedef alias): `type T equals T1, T2 .attrs`
        if *self.peek() == Tok::Equals {
            self.ts.advance();
            let defs = self.sep_list(|p| p.tname_or_void(), &Tok::Comma)?;
            let attrs = self.annots()?;
            return Ok(Decl::Struct(Struct {
                name: tn,
                supers: defs.into_iter().collect(),
                fields: Vec::new(),
                attributes: attrs,
            }));
        }

        // extends clause
        let supers = if *self.peek() == Tok::Extends {
            self.ts.advance();
            self.sep_list(|p| p.tname(), &Tok::Comma)?
                .into_iter()
                .collect()
        } else {
            BTreeSet::new()
        };

        // Optional `=`
        if *self.peek() == Tok::Eq {
            self.ts.advance();
        }

        let attrs = self.annots()?;

        // Fields: `{ field: typ; ... }`
        self.eat(&Tok::LBrace)?;
        let mut fields = Vec::new();
        while *self.peek() != Tok::RBrace {
            let (name, at) = self.typed_field()?;
            fields.push(FieldDecl {
                qualified_name: QualifiedFieldName {
                    enclosing_class: tn.clone(),
                    name,
                },
                typ: at.typ,
                attributes: at.attributes,
            });
            if *self.peek() == Tok::Semicolon {
                self.ts.advance();
            }
        }
        self.eat(&Tok::RBrace)?;

        Ok(Decl::Struct(Struct {
            name: tn,
            supers,
            fields,
            attributes: attrs,
        }))
    }

    fn decl_declare(&mut self) -> Result<Decl, ParseError> {
        self.eat(&Tok::Declare)?;
        let attrs = self.annots()?;
        let qn = self.qualified_pname_and_lparen()?;

        let formals = if *self.peek() == Tok::Ellipsis {
            self.ts.advance();
            None
        } else {
            Some(self.comma_list(|p| p.annotated_typ())?)
        };
        self.eat(&Tok::RParen)?;
        self.eat(&Tok::Colon)?;
        let rt = self.annotated_typ()?;

        Ok(Decl::Procdecl(ProcDecl {
            qualified_name: qn,
            formals_types: formals,
            result_type: rt,
            attributes: attrs,
        }))
    }

    fn decl_define(&mut self) -> Result<Decl, ParseError> {
        self.eat(&Tok::Define)?;
        let attrs = self.annots()?;
        let qn = self.qualified_pname_and_lparen()?;

        let params = self.comma_list(|p| p.typed_var())?;
        self.eat(&Tok::RParen)?;
        self.eat(&Tok::Colon)?;
        let rt = self.annotated_typ()?;

        // Body
        self.eat(&Tok::LBrace)?;

        let locals = if *self.peek() == Tok::LocalKw {
            self.ts.advance();
            self.sep_list(|p| p.typed_var(), &Tok::Comma)?
        } else {
            Vec::new()
        };

        let mut nodes = Vec::new();
        while *self.peek() != Tok::RBrace {
            nodes.push(self.block()?);
        }
        let exit_loc = self.loc();
        self.eat(&Tok::RBrace)?;

        let formals_types: Vec<AnnotatedTyp> = params.iter().map(|(_, t)| t.clone()).collect();
        let param_names: Vec<VarName> = params.into_iter().map(|(n, _)| n).collect();
        let start = nodes
            .first()
            .map(|n| n.label.clone())
            .unwrap_or_else(|| NodeName::new("entry", exit_loc.clone()));

        Ok(Decl::Proc(ProcDesc {
            procdecl: ProcDecl {
                qualified_name: qn,
                formals_types: Some(formals_types),
                result_type: rt,
                attributes: attrs,
            },
            nodes,
            start,
            params: param_names,
            locals,
            exit_loc,
        }))
    }

    // -- Module (Menhir: main) ----------------------------------------------

    fn module(&mut self, source_file: &str) -> Result<Module, ParseError> {
        let mut attrs = Vec::new();
        while *self.peek() == Tok::Dot {
            attrs.push(self.attribute()?);
        }

        let mut decls = Vec::new();
        while !self.ts.at_end() {
            decls.push(self.declaration()?);
        }

        Ok(Module {
            attrs,
            decls,
            source_file: source_file.to_string(),
        })
    }

    // -- Utilities ----------------------------------------------------------

    fn is_ident_like(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Ident(_)
                | Tok::Declare
                | Tok::Define
                | Tok::Extends
                | Tok::Equals
                | Tok::Global
                | Tok::Jmp
                | Tok::Load
                | Tok::LocalKw
                | Tok::Prune
                | Tok::Ret
                | Tok::Store
                | Tok::Throw
                | Tok::Type
                | Tok::Unreachable
        )
    }

    /// Peek at the token N positions ahead (0 = current).
    fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.ts.tokens.get(self.ts.pos + n).map(|(_, t, _)| t)
    }

    /// Parse a comma-separated list (possibly empty, no trailing comma).
    fn comma_list<T>(
        &mut self,
        parse_elem: impl Fn(&mut Self) -> Result<T, ParseError>,
    ) -> Result<Vec<T>, ParseError> {
        let mut items = Vec::new();
        // Check for empty list by looking at common "end" tokens
        if matches!(self.peek(), Tok::RParen | Tok::RBracket | Tok::RBrace) {
            return Ok(items);
        }
        items.push(parse_elem(self)?);
        while *self.peek() == Tok::Comma {
            self.ts.advance();
            items.push(parse_elem(self)?);
        }
        Ok(items)
    }

    /// Parse a separator-separated list (at least one element).
    fn sep_list<T>(
        &mut self,
        parse_elem: impl Fn(&mut Self) -> Result<T, ParseError>,
        sep: &Tok,
    ) -> Result<Vec<T>, ParseError> {
        let mut items = vec![parse_elem(self)?];
        while self.peek() == sep {
            self.ts.advance();
            items.push(parse_elem(self)?);
        }
        Ok(items)
    }
}

// ===========================================================================
// Public API
// ===========================================================================

/// Parse a Textual module from source text.
pub fn parse_module(source: &str, source_file: &str) -> Result<Module, ParseError> {
    let tokens = lexer::lex(source).map_err(|e| ParseError {
        loc: offset_to_loc(source, e.offset),
        message: e.message,
    })?;
    let mut parser = Parser::new(tokens, source);
    parser.module(source_file)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty() {
        let m = parse_module("", "test.sil").unwrap();
        assert!(m.attrs.is_empty());
        assert!(m.decls.is_empty());
    }

    #[test]
    fn test_parse_source_language() {
        let m = parse_module(r#".source_language = "java""#, "test.sil").unwrap();
        assert_eq!(m.lang(), Some("java"));
    }

    #[test]
    fn test_parse_global() {
        let m = parse_module(".source_language = \"java\"\nglobal I : int", "t.sil").unwrap();
        assert!(matches!(&m.decls[0], Decl::Global(g) if g.name.value == "I"));
    }

    #[test]
    fn test_parse_struct() {
        let m = parse_module(
            ".source_language = \"java\"\ntype node = { val: int; next: *node }",
            "t.sil",
        )
        .unwrap();
        assert!(matches!(&m.decls[0], Decl::Struct(s) if s.fields.len() == 2));
    }

    #[test]
    fn test_parse_declare() {
        let m = parse_module(
            ".source_language = \"java\"\ndeclare cons(int, *node) : node",
            "t.sil",
        )
        .unwrap();
        assert!(matches!(&m.decls[0], Decl::Procdecl(p) if p.qualified_name.name.value == "cons"));
    }

    #[test]
    fn test_parse_simple_function() {
        let src = ".source_language = \"java\"\n\ndefine f(x: int) : int {\n  #entry:\n    n0 : int = load &x\n    ret n0\n}";
        let m = parse_module(src, "t.sil").unwrap();
        match &m.decls[0] {
            Decl::Proc(p) => {
                assert_eq!(p.procdecl.qualified_name.name.value, "f");
                assert_eq!(p.params.len(), 1);
                assert_eq!(p.nodes.len(), 1);
                assert_eq!(p.nodes[0].instrs.len(), 1);
            }
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_local_like_param_and_field_names() {
        let src = r#".source_language = "c"

type T = { n0: int }

define f(n1: int) : int {
  #entry:
    n0 : int = load &n1
    ret n0
}"#;
        let m = parse_module(src, "t.sil").unwrap();

        match &m.decls[0] {
            Decl::Struct(s) => {
                assert_eq!(s.fields.len(), 1);
                assert_eq!(s.fields[0].qualified_name.name.value, "n0");
            }
            other => panic!("expected Struct, got {other:?}"),
        }

        match &m.decls[1] {
            Decl::Proc(p) => {
                assert_eq!(p.params.len(), 1);
                assert_eq!(p.params[0].value, "n1");
                assert_eq!(p.nodes[0].instrs.len(), 1);
            }
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_wildcard_field_name() {
        let src = r#".source_language = "c"

type T = { _: int }"#;
        let m = parse_module(src, "t.sil").unwrap();
        match &m.decls[0] {
            Decl::Struct(s) => {
                assert_eq!(s.fields.len(), 1);
                assert_eq!(s.fields[0].qualified_name.name.value, "_");
            }
            other => panic!("expected Struct, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_control_flow() {
        let src = ".source_language = \"java\"\ndefine f(x: int) : void {\n  #entry:\n    n0: int = load &x\n    jmp lab1, lab2\n  #lab1:\n    ret null\n  #lab2:\n    ret null\n}";
        let m = parse_module(src, "t.sil").unwrap();
        match &m.decls[0] {
            Decl::Proc(p) => {
                assert_eq!(p.nodes.len(), 3);
                assert!(matches!(&p.nodes[0].last, Terminator::Jump(c) if c.len() == 2));
            }
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_real_sil_files() {
        let sil_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../infer/tests/codetoanalyze/sil");
        if !sil_dir.exists() {
            eprintln!("Skipping: sil test directory not found");
            return;
        }
        let mut total = 0;
        let mut passed = 0;
        let mut failed = Vec::new();
        fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        walk(&p, files);
                    } else {
                        files.push(p);
                    }
                }
            }
        }
        let mut files = Vec::new();
        walk(&sil_dir, &mut files);
        for f in &files {
            if f.extension().is_none_or(|e| e != "sil") {
                continue;
            }
            let name = f.file_name().unwrap().to_str().unwrap_or("");
            if name.starts_with("syntax_error") || name.starts_with("error") {
                continue;
            }
            total += 1;
            let content = std::fs::read_to_string(f).unwrap();
            match parse_module(&content, name) {
                Ok(_) => passed += 1,
                Err(e) => failed.push((name.to_string(), format!("{e}"))),
            }
        }
        // Known failures: files that use Textual features not yet supported
        // by the Rust parser. Update this set as features are implemented.
        let known_failing: std::collections::HashSet<&str> = [
            "twice.sil",            // duplicate struct/proc declarations
            "type_token_clash.sil", // type keyword clash
        ]
        .into();

        let unexpected: Vec<_> = failed
            .iter()
            .filter(|(name, _)| !known_failing.contains(name.as_str()))
            .collect();

        eprintln!("\n=== SIL file parse results: {passed}/{total} passed ===");
        for (name, err) in &failed {
            let marker = if known_failing.contains(name.as_str()) {
                "KNOWN"
            } else {
                "UNEXPECTED"
            };
            eprintln!("  {marker} FAIL {name}: {err}");
        }
        assert!(
            unexpected.is_empty(),
            "unexpected parse failures: {:?}",
            unexpected,
        );
    }
}
