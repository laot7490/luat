use super::*;

pub(super) struct ConstChecker<'a> {
    pub(super) source: &'a str,
    pub(super) prepared: &'a PreparedSource,
    pub(super) scopes: Vec<HashMap<String, bool>>,
    pub(super) pending_bindings: Vec<HashMap<String, bool>>,
    pub(super) diagnostics: Vec<Diagnostic>,
}

impl ConstChecker<'_> {
    fn assigned(&mut self, name: &str, offset: usize) {
        if self.scopes.iter().rev().find_map(|scope| scope.get(name)) == Some(&true) {
            self.diagnostics.push(Diagnostic::at(
                self.source,
                self.prepared.original_offset(offset),
                name.chars().count(),
                "LT2007",
                format!("Cannot assign to const variable '{name}'."),
            ));
        }
    }
}

impl Visitor for ConstChecker<'_> {
    fn visit_block(&mut self, _: &ast::Block) {
        self.scopes.push(HashMap::new());
    }
    fn visit_block_end(&mut self, _: &ast::Block) {
        self.scopes.pop();
    }

    fn visit_function_body(&mut self, node: &ast::FunctionBody) {
        let mut scope = HashMap::new();
        for parameter in node.parameters() {
            match parameter {
                ast::Parameter::Name(name) => {
                    scope.insert(name.token().to_string(), false);
                }
                ast::Parameter::Ellipsis(_) => {
                    scope.insert("...".to_owned(), false);
                }
                _ => {}
            }
        }
        self.scopes.push(scope);
    }

    fn visit_function_body_end(&mut self, _: &ast::FunctionBody) {
        self.scopes.pop();
    }

    fn visit_numeric_for(&mut self, node: &ast::NumericFor) {
        self.scopes.push(HashMap::from([(
            node.index_variable().token().to_string(),
            false,
        )]));
    }

    fn visit_numeric_for_end(&mut self, _: &ast::NumericFor) {
        self.scopes.pop();
    }

    fn visit_generic_for(&mut self, node: &ast::GenericFor) {
        self.scopes.push(
            node.names()
                .iter()
                .map(|name| (name.token().to_string(), false))
                .collect(),
        );
    }

    fn visit_generic_for_end(&mut self, _: &ast::GenericFor) {
        self.scopes.pop();
    }

    fn visit_const_assignment(&mut self, node: &ast::luau::ConstAssignment) {
        self.pending_bindings.push(
            node.names()
                .iter()
                .map(|name| (name.token().to_string(), true))
                .collect(),
        );
    }

    fn visit_const_assignment_end(&mut self, _: &ast::luau::ConstAssignment) {
        if let (Some(scope), Some(bindings)) = (self.scopes.last_mut(), self.pending_bindings.pop())
        {
            scope.extend(bindings);
        }
    }

    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        self.pending_bindings.push(
            node.names()
                .iter()
                .map(|name| (name.token().to_string(), false))
                .collect(),
        );
    }

    fn visit_local_assignment_end(&mut self, _: &ast::LocalAssignment) {
        if let (Some(scope), Some(bindings)) = (self.scopes.last_mut(), self.pending_bindings.pop())
        {
            scope.extend(bindings);
        }
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        for target in node.variables() {
            if let ast::Var::Name(token) = target
                && let TokenType::Identifier { identifier } = token.token().token_type()
            {
                self.assigned(identifier, token.token().start_position().bytes());
            }
        }
    }

    fn visit_compound_assignment(&mut self, node: &ast::CompoundAssignment) {
        if let ast::Var::Name(token) = node.lhs()
            && let TokenType::Identifier { identifier } = token.token().token_type()
        {
            self.assigned(identifier, token.token().start_position().bytes());
        }
    }
}
