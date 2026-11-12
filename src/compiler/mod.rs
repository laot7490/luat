use full_moon::LuaVersion;

pub(crate) mod imports;
mod lower;
pub(crate) mod syntax;
mod typecheck;

pub(crate) fn format_output(source: &str, generated: &str) -> Result<String, CompileError> {
    let config = stylua_lib::Config {
        syntax: stylua_lib::LuaVersion::CfxLua,
        indent_type: stylua_lib::IndentType::Spaces,
        indent_width: 4,
        column_width: 120,
        line_endings: stylua_lib::LineEndings::Unix,
        quote_style: stylua_lib::QuoteStyle::AutoPreferSingle,
        ..stylua_lib::Config::default()
    };
    stylua_lib::format_code(
        generated,
        config,
        None,
        stylua_lib::OutputVerification::Full,
    )
    .map_err(|error| CompileError {
        diagnostics: vec![crate::Diagnostic::range(
            source,
            0,
            0,
            "LT9001",
            format!("Could not format generated CfxLua: {error}"),
        )],
    })
}

use crate::diagnostic::{CompileError, parser_diagnostics};

/// Checks LuaT source and returns CfxLua with type-only syntax removed.
pub fn compile_source(source: &str) -> Result<String, CompileError> {
    compile_source_with_declarations(source, &[])
}

pub(crate) fn compile_source_with_declarations(
    source: &str,
    declarations: &[String],
) -> Result<String, CompileError> {
    compile_project_source(source, declarations, None, None)
}

pub(crate) fn compile_project_source(
    source: &str,
    declarations: &[String],
    imports: Option<&std::collections::HashMap<String, String>>,
    import_types: Option<&std::collections::HashMap<String, String>>,
) -> Result<String, CompileError> {
    let prepared = syntax::prepare(source);
    let lua_version = LuaVersion::cfxlua().with_luau();
    let parsed = full_moon::parse_fallible(&prepared.text, lua_version);
    if !parsed.errors().is_empty() {
        return Err(CompileError {
            diagnostics: parser_diagnostics(parsed.errors(), "", source, |offset| {
                prepared.original_offset(offset)
            }),
        });
    }

    let ast = parsed.into_ast();
    self::imports::analyze(source, &prepared, &ast, imports.is_some())?;
    check_ast(source, &prepared, &ast, declarations, import_types)?;

    let lowered = lower::lower(source, &prepared, &ast)?;
    let lowered = if let Some(imports) = imports {
        self::imports::rewrite(&lowered, imports)?
    } else {
        lowered
    };
    format_output(source, &lowered)
}

pub(crate) fn check_source_with_declarations(
    source: &str,
    declarations: &[String],
) -> Result<(), CompileError> {
    check_project_source(source, declarations, false, None)
}

pub(crate) fn infer_module_export_type(
    source: &str,
    import_types: &std::collections::HashMap<String, String>,
) -> String {
    typecheck::infer_module_export_type(source, import_types)
}

pub(crate) fn check_project_source(
    source: &str,
    declarations: &[String],
    project_context: bool,
    import_types: Option<&std::collections::HashMap<String, String>>,
) -> Result<(), CompileError> {
    let prepared = syntax::prepare(source);
    let parsed = full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau());
    if !parsed.errors().is_empty() {
        return Err(CompileError {
            diagnostics: parser_diagnostics(parsed.errors(), "", source, |offset| {
                prepared.original_offset(offset)
            }),
        });
    }
    let ast = parsed.into_ast();
    imports::analyze(source, &prepared, &ast, project_context)?;
    check_ast(source, &prepared, &ast, declarations, import_types)
}

pub(crate) fn collect_imports(source: &str) -> Result<Vec<imports::ImportCall>, CompileError> {
    let prepared = syntax::prepare(source);
    let parsed = full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau());
    if !parsed.errors().is_empty() {
        return Err(CompileError {
            diagnostics: parser_diagnostics(parsed.errors(), "", source, |offset| {
                prepared.original_offset(offset)
            }),
        });
    }
    imports::analyze(source, &prepared, &parsed.into_ast(), true)
}

fn check_ast(
    source: &str,
    prepared: &syntax::PreparedSource,
    ast: &full_moon::ast::Ast,
    declarations: &[String],
    import_types: Option<&std::collections::HashMap<String, String>>,
) -> Result<(), CompileError> {
    let diagnostics =
        typecheck::check_with_imports(source, prepared, ast, declarations, import_types);
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(CompileError { diagnostics })
    }
}

#[cfg(test)]
mod tests {
    use super::{check_source_with_declarations, compile_source, compile_source_with_declarations};

    #[test]
    fn formatter_failures_do_not_claim_a_source_token() {
        let error = super::format_output("local original = 1", "\0").unwrap_err();

        assert_eq!(error.diagnostics[0].code, "LT9001");
        assert_eq!(error.diagnostics[0].source_range, 0..0);
    }

    #[test]
    fn preserves_cfxlua_extensions_and_erases_types() {
        let source = "type Name = string\nlocal name: Name = `adder`\ncount += 1\n";
        let output = compile_source(source).unwrap();
        assert!(output.contains("count += 1"));
        assert!(!output.contains("type Name"));
        assert!(!output.contains(": Name"));
    }

    #[test]
    fn lowers_const_bindings_to_lua_const_locals() {
        let output = compile_source("const selam: number = 15").unwrap();
        assert!(output.contains("local selam <const> = 15"));
    }

    #[test]
    fn compiles_luau_if_expressions_to_cfxlua() {
        let source = "local score = 75\nlocal grade = if score >= 70 then 'pass' else 'fail'";
        let output = compile_source(source).unwrap();
        assert!(!output.contains("then 'pass'"));
        assert!(output.contains("local grade"));
        assert!(output.contains("__luat_0 = 'pass'"));
    }

    #[test]
    fn lowers_nested_if_expressions_and_preserves_false_results() {
        let output =
            compile_source("local value = if true then if false then 1 else false else nil")
                .unwrap();
        assert!(output.contains("__luat_0 = false"), "{output}");
        assert!(output.contains("__luat_0 = nil"), "{output}");
        assert!(output.contains("if false then"), "{output}");

        let output =
            compile_source("local value: any = nil\nvalue = if true then false else nil").unwrap();
        assert!(output.contains("value = __luat_0"), "{output}");

        assert!(compile_source("local value = (if true then 1 else 2)").is_ok());
    }

    #[test]
    fn preserves_comments_inside_if_expressions() {
        let output = compile_source(
            "local value = if true then -- chosen branch\n    1 else -- fallback branch\n    2",
        )
        .unwrap();
        assert!(output.contains("-- chosen branch"), "{output}");
        assert!(output.contains("-- fallback branch"), "{output}");
    }

    #[test]
    fn lowers_an_if_expression_as_a_simple_function_argument() {
        let output = compile_source("print(if true then 'yes' else 'no')").unwrap();
        assert!(output.contains("print(__luat_0)"), "{output}");
        assert!(output.contains("if true then"), "{output}");

        let output = compile_source("local value = wrap(if true then 1 else 2)").unwrap();
        assert!(output.contains("local value = wrap(__luat_0)"), "{output}");
    }

    #[test]
    fn if_expressions_do_not_break_unrelated_nil_coalescing() {
        let source = "local label = if true then 'yes' else 'no'\nlocal value = input ?? false";
        let output = compile_source(source).unwrap();
        assert!(output.contains("__luat_1 == nil then"), "{output}");
        assert!(!output.contains("??"));
    }

    #[test]
    fn rejects_incompatible_annotated_literals() {
        let error = compile_source("local count: number = 'many'").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not assignable to type 'number'")
        );
    }

    #[test]
    fn check_and_build_reject_the_same_undeclared_type() {
        let source = "const spawnPoint = vector3(215, -810, 30)\nlocal marker: Marker = { position = spawnPoint }";
        let compiled = compile_source(source).unwrap_err();
        let checked = check_source_with_declarations(source, &[]).unwrap_err();
        assert_eq!(compiled.diagnostics, checked.diagnostics);
        assert_eq!(compiled.diagnostics[0].code, "LT2006");
        assert_eq!(
            &source[compiled.diagnostics[0].source_range.clone()],
            "Marker"
        );
    }

    #[test]
    fn check_and_build_report_the_same_missing_record_field() {
        let source =
            "type Player = { id: number, job: string }\nlocal player: Player = { id = 42 }";
        let compiled = compile_source(source).unwrap_err();
        let checked = check_source_with_declarations(source, &[]).unwrap_err();
        assert_eq!(compiled.diagnostics, checked.diagnostics);
        assert!(
            compiled.diagnostics[0]
                .message
                .contains("required field 'job'")
        );
    }

    #[test]
    fn record_diagnostics_after_templates_point_into_the_original_source() {
        let source = "type P = { name: string }\nlocal text = `Merhaba, {name}`\nlocal bad: P = {}\n-- trailing comment";
        let error = compile_source(source).unwrap_err();
        let diagnostic = &error.diagnostics[0];

        assert_eq!(diagnostic.location.line, 3);
        assert_eq!(&source[diagnostic.source_range.clone()], "{");
    }

    #[test]
    fn parser_errors_after_templates_keep_original_line_and_token_ranges() {
        let source = "local text = `Merhaba, {name}`\nlocal broken = )\n-- trailing comment";
        let error = compile_source(source).unwrap_err();
        let diagnostic = &error.diagnostics[0];

        assert_eq!(diagnostic.location.line, 2);
        assert_eq!(&source[diagnostic.source_range.clone()], "=");
    }

    #[test]
    fn keeps_template_offsets_when_lexing_fails_early() {
        let source = "local text = `Hi {name}`\nlocal broken = 'unterminated\n-- trailing comment";
        let error = compile_source(source).unwrap_err();
        let diagnostic = &error.diagnostics[0];

        assert_eq!(diagnostic.location.line, 2);
        assert!(source[diagnostic.source_range.clone()].starts_with('\''));
    }

    #[test]
    fn conditional_lowering_errors_keep_the_original_coalesce_range() {
        let source = "local text = `Hi {name}`\nlocal value = if true then candidate ?? 1 else 2\n-- trailing comment";
        let error = compile_source(source).unwrap_err();
        let diagnostic = &error.diagnostics[0];

        assert_eq!(diagnostic.code, "LT3001");
        assert_eq!(diagnostic.location.line, 2);
        assert_eq!(&source[diagnostic.source_range.clone()], "??");
    }

    #[test]
    fn type_errors_inside_template_interpolations_map_to_the_expression() {
        let source = "local function use(value: number) end\nlocal text = `result {use('wrong')}`\n-- trailing comment";
        let error = compile_source(source).unwrap_err();
        let diagnostic = &error.diagnostics[0];

        assert_eq!(diagnostic.code, "LT2002");
        assert_eq!(&source[diagnostic.source_range.clone()], "'wrong'");
        assert_eq!(diagnostic.location.line, 2);
    }

    #[test]
    fn composes_template_optional_and_coalesce_maps_for_unicode_crlf_sources() {
        let source = "type Player = { label: string, note?: string }\r\nlocal text = `Ça {name}`\r\nlocal fallback = input ?? 'unknown'\r\nlocal bad: Player = { label = 12 }\r\n-- trailing comment";
        let error = compile_source(source).unwrap_err();
        let diagnostic = &error.diagnostics[0];

        assert_eq!(diagnostic.location.line, 4);
        assert_eq!(&source[diagnostic.source_range.clone()], "12");
    }

    #[test]
    fn imported_generic_record_aliases_keep_check_and_build_diagnostics_equal() {
        let source = "local players: List<Player> = { { id = 1 } }";
        let declarations = [
            "type Player = { id: number, callsign: string }".to_owned(),
            "type List<T> = { T }".to_owned(),
        ];
        let built = compile_source_with_declarations(source, &declarations).unwrap_err();
        let checked = check_source_with_declarations(source, &declarations).unwrap_err();

        assert_eq!(built.diagnostics, checked.diagnostics);
        assert!(built.diagnostics[0].message.contains("[1].callsign"));
        assert_eq!(&source[built.diagnostics[0].source_range.clone()], "{");
    }

    #[test]
    fn tuple_and_vararg_diagnostics_match_between_check_and_build() {
        for source in [
            "local function use(count: number, label: string) end\nlocal function pair(): (number, number) return 1, 2 end\nuse(pair())",
            "local text = `Ça {name}`\r\nlocal function use(value: number) end\r\nuse('yanlış')\r\n",
        ] {
            let built = compile_source(source).unwrap_err();
            let checked = check_source_with_declarations(source, &[]).unwrap_err();
            assert_eq!(built.diagnostics, checked.diagnostics, "{source}");
            assert!(source.is_char_boundary(built.diagnostics[0].source_range.start));
            assert!(source.is_char_boundary(built.diagnostics[0].source_range.end));
        }
    }

    #[test]
    fn generated_tuple_and_vararg_lua_keeps_false_nil_and_forwarding() {
        let source = "local function pair(...: number): (boolean, nil) return false, nil end\nlocal function forward(...: number) return pair(...) end";
        let output = compile_source(source).unwrap();
        assert!(output.contains("return false, nil"), "{output}");
        assert!(output.contains("return pair(...)"), "{output}");
        assert!(full_moon::parse(&output).is_ok(), "{output}");
    }

    #[test]
    fn reports_recursive_alias_cycles_from_declaration_files_at_the_use_site() {
        let source = "local value: Loop = 1";
        let error =
            compile_source_with_declarations(source, &["type Loop = Loop".to_owned()]).unwrap_err();

        assert_eq!(error.diagnostics[0].code, "LT2006");
        assert_eq!(&source[error.diagnostics[0].source_range.clone()], "Loop");
    }

    #[test]
    fn reports_deep_alias_expansion_from_declaration_files_at_the_use_site() {
        let mut declarations = vec!["type A0 = number".to_owned()];
        for index in 1..=129 {
            declarations.push(format!("type A{index} = A{}", index - 1));
        }
        let source = "local value: A129 = 1";
        let error = compile_source_with_declarations(source, &declarations).unwrap_err();

        assert_eq!(error.diagnostics[0].code, "LT2006");
        assert_eq!(&source[error.diagnostics[0].source_range.clone()], "A129");
    }

    #[test]
    fn check_and_build_report_the_same_typecheck_regressions() {
        for source in [
            "local spawnOffset: vector3 = vector4(0.0, 0.0, 1.0, 0.0)",
            "local first: Player = { name = 'Ada' }\nlocal firstName: string = first.ax ?? 'Unknown'",
        ] {
            let declarations = ["type Player = { name: string }".to_owned()];
            let built = compile_source_with_declarations(source, &declarations).unwrap_err();
            let checked = check_source_with_declarations(source, &declarations).unwrap_err();
            assert_eq!(built.diagnostics, checked.diagnostics);
        }
        let source = "local first: Player = { name = 'Ada' }\nlocal firstName: string = first.ax ?? 'Unknown'";
        let error = compile_source_with_declarations(
            source,
            &["type Player = { name: string }".to_owned()],
        )
        .unwrap_err();
        assert_eq!(error.diagnostics[0].code, "LT2005");

        for source in [
            "type Player = { callsign: string }\ntype List<T> = { T }\nlocal players: List<Player> = { {} }",
            "type Dictionary = { [string]: number }\nlocal values: Dictionary = { count = 'bad' }",
            "type A = { a: string }\ntype B = { b: number }\ntype Either = A | B\nlocal value: Either = { a = 'ok', b = 'bad' }",
            "type List<T> = { T }\nlocal values: List<number> = { 1 }\nvalues[2] = 'bad'",
        ] {
            let built = compile_source(source).unwrap_err();
            let checked = check_source_with_declarations(source, &[]).unwrap_err();
            assert_eq!(built.diagnostics, checked.diagnostics, "{source}");
        }
    }

    #[test]
    fn optional_record_fields_compile_and_erase() {
        let output =
            compile_source("type Player = { coords?: vector3 }\nlocal p: Player = {}").unwrap();
        assert!(!output.contains("type Player"));
        assert!(!output.contains("coords?"));
        assert!(compile_source("type P = { name?: string }\nlocal p: P = {name = 2}").is_err());
        assert!(
            compile_source(
                "local name = 'Ada'\n type P = { name?: string }\n local p: P = {name = name}"
            )
            .is_ok()
        );
    }

    #[test]
    fn preserves_source_lines_and_reports_syntax_errors() {
        let error = compile_source("local value: number = 1\nif then").unwrap_err();
        assert_eq!(error.diagnostics[0].location.line, 2);
    }

    #[test]
    fn type_is_still_usable_as_a_runtime_identifier() {
        let output = compile_source("local type = type\nprint(type('x'))").unwrap();
        assert!(output.contains("local type = type"));
    }

    #[test]
    fn removes_generic_aliases_and_function_annotations() {
        let source = "type List<T> = {T}\nlocal function first<T>(items: List<T>): T\n    return items[1]\nend\n";
        let output = compile_source(source).unwrap();
        assert!(!output.contains("type List"));
        assert!(!output.contains("<T>"));
        assert!(!output.contains(": List<T>"));
        assert!(output.contains("return items[1]"));
    }

    #[test]
    fn lowers_coalescing_without_anonymous_function_wrappers() {
        let source = "local fallback = (function(...) return ... end) ?? nil\nlocal value = get_value() ?? -- lazily chosen\n    false";
        let output = compile_source(source).unwrap();
        assert!(output.contains("-- lazily chosen"), "{output}");
        assert!(output.contains("__luat_0 == nil then"), "{output}");
        assert!(output.contains("__luat_1 = false"), "{output}");
        assert!(output.contains("return ..."), "{output}");
        assert_eq!(output.matches("get_value()").count(), 1);
        assert!(!output.contains("function(__luat"), "{output}");
        assert!(!output.contains("??"));
    }

    #[test]
    fn permits_outer_varargs_when_lowered_without_function_scopes() {
        let output =
            compile_source("local function first(...) local value = ... ?? nil end").unwrap();
        assert!(output.contains("local __luat_0 = (...)"));

        let error = compile_source("local text = 'é'\nlocal value = a??b\nif then").unwrap_err();
        assert_eq!(error.diagnostics[0].location.line, 3);
    }

    #[test]
    fn formats_erased_types_and_preserves_fivem_optional_access() {
        let output = compile_source("local value: number=1\nlocal name=input?.field").unwrap();
        assert!(output.contains("local value = 1"));
        assert!(output.contains("input?.field"));
        assert!(!output.lines().any(|line| line.trim().is_empty()));
    }
}
