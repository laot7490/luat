use super::*;

pub(super) struct BuiltinSymbol {
    pub(super) name: String,
    pub(super) file: String,
    pub(super) range: std::ops::Range<usize>,
    pub(super) signature: Option<SignatureInformation>,
    pub(super) kind: CompletionKind,
    pub(super) type_name: String,
    pub(super) documentation: String,
    pub(super) context: &'static str,
}

pub(super) fn hover_markdown(detail: &str, documentation: &str) -> String {
    let detail = crate::compiler::typecheck::display_type(detail);
    let mut contents = format!("```luat\n{detail}\n```");
    let documentation = Documentation::parse(documentation).render();
    if !documentation.trim().is_empty() {
        contents.push_str("\n\n");
        contents.push_str(&documentation);
    }
    contents
}

impl BuiltinSymbol {
    pub(super) fn detail(&self) -> String {
        self.signature.as_ref().map_or_else(
            || format!("{}: {}", self.name, self.type_name),
            |signature| signature.label.clone(),
        )
    }
}

pub(super) fn documentation_before(source: &str, offset: usize) -> String {
    documentation::raw_before(source, offset)
}

pub(super) fn nonempty(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

pub(super) fn parameter_documentation(
    source: &str,
    offset: usize,
    parameters: &[String],
) -> Vec<Option<String>> {
    let documentation = Documentation::before(source, offset);
    parameters
        .iter()
        .map(|parameter| {
            let name = parameter
                .split(':')
                .next()
                .unwrap_or(parameter)
                .trim()
                .trim_end_matches('?');
            documentation.parameters.get(name).cloned().or_else(|| {
                documentation.markdown.lines().find_map(|line| {
                    let marker = format!("**{name}**");
                    line.split_once(&marker)
                        .and_then(|(_, text)| text.strip_prefix(':'))
                        .map(|text| text.trim().to_owned())
                        .or_else(|| {
                            line.split_once(':')
                                .filter(|(documented, _)| documented.trim() == name)
                                .map(|(_, text)| text.trim().to_owned())
                        })
                })
            })
        })
        .collect()
}

pub(super) fn builtin_index() -> &'static BTreeMap<String, Vec<BuiltinSymbol>> {
    static INDEX: OnceLock<BTreeMap<String, Vec<BuiltinSymbol>>> = OnceLock::new();
    INDEX.get_or_init(|| {
        [
            ("lua-shared.d.luat", crate::definitions::LUA_SHARED),
            ("lua-server.d.luat", crate::definitions::LUA_SERVER),
            ("fivem-client.d.luat", crate::definitions::FIVEM_CLIENT),
            ("fivem-server.d.luat", crate::definitions::FIVEM_SERVER),
            ("fivem-shared.d.luat", crate::definitions::FIVEM_SHARED),
            ("runtime-client.d.luat", crate::definitions::RUNTIME_CLIENT),
            ("runtime-server.d.luat", crate::definitions::RUNTIME_SERVER),
            ("runtime-shared.d.luat", crate::definitions::RUNTIME_SHARED),
        ]
        .into_iter()
        .fold(
            BTreeMap::<String, Vec<BuiltinSymbol>>::new(),
            |mut index, (file, source)| {
                let context = if file.contains("client") {
                    "client"
                } else if file.contains("server") {
                    "server"
                } else {
                    "shared"
                };
                if let Ok((prepared, ast, globals)) =
                    crate::compiler::declarations::parse_declaration_file(source)
                {
                    let collector = SemanticCollector::new(source, &prepared);
                    for global in globals {
                        index
                            .entry(global.name.clone())
                            .or_default()
                            .push(BuiltinSymbol {
                                name: global.name,
                                file: file.to_owned(),
                                range: global.location.range,
                                signature: None,
                                kind: if global.type_name.trim_start().starts_with('(')
                                    && global.type_name.contains("->")
                                {
                                    CompletionKind::Function
                                } else {
                                    CompletionKind::Variable
                                },
                                type_name: global.type_name,
                                documentation: global.documentation,
                                context,
                            });
                    }
                    for statement in ast.nodes().stmts() {
                        if let ast::Stmt::TypeDeclaration(alias) = statement {
                            let token = alias.type_name().token();
                            let name = token.to_string();
                            let range = prepared.original_offset(token.start_position().bytes())
                                ..prepared.original_offset(token.end_position().bytes());
                            index.entry(name.clone()).or_default().push(BuiltinSymbol {
                                name,
                                file: file.to_owned(),
                                range: range.clone(),
                                signature: None,
                                kind: CompletionKind::Type,
                                type_name: alias.type_definition().to_string().trim().to_owned(),
                                documentation: documentation_before(source, range.start),
                                context,
                            });
                        }
                        let ast::Stmt::FunctionDeclaration(function) = statement else {
                            continue;
                        };
                        let name = function.name().to_string().trim().to_owned();
                        let range = prepared
                            .original_offset(function.name().start_position().unwrap().bytes())
                            ..prepared
                                .original_offset(function.name().end_position().unwrap().bytes());
                        index.entry(name.clone()).or_default().push(BuiltinSymbol {
                            name: name.clone(),
                            file: file.to_owned(),
                            range: range.clone(),
                            signature: Some(collector.signature(function.body(), &name)),
                            kind: CompletionKind::Function,
                            type_name: "function".to_owned(),
                            documentation: documentation_before(source, range.start),
                            context,
                        });
                    }
                }
                index
            },
        )
    })
}

pub(super) fn builtin_by_short_name(name: &str) -> Vec<&'static BuiltinSymbol> {
    builtin_index()
        .values()
        .flatten()
        .filter(|symbol| symbol.name == name)
        .collect()
}

pub(super) fn builtin_source_path(file: &str) -> PathBuf {
    if let Some(path) = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .map(|parent| parent.join("definitions").join(file))
    }) && path.is_file()
    {
        return path;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("definitions")
        .join(file)
}
