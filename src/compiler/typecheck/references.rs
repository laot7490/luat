use super::*;

pub(super) struct TypeReferenceValidator<'a> {
    pub(super) source: &'a str,
    pub(super) prepared: &'a PreparedSource,
    pub(super) alias_scopes: Vec<HashMap<String, TypeAlias>>,
    pub(super) generic_scopes: Vec<HashSet<String>>,
    pub(super) cycles: HashSet<String>,
    pub(super) deep_aliases: HashSet<String>,
    pub(super) reported_aliases: HashSet<String>,
    pub(super) diagnostics: Vec<Diagnostic>,
}

impl TypeReferenceValidator<'_> {
    fn push_generics(&mut self, generics: Option<&GenericDeclaration>) {
        self.generic_scopes.push(
            generics
                .into_iter()
                .flat_map(|declaration| declaration.generics().iter())
                .filter_map(|parameter| parameter.parameter().tokens().next())
                .map(|token| token.token().to_string())
                .collect(),
        );
    }
}

impl Visitor for TypeReferenceValidator<'_> {
    fn visit_block(&mut self, node: &ast::Block) {
        let mut collector = AliasCollector::default();
        for statement in node.stmts() {
            match statement {
                ast::Stmt::TypeDeclaration(declaration) => {
                    collector.visit_type_declaration(declaration)
                }
                ast::Stmt::ExportedTypeDeclaration(declaration) => {
                    collector.visit_type_declaration(declaration.type_declaration())
                }
                _ => {}
            }
        }
        self.alias_scopes.push(collector.aliases);
    }

    fn visit_block_end(&mut self, _: &ast::Block) {
        self.alias_scopes.pop();
    }

    fn visit_function_body(&mut self, node: &ast::FunctionBody) {
        self.push_generics(node.generics());
    }

    fn visit_function_body_end(&mut self, _: &ast::FunctionBody) {
        self.generic_scopes.pop();
    }

    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        self.push_generics(node.generics());
        let name = node.type_name().token().to_string();
        let code = if self.cycles.contains(&name) {
            Some(format!(
                "Type alias '{name}' has an unguarded recursive cycle."
            ))
        } else if self.deep_aliases.contains(&name) {
            Some(format!(
                "Type alias '{name}' expands through more than 128 aliases."
            ))
        } else {
            None
        };
        if let Some(message) = code
            && self.reported_aliases.insert(name)
        {
            let (Some(start), Some(end)) = (
                node.type_name().start_position(),
                node.type_name().end_position(),
            ) else {
                return;
            };
            self.diagnostics.push(Diagnostic::range(
                self.source,
                self.prepared.original_offset(start.bytes()),
                self.prepared.original_offset(end.bytes()),
                "LT2006",
                message,
            ));
        }
    }

    fn visit_type_declaration_end(&mut self, _: &TypeDeclaration) {
        self.generic_scopes.pop();
    }

    fn visit_type_info_end(&mut self, node: &TypeInfo) {
        if matches!(node, TypeInfo::Callback { .. }) {
            self.generic_scopes.pop();
        }
    }

    fn visit_type_info(&mut self, node: &TypeInfo) {
        let (token, provided) = match node {
            TypeInfo::Callback { generics, .. } => {
                self.push_generics(generics.as_ref());
                return;
            }
            TypeInfo::Basic(name) => (name, 0),
            TypeInfo::Generic { base, generics, .. } => (base, generics.iter().count()),
            _ => return,
        };
        let name = token.token().to_string();
        if self
            .generic_scopes
            .iter()
            .rev()
            .any(|scope| scope.contains(&name))
        {
            return;
        }
        let Some(alias) = self
            .alias_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name))
        else {
            if !matches!(
                name.as_str(),
                "any"
                    | "unknown"
                    | "never"
                    | "nil"
                    | "boolean"
                    | "number"
                    | "integer"
                    | "string"
                    | "table"
                    | "function"
                    | "thread"
                    | "userdata"
                    | "buffer"
                    | "quaternion"
            ) {
                self.diagnostics.push(Diagnostic::range(
                    self.source,
                    self.prepared
                        .original_offset(token.token().start_position().bytes()),
                    self.prepared
                        .original_offset(token.token().end_position().bytes()),
                    "LT2006",
                    format!("Unknown type '{name}'."),
                ));
            }
            return;
        };
        let expansion_error = if self.cycles.contains(&name) {
            Some(format!(
                "Type alias '{name}' has an unguarded recursive cycle."
            ))
        } else if self.deep_aliases.contains(&name) {
            Some(format!(
                "Type alias '{name}' expands through more than 128 aliases."
            ))
        } else {
            None
        };
        if let Some(message) = expansion_error
            && self.reported_aliases.insert(name.clone())
        {
            let Some((start, end)) =
                node.tokens()
                    .fold(None, |range: Option<(usize, usize)>, token| {
                        let start = token.token().start_position().bytes();
                        let end = token.token().end_position().bytes();
                        Some(range.map_or((start, end), |(first, last)| {
                            (first.min(start), last.max(end))
                        }))
                    })
            else {
                return;
            };
            self.diagnostics.push(Diagnostic::range(
                self.source,
                self.prepared.original_offset(start),
                self.prepared.original_offset(end),
                "LT2006",
                message,
            ));
            return;
        }
        if alias.parameters.len() == provided {
            return;
        }
        let Some((start, end)) =
            node.tokens()
                .fold(None, |range: Option<(usize, usize)>, token| {
                    let start = token.token().start_position().bytes();
                    let end = token.token().end_position().bytes();
                    Some(range.map_or((start, end), |(first, last)| {
                        (first.min(start), last.max(end))
                    }))
                })
        else {
            return;
        };
        let expected = alias.parameters.len();
        self.diagnostics.push(Diagnostic::range(
            self.source,
            self.prepared.original_offset(start),
            self.prepared.original_offset(end),
            "LT2006",
            format!(
                "Type alias '{name}' expects {expected} type argument{}, but got {provided}.",
                if expected == 1 { "" } else { "s" }
            ),
        ));
    }
}
