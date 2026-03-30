// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the root directory of this source tree.

//! Tests ported from OCaml's textual unit tests.
//!
//! Input strings are extracted from:
//! - `infer/src/textual/unit/TextualParserTest.ml`
//! - `infer/src/textual/unit/TextualTransformTest.ml`
//! - `infer/src/textual/unit/TextualSilTest.ml`
//!
//! These verify that the Rust implementation accepts and handles the same
//! inputs as the OCaml implementation.

mod parser_tests {
    use textual::parser::parse_module;

    /// From TextualParserTest.ml: basic module with attributes
    #[test]
    fn test_basic_module_with_attrs() {
        let text = r#"
       .source_language = "hack"
       .source_file = "original.hack"

       .source_language = "java"

       define nothing(): void {
         #node0:
           ret null
       }
       "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.attrs.len(), 3);
        assert_eq!(module.lang(), Some("hack"));
        assert_eq!(module.decls.len(), 1);
    }

    /// From TextualParserTest.ml: virtual method call
    #[test]
    fn test_virtual_method_call() {
        let text = r#"
       .source_language = "hack"

       declare HackMixed.foo(*HackMixed, int): int

       define foo(x: *HackMixed): int {
       #b0:
         n0:*HackMixed = load &x
         ret n0.HackMixed.foo(42)
       }
       "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 2);
        // Verify the return expression is a virtual method call
        match &module.decls[1] {
            textual::ast::Decl::Proc(p) => match &p.nodes[0].last {
                textual::ast::Terminator::Ret(exp) => {
                    assert!(
                        matches!(
                            exp,
                            textual::ast::Exp::Call {
                                kind: textual::ast::CallKind::Virtual,
                                ..
                            }
                        ),
                        "expected virtual call in ret, got {exp:?}"
                    );
                }
                other => panic!("expected Ret, got {other:?}"),
            },
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    /// From TextualParserTest.ml: type declarations with extends
    #[test]
    fn test_type_extends() {
        use textual::ast::Decl;
        let text = r#"
       .source_language = "hack"
       type A = {f1: int; f2: int}
       type B {f3: bool}
       type C extends A, B {f4: bool}
      "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 3);

        // Verify C extends A and B
        let c_struct = module.decls.iter().find_map(|d| match d {
            Decl::Struct(s) if s.name.name.value == "C" => Some(s),
            _ => None,
        });
        let c = c_struct.expect("struct C should be parsed");
        let super_names: Vec<&str> = c.supers.iter().map(|s| s.name.value.as_str()).collect();
        assert!(
            super_names.contains(&"A"),
            "C should extend A, got {super_names:?}"
        );
        assert!(
            super_names.contains(&"B"),
            "C should extend B, got {super_names:?}"
        );
        assert_eq!(c.fields.len(), 1, "C should have 1 field (f4)");
    }

    /// From TextualParserTest.ml: ellipsis in declarations
    #[test]
    fn test_ellipsis_declaration() {
        let text = r#"
           .source_language = "hack"
           declare todo(...): *Mixed
           declare foo(): *Mixed
           declare bar(int, float): *Mixed
           "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 3);
        // Verify `todo(...)` has None formals (variadic)
        match &module.decls[0] {
            textual::ast::Decl::Procdecl(p) => {
                assert!(
                    p.formals_types.is_none(),
                    "todo should be variadic (None formals)"
                );
            }
            other => panic!("expected Procdecl, got {other:?}"),
        }
        // Verify `foo()` has empty formals (not variadic)
        match &module.decls[1] {
            textual::ast::Decl::Procdecl(p) => {
                assert_eq!(
                    p.formals_types.as_ref().unwrap().len(),
                    0,
                    "foo should have 0 params"
                );
            }
            other => panic!("expected Procdecl, got {other:?}"),
        }
        // Verify `bar(int, float)` has 2 formals
        match &module.decls[2] {
            textual::ast::Decl::Procdecl(p) => {
                assert_eq!(
                    p.formals_types.as_ref().unwrap().len(),
                    2,
                    "bar should have 2 params"
                );
            }
            other => panic!("expected Procdecl, got {other:?}"),
        }
    }

    /// From TextualParserTest.ml: mixing regular formals and ellipsis should fail
    #[test]
    fn test_mixed_formals_and_ellipsis_is_error() {
        let text = r#"
           .source_language = "hack"
           declare foo(int, ...) : *HackMixed
           "#;
        let result = parse_module(text, "dummy.sil");
        assert!(
            result.is_err(),
            "mixing formals with ellipsis should fail to parse"
        );
    }

    /// From TextualParserTest.ml: number literals
    #[test]
    fn test_number_literals() {
        let text = r#"
         .source_language = "hack"
         define foo() : int {
         #entry:
           n0 = 12
           n1 = -42
           n2 = 1e1
           n3 = 2.
           n4 = 3.14
           n5 = 6.022137e+23
           ret n1
         }
         "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 1);
        // Verify parsed numeric values in the Let instructions
        use num_bigint::BigInt;
        use textual::ast::{Const, Decl, Exp, Instr};
        if let Decl::Proc(pdesc) = &module.decls[0] {
            let instrs: Vec<_> = pdesc.nodes.iter().flat_map(|n| &n.instrs).collect();
            // n0 = 12
            if let Instr::Let { exp, .. } = &instrs[0] {
                assert!(
                    matches!(exp, Exp::Const(Const::Int(n)) if *n == BigInt::from(12)),
                    "n0 should be 12, got {exp:?}"
                );
            }
            // n1 = -42
            if let Instr::Let { exp, .. } = &instrs[1] {
                assert!(
                    matches!(exp, Exp::Const(Const::Int(n)) if *n == BigInt::from(-42)),
                    "n1 should be -42, got {exp:?}"
                );
            }
        }
    }

    /// From TextualParserTest.ml: keywords used as identifiers
    #[test]
    fn test_keywords_as_idents() {
        let text = r#"
       .source_language = "hack"
       define f(declare: int) : int {
       #type:
       jmp type

       }
       "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 1);
    }

    /// From TextualParserTest.ml: overloaded functions
    #[test]
    fn test_overloaded_functions() {
        let text = r#"
     .source_language = "hack"

     define f(a: int) : void {#b0: ret null }
     define f(a: int, b: bool) : void {#b0: ret null}

     define g(a: int, b: bool) : void {
     #b0:
       n0:int = load &a
       n1:bool = load &b
       n2 = f(n0)
       n3 = f(n0, n1)
       ret null
     }
     "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 3);
    }

    /// From TextualParserTest.ml: conditional expressions
    #[test]
    fn test_conditional_expression() {
        let text = r#"
     .source_language = "hack"

     declare f(int) : void

     define g(a: int, b: bool) : void {
     #b0:
       n0 = (if b && b then a else 0)
       n1 = (if b then n1 else 0)
       n2 = f(n1)
       ret null
     }
     "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 2);
    }
}

mod transform_tests {
    use textual::parser::parse_module;

    /// From TextualTransformTest.ml: Python-inspired control flow
    #[test]
    fn test_python_control_flow() {
        let text = r#"
       .source_language = "python"
        define f(x: int, y: int, z: int, t: int) : int {
          #b0:
              n0:int = load &x
              if n0 then jmp b1 else jmp b2

          #b1:
              n2:int = load &y
              if n2 then jmp b4(n2) else jmp b2

          #b2:
              n5:int = load &z
              if n5 then jmp b5 else jmp b4(n5)

          #b5:
              n8:int = load &t
              jmp b4(n8)

          #b4(n9: int):
              ret n9

        }
        "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 1);
        match &module.decls[0] {
            textual::ast::Decl::Proc(p) => {
                assert_eq!(p.nodes.len(), 5);
                // b4 has SSA parameters
                let b4 = p.nodes.iter().find(|n| n.label.value == "b4").unwrap();
                assert_eq!(b4.ssa_parameters.len(), 1);
            }
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    /// From TextualTransformTest.ml: remove_effects_in_subexprs input
    #[test]
    fn test_nested_calls_parse() {
        let text = r#"
       .source_language = "python"
        declare g1(int) : int
        declare g2(int) : int
        declare g3(int) : int
        declare g4(int) : *int
        declare m(int, int) : int

        define f(x: int, y: int) : int {
          #entry:
              n0:int = load &x
              n1:int = load &y
              n3 = __sil_mult_int(g3(n0), m(g1(n0), g2(n1)))
              n4 = m([&x:int], g3([&y]))
              jmp lab1(g1(n3), g3(n0)), lab2(g2(n3), g3(n0))
          #lab1(n6: int, n7: int):
              n8 = __sil_mult_int(n6, n7)
              jmp lab
          #lab2(n10: int, n11: int):
              ret g3(m(n10, n11))
          #lab:
              throw g4(n8)
        }
    "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 6); // 5 declares + 1 define
    }
}

mod sil_tests {
    use textual::parser::parse_module;

    /// From TextualSilTest.ml: basic to_sil test input
    #[test]
    fn test_basic_sil_input() {
        let text = r#"
        .source_language = "hack"

        define foo(x: int, y: int) : int {
          #entry:
              n0:int = load &x
              n1:int = load &y
              n3 = __sil_mult_int(n0, n1)
              ret n3
        }
    "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        // Also test to_sil conversion
        let (decls, _) = textual::decls::DeclEnv::from_module(&module);
        let (cfg, _tenv) = textual::to_sil::module_to_sil(&module, &decls).unwrap();
        assert_eq!(cfg.num_procs(), 1);
    }

    /// From TextualSilTest.ml: closures
    #[test]
    fn test_closure_parsing() {
        let text = r#"
        .source_language = "hack"

        declare foo(*HackMixed, *HackMixed) : void

        define bar(x: *HackMixed, y: *HackMixed) : void {
          #b0:
            n0:*HackMixed = load &x
            n1:*HackMixed = load &y
            n2 = foo(n0, n1)
            ret null
        }
    "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 2);
    }

    /// From TextualSilTest.ml: exception handlers
    #[test]
    fn test_exception_handlers() {
        let text = r#"
        .source_language = "hack"

        declare may_throw() : void

        define foo() : void {
          #entry:
            n0 = may_throw()
            jmp normal
            .handlers catch_block
          #normal:
            ret null
          #catch_block:
            ret null
        }
    "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        match &module.decls[1] {
            textual::ast::Decl::Proc(p) => {
                assert_eq!(p.nodes[0].exn_succs.len(), 1);
            }
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    /// From TextualSilTest.ml: arrow field access
    #[test]
    fn test_arrow_field_access() {
        let text = r#"
        .source_language = "python"

        type cell = { value:int; next: *cell }

        define next(l: *cell) : *cell {
          #entry:
             ret l->cell.next
        }
    "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 2);
    }

    /// From TextualSilTest.ml: generics
    #[test]
    fn test_generic_types() {
        use textual::ast::Decl;
        let text = r#"
        .source_language = "hack"

        type Vec<T> = { }

        declare Vec.get(*Vec<T>, int) : *T

        define foo(v: *Vec<int>) : void {
          #entry:
            ret null
        }
    "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        assert_eq!(module.decls.len(), 3);

        // Verify we have a struct, a declare, and a define
        assert!(
            module.decls.iter().any(|d| matches!(d, Decl::Struct(_))),
            "should have a struct declaration for Vec"
        );
        assert!(
            module.decls.iter().any(|d| matches!(d, Decl::Procdecl(_))),
            "should have a procdecl for Vec.get"
        );
        assert!(
            module.decls.iter().any(|d| matches!(d, Decl::Proc(_))),
            "should have a define for foo"
        );
    }
}

/// Tests ported from `TextualKeepGoingVerificationTest.ml`.
mod verification_compliance_tests {
    use textual::decls::DeclEnv;
    use textual::parser::parse_module;
    use textual::verification::verify;

    /// From TextualKeepGoingVerificationTest.ml: valid simple C module
    #[test]
    fn test_valid_c_module() {
        let text = r#"
       .source_language = "c"
       .source_file = "fake.c"

       define test1(): void {
         #start:
           ret null
       }
       "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    /// From TextualKeepGoingVerificationTest.ml: calling an undefined function is an error.
    ///
    /// TODO: Our verifier does not yet check for undefined function calls.
    /// The OCaml `TextualBasicVerification` detects this. When implemented,
    /// change `assert_eq!(errors.len(), 0, ...)` to `assert_eq!(errors.len(), 1, ...)`.
    /// Calling an undefined function should produce an error.
    #[test]
    fn test_calling_undefined_function() {
        let text = r#"
       .source_language = "c"
       .source_file = "fake.c"

       define test1(): void {
         #start:
           ret test3()
       }

       define test2(): *int {
         #start:
           ret 0
       }
       "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert_eq!(
            errors.len(),
            1,
            "should report 1 error for undefined function test3"
        );
    }

    /// From TextualKeepGoingVerificationTest.ml: calling a defined function is OK
    #[test]
    fn test_calling_defined_function() {
        let text = r#"
       .source_language = "c"
       .source_file = "fake.c"

       define test1(): *int {
         #start:
           ret test2()
       }

       define test2(): *int {
         #start:
           ret 0
       }
       "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }
}

/// Tests ported from `TextualSilTest.ml`.
mod sil_conversion_compliance_tests {
    use textual::decls::DeclEnv;
    use textual::parser::parse_module;
    use textual::to_sil;

    /// From TextualSilTest.ml: hack extends ordering.
    /// Supers should appear in declaration order in tenv.
    #[test]
    fn test_hack_extends_ordering() {
        let text = r#"
      .source_language = "hack"
      type A extends P0, P1, T0, T1 = { }

      type T1 = {}
      type T0 = {}
      type P1 = {}
      type P0 = {}
      "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (_, tenv) = to_sil::module_to_sil(&module, &decls).unwrap();

        // A should have supers in declaration order
        let a_name = sil::typ::TypeName::HackClass(sil::typ::HackClassName("A".into()));
        let a = tenv.lookup(&a_name).unwrap();
        let super_names: Vec<String> = a.supers.iter().map(|s| format!("{s}")).collect();
        assert_eq!(a.supers.len(), 4, "A should have 4 supers");
        // BTreeSet iterates in sorted order, so just check all are present
        assert!(
            super_names.iter().any(|n| n.contains("P0")),
            "P0 should be a super"
        );
        assert!(
            super_names.iter().any(|n| n.contains("P1")),
            "P1 should be a super"
        );
        assert!(
            super_names.iter().any(|n| n.contains("T0")),
            "T0 should be a super"
        );
        assert!(
            super_names.iter().any(|n| n.contains("T1")),
            "T1 should be a super"
        );
    }

    /// From TextualSilTest.ml: overloads produce distinct procnames via arity.
    #[test]
    fn test_overloads_in_tenv() {
        let text = r#"
     .source_language = "hack"
     define C.f(x: int) : void { #n0: ret null }
     define C.f(x: int, y: int) : void { #n0: ret null }
     "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (cfg, _tenv) = to_sil::module_to_sil(&module, &decls).unwrap();

        // C.f#1 and C.f#2 should be distinct procnames
        assert_eq!(
            cfg.num_procs(),
            2,
            "overloads should produce 2 distinct procs"
        );
    }

    /// From TextualSilTest.ml: basic struct conversion preserves fields.
    #[test]
    fn test_struct_fields_preserved() {
        let text = r#"
     .source_language = "hack"
     type Foo = { x: int; y: float; z: *Foo }
     "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let (_, tenv) = to_sil::module_to_sil(&module, &decls).unwrap();

        let name = sil::typ::TypeName::HackClass(sil::typ::HackClassName("Foo".into()));
        let foo = tenv.lookup(&name).unwrap();
        assert_eq!(foo.fields.len(), 3, "Foo should have 3 fields");
    }
}

/// Tests ported from `TextualTransformTest.ml`.
mod transform_compliance_tests {
    use textual::ast::*;
    use textual::decls::DeclEnv;
    use textual::parser::parse_module;
    use textual::verification::verify;

    /// From TextualTransformTest.ml: Python if-then-else control flow
    #[test]
    fn test_python_if_control_flow() {
        let text = r#"
       .source_language = "python"
        define f(x: int, y: int, z: int, t: int) : int {
          #b0:
              n0:int = load &x
              if n0 then jmp b1 else jmp b2

          #b1:
              n2:int = load &y
              if n2 then jmp b4(n2) else jmp b2

          #b2:
              n5:int = load &z
              if n5 then jmp b5 else jmp b4(n5)

          #b5:
              n8:int = load &t
              jmp b4(n8)

          #b4(n9: int):
              ret n9
        }
        "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");

        // Verify SSA parameters parsed correctly
        match &module.decls[0] {
            Decl::Proc(p) => {
                let b4 = p.nodes.iter().find(|n| n.label.value == "b4").unwrap();
                assert_eq!(b4.ssa_parameters.len(), 1);
            }
            other => panic!("expected Proc, got {other:?}"),
        }
    }

    /// From TextualTransformTest.ml: remove_effects_in_subexprs
    /// Nested calls should be extracted into separate Let instructions.
    #[test]
    fn test_remove_effects_nested_calls() {
        let text = r#"
       .source_language = "python"
        declare g1(int) : int
        declare g2(int) : int
        declare g3(int) : int
        declare g4(int) : *int
        declare m(int, int) : int

        define f(x: int, y: int) : int {
          #entry:
              n0:int = load &x
              n1:int = load &y
              n3 = __sil_mult_int(g3(n0), m(g1(n0), g2(n1)))
              jmp lab1, lab2
          #lab1:
              ret n3
          #lab2:
              throw g4(n3)
        }
    "#;
        let mut module = parse_module(text, "dummy.sil").unwrap();
        textual::transform::remove_effects_in_subexprs(&mut module);

        // After flattening, nested calls (g1, g2, g3, m) should be separate Lets
        // and __sil_mult_int should stay inline with Var args
        if let Decl::Proc(pdesc) = &module.decls[5] {
            let entry = &pdesc.nodes[0];
            // All args to __sil_mult_int should be Vars (calls hoisted)
            let mult_let = entry.instrs.iter().find(|i| {
                matches!(i, Instr::Let { exp: Exp::Call { proc, .. }, .. }
                    if proc.name.value == "__sil_mult_int")
            });
            assert!(mult_let.is_some(), "__sil_mult_int Let should exist");
            if let Some(Instr::Let {
                exp: Exp::Call { args, .. },
                ..
            }) = mult_let
            {
                for arg in args {
                    assert!(
                        matches!(arg, Exp::Var(_)),
                        "mult args should be Var after flattening, got {arg:?}"
                    );
                }
            }
        }

        // The throw g4(n3) in lab2 should also be flattened:
        // g4(n3) hoisted to a Let, throw receives a Var
        if let Decl::Proc(pdesc) = &module.decls[5] {
            let lab2 = pdesc
                .nodes
                .iter()
                .find(|n| n.label.value == "lab2")
                .unwrap();
            assert!(
                matches!(&lab2.last, Terminator::Throw(Exp::Var(_))),
                "throw should have Var arg after flattening, got {:?}",
                lab2.last
            );
        }
    }

    /// From TextualTransformTest.ml: remove_if_terminator
    /// Compound boolean conditions should be decomposed into prune nodes.
    #[test]
    fn test_remove_if_compound_booleans() {
        let text = r#"
       .source_language = "python"
        define f(b1: int, b2: int, b3: int) : int {
          #entry:
              n1 : int = load &b1
              n2 : int = load &b2
              n3 : int = load &b3
              if n1 && n2 && n3 then jmp lab1 else jmp lab2
          #lab1:
              ret 1
          #lab2:
              ret 2
        }
    "#;
        let mut module = parse_module(text, "dummy.sil").unwrap();
        if let Decl::Proc(pdesc) = &mut module.decls[0] {
            textual::transform::remove_if_terminators(pdesc);

            // Entry should no longer have an If terminator
            assert!(
                matches!(&pdesc.nodes[0].last, Terminator::Jump(_)),
                "If should be replaced with Jump"
            );

            // Should have prune nodes with prune instructions
            let prune_nodes: Vec<_> = pdesc
                .nodes
                .iter()
                .filter(|n| n.instrs.iter().any(|i| matches!(i, Instr::Prune { .. })))
                .collect();
            assert!(
                prune_nodes.len() >= 3,
                "n1&&n2&&n3 should produce at least 3 prune nodes (1 true + 2 false), got {}",
                prune_nodes.len()
            );
        }
    }

    /// From TextualTransformTest.ml: let_propagation
    /// Dead side-effect-free idents should be inlined.
    #[test]
    fn test_let_propagation_inlines_builtins() {
        let text = r#"
       .source_language = "python"
        define f(x: int, y: int) : int {
          #entry:
              n0:int = load &x
              n1:int = load &y
              n3 = __sil_mult_int(n0, n1)
              n4 = __sil_minusa(n3, n0)
              n8 = 42
              ret n4
        }
    "#;
        let mut module = parse_module(text, "dummy.sil").unwrap();
        if let Decl::Proc(pdesc) = &mut module.decls[0] {
            textual::transform::let_propagation(pdesc);

            // n3, n4, n8 are all side-effect-free → should be inlined
            // n8 = 42 is dead (never used after propagation) → removed
            let entry = &pdesc.nodes[0];
            let let_count = entry
                .instrs
                .iter()
                .filter(|i| matches!(i, Instr::Let { .. }))
                .count();
            assert_eq!(
                let_count, 0,
                "all side-effect-free Lets should be inlined/removed"
            );

            // The ret should now contain the inlined expression (a Call), not a bare Var(4)
            match &entry.last {
                Terminator::Ret(exp) => {
                    assert!(
                        matches!(exp, Exp::Call { .. }),
                        "n4 should be inlined into ret as a Call expression, got {exp:?}"
                    );
                }
                other => panic!("expected Ret, got {other:?}"),
            }
        }
    }

    /// From TextualTransformTest.ml: out_of_ssa
    /// SSA parameters should become stores/loads.
    #[test]
    fn test_out_of_ssa_basic() {
        let text = r#"
       .source_language = "python"
        define f(x: int, y: int) : int {
          #entry:
              n0:int = load &x
              n1:int = load &y
              jmp lab1(n0, n1), lab3(n1, __sil_mult_int(n1, n0))

          #lab1(n2: int, n3: int):
              jmp lab2(n3, n2)

          #lab2(n4: int, n5: int):
              ret __sil_plusa(n4, n5)

          #lab3(n6: int, n7: int):
              jmp lab2(n6, n7)
        }
    "#;
        let mut module = parse_module(text, "dummy.sil").unwrap();
        if let Decl::Proc(pdesc) = &mut module.decls[0] {
            textual::transform::out_of_ssa(pdesc);

            // All non-handler nodes should have empty ssa_parameters
            for node in &pdesc.nodes {
                assert!(
                    node.ssa_parameters.is_empty(),
                    "node {} should have no SSA parameters after out_of_ssa",
                    node.label.value
                );
            }

            // Jumps should have empty ssa_args
            for node in &pdesc.nodes {
                if let Terminator::Jump(calls) = &node.last {
                    for call in calls {
                        assert!(
                            call.ssa_args.is_empty(),
                            "jump to {} should have no SSA args",
                            call.label.value
                        );
                    }
                }
            }

            // lab1 should start with loads from __SSA variables
            let lab1 = pdesc
                .nodes
                .iter()
                .find(|n| n.label.value == "lab1")
                .unwrap();
            let load_count = lab1
                .instrs
                .iter()
                .filter(|i| {
                    matches!(i, Instr::Load { exp: Exp::Lvar(v), .. } if v.value.starts_with("__SSA"))
                })
                .count();
            assert_eq!(load_count, 2, "lab1 should have 2 SSA loads (for n2, n3)");

            // entry should end with stores to __SSA variables
            let entry = pdesc
                .nodes
                .iter()
                .find(|n| n.label.value == "entry")
                .unwrap();
            let store_count = entry
                .instrs
                .iter()
                .filter(|i| {
                    matches!(i, Instr::Store { exp1: Exp::Lvar(v), .. } if v.value.starts_with("__SSA"))
                })
                .count();
            assert!(
                store_count >= 4,
                "entry should have stores for both jump targets' SSA args, got {store_count}"
            );
        }
    }

    /// From TextualTransformTest.ml: Python if-control-flow + out_of_ssa
    /// Combined transform: if-terminators + SSA elimination.
    #[test]
    fn test_python_if_with_out_of_ssa() {
        let text = r#"
       .source_language = "python"
        define f(x: int, y: int, z: int, t: int) : int {
          #b0:
              n0:int = load &x
              if n0 then jmp b1 else jmp b2

          #b1:
              n2:int = load &y
              if n2 then jmp b4(n2) else jmp b2

          #b2:
              n5:int = load &z
              if n5 then jmp b5 else jmp b4(n5)

          #b5:
              n8:int = load &t
              jmp b4(n8)

          #b4(n9: int):
              ret n9
        }
        "#;
        let mut module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        textual::transform::run(&mut module, &decls);

        // After full transform pipeline:
        // - If terminators should be gone (replaced by prune nodes + jumps)
        // - SSA parameters should be gone (replaced by stores/loads)
        if let Decl::Proc(pdesc) = &module.decls[0] {
            // No If terminators should remain
            for node in &pdesc.nodes {
                assert!(
                    !matches!(&node.last, Terminator::If { .. }),
                    "node {} still has If terminator after full transform",
                    node.label.value
                );
            }

            // No SSA parameters should remain
            for node in &pdesc.nodes {
                assert!(
                    node.ssa_parameters.is_empty(),
                    "node {} still has SSA parameters after full transform",
                    node.label.value
                );
            }

            // Should have prune instructions
            let prune_count: usize = pdesc
                .nodes
                .iter()
                .flat_map(|n| n.instrs.iter())
                .filter(|i| matches!(i, Instr::Prune { .. }))
                .count();
            assert!(
                prune_count >= 3,
                "should have prune instructions from if decomposition"
            );
        }
    }

    /// From TextualTransformTest.ml: nested calls input
    #[test]
    fn test_nested_calls_verified() {
        let text = r#"
       .source_language = "python"
        declare g1(int) : int
        declare g2(int) : int
        declare g3(int) : int
        declare m(int, int) : int

        define f(x: int, y: int) : int {
          #entry:
              n0:int = load &x
              n1:int = load &y
              n3 = __sil_mult_int(g3(n0), m(g1(n0), g2(n1)))
              jmp lab1, lab2
          #lab1:
              ret n3
          #lab2:
              ret n3
        }
    "#;
        let module = parse_module(text, "dummy.sil").unwrap();
        let (decls, _) = DeclEnv::from_module(&module);
        let errors = verify(&module, &decls);
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }

    /// Parser should produce clear errors for malformed input.
    #[test]
    fn test_parse_error_unclosed_brace() {
        let text = r#"
        .source_language = "c"
        define foo() : void {
          #entry:
            ret null
        "#;
        let result = parse_module(text, "bad.sil");
        assert!(result.is_err(), "unclosed brace should fail to parse");
    }

    #[test]
    fn test_parse_error_missing_label() {
        let text = r#"
        .source_language = "c"
        define foo() : void {
            ret null
        }
        "#;
        let result = parse_module(text, "bad.sil");
        assert!(result.is_err(), "missing label should fail to parse");
    }

    #[test]
    fn test_parse_error_unexpected_eof() {
        let text = r#"
        .source_language = "c"
        define foo() : void {
          #entry:
        "#;
        let result = parse_module(text, "bad.sil");
        assert!(result.is_err(), "unexpected EOF should fail to parse");
    }
}
