use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SymbolKind {
    Local,
    Parameter,
    TypeAlias,
    Function,
}

#[derive(Clone)]
pub(super) struct Symbol {
    pub(super) id: usize,
    pub(super) name: String,
    pub(super) kind: SymbolKind,
    pub(super) range: std::ops::Range<usize>,
    pub(super) visible: std::ops::Range<usize>,
    pub(super) scope: std::ops::Range<usize>,
    pub(super) scope_depth: usize,
    pub(super) type_name: String,
    pub(super) signature: Option<SignatureInformation>,
    pub(super) readonly: bool,
}

#[derive(Clone)]
pub(super) struct Occurrence {
    pub(super) symbol: usize,
    pub(super) range: std::ops::Range<usize>,
    pub(super) declaration: bool,
}

#[derive(Clone)]
pub(super) struct AnalyzedFile {
    pub(super) parsed: Arc<ParsedSource>,
    pub(super) tokens: Arc<Vec<Token>>,
    pub(super) identifiers: Arc<Vec<IdentifierToken>>,
    pub(super) symbols_by_name: Arc<HashMap<String, Vec<usize>>>,
    pub(super) standalone_diagnostics: OnceLock<Vec<Diagnostic>>,
    pub(super) base_diagnostics: Vec<Diagnostic>,
    pub(super) diagnostics: Vec<Diagnostic>,
    pub(super) symbols: Vec<Symbol>,
    pub(super) occurrences: Vec<Occurrence>,
    pub(super) syntax_valid: bool,
    pub(super) semantic_roles: Vec<(std::ops::Range<usize>, u32, u32)>,
    pub(super) editor: editor::FileAssistance,
}

impl std::ops::Deref for AnalyzedFile {
    type Target = ParsedSource;
    fn deref(&self) -> &Self::Target {
        &self.parsed
    }
}

impl AnalyzedFile {
    pub(super) fn new(source: &str, diagnostics: Vec<Diagnostic>) -> Self {
        let prepared = syntax::prepare(source);
        Self::from_prepared(source, prepared, diagnostics)
    }

    pub(super) fn from_prepared(
        source: &str,
        prepared: syntax::PreparedSource,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self::from_parsed(
            Arc::new(ParsedSource::new_prepared(source, prepared)),
            diagnostics,
        )
    }

    pub(super) fn from_ast(
        source: &str,
        prepared: syntax::PreparedSource,
        ast: &ast::Ast,
        diagnostics: Vec<Diagnostic>,
        syntax_valid: bool,
    ) -> Self {
        Self::from_parsed(
            Arc::new(ParsedSource {
                source: source.to_owned(),
                prepared,
                ast: ast.clone(),
                diagnostics: diagnostics.clone(),
                syntax_valid,
                imports: Default::default(),
            }),
            diagnostics,
        )
    }

    pub(super) fn from_parsed(parsed: Arc<ParsedSource>, mut diagnostics: Vec<Diagnostic>) -> Self {
        for diagnostic in &parsed.diagnostics {
            if !diagnostics.contains(diagnostic) {
                diagnostics.push(diagnostic.clone());
            }
        }
        let source = &parsed.source;
        let prepared = &parsed.prepared;
        let ast = &parsed.ast;
        let mut collector = SemanticCollector::new(source, prepared);
        collector.visit_ast(ast);
        let semantic_roles = collector.tokens.clone();
        let (mut symbols, mut occurrences) = collector.finish(source);
        if !parsed.syntax_valid {
            recover_partial_locals(source, prepared, &mut symbols, &mut occurrences);
        }
        occurrences.sort_by_key(|occurrence| occurrence.range.start);
        let mut symbols_by_name = HashMap::<String, Vec<usize>>::new();
        for symbol in &symbols {
            symbols_by_name
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.id);
        }
        let editor = editor::FileAssistance::collect(source, prepared, ast);
        let tokens = Arc::new(lex(&prepared.text));
        let identifiers = Arc::new(scan_identifiers(source));
        Self {
            syntax_valid: parsed.syntax_valid,
            parsed,
            tokens,
            identifiers,
            symbols_by_name: Arc::new(symbols_by_name),
            standalone_diagnostics: OnceLock::new(),
            base_diagnostics: diagnostics.clone(),
            diagnostics,
            symbols,
            occurrences,
            semantic_roles,
            editor,
        }
    }

    pub(super) fn occurrence_at(&self, byte_offset: usize) -> Option<&Occurrence> {
        let offset = clamp_offset(&self.source, byte_offset);
        let index = self
            .occurrences
            .partition_point(|occurrence| occurrence.range.end < offset);
        self.occurrences.get(index).filter(|occurrence| {
            occurrence.range.contains(&offset)
                || (occurrence.range.end == offset && occurrence.range.start < offset)
        })
    }

    pub(super) fn identifier_range_at(&self, offset: usize) -> Option<std::ops::Range<usize>> {
        let index = self
            .identifiers
            .partition_point(|token| token.range.end < offset);
        self.identifiers
            .get(index)
            .filter(|token| token.range.start <= offset)
            .map(|token| token.range.clone())
    }

    pub(super) fn member_completions(&self, offset: usize) -> Option<Vec<CompletionItem>> {
        let receiver_range = receiver_before_dot(&self.source, offset)?;
        if self.source[..receiver_range.start]
            .trim_end()
            .ends_with('.')
        {
            return Some(Vec::new());
        }
        let receiver = &self.source[receiver_range];
        let prefix = completion_prefix(&self.source, offset);
        let symbol = self.visible_symbol(receiver, offset);
        if let Some(symbol) = symbol {
            let mut type_name = symbol.type_name.as_str();
            if let Some(alias) = self.symbols.iter().find(|candidate| {
                candidate.kind == SymbolKind::TypeAlias && candidate.name == type_name
            }) {
                type_name = &alias.type_name;
            }
            if let Some(body) = type_name
                .trim()
                .strip_prefix('{')
                .and_then(|ty| ty.strip_suffix('}'))
            {
                return Some(
                    split_type_fields(body)
                        .into_iter()
                        .filter_map(|field| {
                            let (name, ty) = field.split_once(':')?;
                            let name = name.trim();
                            (!name.is_empty() && !name.starts_with('[')).then(|| CompletionItem {
                                label: name.to_owned(),
                                detail: ty.trim().to_owned(),
                                kind: if ty.trim_start().starts_with('(') && ty.contains("->") {
                                    CompletionKind::Function
                                } else {
                                    CompletionKind::Variable
                                },
                                ..Default::default()
                            })
                        })
                        .filter(|item| item.label.starts_with(prefix))
                        .collect(),
                );
            }
            return Some(Vec::new());
        }
        let mut items = builtin_index()
            .iter()
            .flat_map(|(name, symbols)| symbols.iter().map(move |symbol| (name, symbol)))
            .filter_map(|(name, symbol)| {
                name.strip_prefix(&format!("{receiver}."))
                    .map(|field| (field, symbol))
            })
            .map(|(field, symbol)| CompletionItem {
                label: field.to_owned(),
                detail: format!("{} [{}]", symbol.detail(), symbol.context),
                kind: symbol.kind,
                ..Default::default()
            })
            .filter(|item| !item.label.contains('.') && item.label.starts_with(prefix))
            .map(|item| (item.label.clone(), item))
            .collect::<BTreeMap<_, _>>();
        if let Some(global) = builtin_index()
            .get(receiver)
            .and_then(|symbols| symbols.first())
            && let Some(alias) = builtin_index()
                .get(&global.type_name)
                .and_then(|symbols| symbols.first())
            && let Some(body) = alias
                .type_name
                .strip_prefix('{')
                .and_then(|ty| ty.strip_suffix('}'))
        {
            for field in split_type_fields(body) {
                if let Some((name, ty)) = field.split_once(':') {
                    let name = name.trim();
                    if name.starts_with(prefix) && !name.starts_with('[') {
                        items
                            .entry(name.to_owned())
                            .or_insert_with(|| CompletionItem {
                                label: name.to_owned(),
                                detail: ty.trim().to_owned(),
                                kind: if ty.trim_start().starts_with('(') && ty.contains("->") {
                                    CompletionKind::Function
                                } else {
                                    CompletionKind::Variable
                                },
                                ..Default::default()
                            });
                    }
                }
            }
        }
        Some(items.into_values().collect())
    }

    pub(super) fn visible_symbol(&self, name: &str, offset: usize) -> Option<&Symbol> {
        self.symbols_by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&id| &self.symbols[id])
            .filter(|symbol| {
                symbol.name == name
                    && symbol.visible.start <= offset
                    && symbol.scope.start <= offset
                    && offset <= symbol.scope.end
            })
            .max_by_key(|symbol| (symbol.scope_depth, symbol.visible.start))
    }
}

fn recover_partial_locals(
    source: &str,
    prepared: &syntax::PreparedSource,
    symbols: &mut Vec<Symbol>,
    occurrences: &mut Vec<Occurrence>,
) {
    let mut line_start = 0;
    for line in prepared.text.split_inclusive('\n') {
        let is_declaration =
            line.trim_start().starts_with("local ") || line.trim_start().starts_with("const ");
        if is_declaration
            && !lex(&prepared.text[..line_start]).iter().any(|token| {
                matches!(
                    token.to_string().trim(),
                    "do" | "then" | "repeat" | "function"
                )
            })
        {
            let line_prepared = syntax::prepare(line);
            let parsed =
                full_moon::parse_fallible(&line_prepared.text, LuaVersion::cfxlua().with_luau());
            if parsed.errors().is_empty() {
                let mut collector = SemanticCollector::new(line, &line_prepared);
                collector.visit_ast(&parsed.into_ast());
                let (line_symbols, _) = collector.finish(line);
                for symbol in line_symbols {
                    let start = prepared.original_offset(line_start + symbol.range.start);
                    let end = prepared.original_offset(line_start + symbol.range.end);
                    let visible_start = prepared.original_offset(line_start + symbol.visible.start);
                    if let Some(existing) = symbols
                        .iter_mut()
                        .find(|existing| existing.range.start == start && existing.range.end == end)
                    {
                        existing.visible.start = visible_start;
                        existing.visible.end = source.len();
                        existing.scope = 0..source.len();
                        existing.scope_depth = 0;
                        continue;
                    }
                    let id = symbols.len();
                    symbols.push(Symbol {
                        id,
                        name: symbol.name,
                        kind: symbol.kind,
                        range: start..end,
                        visible: visible_start..source.len(),
                        scope: 0..source.len(),
                        scope_depth: 0,
                        type_name: symbol.type_name,
                        signature: symbol.signature,
                        readonly: symbol.readonly,
                    });
                    occurrences.push(Occurrence {
                        symbol: id,
                        range: start..end,
                        declaration: true,
                    });
                }
            }
        }
        line_start += line.len();
    }
}

struct Scope {
    range: std::ops::Range<usize>,
    depth: usize,
}

struct PendingSymbol {
    name: String,
    kind: SymbolKind,
    range: std::ops::Range<usize>,
    visible_start: usize,
    scope_id: usize,
    type_name: String,
    signature: Option<SignatureInformation>,
    readonly: bool,
}

pub(super) struct SemanticCollector<'a> {
    source: &'a str,
    prepared: &'a syntax::PreparedSource,
    scopes: Vec<Scope>,
    scope_stack: Vec<usize>,
    pending_parameters: Vec<Vec<(String, std::ops::Range<usize>, String)>>,
    symbols: Vec<PendingSymbol>,
    tokens: Vec<(std::ops::Range<usize>, u32, u32)>,
}

impl<'a> SemanticCollector<'a> {
    pub(super) fn new(source: &'a str, prepared: &'a syntax::PreparedSource) -> Self {
        Self {
            source,
            prepared,
            scopes: vec![Scope {
                range: 0..source.len(),
                depth: 0,
            }],
            scope_stack: vec![0],
            pending_parameters: Vec::new(),
            symbols: Vec::new(),
            tokens: Vec::new(),
        }
    }

    fn add_symbol(&mut self, symbol: PendingSymbol) {
        self.symbols.push(symbol);
    }

    fn offset(&self, prepared: usize) -> usize {
        self.prepared.original_offset(prepared)
    }

    fn identifier_range(&self, token: &Token) -> Option<std::ops::Range<usize>> {
        let start = token.start_position().bytes();
        let end = token.end_position().bytes();
        Some(self.offset(start)..self.offset(end))
    }

    fn parameters(
        &self,
        body: &ast::FunctionBody,
    ) -> Vec<(String, std::ops::Range<usize>, String)> {
        body.parameters()
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| {
                let ast::Parameter::Name(name) = parameter else {
                    return None;
                };
                let token = name.token();
                let range = token.start_position().bytes()..token.end_position().bytes();
                let range = self.offset(range.start)..self.offset(range.end);
                let type_name = body.type_specifiers().nth(index).flatten().map_or_else(
                    || "any".to_owned(),
                    |specifier| specifier.type_info().to_string().trim().to_owned(),
                );
                Some((token.to_string(), range, type_name))
            })
            .collect()
    }

    pub(super) fn signature(&self, body: &ast::FunctionBody, name: &str) -> SignatureInformation {
        let parameters = function_parameters(body);
        let offset = body
            .start_position()
            .map(|position| self.offset(position.bytes()))
            .unwrap_or_default();
        SignatureInformation {
            label: format!(
                "function {name}({}){}",
                parameters.join(", "),
                body.return_type()
                    .map_or_else(String::new, |specifier| format!(
                        ": {}",
                        specifier.type_info().to_string().trim()
                    ))
            ),
            documentation: nonempty(Documentation::before(self.source, offset).render()),
            parameter_documentation: parameter_documentation(self.source, offset, &parameters),
            parameters,
        }
    }

    pub(super) fn finish(self, source: &str) -> (Vec<Symbol>, Vec<Occurrence>) {
        let symbols = self
            .symbols
            .into_iter()
            .enumerate()
            .map(|(id, pending)| {
                let scope = &self.scopes[pending.scope_id];
                Symbol {
                    id,
                    name: pending.name,
                    kind: pending.kind,
                    range: pending.range,
                    visible: pending.visible_start..scope.range.end,
                    scope: scope.range.clone(),
                    scope_depth: scope.depth,
                    type_name: pending.type_name,
                    signature: pending.signature,
                    readonly: pending.readonly,
                }
            })
            .collect::<Vec<_>>();
        let prepared = self.prepared;
        let declarations = symbols
            .iter()
            .map(|symbol| ((symbol.range.start, symbol.range.end), symbol.id))
            .collect::<HashMap<_, _>>();
        let mut names = HashMap::<&str, Vec<&Symbol>>::new();
        for symbol in &symbols {
            names.entry(&symbol.name).or_default().push(symbol);
        }
        let mut occurrences = Vec::new();
        for token in scan_identifiers(&prepared.text) {
            let start = prepared.original_offset(token.range.start);
            let end = prepared.original_offset(token.range.end);
            let name = token.name;
            let declaration = declarations.get(&(start, end)).copied();
            if let Some(symbol) = declaration {
                occurrences.push(Occurrence {
                    symbol,
                    range: start..end,
                    declaration: true,
                });
                continue;
            }
            let type_position = is_type_reference_position(source, start);
            if is_field_or_type_position(source, start, end) && !type_position {
                continue;
            }
            if type_position
                && !names
                    .get(name.as_str())
                    .into_iter()
                    .flatten()
                    .any(|symbol| {
                        symbol.kind == SymbolKind::TypeAlias
                            && symbol.name == name
                            && symbol.visible.start <= start
                            && symbol.scope.contains(&start)
                    })
            {
                continue;
            }
            let resolved = names
                .get(name.as_str())
                .into_iter()
                .flatten()
                .filter(|symbol| {
                    symbol.name == name
                        && if type_position {
                            symbol.kind == SymbolKind::TypeAlias
                        } else {
                            symbol.kind != SymbolKind::TypeAlias
                        }
                        && symbol.visible.start <= start
                        && symbol.scope.contains(&start)
                })
                .max_by_key(|symbol| (symbol.scope_depth, symbol.visible.start));
            if let Some(symbol) = resolved {
                occurrences.push(Occurrence {
                    symbol: symbol.id,
                    range: start..end,
                    declaration: false,
                });
            }
        }
        (symbols, occurrences)
    }
}

impl Visitor for SemanticCollector<'_> {
    fn visit_function_body(&mut self, body: &ast::FunctionBody) {
        self.pending_parameters.push(self.parameters(body));
    }
    fn visit_type_info(&mut self, info: &ast::luau::TypeInfo) {
        if let ast::luau::TypeInfo::Basic(name) = info
            && let Some(range) = self.identifier_range(name.token())
        {
            let builtin = matches!(
                name.token().to_string().as_str(),
                "any"
                    | "boolean"
                    | "integer"
                    | "never"
                    | "number"
                    | "string"
                    | "table"
                    | "thread"
                    | "unknown"
                    | "userdata"
                    | "vector2"
                    | "vector3"
                    | "vector4"
            );
            self.tokens.push((range, 2, if builtin { 4 } else { 0 }));
        }
    }
    fn visit_function_call(&mut self, call: &ast::FunctionCall) {
        let mut target = match call.prefix() {
            ast::Prefix::Name(name) => Some(name),
            _ => None,
        };
        for suffix in call.suffixes() {
            match suffix {
                ast::Suffix::Index(ast::Index::Dot { name, .. }) => target = Some(name),
                ast::Suffix::Call(ast::Call::MethodCall(method)) => {
                    if let Some(range) = self.identifier_range(method.name().token()) {
                        self.tokens.push((range, 3, 0));
                    }
                    target = None;
                }
                ast::Suffix::Call(_) => {
                    if let Some(name) = target.take()
                        && let Some(range) = self.identifier_range(name.token())
                    {
                        self.tokens.push((range, 3, 0));
                    }
                }
                _ => target = None,
            }
        }
    }

    fn visit_block(&mut self, node: &ast::Block) {
        let start = node
            .start_position()
            .map_or(0, |position| self.offset(position.bytes()));
        let end = node
            .end_position()
            .map_or(self.source.len(), |position| self.offset(position.bytes()));
        let id = self.scopes.len();
        let depth = self.scope_stack.len();
        self.scopes.push(Scope {
            range: start..end.max(start),
            depth,
        });
        self.scope_stack.push(id);
        if let Some(parameters) = self.pending_parameters.pop() {
            for (name, range, type_name) in parameters {
                self.add_symbol(PendingSymbol {
                    name,
                    kind: SymbolKind::Parameter,
                    range,
                    visible_start: start,
                    scope_id: id,
                    type_name,
                    signature: None,
                    readonly: false,
                });
            }
        }
    }

    fn visit_block_end(&mut self, _: &ast::Block) {
        if self.scope_stack.len() > 1 {
            self.scope_stack.pop();
        }
    }

    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        let visible_start = node.end_position().map_or_else(
            || node.start_position().map_or(0, |position| position.bytes()),
            |position| position.bytes(),
        );
        for (index, name) in node.names().iter().enumerate() {
            let Some(range) = self.identifier_range(name.token()) else {
                continue;
            };
            let type_name = node.type_specifiers().nth(index).flatten().map_or_else(
                || {
                    node.expressions()
                        .iter()
                        .nth(index)
                        .map_or_else(|| "any".to_owned(), infer_literal)
                },
                |specifier| specifier.type_info().to_string().trim().to_owned(),
            );
            let signature =
                node.expressions()
                    .iter()
                    .nth(index)
                    .and_then(|expression| match expression {
                        ast::Expression::Function(function) => {
                            Some(self.signature(function.body(), &name.token().to_string()))
                        }
                        _ => None,
                    });
            self.add_symbol(PendingSymbol {
                name: name.token().to_string(),
                kind: SymbolKind::Local,
                range,
                visible_start: self.offset(visible_start),
                scope_id: *self.scope_stack.last().unwrap_or(&0),
                type_name,
                signature,
                readonly: false,
            });
        }
    }

    fn visit_const_assignment(&mut self, node: &ast::luau::ConstAssignment) {
        let visible_start = node.end_position().map_or_else(
            || node.start_position().map_or(0, |position| position.bytes()),
            |position| position.bytes(),
        );
        for (index, name) in node.names().iter().enumerate() {
            let Some(range) = self.identifier_range(name.token()) else {
                continue;
            };
            let type_name = node.type_specifiers().nth(index).flatten().map_or_else(
                || {
                    node.expressions()
                        .iter()
                        .nth(index)
                        .map_or_else(|| "any".to_owned(), infer_literal)
                },
                |specifier| specifier.type_info().to_string().trim().to_owned(),
            );
            self.add_symbol(PendingSymbol {
                name: name.token().to_string(),
                kind: SymbolKind::Local,
                range,
                visible_start: self.offset(visible_start),
                scope_id: *self.scope_stack.last().unwrap_or(&0),
                type_name,
                signature: None,
                readonly: true,
            });
        }
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        let name = node.name().token().to_string();
        let Some(range) = self.identifier_range(node.name().token()) else {
            return;
        };
        let visible_start = node.start_position().map_or(0, |position| position.bytes());
        let signature = self.signature(node.body(), &name);
        self.add_symbol(PendingSymbol {
            name,
            kind: SymbolKind::Function,
            range,
            visible_start: self.offset(visible_start),
            scope_id: *self.scope_stack.last().unwrap_or(&0),
            type_name: "function".to_owned(),
            signature: Some(signature),
            readonly: false,
        });
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        if node.name().names().len() == 1 && node.name().method_name().is_none() {
            let token = node.name().names().iter().next().unwrap();
            let name = token.token().to_string();
            if let Some(range) = self.identifier_range(token.token()) {
                self.add_symbol(PendingSymbol {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    range,
                    visible_start: 0,
                    scope_id: 0,
                    type_name: "function".to_owned(),
                    signature: Some(self.signature(node.body(), &name)),
                    readonly: false,
                });
            }
        }
    }

    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        let token = node.type_name();
        let Some(range) = self.identifier_range(token.token()) else {
            return;
        };
        let Some(visible_start) = token.end_position().map(|position| position.bytes()) else {
            return;
        };
        self.add_symbol(PendingSymbol {
            name: token.token().to_string(),
            kind: SymbolKind::TypeAlias,
            range,
            visible_start: self.offset(visible_start),
            scope_id: if self.scope_stack.len() == 2 {
                0
            } else {
                *self.scope_stack.last().unwrap_or(&0)
            },
            type_name: node.type_definition().to_string().trim().to_owned(),
            signature: None,
            readonly: false,
        });
    }
}

fn function_parameters(body: &ast::FunctionBody) -> Vec<String> {
    body.parameters()
        .iter()
        .zip(body.type_specifiers().chain(std::iter::repeat(None)))
        .filter_map(|(parameter, specifier)| {
            let ty = specifier.map(|specifier| specifier.type_info().to_string().trim().to_owned());
            match parameter {
                ast::Parameter::Name(token) => Some(format!(
                    "{}: {}",
                    token.token(),
                    ty.as_deref().unwrap_or("any")
                )),
                ast::Parameter::Ellipsis(_) => {
                    Some(ty.map_or_else(|| "...".to_owned(), |ty| format!("...: {ty}")))
                }
                _ => None,
            }
        })
        .collect()
}

fn infer_literal(expression: &ast::Expression) -> String {
    match expression {
        ast::Expression::TableConstructor(table) => {
            let fields = table
                .fields()
                .iter()
                .filter_map(|field| {
                    let ast::Field::NameKey { key, value, .. } = field else {
                        return None;
                    };
                    Some(format!("{}: {}", key.token(), infer_literal(value)))
                })
                .collect::<Vec<_>>();
            return if fields.is_empty() {
                "table".to_owned()
            } else {
                format!("{{ {} }}", fields.join(", "))
            };
        }
        ast::Expression::Function(function) => {
            let body = function.body();
            let returns = body.return_type().map_or_else(
                || "any".to_owned(),
                |specifier| specifier.type_info().to_string().trim().to_owned(),
            );
            return format!("({}) -> {returns}", function_parameters(body).join(", "));
        }
        _ => {}
    }
    let value = expression.to_string();
    let value = value.trim();
    if value.starts_with(['\'', '"']) {
        "string".to_owned()
    } else if value.parse::<f64>().is_ok() {
        "number".to_owned()
    } else if matches!(value, "true" | "false") {
        "boolean".to_owned()
    } else if value.starts_with('{') {
        "table".to_owned()
    } else if value.starts_with("function") {
        "function".to_owned()
    } else {
        "any".to_owned()
    }
}
