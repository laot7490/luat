use super::*;

fn analyze(source: &str) -> (AnalysisSnapshot, PathBuf) {
    let path = PathBuf::from("target/tests/assistance.luat");
    let mut host = AnalysisHost::new();
    host.set_file(path.clone(), source.to_owned());
    (host.snapshot(), path)
}

#[test]
fn callback_record_hover_preserves_named_parameters_and_return_types() {
    for declaration in ["local", "const"] {
        let source = format!(
            "typedef Player = {{ name: string }}\nlocal player: Player\n{declaration} callbacks = {{ render = function(value: Player): string return value.name end }}\nlocal rendered = callbacks.render(player)"
        );
        let (snapshot, path) = analyze(&source);
        let offset = source.rfind("render(player)").unwrap();
        let hover = snapshot.hover(&path, offset).unwrap();
        assert_eq!(&source[hover.range], "render");
        assert!(
            hover
                .contents
                .contains("function callbacks.render(value: Player): string")
        );
        let help = snapshot
            .signature_help(&path, source.rfind("player)").unwrap())
            .unwrap();
        assert_eq!(help.signatures[0].parameters, ["value: Player"]);
        assert!(help.signatures[0].label.ends_with(": string"));
        let completion = snapshot
            .completions(&path, offset)
            .into_iter()
            .find(|item| item.label == "render")
            .unwrap();
        assert_eq!(completion.kind, CompletionKind::Function);
        assert!(completion.detail.contains("value: Player"));
    }
}

#[test]
fn nested_callback_record_hover_keeps_plain_fields_and_empty_parameters() {
    let source = "local callbacks = { nested = { render = function(): string return 'ok' end }, title = 'hello', count = 1 }\ncallbacks.nested.render()\nprint(callbacks.title, callbacks.count)";
    let (snapshot, path) = analyze(source);
    let hover = snapshot
        .hover(&path, source.rfind("render()").unwrap())
        .unwrap();
    assert!(
        hover
            .contents
            .contains("function callbacks.nested.render(): string")
    );
    for (name, ty) in [("title", "string"), ("count", "number")] {
        let hover = snapshot.hover(&path, source.rfind(name).unwrap()).unwrap();
        assert!(hover.contents.contains(&format!("{name}: {ty}")));
    }
    assert!(
        snapshot
            .hover(&path, source.find("'hello'").unwrap() + 1)
            .is_none()
    );
}

#[test]
fn callback_record_hover_handles_method_access_and_explicit_self() {
    let source = "local service = { current = player, getName = function(self): string return self.current.name end }\nlocal currentName = service:getName()\nservice.getName(service)";
    let (snapshot, path) = analyze(&source);
    for (call, expected) in [
        ("service:getName()", "function service:getName(): string"),
        ("service.getName(service)", "function service.getName("),
    ] {
        let offset = source.find(call).unwrap() + call.find("getName").unwrap();
        let hover = snapshot.hover(&path, offset).unwrap();
        assert_eq!(&source[hover.range], "getName");
        assert!(hover.contents.contains(expected));
        if call.contains(".getName") {
            assert!(hover.contents.contains("self:"));
            assert!(hover.contents.contains(": string"));
        }
    }
}

#[test]
fn template_interpolations_keep_member_semantic_tokens() {
    let source = "local player = { name = 'Ada' }\nlocal status = 'ready'\nlocal greeting = `Hoş geldin, {player.name}! Durum: {status}.`";
    let (snapshot, path) = analyze(&source);
    let tokens = snapshot.semantic_tokens(&path);
    for name in ["player", "name", "status"] {
        let start = source.rfind(name).unwrap();
        let token = tokens
            .iter()
            .find(|(range, _, _)| range.start == start)
            .unwrap();
        assert_eq!(&source[token.0.clone()], name);
    }
}

#[test]
fn callback_record_hover_uses_updated_buffers_and_keeps_old_snapshots() {
    let path = PathBuf::from("target/tests/callback-hover.luat");
    let mut host = AnalysisHost::new();
    let old_source = "local callbacks = { render = function(value: string): string return value end }\ncallbacks.render('ok')";
    host.set_file(path.clone(), old_source.to_owned());
    let old = host.snapshot();
    let new_source = old_source.replace("string", "number");
    host.set_file(path.clone(), new_source.clone());
    let new = host.snapshot();
    for (snapshot, source, ty) in [
        (old, old_source, "string"),
        (new, new_source.as_str(), "number"),
    ] {
        let hover = snapshot
            .hover(&path, source.rfind("render(").unwrap())
            .unwrap();
        assert!(
            hover
                .contents
                .contains(&format!("function callbacks.render(value: {ty}): {ty}"))
        );
    }
}

#[test]
fn callback_record_hover_handles_untyped_parameters_and_varargs() {
    let source = "local callbacks = { render = function(value, count: number, ...: string) return value end }\ncallbacks.render(1, 2, 'x')";
    let (snapshot, path) = analyze(source);
    let hover = snapshot
        .hover(&path, source.rfind("render(").unwrap())
        .unwrap();
    assert!(
        hover
            .contents
            .contains("function callbacks.render(value: any, count: number, ...: string): any")
    );
}

#[test]
fn callback_record_hover_rejects_missing_fields_and_incomplete_functions() {
    for source in [
        "local callbacks = {}\ncallbacks.render()",
        "local callbacks = { title = 'hello' }\ncallbacks.render()",
        "local callbacks = { render = function(value:",
    ] {
        let (snapshot, path) = analyze(source);
        assert!(
            snapshot
                .hover(&path, source.rfind("render").unwrap())
                .is_none()
        );
    }
}

#[test]
fn markdown_comments_preserve_links_and_typeless_parameter_descriptions() {
    let source = "---[Native Documentation](https://docs.fivem.net/natives/?_0xE7490533)  \n---next line\n---@param value string description\n---@return Sonuç\nlocal function documented(value: string): string return value end\ndocumented('x')";
    let (snapshot, path) = analyze(source);
    let hover = snapshot
        .hover(&path, source.rfind("documented").unwrap())
        .unwrap();
    assert!(!hover.contents.contains("**Documentation**"));
    assert!(hover.contents.contains(
        "[Native Documentation](https://docs.fivem.net/natives/?_0xE7490533)  \nnext line"
    ));
    let help = snapshot
        .signature_help(&path, source.rfind("'x'").unwrap())
        .unwrap();
    assert_eq!(
        help.signatures[0].parameter_documentation[0].as_deref(),
        Some("string description")
    );
    let (snapshot, path) = analyze("---@param x Açıklama\nlocal function f(x) end\nf(1)");
    assert_eq!(
        snapshot
            .signature_help(&path, snapshot.source(&path).unwrap().len() - 1)
            .unwrap()
            .signatures[0]
            .parameter_documentation[0]
            .as_deref(),
        Some("Açıklama")
    );
}

#[test]
fn signature_help_selects_innermost_call_and_returns_to_outer() {
    let source = "local function outer(value: string, second: number) end\nlocal function inner(value: number): string return tostring(value) end\nouter(inner(1), 2)";
    let (snapshot, path) = analyze(source);
    let inner = snapshot
        .signature_help(&path, source.rfind("1)").unwrap())
        .unwrap();
    assert!(
        inner.signatures[0].label.starts_with("function inner("),
        "{:?}",
        inner
    );
    let outer = snapshot
        .signature_help(&path, source.rfind('2').unwrap())
        .unwrap();
    assert!(outer.signatures[0].label.starts_with("function outer("));
    assert_eq!(outer.active_parameter, 1);
}

#[test]
fn standard_lua_declarations_are_indexed_and_navigable() {
    for text in [
        include_str!("../../lsp/definitions/lua-shared.d.luat"),
        include_str!("../../lsp/definitions/lua-server.d.luat"),
    ] {
        crate::compiler::declarations::parse_declaration_file(text).unwrap();
    }
    let source = "print('x')\nmath.abs(-1)\nstring.format('%s', 'x')\nutf8.len('é')\ncoroutine.yield()\ntable.pack(1)";
    let (snapshot, path) = analyze(source);
    for name in ["print", "abs", "format", "len", "yield", "pack"] {
        let offset = source.find(name).unwrap();
        let hover = snapshot.hover(&path, offset).unwrap();
        assert!(hover.contents.contains("function "), "{name}: {hover:?}");
        assert!(
            snapshot
                .definition(&path, offset)
                .unwrap()
                .path
                .ends_with("lua-shared.d.luat")
        );
    }
    assert!(
        snapshot.signature_help(&path, 7).unwrap().signatures[0]
            .label
            .contains("...: any")
    );
    assert!(
        snapshot
            .completions(&path, 0)
            .iter()
            .any(|item| item.label == "print" && item.documentation.is_some())
    );
}

#[test]
fn statement_start_typedef_completion_inserts_a_type_alias_declaration() {
    let (snapshot, path) = analyze("typedef");
    let items = snapshot.completions(&path, 7);

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].label, "typedef");
    assert_eq!(items[0].kind, CompletionKind::Snippet);
    assert_eq!(
        items[0].insert_text.as_deref(),
        Some("typedef ${1:Name} = ${2:any}")
    );

    let source = "local value = type";
    let (snapshot, path) = analyze(source);
    assert!(
        snapshot
            .completions(&path, source.len())
            .iter()
            .any(|item| item.label == "type" && item.kind == CompletionKind::Function)
    );
}

#[test]
fn old_alias_declarations_report_syntax_errors_in_lsp_diagnostics() {
    let source = "-- 😀\ntype Player = string";
    let (snapshot, path) = analyze(source);
    let diagnostic = snapshot.diagnostics(&path).unwrap().first().unwrap();
    assert_eq!(diagnostic.code, "LT1001");
    assert_eq!(&source[diagnostic.source_range.clone()], "type");
    assert!(diagnostic.message.contains("Invalid type alias syntax."));
    assert!(!diagnostic.message.contains("Use 'typedef'"));
}

#[test]
fn server_standard_library_is_hidden_from_client_contexts() {
    let (mut snapshot, path) = analyze("os.clock()\nprint(1)\nos.");
    Arc::make_mut(&mut snapshot.contexts)
        .insert(path.clone(), vec![crate::ExecutionContext::Client]);
    assert!(snapshot.hover(&path, 3).is_none());
    assert!(
        !snapshot
            .completions(&path, 0)
            .iter()
            .any(|item| item.label == "os")
    );
    assert!(
        snapshot
            .completions(&path, snapshot.source(&path).unwrap().len())
            .is_empty()
    );
    Arc::make_mut(&mut snapshot.contexts)
        .insert(path.clone(), vec![crate::ExecutionContext::Server]);
    assert!(snapshot.hover(&path, 3).is_some());
    assert!(
        snapshot
            .completions(&path, snapshot.source(&path).unwrap().len())
            .iter()
            .any(|item| item.label == "clock")
    );
}

#[test]
fn records_resolve_generic_aliases_and_show_field_descriptions() {
    let source = "---@field title Başlık\ntypedef Options<T> = { title: T, enabled: boolean?, nested: { count: number } }\nlocal options: Options<string> = { title = 'x' }\nprint(options.title)\noptions.";
    let (snapshot, path) = analyze(source);
    let items = snapshot.completions(&path, source.len());
    let title = items.iter().find(|item| item.label == "title").unwrap();
    assert_eq!(title.detail, "string");
    assert_eq!(title.documentation.as_deref(), Some("Başlık"));
    assert_eq!(title.kind, CompletionKind::Property);
    let hover = snapshot
        .hover(&path, source.find("options.title").unwrap() + 8)
        .unwrap();
    assert!(hover.contents.contains("Başlık"));
    let (snapshot, path) = analyze(
        "local function configure(options: { title: string, enabled: boolean? }) end\nconfigure({ title = 'x', en",
    );
    let items = snapshot.completions(&path, snapshot.source(&path).unwrap().len());
    assert_eq!(
        items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["enabled"]
    );
    let (snapshot, path) = analyze(
        "typedef A = { common: string, left: number }\ntypedef B = { common: string, right: number }\nlocal x: A | B\nx.",
    );
    assert_eq!(
        snapshot
            .completions(&path, snapshot.source(&path).unwrap().len())
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["common"]
    );
}

#[test]
fn unused_hints_track_reads_shadowing_and_safe_renames() {
    let source = "local written = 1\nwritten = 2\nlocal function callback(used: number, ignored: string)\n local function capture() return used end\n print(capture())\nend\ncallback(1, 'x')";
    let (snapshot, path) = analyze(source);
    let hints = snapshot.editor_diagnostics(&path);
    let names = hints
        .iter()
        .map(|hint| &source[hint.range.clone()])
        .collect::<Vec<_>>();
    assert!(names.contains(&"ignored"));
    assert!(names.contains(&"written"));
    assert!(!names.contains(&"used"));
    let ignored = hints
        .iter()
        .find(|hint| &source[hint.range.clone()] == "ignored")
        .unwrap();
    let fix = snapshot.unused_fix(&path, ignored.range.start).unwrap();
    assert_eq!(fix.changes[&path][0].new_text, "_ignored");
    let (quiet, quiet_path) = analyze("local _intentional = 1");
    assert!(quiet.editor_diagnostics(&quiet_path).is_empty());
    let (snapshot, path) = analyze("local function f(x, _x) end\nf()");
    assert!(
        snapshot
            .unused_fix(&path, snapshot.source(&path).unwrap().find("x,").unwrap())
            .is_none()
    );
}

#[test]
fn document_symbols_and_opt_in_inlay_data_use_source_ranges() {
    let source = "local count = 1\nlocal function outer(value: number)\n local function inner(text: string) end\n inner('hi')\nend\nouter(2)";
    let (snapshot, path) = analyze(source);
    let symbols = snapshot.document_symbols(&path);
    let outer = symbols
        .iter()
        .find(|symbol| symbol.name == "outer")
        .unwrap();
    assert!(outer.range.start <= outer.selection.start && outer.range.end >= outer.selection.end);
    assert!(outer.children.iter().any(|symbol| symbol.name == "inner"));
    let hints = snapshot.inlay_hints(&path, 0..source.len(), true, true);
    assert!(hints.iter().any(|hint| hint.label == ": number"));
    assert!(hints.iter().any(|hint| hint.label == "value:"));
    assert!(
        snapshot
            .inlay_hints(&path, 0..source.len(), false, false)
            .is_empty()
    );
}

#[test]
fn incomplete_function_symbols_keep_their_selection_inside_the_range() {
    let (snapshot, path) = analyze("local function f(");
    let symbol = snapshot
        .document_symbols(&path)
        .into_iter()
        .find(|symbol| symbol.name == "f")
        .unwrap();
    assert!(symbol.range.start <= symbol.selection.start);
    assert!(symbol.range.end >= symbol.selection.end);
}

#[test]
fn documentation_completion_and_deprecation_are_contextual() {
    let source = "---@param va\nlocal function f(value: string): string return value end";
    let (snapshot, path) = analyze(source);
    assert!(
        snapshot
            .completions(&path, source.find('\n').unwrap())
            .iter()
            .any(|item| item.label == "value")
    );
    let source = "---\nlocal function f(value: string): string return value end";
    let (snapshot, path) = analyze(source);
    let items = snapshot.completions(&path, 3);
    let snippet = items
        .iter()
        .find_map(|item| item.insert_text.as_deref())
        .unwrap();
    assert!(snippet.contains("---@param value "));
    assert!(!snippet.contains("value string"));
    assert!(snippet.contains("---@return "));
    let source = "---@deprecated Use replacement.\nlocal function old() end\nold()";
    let (snapshot, path) = analyze(source);
    assert!(
        snapshot
            .editor_diagnostics(&path)
            .iter()
            .any(|diagnostic| diagnostic.deprecated)
    );
    assert!(
        snapshot
            .semantic_tokens(&path)
            .iter()
            .any(
                |(range, _, modifiers)| range.start == source.rfind("old").unwrap()
                    && modifiers & 8 != 0
            )
    );
}

#[test]
fn signature_help_handles_method_self_and_rejects_non_calls() {
    let source = "local object = {}\nfunction object:method(value: number) end\nobject:method(1)";
    let (snapshot, path) = analyze(source);
    let help = snapshot
        .signature_help(&path, source.rfind('1').unwrap())
        .unwrap();
    assert_eq!(help.signatures[0].parameters, ["value: number"]);
    let source = "local function f(value: number) end\nf((1))";
    let (snapshot, path) = analyze(source);
    assert_eq!(
        snapshot
            .signature_help(&path, source.rfind('1').unwrap())
            .unwrap()
            .signatures[0]
            .parameters,
        ["value: number"]
    );
}

#[test]
fn standard_inventory_is_complete_and_declarations_validate() {
    let mut sources = Vec::new();
    for (name, text) in [
        (
            "lua-shared.d.luat",
            include_str!("../../lsp/definitions/lua-shared.d.luat"),
        ),
        (
            "lua-server.d.luat",
            include_str!("../../lsp/definitions/lua-server.d.luat"),
        ),
    ] {
        for line in text.lines() {
            if let Some(name) = line
                .strip_prefix("---")
                .and_then(|line| line.strip_prefix("Lua 5.4 `"))
            {
                panic!("missing Lua manual summary: {name}");
            }
            if let Some(name) = line
                .strip_prefix("function ")
                .and_then(|line| line.split_once('(').map(|(name, _)| name))
            {
                assert!(
                    builtin_index().get(name).is_some_and(|symbols| symbols
                        .iter()
                        .any(|symbol| symbol.file.ends_with(".d.luat"))),
                    "{name}"
                );
            }
        }
        sources.push(crate::compiler::declarations::DeclarationSource {
            path: Some(PathBuf::from(name)),
            context: None,
            text: text.to_owned(),
        });
    }
    crate::compiler::declarations::DeclarationSet::from_sources(sources).unwrap();
}

#[test]
fn standard_library_docs_reach_hover_and_completion() {
    let source = "rawget({}, 'key')\ncoroutine.resume(co)\nstring.find('x', 'x')\nio.type(file)";
    let (snapshot, path) = analyze(source);
    for (name, expected) in [
        ("rawget", "bypassing the `__index` metamethod"),
        ("resume", "continues `co`"),
        ("find", "Searches `s` for `pattern`"),
        ("type", "open file handle"),
    ] {
        let offset = source.rfind(name).unwrap();
        assert!(
            snapshot
                .hover(&path, offset)
                .unwrap()
                .contents
                .contains(expected),
            "{name}"
        );
        assert!(
            snapshot
                .completions(&path, offset + name.len())
                .iter()
                .any(|item| {
                    item.label == name
                        && item
                            .documentation
                            .as_deref()
                            .is_some_and(|doc| doc.contains(expected))
                }),
            "completion: {name}"
        );
    }
}

#[test]
fn records_and_inlays_avoid_unknowns_and_support_deep_context() {
    for ty in ["any", "Unknown", "Cycle"] {
        let source = format!("typedef Cycle = Cycle\nlocal x: {ty}\nx.");
        let (snapshot, path) = analyze(&source);
        assert!(snapshot.completions(&path, source.len()).is_empty(), "{ty}");
    }
    let source = "local function f(options: { nested: { deeper: { value: string, extra: boolean } } }) end\nf({ nested = { deeper = { va";
    let (snapshot, path) = analyze(source);
    assert_eq!(
        snapshot
            .completions(&path, source.len())
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        ["value"]
    );
    let source =
        "local function f(first: number, second: string) end\nlocal text = 'ş🙂'\nf (1, 'x')";
    let (snapshot, path) = analyze(source);
    assert_eq!(
        snapshot
            .signature_help(&path, source.rfind("'x'").unwrap())
            .unwrap()
            .active_parameter,
        1
    );
    assert!(
        snapshot
            .inlay_hints(&path, 0..source.len(), true, true)
            .iter()
            .any(|hint| hint.label == "second:")
    );
    assert!(snapshot.inlay_hints(&path, 0..1, true, true).is_empty());
}

#[test]
fn review_regressions_keep_record_values_and_strings_out_of_field_completion() {
    let source = "local expression = 1\nlocal function f(x: { title: string, enabled: boolean }) end\nf({ title = ";
    let (snapshot, path) = analyze(source);
    assert!(
        snapshot
            .completions(&path, source.len())
            .iter()
            .any(|item| item.label == "expression")
    );
    for text in [
        "local value: { title: string }\nlocal s = [[ value.",
        "local value: { title: string }\n-- value.",
    ] {
        let (snapshot, path) = analyze(text);
        assert!(snapshot.completions(&path, text.len()).is_empty());
    }
}

#[test]
fn review_regressions_preserve_method_receiver_and_callback_parameters() {
    let source = "local file: LuaFile\nfile:seek(";
    let (snapshot, path) = analyze(source);
    let help = snapshot.signature_help(&path, source.len()).unwrap();
    assert_eq!(
        help.signatures[0].parameters,
        ["whence: string?", "offset: number?"]
    );
    let source = "local x: { run: ((number) -> string, number) -> boolean }\nx.run(";
    let (snapshot, path) = analyze(source);
    let help = snapshot.signature_help(&path, source.len()).unwrap();
    assert_eq!(
        help.signatures[0].parameters,
        ["arg1: (number) -> string", "arg2: number"]
    );
    assert!(help.signatures[0].label.ends_with(": boolean"));
    let source = "local x = {}\nfunction x:run(value: number) end\nx.run(";
    let (snapshot, path) = analyze(source);
    let help = snapshot.signature_help(&path, source.len()).unwrap();
    assert!(help.signatures[0].parameters[0].starts_with("self:"));
    assert_eq!(help.signatures[0].parameters[1], "value: number");
}

#[test]
fn review_regressions_unused_function_fix_and_global_capture() {
    let (snapshot, path) = analyze("local function unused() end");
    assert_eq!(
        snapshot.unused_fix(&path, 16).unwrap().changes[&path][0].new_text,
        "_unused"
    );
    let source = "local unused = 1\nprint(_unused)";
    let (snapshot, path) = analyze(source);
    assert!(
        snapshot
            .editor_diagnostics(&path)
            .iter()
            .any(|hint| hint.unnecessary)
    );
    assert!(snapshot.unused_fix(&path, 6).is_none());
}
