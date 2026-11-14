use super::*;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorSymbolKind {
    Variable,
    Function,
    Type,
}

#[derive(Clone, Debug)]
pub struct EditorSymbol {
    pub name: String,
    pub range: Range<usize>,
    pub selection: Range<usize>,
    pub kind: EditorSymbolKind,
    pub children: Vec<EditorSymbol>,
    pub(super) local: bool,
}

#[derive(Clone, Debug)]
pub struct EditorDiagnostic {
    pub range: Range<usize>,
    pub code: &'static str,
    pub message: String,
    pub unnecessary: bool,
    pub deprecated: bool,
}

#[derive(Clone, Debug)]
pub struct EditorInlayHint {
    pub offset: usize,
    pub label: String,
    pub parameter: bool,
}

#[derive(Clone, Default)]
pub(super) struct FileAssistance {
    pub(super) declarations: Vec<EditorSymbol>,
    writes: Vec<Range<usize>>,
    explicit_types: HashSet<usize>,
    methods: Vec<(String, usize, SignatureInformation)>,
}

impl FileAssistance {
    pub(super) fn collect(source: &str, prepared: &syntax::PreparedSource, ast: &ast::Ast) -> Self {
        let mut collector = AssistanceCollector {
            source,
            prepared,
            result: Self::default(),
        };
        collector.visit_ast(ast);
        collector.result
    }
}

struct AssistanceCollector<'a> {
    source: &'a str,
    prepared: &'a syntax::PreparedSource,
    result: FileAssistance,
}

impl AssistanceCollector<'_> {
    fn range(&self, node: &impl Node) -> Range<usize> {
        self.prepared
            .original_offset(node.start_position().map_or(0, |p| p.bytes()))
            ..self
                .prepared
                .original_offset(node.end_position().map_or(0, |p| p.bytes()))
    }

    fn add(
        &mut self,
        name: String,
        selection: Range<usize>,
        range: Range<usize>,
        kind: EditorSymbolKind,
        local: bool,
    ) {
        let range = range.start.min(selection.start)..range.end.max(selection.end);
        self.result.declarations.push(EditorSymbol {
            name,
            selection,
            range,
            kind,
            children: Vec::new(),
            local,
        });
    }
}

impl Visitor for AssistanceCollector<'_> {
    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        for (index, name) in node.names().iter().enumerate() {
            let selection = self.range(name);
            if node.type_specifiers().nth(index).flatten().is_some() {
                self.result.explicit_types.insert(selection.start);
            }
            let kind = if node
                .expressions()
                .iter()
                .nth(index)
                .is_some_and(|e| matches!(e, ast::Expression::Function(_)))
            {
                EditorSymbolKind::Function
            } else {
                EditorSymbolKind::Variable
            };
            self.add(
                name.token().to_string(),
                selection,
                self.range(node),
                kind,
                true,
            );
        }
    }

    fn visit_const_assignment(&mut self, node: &ast::luau::ConstAssignment) {
        for (index, name) in node.names().iter().enumerate() {
            let selection = self.range(name);
            if node.type_specifiers().nth(index).flatten().is_some() {
                self.result.explicit_types.insert(selection.start);
            }
            self.add(
                name.token().to_string(),
                selection,
                self.range(node),
                EditorSymbolKind::Variable,
                true,
            );
        }
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        self.add(
            node.name().token().to_string(),
            self.range(node.name()),
            self.range(node),
            EditorSymbolKind::Function,
            true,
        );
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        let name = node.name().to_string().trim().to_owned();
        let signature =
            SemanticCollector::new(self.source, self.prepared).signature(node.body(), &name);
        self.result
            .methods
            .push((name.replace(':', "."), self.range(node).start, signature));
        let selection = node
            .name()
            .method_name()
            .map(|name| self.range(name))
            .unwrap_or_else(|| self.range(node.name()));
        self.add(
            node.name().to_string().trim().to_owned(),
            selection,
            self.range(node),
            EditorSymbolKind::Function,
            false,
        );
    }

    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        self.add(
            node.type_name().token().to_string(),
            self.range(node.type_name()),
            self.range(node),
            EditorSymbolKind::Type,
            true,
        );
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        for target in node.variables() {
            if let ast::Var::Name(name) = target {
                let range = self.range(name);
                self.result.writes.push(range.clone());
                self.add(
                    name.token().to_string(),
                    range,
                    self.range(node),
                    EditorSymbolKind::Variable,
                    false,
                );
            }
        }
    }
}

impl AnalysisSnapshot {
    pub(super) fn member_signature(
        &self,
        path: &Path,
        offset: usize,
    ) -> Option<SignatureInformation> {
        let file = self.files.get(path)?;
        let range = identifier_range_at(&file.source, offset)?;
        let before = file.source[..range.start].trim_end();
        let separator = before.chars().last()?;
        if !matches!(separator, '.' | ':') {
            return None;
        }
        let receiver_end = before.len() - 1;
        let receiver_start = file.source[..receiver_end]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .map_or(0, |i| i + 1);
        let receiver = &file.source[receiver_start..receiver_end];
        let name = &file.source[range.clone()];
        let qualified = format!("{receiver}.{name}");
        if let Some((_, _, signature)) =
            file.editor
                .methods
                .iter()
                .rev()
                .find(|(name, declaration, _)| {
                    name == &qualified && *declaration < offset && {
                        let root = receiver.split('.').next().unwrap_or(receiver);
                        file.visible_symbol(root, *declaration).map(|s| s.id)
                            == file.visible_symbol(root, offset).map(|s| s.id)
                    }
                })
        {
            let mut signature = signature.clone();
            if separator == '.'
                && signature
                    .label
                    .starts_with(&format!("function {receiver}:"))
            {
                signature.parameters.insert(0, "self: table".to_owned());
                signature.parameter_documentation.insert(0, None);
                let returns = signature
                    .label
                    .rsplit_once("): ")
                    .map(|(_, returns)| format!(": {returns}"))
                    .unwrap_or_default();
                signature.label = format!(
                    "function {receiver}.{name}({}){returns}",
                    signature.parameters.join(", ")
                );
            }
            if separator == ':'
                && signature
                    .parameters
                    .first()
                    .is_some_and(|p| p.split(':').next() == Some("self"))
            {
                signature.parameters.remove(0);
                signature.parameter_documentation.remove(0);
                if let Some((_, returns)) = signature.label.rsplit_once("): ") {
                    signature.label = format!(
                        "function {receiver}:{name}({}): {returns}",
                        signature.parameters.join(", ")
                    );
                } else {
                    signature.label = format!(
                        "function {receiver}:{name}({})",
                        signature.parameters.join(", ")
                    );
                }
            }
            return Some(signature);
        }
        if self.imported_member_at(path, offset).is_some()
            || self.builtin_at(path, offset).is_some()
        {
            return None;
        }
        self.record_member_signature(path, offset, receiver, name, separator == ':')
    }

    pub fn editor_diagnostics(&self, path: &Path) -> Vec<EditorDiagnostic> {
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        if !file.syntax_valid || path.to_string_lossy().ends_with(".d.luat") {
            return Vec::new();
        }
        let mut result = Vec::new();
        for symbol in &file.symbols {
            let local_function = symbol.kind == SymbolKind::Function
                && file
                    .editor
                    .declarations
                    .iter()
                    .any(|d| d.selection == symbol.range && d.local);
            if symbol.name.starts_with('_')
                || !(matches!(symbol.kind, SymbolKind::Parameter | SymbolKind::Local)
                    || local_function)
            {
                continue;
            }
            let read = file.occurrences.iter().any(|occurrence| {
                occurrence.symbol == symbol.id
                    && !occurrence.declaration
                    && !file.editor.writes.contains(&occurrence.range)
            });
            if read {
                continue;
            }
            let code = if symbol.kind == SymbolKind::Parameter {
                "LTH1001"
            } else if local_function || symbol.signature.is_some() {
                "LTH1003"
            } else {
                "LTH1002"
            };
            result.push(EditorDiagnostic {
                range: symbol.range.clone(),
                code,
                message: format!("'{}' is never read.", symbol.name),
                unnecessary: true,
                deprecated: false,
            });
        }
        for token in scan_identifiers(&file.prepared.text) {
            let offset = file.prepared.original_offset(token.range.start);
            if file
                .occurrence_at(offset)
                .is_some_and(|occurrence| occurrence.declaration)
            {
                continue;
            }
            if let Some(message) = self.deprecated_at(path, offset) {
                result.push(EditorDiagnostic {
                    range: offset..file.prepared.original_offset(token.range.end),
                    code: "LTH1004",
                    message: if message.is_empty() {
                        format!("'{}' is deprecated.", token.name)
                    } else {
                        message
                    },
                    unnecessary: false,
                    deprecated: true,
                });
            }
        }
        result
    }

    pub(super) fn deprecated_at(&self, path: &Path, offset: usize) -> Option<String> {
        let file = self.files.get(path)?;
        let raw = if let Some((target, _, member)) = self.imported_member_at(path, offset) {
            documentation_before(&self.files.get(target)?.source, member.range.start)
        } else if let Some(occurrence) = file.occurrence_at(offset) {
            documentation_before(&file.source, file.symbols[occurrence.symbol].range.start)
        } else if let Some(symbol) = self.project_symbol_at(path, offset) {
            symbol.documentation.clone()
        } else {
            self.builtin_at(path, offset)?.1.documentation.clone()
        };
        Documentation::parse(&raw).deprecated
    }

    pub fn unused_fix(&self, path: &Path, offset: usize) -> Option<WorkspaceEdit> {
        let file = self.files.get(path)?;
        let diagnostic = self
            .editor_diagnostics(path)
            .into_iter()
            .find(|d| d.unnecessary && d.range.contains(&offset))?;
        let name = &file.source[diagnostic.range.clone()];
        self.rename(path, diagnostic.range.start, &format!("_{name}"))
            .ok()
    }

    pub fn document_symbols(&self, path: &Path) -> Vec<EditorSymbol> {
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        let mut declarations = file.editor.declarations.clone();
        declarations.retain(|declaration| {
            declaration.local
                || !file.editor.writes.contains(&declaration.selection)
                || file
                    .visible_symbol(&declaration.name, declaration.selection.start)
                    .is_none()
        });
        declarations
            .sort_by_key(|symbol| (symbol.range.start, std::cmp::Reverse(symbol.range.end)));
        declarations.dedup_by(|a, b| a.selection == b.selection);
        fn insert(items: &mut Vec<EditorSymbol>, symbol: EditorSymbol) {
            if let Some(parent) = items.iter_mut().rev().find(|parent| {
                parent.kind == EditorSymbolKind::Function
                    && parent.range.start < symbol.range.start
                    && parent.range.end >= symbol.range.end
            }) {
                insert(&mut parent.children, symbol);
            } else {
                items.push(symbol);
            }
        }
        let mut result = Vec::new();
        for declaration in declarations {
            insert(&mut result, declaration);
        }
        result
    }

    pub fn workspace_symbols(&self, query: &str) -> Vec<(PathBuf, EditorSymbol)> {
        let query = query.to_lowercase();
        let mut result = Vec::new();
        for path in self.files.keys() {
            if path.to_string_lossy().ends_with(".d.luat") {
                continue;
            }
            for symbol in self.document_symbols(path) {
                if symbol.name.to_lowercase().contains(&query) {
                    result.push((path.clone(), symbol));
                }
            }
        }
        result
    }

    pub fn inlay_hints(
        &self,
        path: &Path,
        range: Range<usize>,
        parameter_names: bool,
        variable_types: bool,
    ) -> Vec<EditorInlayHint> {
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        if variable_types && file.syntax_valid {
            for symbol in &file.symbols {
                if symbol.kind == SymbolKind::Local
                    && symbol.signature.is_none()
                    && symbol.type_name != "any"
                    && symbol.type_name != "unknown"
                    && !file.editor.explicit_types.contains(&symbol.range.start)
                    && range.contains(&symbol.range.end)
                {
                    result.push(EditorInlayHint {
                        offset: symbol.range.end,
                        label: format!(": {}", symbol.type_name),
                        parameter: false,
                    });
                }
            }
        }
        if parameter_names {
            let tokens = lex(&file.prepared.text);
            for (open, token) in tokens.iter().enumerate() {
                if token.to_string().trim() != "("
                    || open == 0
                    || !matches!(tokens[open - 1].token_type(), TokenType::Identifier { .. })
                {
                    continue;
                }
                let name_offset = file
                    .prepared
                    .original_offset(tokens[open - 1].start_position().bytes());
                if file
                    .editor
                    .declarations
                    .iter()
                    .any(|d| d.selection.contains(&name_offset))
                {
                    continue;
                }
                let mut nesting = 0usize;
                let mut argument = 0;
                let mut start = true;
                for token in &tokens[open + 1..] {
                    let text = token.to_string();
                    let text = text.trim();
                    if nesting == 0 && text == ")" {
                        break;
                    }
                    if nesting == 0 && text == "," {
                        argument += 1;
                        start = true;
                        continue;
                    }
                    let offset = file
                        .prepared
                        .original_offset(token.start_position().bytes());
                    if start && !text.is_empty() {
                        if range.contains(&offset) {
                            if let Some(help) = self.signature_help(path, offset) {
                                if help.signatures.len() == 1 {
                                    if let Some(parameter) =
                                        help.signatures[0].parameters.get(argument)
                                    {
                                        let name = parameter
                                            .split(':')
                                            .next()
                                            .unwrap_or(parameter)
                                            .trim()
                                            .trim_end_matches('?');
                                        if name != "..."
                                            && name != text
                                            && !name.strip_prefix("arg").is_some_and(|suffix| {
                                                !suffix.is_empty()
                                                    && suffix.chars().all(|c| c.is_ascii_digit())
                                            })
                                        {
                                            result.push(EditorInlayHint {
                                                offset,
                                                label: format!("{name}:"),
                                                parameter: true,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                        start = false;
                    }
                    match text {
                        "(" | "{" | "[" => nesting += 1,
                        ")" | "}" | "]" => nesting = nesting.saturating_sub(1),
                        _ => {}
                    }
                }
            }
        }
        result.sort_by_key(|hint| hint.offset);
        result.dedup_by(|a, b| a.offset == b.offset && a.label == b.label);
        result
    }
}
