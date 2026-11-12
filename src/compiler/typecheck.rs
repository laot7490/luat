mod aliases;
mod assignability;
mod const_check;
#[cfg(test)]
mod tests;
mod values;

use aliases::*;
use assignability::*;
use const_check::ConstChecker;

use values::ValuePack;

use std::collections::{HashMap, HashSet};

use full_moon::{
    LuaVersion,
    ast::{
        self, Expression,
        luau::{GenericDeclaration, TypeDeclaration, TypeInfo},
    },
    node::Node,
    tokenizer::TokenType,
    visitors::Visitor,
};

use crate::compiler::syntax::PreparedSource;
use crate::diagnostic::Diagnostic;

struct AliasCollector {
    aliases: HashMap<String, TypeAlias>,
}

#[derive(Clone)]
struct TypeAlias {
    parameters: Vec<String>,
    definition: String,
}

struct TypeReferenceValidator<'a> {
    source: &'a str,
    prepared: &'a PreparedSource,
    alias_scopes: Vec<HashMap<String, TypeAlias>>,
    generic_scopes: Vec<HashSet<String>>,
    cycles: HashSet<String>,
    deep_aliases: HashSet<String>,
    reported_aliases: HashSet<String>,
    diagnostics: Vec<Diagnostic>,
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
        let mut collector = AliasCollector {
            aliases: HashMap::new(),
        };
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

struct TypeMismatch {
    field_path: Vec<String>,
    range: std::ops::Range<usize>,
    kind: TypeMismatchKind,
}

enum TypeMismatchKind {
    MissingField { expected: String },
    UnexpectedField,
    IncompatibleType { actual: String, expected: String },
    TooDeep,
}

type ActualTableField = (String, String, String, usize, usize, usize);

impl Visitor for AliasCollector {
    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        let name = node.type_name().token().to_string();
        let parameters = node
            .generics()
            .into_iter()
            .flat_map(|generics| generics.generics().iter())
            .map(|parameter| parameter.parameter().to_string().trim().to_owned())
            .collect();
        self.aliases.insert(
            name,
            TypeAlias {
                parameters,
                definition: strip_lua_comments(node.type_definition().to_string().trim()),
            },
        );
    }
}

struct TypeChecker<'a> {
    source: &'a str,
    prepared: &'a PreparedSource,
    aliases: HashMap<String, TypeAlias>,
    alias_scope_changes: Vec<Vec<(String, Option<TypeAlias>)>>,
    functions: HashMap<String, FunctionSignature>,
    scopes: Vec<HashMap<String, String>>,
    block_scopes: Vec<bool>,
    repeat_scope_pending: usize,
    return_types: Vec<Option<String>>,
    function_names: Vec<String>,
    function_scope_changes: Vec<Vec<(String, Option<FunctionSignature>)>>,
    pending_local_bindings: Vec<HashMap<String, String>>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone)]
struct FunctionSignature {
    parameter_names: Vec<String>,
    parameters: Vec<String>,
    returns: ValuePack,
    vararg: Option<String>,
    generic_parameters: Vec<String>,
}

#[derive(Default)]
struct GenericBindings {
    types: HashMap<String, String>,
    conflicts: HashSet<String>,
}

#[derive(Default)]
struct FunctionCollector {
    functions: HashMap<String, FunctionSignature>,
}

#[derive(Default)]
struct InitializerCollector {
    expression: Option<Expression>,
}

impl Visitor for InitializerCollector {
    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        if self.expression.is_none() {
            self.expression = node.expressions().iter().next().cloned();
        }
    }
}

impl Visitor for FunctionCollector {
    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        self.add_ast(node.name().to_string(), node.body());
    }
}

impl FunctionCollector {
    fn add_vector_constructors(&mut self) {
        for (name, dimensions) in [("vector2", 2), ("vector3", 3), ("vector4", 4)] {
            self.functions.insert(
                name.to_owned(),
                FunctionSignature {
                    parameter_names: Vec::new(),
                    parameters: vec!["number".to_owned(); dimensions],
                    returns: ValuePack::single(name.to_owned()),
                    vararg: None,
                    generic_parameters: Vec::new(),
                },
            );
        }
    }

    fn add_ast(&mut self, name: String, body: &ast::FunctionBody) {
        if name.is_empty() || name.contains(['.', ':']) {
            return;
        }
        let mut parameter_names = Vec::new();
        let mut parameter_types = Vec::new();
        let mut vararg = None;
        for (index, parameter) in body.parameters().iter().enumerate() {
            match parameter {
                ast::Parameter::Name(token) => {
                    parameter_names.push(token.token().to_string());
                    parameter_types.push(body.type_specifiers().nth(index).flatten().map_or_else(
                        || "any".to_owned(),
                        |specifier| specifier.type_info().to_string(),
                    ));
                }
                ast::Parameter::Ellipsis(_) => {
                    vararg = body
                        .type_specifiers()
                        .nth(index)
                        .flatten()
                        .map(|specifier| specifier.type_info().to_string());
                }
                _ => {}
            }
        }
        let generic_parameters = body.generics().map_or_else(Vec::new, |generics| {
            let declaration = generics.to_string();
            declaration
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .split(',')
                .map(|value| value.trim().to_owned())
                .collect()
        });
        let returns = ValuePack::from_type(&body.return_type().map_or_else(
            || "any".to_owned(),
            |specifier| specifier.type_info().to_string(),
        ));
        self.functions.insert(
            name,
            FunctionSignature {
                parameter_names,
                parameters: parameter_types,
                returns,
                vararg,
                generic_parameters,
            },
        );
    }
}

impl TypeChecker<'_> {
    fn primitive_kind(&self, type_name: &str) -> Option<&'static str> {
        let (type_name, optional) = resolve_optional_base_type(type_name, &self.aliases);
        if optional {
            return None;
        }
        match type_name.as_str() {
            "number" | "integer" => Some("number"),
            "string" => Some("string"),
            "boolean" | "true" | "false" => Some("boolean"),
            "nil" => Some("nil"),
            value if value.starts_with(['\'', '"']) => Some("string"),
            _ => None,
        }
    }

    fn expression_list_values<'a>(
        &self,
        expressions: &'a [Expression],
        value_count: usize,
    ) -> (Vec<(String, Option<&'a Expression>)>, ValuePack) {
        let mut values = vec![("nil".to_owned(), None); value_count];
        let mut pack = ValuePack::default();
        let mut value_index = 0;
        for (expression_index, expression) in expressions.iter().enumerate() {
            let expression_pack =
                self.expression_value_pack(expression, expression_index + 1 == expressions.len());
            for ty in &expression_pack.fixed {
                if let Some(value) = values.get_mut(value_index) {
                    *value = (ty.clone(), Some(expression));
                }
                value_index += 1;
            }
            pack.fixed.extend(expression_pack.fixed);
            if let Some(variadic) = expression_pack.variadic {
                for value in values.iter_mut().skip(value_index) {
                    *value = (variadic.clone(), Some(expression));
                }
                pack.variadic = Some(variadic);
            }
        }
        (values, pack)
    }

    fn expression_value_pack(&self, expression: &Expression, expands: bool) -> ValuePack {
        match expression {
            Expression::FunctionCall(_) if expands => {
                let type_name = self.infer_ast_expression_type(expression);
                if type_name == "any" {
                    ValuePack {
                        fixed: Vec::new(),
                        variadic: Some("any".to_owned()),
                    }
                } else {
                    ValuePack::from_type(&type_name)
                }
            }
            Expression::Symbol(token) if expands && token.token().to_string() == "..." => {
                ValuePack {
                    fixed: Vec::new(),
                    variadic: Some(self.infer_ast_expression_type(expression)),
                }
            }
            Expression::Parentheses { expression, .. } => {
                let pack = ValuePack::from_type(&self.infer_ast_expression_type(expression));
                ValuePack::single(pack.value_at(0))
            }
            _ => ValuePack::single(self.infer_ast_expression_type(expression)),
        }
    }

    fn record_errors(
        &self,
        actual: &str,
        expected: &str,
        byte: usize,
        code: &'static str,
        context: &str,
    ) -> Vec<Diagnostic> {
        let mismatches =
            match check_table_assignability(actual, expected, &self.aliases, &|value| {
                self.infer_expression_type(value)
            }) {
                Some(Err(mismatches)) => mismatches,
                _ => return Vec::new(),
            };
        let expected_name = expected.split('<').next().unwrap_or(expected).trim();
        mismatches
            .into_iter()
            .map(|mismatch| {
                let path = format_field_path(&mismatch.field_path);
                let (diagnostic_code, message) = match mismatch.kind {
                    TypeMismatchKind::MissingField { expected } => (code, format!(
                        "{context} required field '{path}' of type '{expected}' is missing from type '{expected_name}'."
                    )),
                    TypeMismatchKind::UnexpectedField => (code, format!(
                        "{context} field '{path}' is not allowed in type '{expected_name}'."
                    )),
                    TypeMismatchKind::IncompatibleType { actual, expected } => (code, format!(
                        "{context} Field '{path}': type '{actual}' is not assignable to type '{expected}'."
                    )),
                    TypeMismatchKind::TooDeep => (
                        "LT2006",
                        format!("{context} type comparison exceeded the supported depth of 128."),
                    ),
                };
                Diagnostic::range(
                    self.source,
                    self.prepared.original_offset(byte + mismatch.range.start),
                    self.prepared.original_offset(byte + mismatch.range.end),
                    diagnostic_code,
                    message,
                )
            })
            .collect()
    }

    fn is_known_record(&self, expression: &str) -> bool {
        let ty = self.infer_expression_type(expression);
        resolve_optional_base_type(&ty, &self.aliases)
            .0
            .trim()
            .starts_with('{')
    }

    fn enter_function(&mut self, name: &str) {
        let signature = self.functions.get(name).cloned();
        self.return_types
            .push(signature.as_ref().and_then(|signature| {
                (signature.returns.type_name() != "any").then(|| signature.returns.type_name())
            }));
        self.function_names.push(name.to_owned());
        let mut scope = HashMap::new();
        if let Some(signature) = signature {
            for (name, ty) in signature.parameter_names.iter().zip(&signature.parameters) {
                scope.insert(name.clone(), ty.clone());
            }
            if let Some(vararg_type) = signature.vararg {
                scope.insert("...".to_owned(), vararg_type);
            }
        }
        self.scopes.push(scope);
    }

    fn infer_expression_type(&self, expression: &str) -> String {
        let mut expression = expression.trim();
        while expression.starts_with('(')
            && matching_delimiter(expression, 0, '(', ')') == Some(expression.len() - 1)
        {
            expression = expression[1..expression.len() - 1].trim();
        }
        if expression.starts_with(['\'', '"']) {
            return expression.to_owned();
        }
        if let Some(literal) = infer_literal_type(expression) {
            return literal.to_owned();
        }
        if expression.starts_with("if ") {
            let parsed = full_moon::parse_fallible(
                &format!("local __luat_type = {expression}"),
                full_moon::LuaVersion::cfxlua().with_luau(),
            );
            if parsed.errors().is_empty() {
                let mut initializer = InitializerCollector::default();
                initializer.visit_ast(&parsed.into_ast());
                if let Some(expression) = initializer.expression {
                    return self.infer_ast_expression_type(&expression);
                }
            }
        }
        if expression.starts_with("function") {
            return "function".to_owned();
        }
        if let Some(ty) = self
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(expression))
        {
            return ty.clone();
        }
        if let Some((receiver, key)) = split_index_access(expression) {
            return self
                .infer_index_type(receiver, key)
                .unwrap_or_else(|| "any".to_owned());
        }
        if let Some((receiver, field, optional)) = split_property_access(expression) {
            return self
                .infer_field_type(receiver, field, optional)
                .unwrap_or_else(|| "any".to_owned());
        }
        if let Some(open) = expression.find('(')
            && matching_delimiter(expression, open, '(', ')').is_some()
        {
            let name = expression[..open].trim();
            if let Some(signature) = self.functions.get(name) {
                let args = split_top_level(
                    &expression[open + 1..matching_delimiter(expression, open, '(', ')').unwrap()],
                    ',',
                );
                return infer_generic_return(signature, &args, self);
            }
            return "any".to_owned();
        }
        if expression.starts_with('{') {
            return "table".to_owned();
        }
        if expression.starts_with(['\'', '"']) {
            return "string".to_owned();
        }
        "any".to_owned()
    }

    fn infer_ast_expression_type(&self, expression: &Expression) -> String {
        match expression {
            Expression::IfExpression(value) => {
                let mut types = vec![self.infer_ast_expression_type(value.if_expression())];
                if let Some(else_ifs) = value.else_if_expressions() {
                    types.extend(
                        else_ifs
                            .iter()
                            .map(|branch| self.infer_ast_expression_type(branch.expression())),
                    );
                }
                types.push(self.infer_ast_expression_type(value.else_expression()));
                if types.iter().any(|ty| ty == "any") {
                    return "any".to_owned();
                }
                types.sort();
                types.dedup();
                if types
                    .iter()
                    .all(|ty| matches!(ty.as_str(), "integer" | "number"))
                {
                    return "number".to_owned();
                }
                types.join(" | ")
            }
            Expression::TypeAssertion { type_assertion, .. } => {
                type_assertion.cast_to().to_string()
            }
            Expression::BinaryOperator { binop, lhs, rhs } => {
                let operator = binop.token().to_string();
                if matches!(operator.trim(), "==" | "~=" | "<" | "<=" | ">" | ">=") {
                    return "boolean".to_owned();
                }
                if matches!(operator.trim(), "+" | "-" | "*" | "/" | "//" | "%" | "^") {
                    let left = self.infer_ast_expression_type(lhs);
                    let right = self.infer_ast_expression_type(rhs);
                    if matches!(left.as_str(), "integer" | "number")
                        && matches!(right.as_str(), "integer" | "number")
                    {
                        return "number".to_owned();
                    }
                }
                if operator.trim() == ".." {
                    let left = self.primitive_kind(&self.infer_ast_expression_type(lhs));
                    let right = self.primitive_kind(&self.infer_ast_expression_type(rhs));
                    if matches!(
                        (left, right),
                        (Some("string" | "number"), Some("string" | "number"))
                    ) {
                        return "string".to_owned();
                    }
                    return "any".to_owned();
                }
                if matches!(operator.trim(), "and" | "or") {
                    let left = self.infer_ast_expression_type(lhs);
                    let right = self.infer_ast_expression_type(rhs);
                    if binop.token().start_position().is_some_and(|position| {
                        self.prepared.coalesce_operators.contains(&position.bytes())
                    }) {
                        let (left, optional) = resolve_optional_base_type(&left, &self.aliases);
                        if !optional {
                            return left;
                        }
                        let mut members = split_union(&left)
                            .into_iter()
                            .filter(|member| *member != "nil")
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        members.extend(split_union(&right).into_iter().map(str::to_owned));
                        if members.iter().any(|member| member == "any") {
                            return "any".to_owned();
                        }
                        members.sort();
                        members.dedup();
                        if members
                            .iter()
                            .all(|member| matches!(member.as_str(), "integer" | "number"))
                        {
                            return "number".to_owned();
                        }
                        return members.join(" | ");
                    }
                    let mut members = vec![left, right];
                    if members.iter().any(|member| member == "any") {
                        return "any".to_owned();
                    }
                    members.sort();
                    members.dedup();
                    return members.join(" | ");
                }
                self.infer_expression_type(&expression.to_string())
            }
            Expression::UnaryOperator { unop, expression } => match unop.to_string().trim() {
                "not" => "boolean".to_owned(),
                "#" => "number".to_owned(),
                "-" | "~"
                    if matches!(
                        self.infer_ast_expression_type(expression).as_str(),
                        "integer" | "number"
                    ) =>
                {
                    "number".to_owned()
                }
                _ => "any".to_owned(),
            },
            _ => self.infer_expression_type(&expression.to_string()),
        }
    }

    fn if_branch_error(
        &self,
        expression: &Expression,
        expected: &str,
        code: &'static str,
    ) -> Vec<Diagnostic> {
        let value = match expression {
            Expression::IfExpression(value) => value,
            Expression::Parentheses { expression, .. } => {
                return self.if_branch_error(expression, expected, code);
            }
            _ => return Vec::new(),
        };
        let mut errors = Vec::new();
        let mut branches = vec![value.if_expression()];
        if let Some(else_ifs) = value.else_if_expressions() {
            branches.extend(else_ifs.iter().map(|branch| branch.expression()));
        }
        branches.push(value.else_expression());
        for branch in branches {
            if matches!(branch, Expression::IfExpression(_)) {
                errors.extend(self.if_branch_error(branch, expected, code));
                continue;
            }
            let actual_source = branch.to_string();
            let actual = self.infer_ast_expression_type(branch);
            let byte = branch
                .start_position()
                .map(|position| position.bytes())
                .unwrap_or_default();
            let end_byte = branch
                .end_position()
                .map(|position| position.bytes())
                .unwrap_or(byte);
            let assignable = if actual == "table" {
                let record_errors = self.record_errors(&actual_source, expected, byte, code, "");
                if !record_errors.is_empty() {
                    errors.extend(record_errors);
                    continue;
                }
                check_table_assignability(&actual_source, expected, &self.aliases, &|value| {
                    self.infer_expression_type(value)
                })
                .map(|result| result.is_ok())
                .or_else(|| is_assignable(&actual, expected, &self.aliases, &mut Vec::new()))
            } else {
                is_assignable(&actual, expected, &self.aliases, &mut Vec::new())
            };
            if assignable == Some(false) {
                errors.push(Diagnostic::range(
                    self.source,
                    self.prepared.original_offset(byte),
                    self.prepared.original_offset(end_byte),
                    code,
                    format!("Type '{actual}' is not assignable to type '{expected}'."),
                ));
            }
        }
        errors
    }

    fn infer_field_type(
        &self,
        receiver: &str,
        field: &str,
        optional_access: bool,
    ) -> Option<String> {
        let receiver_type = self.infer_expression_type(receiver);
        let (definition, optional_receiver) =
            resolve_optional_base_type(&receiver_type, &self.aliases);
        let body = definition.trim().strip_prefix('{')?.strip_suffix('}')?;
        let field_type = split_table_type_fields(body)
            .into_iter()
            .find_map(|field_declaration| {
                let (name, ty) = split_once_top_level(field_declaration, ':')?;
                (!name.trim().starts_with('[') && name.trim().trim_matches(['\'', '"']) == field)
                    .then(|| ty.trim().to_owned())
            })
            .or_else(|| {
                let key_type = "'key'";
                split_table_type_fields(body).into_iter().find_map(|part| {
                    let (name, ty) = split_once_top_level(part, ':')?;
                    let expected_key = name.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
                    (is_assignable(key_type, expected_key, &self.aliases, &mut Vec::new())
                        == Some(true))
                    .then(|| ty.trim().to_owned())
                })
            })?;
        if optional_access || optional_receiver {
            Some(if field_type.ends_with('?') {
                field_type
            } else {
                format!("{field_type}?")
            })
        } else {
            Some(field_type)
        }
    }

    fn infer_index_type(&self, receiver: &str, key: &str) -> Option<String> {
        let types = self.infer_index_types(receiver, key)?;
        let mut inferred = types.join(" | ");
        let (_, optional) =
            resolve_optional_base_type(&self.infer_expression_type(receiver), &self.aliases);
        if optional && !inferred.ends_with('?') {
            inferred.push('?');
        }
        Some(inferred)
    }

    fn infer_index_write_type(&self, receiver: &str, key: &str) -> Option<String> {
        let mut types = self.infer_index_types(receiver, key)?;
        let mut seen = HashSet::new();
        types.retain(|ty| seen.insert(ty.clone()));
        Some(if types.len() == 1 {
            types.pop().unwrap()
        } else {
            types
                .into_iter()
                .map(|ty| format!("({ty})"))
                .collect::<Vec<_>>()
                .join(" & ")
        })
    }

    fn infer_index_types(&self, receiver: &str, key: &str) -> Option<Vec<String>> {
        let receiver_type = self.infer_expression_type(receiver);
        let (definition, _) = resolve_optional_base_type(&receiver_type, &self.aliases);
        let body = definition.trim().strip_prefix('{')?.strip_suffix('}')?;
        let key_type = self.infer_expression_type(key);
        let parts = split_table_type_fields(body);

        if let Some(keys) = string_key_options(&key_type, &self.aliases) {
            let mut types = Vec::with_capacity(keys.len());
            for key in keys {
                let field_type = parts.iter().find_map(|part| {
                    let (name, value_type) = split_once_top_level(part, ':')?;
                    (!name.trim().starts_with('[') && name.trim().trim_matches(['\'', '"']) == key)
                        .then(|| value_type.trim().to_owned())
                });
                if let Some(field_type) = field_type {
                    types.push(field_type);
                    continue;
                }
                let indexer_type = parts.iter().find_map(|part| {
                    let (name, value_type) = split_once_top_level(part, ':')?;
                    let indexer = name.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
                    is_assignable(&key_type, indexer, &self.aliases, &mut Vec::new())
                        .filter(|assignable| *assignable)
                        .map(|_| value_type.trim().to_owned())
                });
                types.push(indexer_type?);
            }
            return Some(types);
        }

        if parts.len() == 1 && !parts[0].contains(':') {
            return matches!(key_type.as_str(), "integer" | "number")
                .then(|| vec![parts[0].trim().to_owned()]);
        }

        for part in parts {
            let Some((name, value_type)) = split_once_top_level(part, ':') else {
                continue;
            };
            let Some(expected_key) = name
                .trim()
                .strip_prefix('[')
                .and_then(|name| name.strip_suffix(']'))
            else {
                continue;
            };
            if is_assignable(
                &key_type,
                expected_key.trim(),
                &self.aliases,
                &mut Vec::new(),
            ) == Some(true)
            {
                return Some(vec![value_type.trim().to_owned()]);
            }
        }
        None
    }

    fn infer_table_literal_type(&self, expression: &str) -> Option<String> {
        let parts = actual_table_fields(expression, &|value| self.infer_expression_type(value))?;
        if parts.is_empty() {
            return None;
        }
        let mut named_fields = Vec::new();
        let mut array_items = Vec::new();
        for (name, raw_key, value, ..) in parts {
            let value_type = self.infer_expression_type(&value);
            let value_type = if value_type == "table" {
                self.infer_table_literal_type(&value).unwrap_or(value_type)
            } else {
                value_type
            };
            if raw_key.is_empty() {
                array_items.push(value_type);
            } else {
                let field_name = if name == raw_key
                    && let Some(key) = raw_key
                        .strip_prefix('[')
                        .and_then(|key| key.strip_suffix(']'))
                {
                    format!("[{}]", self.infer_expression_type(key.trim()))
                } else {
                    name
                };
                named_fields.push(format!("{field_name}: {value_type}"));
            }
        }
        if !named_fields.is_empty() && !array_items.is_empty() {
            return None;
        }
        if !named_fields.is_empty() {
            return Some(format!("{{ {} }}", named_fields.join(", ")));
        }
        array_items.sort();
        array_items.dedup();
        if array_items
            .iter()
            .all(|item| matches!(item.as_str(), "integer" | "number"))
        {
            array_items = vec!["number".to_owned()];
        }
        Some(format!("{{ {} }}", array_items.join(" | ")))
    }
}

impl Visitor for TypeChecker<'_> {
    fn visit_expression(&mut self, node: &Expression) {
        let invalid = match node {
            Expression::BinaryOperator { binop, lhs, rhs } => {
                let operator = binop.token().to_string();
                let left = self.primitive_kind(&self.infer_ast_expression_type(lhs));
                let right = self.primitive_kind(&self.infer_ast_expression_type(rhs));
                let invalid = match operator.trim() {
                    "+" | "-" | "*" | "/" | "//" | "%" | "^" | "&" | "|" | "<<" | ">>" => {
                        matches!((left, right), (Some(left), Some(right)) if left != "number" || right != "number")
                    }
                    "<" | "<=" | ">" | ">=" => {
                        matches!((left, right), (Some(left), Some(right)) if left != right || !matches!(left, "number" | "string"))
                    }
                    ".." => {
                        matches!((left, right), (Some(left), Some(right)) if !matches!(left, "number" | "string") || !matches!(right, "number" | "string"))
                    }
                    _ => false,
                };
                invalid.then_some((binop.token().start_position(), operator))
            }
            Expression::UnaryOperator { unop, expression } => {
                let operator = unop.to_string();
                let kind = self.primitive_kind(&self.infer_ast_expression_type(expression));
                let invalid = match operator.trim() {
                    "-" | "~" => kind.is_some_and(|kind| kind != "number"),
                    "#" => kind.is_some_and(|kind| !matches!(kind, "string" | "table")),
                    _ => false,
                };
                invalid.then_some((unop.token().start_position(), operator))
            }
            _ => None,
        };
        if let Some((Some(position), operator)) = invalid {
            self.diagnostics.push(Diagnostic::at(
                self.source,
                self.prepared.original_offset(position.bytes()),
                operator.chars().count().max(1),
                "LT2001",
                format!("Operator '{operator}' has incompatible operand types."),
            ));
        }
    }

    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        let mut collector = AliasCollector {
            aliases: HashMap::new(),
        };
        collector.visit_type_declaration(node);
        for (name, alias) in collector.aliases {
            let previous = self.aliases.insert(name.clone(), alias);
            if let Some(scope) = self.alias_scope_changes.last_mut() {
                scope.push((name, previous));
            }
        }
    }

    fn visit_const_assignment(&mut self, node: &ast::luau::ConstAssignment) {
        let expressions: Vec<_> = node.expressions().iter().cloned().collect();
        let (values, _) = self.expression_list_values(&expressions, node.names().len());
        let mut bindings = HashMap::new();
        for (index, name) in node.names().iter().enumerate() {
            let name = name.token().to_string();
            let annotation = node.type_specifiers().nth(index).flatten();
            let expected = annotation
                .as_ref()
                .map(|annotation| annotation.type_info().to_string());
            let (actual_type, value) = values[index].clone();
            let actual_source = value.map_or_else(|| "nil".to_owned(), ToString::to_string);
            if let (Some(expected), Some(value)) = (expected.as_deref(), value) {
                let errors = self.if_branch_error(value, expected, "LT2001");
                if !errors.is_empty() {
                    self.diagnostics.extend(errors);
                    bindings.insert(name, expected.to_owned());
                    continue;
                }
            }
            if let (Some(annotation), Some(expected)) = (annotation, expected.as_deref()) {
                let assignability = if actual_type == "table" {
                    check_table_assignability(&actual_source, expected, &self.aliases, &|value| {
                        self.infer_expression_type(value)
                    })
                    .map(|result| result.is_ok())
                    .or_else(|| {
                        is_assignable(&actual_type, expected, &self.aliases, &mut Vec::new())
                    })
                } else {
                    is_assignable(&actual_type, expected, &self.aliases, &mut Vec::new())
                };
                if assignability == Some(false) {
                    let byte = annotation
                        .start_position()
                        .map(|position| position.bytes())
                        .unwrap_or_default();
                    let value_byte = value
                        .and_then(Node::start_position)
                        .map(|position| position.bytes())
                        .unwrap_or(byte);
                    let context = format!("Variable '{name}':");
                    let errors = self.record_errors(
                        &actual_source,
                        expected,
                        value_byte,
                        "LT2001",
                        &context,
                    );
                    if errors.is_empty() {
                        self.diagnostics.push(Diagnostic::at(
                            self.source,
                            self.prepared.original_offset(byte),
                            expected.chars().count().max(1),
                            "LT2001",
                            format!("Variable '{name}': type '{actual_type}' is not assignable to type '{expected}'."),
                        ));
                    } else {
                        self.diagnostics.extend(errors);
                    }
                }
            }
            let inferred_table_type = expected
                .is_none()
                .then(|| self.infer_table_literal_type(&actual_source))
                .flatten();
            bindings.insert(
                name,
                expected
                    .or(inferred_table_type)
                    .unwrap_or_else(|| widen_inferred_type(&actual_type)),
            );
        }
        self.pending_local_bindings.push(bindings);
    }

    fn visit_const_assignment_end(&mut self, _: &ast::luau::ConstAssignment) {
        if let (Some(scope), Some(bindings)) =
            (self.scopes.last_mut(), self.pending_local_bindings.pop())
        {
            scope.extend(bindings);
        }
    }

    fn visit_anonymous_function(&mut self, node: &ast::AnonymousFunction) {
        let body = node.body();
        let mut scope = HashMap::new();
        for (index, parameter) in body.parameters().iter().enumerate() {
            if let ast::Parameter::Name(name) = parameter {
                let ty = body.type_specifiers().nth(index).flatten().map_or_else(
                    || "any".to_owned(),
                    |specifier| specifier.type_info().to_string(),
                );
                scope.insert(name.token().to_string(), ty);
            }
        }
        self.scopes.push(scope);
        self.function_names.push("<anonymous>".to_owned());
        self.return_types.push(
            body.return_type()
                .map(|specifier| specifier.type_info().to_string()),
        );
        if let Some(vararg_type) =
            body.parameters()
                .iter()
                .enumerate()
                .find_map(|(index, parameter)| {
                    matches!(parameter, ast::Parameter::Ellipsis(_)).then(|| {
                        body.type_specifiers().nth(index).flatten().map_or_else(
                            || "any".to_owned(),
                            |specifier| specifier.type_info().to_string(),
                        )
                    })
                })
            && let Some(scope) = self.scopes.last_mut()
        {
            scope.insert("...".to_owned(), vararg_type);
        }
    }

    fn visit_anonymous_function_end(&mut self, _: &ast::AnonymousFunction) {
        self.return_types.pop();
        self.function_names.pop();
        self.scopes.pop();
    }

    fn visit_numeric_for(&mut self, node: &ast::NumericFor) {
        let mut scope = HashMap::new();
        scope.insert(
            node.index_variable().token().to_string(),
            "number".to_owned(),
        );
        self.scopes.push(scope);
    }

    fn visit_numeric_for_end(&mut self, _: &ast::NumericFor) {
        self.scopes.pop();
    }

    fn visit_generic_for(&mut self, node: &ast::GenericFor) {
        let mut scope = HashMap::new();
        let names = node.names().iter().collect::<Vec<_>>();
        let mut types = vec!["any".to_owned(); names.len()];
        if let Some(iterator) = node.expressions().iter().next().map(ToString::to_string)
            && let Some(open) = iterator.find('(')
            && let Some(close) = matching_delimiter(&iterator, open, '(', ')')
        {
            let receiver = iterator[open + 1..close].trim();
            if iterator[..open].trim() == "ipairs" && types.len() >= 2 {
                types[0] = "number".to_owned();
                types[1] = self
                    .infer_index_type(receiver, "1")
                    .unwrap_or_else(|| "any".to_owned());
            } else if iterator[..open].trim() == "pairs" && types.len() >= 2 {
                let receiver_type = self.infer_expression_type(receiver);
                let (definition, _) = resolve_optional_base_type(&receiver_type, &self.aliases);
                if let Some(body) = definition
                    .trim()
                    .strip_prefix('{')
                    .and_then(|body| body.strip_suffix('}'))
                {
                    let fields = split_table_type_fields(body);
                    if let Some((key, value)) = fields.iter().find_map(|field| {
                        let (key, value) = split_once_top_level(field, ':')?;
                        Some((
                            key.trim().strip_prefix('[')?.strip_suffix(']')?.trim(),
                            value.trim(),
                        ))
                    }) {
                        types[0] = key.to_owned();
                        types[1] = value.to_owned();
                    } else {
                        types[0] = "string".to_owned();
                        let mut values = fields
                            .iter()
                            .filter_map(|field| {
                                split_once_top_level(field, ':').map(|(_, value)| value.trim())
                            })
                            .collect::<Vec<_>>();
                        values.sort_unstable();
                        values.dedup();
                        if !values.is_empty() {
                            types[1] = values.join(" | ");
                        }
                    }
                }
            }
        }
        if let Some(iterator) = node.expressions().iter().next()
            && !iterator.to_string().starts_with("ipairs(")
            && !iterator.to_string().starts_with("pairs(")
        {
            let pack = self.expression_value_pack(iterator, true);
            types = (0..names.len()).map(|index| pack.value_at(index)).collect();
        }
        for (name, ty) in names.into_iter().zip(types) {
            scope.insert(name.token().to_string(), ty);
        }
        self.scopes.push(scope);
    }

    fn visit_generic_for_end(&mut self, _: &ast::GenericFor) {
        self.scopes.pop();
    }

    fn visit_block(&mut self, _: &ast::Block) {
        let push_scope = self.repeat_scope_pending == 0;
        if push_scope {
            self.scopes.push(HashMap::new());
            self.function_scope_changes.push(Vec::new());
            self.alias_scope_changes.push(Vec::new());
        } else {
            self.repeat_scope_pending -= 1;
        }
        self.block_scopes.push(push_scope);
    }

    fn visit_block_end(&mut self, _: &ast::Block) {
        if self.block_scopes.pop().unwrap_or(false) {
            self.scopes.pop();
            restore_function_scope(&mut self.functions, self.function_scope_changes.pop());
            restore_alias_scope(&mut self.aliases, self.alias_scope_changes.pop());
        }
    }

    fn visit_repeat(&mut self, _: &ast::Repeat) {
        self.scopes.push(HashMap::new());
        self.function_scope_changes.push(Vec::new());
        self.alias_scope_changes.push(Vec::new());
        self.repeat_scope_pending += 1;
    }

    fn visit_repeat_end(&mut self, _: &ast::Repeat) {
        self.scopes.pop();
        restore_function_scope(&mut self.functions, self.function_scope_changes.pop());
        restore_alias_scope(&mut self.aliases, self.alias_scope_changes.pop());
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        let name = node.name().token().to_string();
        let mut collector = FunctionCollector::default();
        collector.add_ast(name.clone(), node.body());
        if let Some(signature) = collector.functions.remove(&name) {
            let previous = self.functions.insert(name.clone(), signature);
            if let Some(scope) = self.function_scope_changes.last_mut() {
                scope.push((name.clone(), previous));
            }
        }
        self.enter_function(&name);
    }

    fn visit_local_function_end(&mut self, _: &ast::LocalFunction) {
        self.return_types.pop();
        self.function_names.pop();
        self.scopes.pop();
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        let name = node.name().to_string();
        self.enter_function(&name);
    }

    fn visit_function_declaration_end(&mut self, _: &ast::FunctionDeclaration) {
        self.return_types.pop();
        self.function_names.pop();
        self.scopes.pop();
    }

    fn visit_return(&mut self, node: &ast::Return) {
        let Some(expected) = self.return_types.last().and_then(Option::as_ref) else {
            return;
        };
        let expected_pack = ValuePack::from_type(expected);
        if expected_pack.fixed.len() > 1 {
            let expected_values = expected_pack.fixed;
            let actual_values: Vec<_> = node.returns().iter().cloned().collect();
            let (returned_values, actual_pack) =
                self.expression_list_values(&actual_values, expected_values.len() + 1);
            if actual_pack.fixed.len() > expected_values.len()
                && actual_pack.variadic.is_none()
                && let Some((_, Some(extra))) = returned_values.get(expected_values.len())
            {
                let start = extra
                    .start_position()
                    .map(|position| position.bytes())
                    .unwrap_or_default();
                self.diagnostics.push(Diagnostic::at(
                    self.source,
                    self.prepared.original_offset(start),
                    extra.to_string().chars().count().max(1),
                    "LT2004",
                    format!(
                        "Function '{}' returns more values than its declared tuple type.",
                        self.function_names
                            .last()
                            .map(String::as_str)
                            .unwrap_or("<anonymous>")
                    ),
                ));
            }
            for (index, expected_type) in expected_values.iter().enumerate() {
                let (actual_type, actual_expression) = &returned_values[index];
                if is_assignable(actual_type, expected_type, &self.aliases, &mut Vec::new())
                    == Some(false)
                {
                    let start = actual_expression
                        .and_then(Node::start_position)
                        .map(|position| position.bytes())
                        .unwrap_or_default();
                    let length = actual_expression.map_or(1, |expression| {
                        expression.to_string().chars().count().max(1)
                    });
                    self.diagnostics.push(Diagnostic::at(self.source, self.prepared.original_offset(start), length, "LT2004", format!("Return value {} of function '{}': type '{}' is not assignable to return type '{}'.", index + 1, self.function_names.last().map(String::as_str).unwrap_or("<anonymous>"), actual_type, expected_type)));
                }
            }
            return;
        }
        let actual_values: Vec<_> = node.returns().iter().cloned().collect();
        if let Some(expression) = actual_values.first() {
            let errors = self.if_branch_error(expression, expected, "LT2004");
            if !errors.is_empty() {
                self.diagnostics.extend(errors);
                return;
            }
        }
        let (returned_values, _) = self.expression_list_values(&actual_values, 1);
        let (actual, expression) = &returned_values[0];
        let actual = actual.clone();
        let actual_source = expression.map_or_else(|| "nil".to_owned(), ToString::to_string);
        let assignability = if actual == "table" {
            check_table_assignability(&actual_source, expected, &self.aliases, &|value| {
                self.infer_expression_type(value)
            })
            .map(|result| result.is_ok())
            .or_else(|| is_assignable(&actual, expected, &self.aliases, &mut Vec::new()))
        } else {
            is_assignable(&actual, expected, &self.aliases, &mut Vec::new())
        };
        if assignability == Some(false) {
            let byte = node
                .start_position()
                .map(|position| position.bytes())
                .unwrap_or_default();
            let highlight = expression
                .and_then(Node::start_position)
                .map(|position| position.bytes())
                .unwrap_or(byte);
            let function = self
                .function_names
                .last()
                .map(String::as_str)
                .unwrap_or("<anonymous>");
            let context = format!("Return value of function '{function}':");
            let errors =
                self.record_errors(&actual_source, expected, highlight, "LT2004", &context);
            if !errors.is_empty() {
                self.diagnostics.extend(errors);
                return;
            }
            self.diagnostics.push(Diagnostic::at(
                self.source,
                self.prepared.original_offset(highlight),
                actual_source.chars().count().max(1),
                "LT2004",
                format!(
                    "Return value of function '{}': type '{actual}' is not assignable to return type '{expected}'.",
                    self.function_names.last().map(String::as_str).unwrap_or("<anonymous>")
                ),
            ));
        }
    }

    fn visit_function_call(&mut self, node: &ast::FunctionCall) {
        let call = node.to_string();
        let Some(open) = call.find('(') else {
            return;
        };
        let Some(close) = matching_delimiter(&call, open, '(', ')') else {
            return;
        };
        let name = call[..open].trim();
        let args = split_top_level(&call[open + 1..close], ',');
        if self
            .scopes
            .iter()
            .rev()
            .any(|scope| scope.contains_key(name))
        {
            return;
        }
        let Some(signature) = self.functions.get(name).cloned() else {
            return;
        };
        let argument_nodes = node
            .suffixes()
            .filter_map(|suffix| match suffix {
                ast::Suffix::Call(ast::Call::AnonymousCall(arguments)) => {
                    match arguments.as_ref() {
                        ast::FunctionArgs::Parentheses { arguments, .. } => {
                            Some(arguments.iter().collect::<Vec<_>>())
                        }
                        _ => None,
                    }
                }
                _ => None,
            })
            .last()
            .unwrap_or_default();
        let mut expanded_args = Vec::new();
        let mut has_unknown_argument_count = false;
        for (index, argument) in args.iter().enumerate() {
            let offset = argument.as_ptr() as usize - call.as_ptr() as usize;
            let pack = argument_nodes.get(index).map_or_else(
                || ValuePack::single(self.infer_expression_type(argument)),
                |expression| self.expression_value_pack(expression, index + 1 == args.len()),
            );
            for ty in pack.fixed {
                expanded_args.push(((*argument).to_owned(), ty, offset));
            }
            if let Some(variadic) = pack.variadic {
                has_unknown_argument_count = true;
                expanded_args.push(((*argument).to_owned(), variadic, offset));
            }
        }
        if !has_unknown_argument_count {
            for _ in expanded_args.len()..signature.parameters.len() {
                expanded_args.push(("nil".to_owned(), "nil".to_owned(), 0));
            }
        }
        let bindings = generic_bindings(&signature, &args, self);
        for (index, ((actual, actual_type, argument_offset), expected)) in
            expanded_args.iter().zip(&signature.parameters).enumerate()
        {
            let mut expected_type = expected.clone();
            for generic in &signature.generic_parameters {
                expected_type = substitute_type_name(
                    &expected_type,
                    generic,
                    bindings.types.get(generic).map_or("any", String::as_str),
                );
            }
            let conflicts_with_argument = signature.generic_parameters.iter().any(|generic| {
                bindings.conflicts.contains(generic)
                    && substitute_type_name(expected, generic, "__generic_conflict")
                        != expected.as_str()
                    && bindings.types.get(generic).is_some_and(|bound| {
                        is_assignable(actual_type, bound, &self.aliases, &mut Vec::new())
                            == Some(false)
                    })
            });
            let assignability = if conflicts_with_argument {
                Some(false)
            } else if actual_type == "table" {
                check_table_assignability(actual, &expected_type, &self.aliases, &|value| {
                    self.infer_expression_type(value)
                })
                .map(|result| result.is_ok())
                .or_else(|| {
                    is_assignable(actual_type, &expected_type, &self.aliases, &mut Vec::new())
                })
            } else {
                is_assignable(actual_type, &expected_type, &self.aliases, &mut Vec::new())
            };
            if assignability == Some(false) {
                let byte = node
                    .start_position()
                    .map(|position| position.bytes())
                    .unwrap_or_default();
                let argument_byte = byte + argument_offset;
                let parameter = signature
                    .parameter_names
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or("parameter");
                let context = format!(
                    "Argument {} ('{parameter}') of function '{name}':",
                    index + 1
                );
                let errors = self.record_errors(
                    actual.trim(),
                    &expected_type,
                    argument_byte,
                    "LT2002",
                    &context,
                );
                if !errors.is_empty() {
                    self.diagnostics.extend(errors);
                    continue;
                }
                let highlight = self.prepared.original_offset(argument_byte);
                self.diagnostics.push(Diagnostic::at(
                    self.source,
                    highlight,
                    actual.trim().chars().count().max(1),
                    "LT2002",
                    format!("Argument {} ('{parameter}') of function '{name}': type '{actual_type}' is not assignable to parameter type '{expected_type}'.", index + 1),
                ));
            }
        }
        if let Some(expected) = signature.vararg.as_deref() {
            for (index, (actual, actual_type, _)) in expanded_args
                .iter()
                .enumerate()
                .skip(signature.parameters.len())
            {
                if is_assignable(actual_type, expected, &self.aliases, &mut Vec::new())
                    == Some(false)
                {
                    let start = call.find(actual).unwrap_or_default();
                    self.diagnostics.push(Diagnostic::at(
                        self.source,
                        self.prepared.original_offset(node.start_position().map(|p| p.bytes()).unwrap_or_default() + start),
                        actual.chars().count().max(1),
                        "LT2002",
                        format!("Argument {} of function '{name}': type '{actual_type}' is not assignable to vararg type '{expected}'.", index + 1),
                    ));
                }
            }
        }
        let wrong_count = !has_unknown_argument_count
            && signature.vararg.is_none()
            && expanded_args.len() > signature.parameters.len();
        if wrong_count {
            let byte = node
                .start_position()
                .map(|position| position.bytes())
                .unwrap_or_default();
            self.diagnostics.push(Diagnostic::at(
                self.source,
                self.prepared.original_offset(byte),
                name.chars().count().max(1),
                "LT2003",
                format!(
                    "Function '{name}' expects {}{} {}, but got {}.",
                    signature.parameters.len(),
                    if signature.vararg.is_some() {
                        " or more"
                    } else {
                        ""
                    },
                    if signature.parameters.len() == 1 {
                        "argument"
                    } else {
                        "arguments"
                    },
                    expanded_args.len()
                ),
            ));
        }
    }

    fn visit_var_expression(&mut self, node: &ast::VarExpression) {
        let mut receiver = node.prefix().to_string();
        for suffix in node.suffixes() {
            match suffix {
                ast::Suffix::Index(ast::Index::Dot { name, .. }) => {
                    let field = name.token().to_string();
                    if self.infer_field_type(&receiver, &field, false).is_none()
                        && self.is_known_record(&receiver)
                    {
                        let Some(start) = name
                            .start_position()
                            .map(|position| self.prepared.original_offset(position.bytes()))
                        else {
                            receiver.push_str(&suffix.to_string());
                            continue;
                        };
                        let end = name
                            .end_position()
                            .map(|position| self.prepared.original_offset(position.bytes()))
                            .unwrap_or(start);
                        self.diagnostics.push(Diagnostic::range(
                            self.source,
                            start,
                            end,
                            "LT2005",
                            format!(
                                "Type '{}' has no field '{field}' in expression '{receiver}.{field}'.",
                                self.infer_expression_type(&receiver),
                            ),
                        ));
                        break;
                    }
                }
                ast::Suffix::Index(ast::Index::Brackets { expression, .. }) => {
                    let key = expression.to_string();
                    if self.infer_index_type(&receiver, &key).is_none()
                        && self.is_known_record(&receiver)
                        && let (Some(start), Some(end)) =
                            (expression.start_position(), expression.end_position())
                    {
                        self.diagnostics.push(Diagnostic::range(
                            self.source,
                            self.prepared.original_offset(start.bytes()),
                            self.prepared.original_offset(end.bytes()),
                            "LT2005",
                            format!(
                                "Type '{}' has no field '{key}' in expression '{receiver}[{key}]'.",
                                self.infer_expression_type(&receiver),
                            ),
                        ));
                        break;
                    }
                }
                _ => {}
            }
            receiver.push_str(&suffix.to_string());
        }
    }

    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        let mut pending_bindings = HashMap::new();
        let expressions: Vec<_> = node.expressions().iter().cloned().collect();
        let (values, _) = self.expression_list_values(&expressions, node.names().len());
        for (index, name) in node.names().iter().enumerate() {
            let name = name.to_string().trim().to_owned();
            let annotation = node.type_specifiers().nth(index).flatten();
            let expected = annotation
                .as_ref()
                .map(|annotation| annotation.type_info().to_string().trim().to_owned());
            let (actual_type, value) = values[index].clone();
            let actual_source = value.map_or_else(|| "nil".to_owned(), ToString::to_string);

            if let (Some(expected), Some(value)) = (expected.as_deref(), value) {
                let errors = self.if_branch_error(value, expected, "LT2001");
                if !errors.is_empty() {
                    self.diagnostics.extend(errors);
                    pending_bindings.insert(name, expected.to_owned());
                    continue;
                }
            }

            if let (Some(annotation), Some(expected)) = (annotation, expected.as_ref()) {
                let actual_type = actual_type.as_str();
                let actual_source = actual_source.as_str();
                let assignability = if actual_type == "table" {
                    check_table_assignability(actual_source, expected, &self.aliases, &|value| {
                        self.infer_expression_type(value)
                    })
                    .map(|result| result.is_ok())
                    .or_else(|| {
                        is_assignable(actual_type, expected, &self.aliases, &mut Vec::new())
                    })
                } else {
                    is_assignable(actual_type, expected, &self.aliases, &mut Vec::new())
                };
                if assignability == Some(false) {
                    let byte = annotation
                        .start_position()
                        .map(|position| position.bytes())
                        .unwrap_or_default();
                    let value_byte = value
                        .and_then(Node::start_position)
                        .map(|position| position.bytes())
                        .unwrap_or(byte);
                    let context = format!("Variable '{name}':");
                    let errors =
                        self.record_errors(actual_source, expected, value_byte, "LT2001", &context);
                    if !errors.is_empty() {
                        self.diagnostics.extend(errors);
                    } else {
                        self.diagnostics.push(Diagnostic::at(
                            self.source,
                            self.prepared.original_offset(byte),
                            expected.chars().count().max(1),
                            "LT2001",
                            format!("Variable '{name}': type '{actual_type}' is not assignable to type '{expected}'."),
                        ));
                    }
                }
            }
            let inferred_table_type = expected
                .is_none()
                .then(|| self.infer_table_literal_type(&actual_source))
                .flatten();
            let inferred_type = widen_inferred_type(&actual_type);
            if let Some(ty) = expected.or(inferred_table_type).or(Some(inferred_type)) {
                pending_bindings.insert(name, ty);
            }
        }
        self.pending_local_bindings.push(pending_bindings);
    }

    fn visit_local_assignment_end(&mut self, _: &ast::LocalAssignment) {
        if let (Some(scope), Some(bindings)) =
            (self.scopes.last_mut(), self.pending_local_bindings.pop())
        {
            scope.extend(bindings);
        }
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        let expressions: Vec<_> = node.expressions().iter().cloned().collect();
        let (values, _) = self.expression_list_values(&expressions, node.variables().len());
        for (index, target) in node.variables().iter().enumerate() {
            let target_source = target.to_string();
            let target_name = target_source.trim();
            let (actual_type, value) = values[index].clone();
            let actual_source = value.map_or_else(|| "nil".to_owned(), ToString::to_string);
            let expected = split_property_access(target_name)
                .and_then(|(receiver, field, optional)| {
                    self.infer_field_type(receiver, field, optional)
                })
                .or_else(|| {
                    split_index_access(target_name)
                        .and_then(|(receiver, key)| self.infer_index_write_type(receiver, key))
                })
                .or_else(|| {
                    self.scopes
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(target_name).cloned())
                });
            if let (Some(value), Some(expected)) = (value, expected.as_deref()) {
                let errors = self.if_branch_error(value, expected, "LT2001");
                if !errors.is_empty() {
                    self.diagnostics.extend(errors);
                    continue;
                }
            }

            if let Some(expected) = expected.as_deref() {
                let assignability = if actual_type == "table" {
                    check_table_assignability(&actual_source, expected, &self.aliases, &|value| {
                        self.infer_expression_type(value)
                    })
                    .map(|result| result.is_ok())
                    .or_else(|| {
                        is_assignable(&actual_type, expected, &self.aliases, &mut Vec::new())
                    })
                } else {
                    is_assignable(&actual_type, expected, &self.aliases, &mut Vec::new())
                };
                if assignability == Some(false) {
                    let byte = target
                        .start_position()
                        .map(|position| position.bytes())
                        .unwrap_or_default();
                    let value_byte = value
                        .and_then(Node::start_position)
                        .map(|position| position.bytes())
                        .unwrap_or(byte);
                    let context = format!("Variable '{target_name}':");
                    let errors = self.record_errors(
                        &actual_source,
                        expected,
                        value_byte,
                        "LT2001",
                        &context,
                    );
                    if !errors.is_empty() {
                        self.diagnostics.extend(errors);
                        continue;
                    }
                    self.diagnostics.push(Diagnostic::at(
                        self.source,
                        self.prepared.original_offset(byte),
                        target_name.chars().count().max(1),
                        "LT2001",
                        format!("Variable '{target_name}': type '{actual_type}' is not assignable to type '{expected}'."),
                    ));
                }
            } else if is_identifier(target_name) && actual_type != "any" {
                let inferred_type = self
                    .infer_table_literal_type(&actual_source)
                    .unwrap_or(actual_type);
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(target_name.to_owned(), inferred_type);
                }
            }
        }
    }

    fn visit_compound_assignment(&mut self, node: &ast::CompoundAssignment) {
        let name = node.lhs().to_string();
        let name = name.trim();
        let Some(expected) = self.scopes.iter().rev().find_map(|scope| scope.get(name)) else {
            return;
        };
        let actual = self.infer_ast_expression_type(node.rhs());
        if is_assignable(&actual, expected, &self.aliases, &mut Vec::new()) == Some(false) {
            let start = node
                .rhs()
                .start_position()
                .map(|position| position.bytes())
                .unwrap_or_default();
            self.diagnostics.push(Diagnostic::at(
                self.source,
                self.prepared.original_offset(start),
                node.rhs().to_string().chars().count().max(1),
                "LT2001",
                format!("Compound assignment to '{name}': type '{actual}' is not assignable to type '{expected}'."),
            ));
        }
    }
}

fn restore_function_scope(
    functions: &mut HashMap<String, FunctionSignature>,
    changes: Option<Vec<(String, Option<FunctionSignature>)>>,
) {
    for (name, previous) in changes.unwrap_or_default().into_iter().rev() {
        if let Some(signature) = previous {
            functions.insert(name, signature);
        } else {
            functions.remove(&name);
        }
    }
}

fn restore_alias_scope(
    aliases: &mut HashMap<String, TypeAlias>,
    changes: Option<Vec<(String, Option<TypeAlias>)>>,
) {
    for (name, previous) in changes.unwrap_or_default().into_iter().rev() {
        if let Some(alias) = previous {
            aliases.insert(name, alias);
        } else {
            aliases.remove(&name);
        }
    }
}

fn split_property_access(expression: &str) -> Option<(&str, &str, bool)> {
    if let Some(index) = expression.rfind("?.") {
        return Some((
            expression[..index].trim(),
            expression[index + 2..].trim(),
            true,
        ));
    }
    let index = expression.rfind('.')?;
    let field = expression[index + 1..].trim();
    is_identifier(field).then(|| (expression[..index].trim(), field, false))
}

fn split_index_access(expression: &str) -> Option<(&str, &str)> {
    let expression = expression.trim();
    if !expression.ends_with(']') {
        return None;
    }
    let close = expression.len() - 1;
    expression
        .char_indices()
        .filter(|(_, character)| *character == '[')
        .find_map(|(open, _)| {
            (matching_delimiter(expression, open, '[', ']') == Some(close)).then(|| {
                (
                    expression[..open].trim(),
                    expression[open + 1..close].trim(),
                )
            })
        })
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_alphabetic())
        && characters.all(|character| character == '_' || character.is_alphanumeric())
}

fn generic_bindings(
    signature: &FunctionSignature,
    arguments: &[&str],
    checker: &TypeChecker<'_>,
) -> GenericBindings {
    let mut bindings = GenericBindings::default();
    for (expected, argument) in signature.parameters.iter().zip(arguments) {
        let actual = checker.infer_expression_type(argument);
        bind_generic_type(
            expected,
            &actual,
            &signature.generic_parameters,
            &mut bindings.types,
            &mut bindings.conflicts,
        );
    }
    bindings
}

fn bind_generic_type(
    expected: &str,
    actual: &str,
    parameters: &[String],
    bindings: &mut HashMap<String, String>,
    conflicts: &mut HashSet<String>,
) {
    let expected = expected.trim();
    let expected = expected
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(expected);
    let actual = actual.trim();
    if actual == "any" {
        return;
    }
    if parameters.iter().any(|parameter| parameter == expected) {
        bindings
            .entry(expected.to_owned())
            .and_modify(|bound| {
                if matches!(
                    (bound.as_str(), actual),
                    ("integer", "number") | ("number", "integer")
                ) {
                    *bound = "number".to_owned();
                } else if bound.starts_with(['\'', '"']) && actual.starts_with(['\'', '"']) {
                    *bound = "string".to_owned();
                } else if bound != actual {
                    conflicts.insert(expected.to_owned());
                }
            })
            .or_insert_with(|| actual.to_owned());
        return;
    }
    if let (Some(expected_inner), Some(actual_inner)) =
        (expected.strip_suffix('?'), actual.strip_suffix('?'))
    {
        bind_generic_type(
            expected_inner,
            actual_inner,
            parameters,
            bindings,
            conflicts,
        );
        return;
    }
    let Some((expected_name, expected_arguments)) = generic_type_parts(expected) else {
        return;
    };
    let Some((actual_name, actual_arguments)) = generic_type_parts(actual) else {
        return;
    };
    if expected_name != actual_name || expected_arguments.len() != actual_arguments.len() {
        return;
    }
    for (expected_argument, actual_argument) in expected_arguments.iter().zip(actual_arguments) {
        bind_generic_type(
            expected_argument,
            actual_argument,
            parameters,
            bindings,
            conflicts,
        );
    }
}

fn generic_type_parts(value: &str) -> Option<(&str, Vec<&str>)> {
    let start = value.find('<')?;
    let end = matching_delimiter(value, start, '<', '>')?;
    if !value[end + 1..].trim().is_empty() {
        return None;
    }
    Some((
        value[..start].trim(),
        split_top_level(&value[start + 1..end], ','),
    ))
}

fn infer_generic_return(
    signature: &FunctionSignature,
    arguments: &[&str],
    checker: &TypeChecker<'_>,
) -> String {
    let mut returns = signature.returns.clone();
    let bindings = generic_bindings(signature, arguments, checker);
    for parameter in &signature.generic_parameters {
        returns.fixed = returns
            .fixed
            .iter()
            .map(|value| {
                substitute_type_name(
                    value,
                    parameter,
                    bindings.types.get(parameter).map_or("any", String::as_str),
                )
            })
            .collect();
        returns.variadic = returns.variadic.as_deref().map(|value| {
            substitute_type_name(
                value,
                parameter,
                bindings.types.get(parameter).map_or("any", String::as_str),
            )
        });
    }
    returns.type_name()
}

fn check_table_assignability(
    actual: &str,
    expected: &str,
    aliases: &HashMap<String, TypeAlias>,
    infer: &dyn Fn(&str) -> String,
) -> Option<Result<(), Vec<TypeMismatch>>> {
    check_table_assignability_at_depth(actual, expected, aliases, infer, 0)
}

fn check_table_assignability_at_depth(
    actual: &str,
    expected: &str,
    aliases: &HashMap<String, TypeAlias>,
    infer: &dyn Fn(&str) -> String,
    depth: usize,
) -> Option<Result<(), Vec<TypeMismatch>>> {
    if let Some(result) = check_if_expression_assignability(actual, expected, aliases, infer, depth)
    {
        return Some(result);
    }
    if depth > 128 {
        let start = actual.find('{').unwrap_or_default();
        return Some(Err(vec![TypeMismatch {
            field_path: Vec::new(),
            range: start..start + 1,
            kind: TypeMismatchKind::TooDeep,
        }]));
    }
    let expected = expected.trim();
    let expected = expected.strip_suffix('?').unwrap_or(expected).trim();
    let expected = expected
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(expected);
    let mut resolved = expected;
    let mut seen = Vec::new();
    let mut owned;
    while let Some(alias) = resolve_alias(resolved, aliases) {
        if seen.len() >= 128 {
            let start = actual.find('{').unwrap_or_default();
            return Some(Err(vec![TypeMismatch {
                field_path: Vec::new(),
                range: start..start + 1,
                kind: TypeMismatchKind::TooDeep,
            }]));
        }
        let name = resolved.split('<').next().unwrap_or(resolved).trim();
        if seen.iter().any(|seen_name| seen_name == name) {
            return None;
        }
        seen.push(name.to_owned());
        owned = alias;
        let definition = owned.trim();
        resolved = definition.strip_suffix('?').unwrap_or(definition).trim();
    }
    let union = split_union(resolved);
    if union.len() > 1 {
        let mut best_mismatch: Option<Vec<TypeMismatch>> = None;
        for member in union {
            if let Some(result) =
                check_table_assignability_at_depth(actual, member, aliases, infer, depth + 1)
            {
                match result {
                    Ok(()) => return Some(Ok(())),
                    Err(mismatches)
                        if best_mismatch
                            .as_ref()
                            .is_none_or(|best| mismatches.len() < best.len()) =>
                    {
                        best_mismatch = Some(mismatches);
                    }
                    Err(_) => {}
                }
            }
        }
        return best_mismatch.map(Err);
    }
    let intersections = split_top_level(resolved, '&');
    if intersections.len() > 1 {
        let mut fields = Vec::new();
        for member in intersections {
            let definition =
                resolve_alias_chain(member, aliases).unwrap_or_else(|| member.to_owned());
            let body = definition
                .trim()
                .strip_prefix('{')
                .and_then(|body| body.strip_suffix('}'))?;
            fields.extend(split_table_type_fields(body).into_iter().map(str::to_owned));
        }
        owned = format!("{{ {} }}", fields.join(", "));
        resolved = owned.trim();
    }
    let body = resolved.strip_prefix('{')?.strip_suffix('}')?;
    let expected_parts = split_table_type_fields(body);
    let expected_fields = expected_parts
        .iter()
        .filter(|field| {
            split_once_top_level(field, ':').is_some_and(|(name, _)| !name.trim().starts_with('['))
        })
        .filter_map(|field| {
            let (name, ty) = split_once_top_level(field, ':')?;
            Some((name.trim().trim_matches(['\'', '"']), ty.trim()))
        })
        .collect::<Vec<_>>();
    let expected_indexers = expected_parts
        .into_iter()
        .filter_map(|field| {
            let (name, ty) = split_once_top_level(field, ':')?;
            let key_type = name.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
            Some((key_type, ty.trim()))
        })
        .collect::<Vec<_>>();
    if expected_fields.is_empty() && expected_indexers.is_empty() {
        let items = split_table_type_fields(body);
        let [item_type] = items.as_slice() else {
            return None;
        };
        if item_type.contains(':') {
            return None;
        }
        return Some(check_array_assignability(
            actual, item_type, aliases, infer, depth,
        ));
    }
    let actual_fields = actual_table_fields(actual, infer)?;
    let mut mismatches = Vec::new();
    for (name, expected_type) in &expected_fields {
        let optional = is_optional_type(expected_type, aliases);
        let expected_type = *expected_type;
        let Some((_, _, value, _, _, value_offset)) =
            actual_fields.iter().find(|(actual, ..)| actual == name)
        else {
            if optional {
                continue;
            }
            let offset = actual.find('{').unwrap_or_default();
            mismatches.push(TypeMismatch {
                field_path: vec![(*name).to_owned()],
                range: offset..offset + 1,
                kind: TypeMismatchKind::MissingField {
                    expected: expected_type.to_owned(),
                },
            });
            continue;
        };
        let actual_type = infer(value);
        let assignable = if actual_type == "table" {
            check_table_assignability_at_depth(value, expected_type, aliases, infer, depth + 1)
                .or_else(|| {
                    is_assignable(&actual_type, expected_type, aliases, &mut Vec::new()).map(
                        |okay| {
                            if okay {
                                Ok(())
                            } else {
                                Err(vec![TypeMismatch {
                                    field_path: vec![(*name).to_owned()],
                                    range: *value_offset..*value_offset + value.trim().len(),
                                    kind: TypeMismatchKind::IncompatibleType {
                                        actual: actual_type.clone(),
                                        expected: expected_type.to_owned(),
                                    },
                                }])
                            }
                        },
                    )
                })
        } else {
            is_assignable(&actual_type, expected_type, aliases, &mut Vec::new()).map(|okay| {
                if okay {
                    Ok(())
                } else {
                    Err(vec![TypeMismatch {
                        field_path: vec![(*name).to_owned()],
                        range: *value_offset..*value_offset + value.trim().len(),
                        kind: TypeMismatchKind::IncompatibleType {
                            actual: actual_type.clone(),
                            expected: expected_type.to_owned(),
                        },
                    }])
                }
            })
        };
        if let Some(Err(mut nested)) = assignable {
            if actual_type == "table" {
                for mismatch in &mut nested {
                    mismatch.field_path.insert(0, (*name).to_owned());
                }
                for mismatch in &mut nested {
                    mismatch.range.start += value_offset;
                    mismatch.range.end += value_offset;
                }
                mismatches.extend(nested);
            } else {
                mismatches.extend(nested);
            }
        }
    }
    for (name, raw_key, value, _, key_offset, value_offset) in actual_fields {
        if !expected_fields
            .iter()
            .any(|(expected, _)| *expected == name)
        {
            if let Some((expected_key, expected_value)) = expected_indexers.first() {
                let actual_key = if raw_key.is_empty() {
                    "number".to_owned()
                } else if let Some(key) = raw_key
                    .trim()
                    .strip_prefix('[')
                    .and_then(|key| key.strip_suffix(']'))
                {
                    infer(key.trim())
                } else {
                    "string".to_owned()
                };
                if is_assignable(&actual_key, expected_key, aliases, &mut Vec::new()) == Some(false)
                {
                    mismatches.push(TypeMismatch {
                        field_path: vec![name.to_owned()],
                        range: key_offset..key_offset + raw_key.len(),
                        kind: TypeMismatchKind::UnexpectedField,
                    });
                    continue;
                }
                let actual_type = infer(&value);
                let value_result = if actual_type == "table" {
                    check_table_assignability_at_depth(
                        &value,
                        expected_value,
                        aliases,
                        infer,
                        depth + 1,
                    )
                } else {
                    None
                }
                .or_else(|| {
                    is_assignable(&actual_type, expected_value, aliases, &mut Vec::new()).map(
                        |okay| {
                            if okay {
                                Ok(())
                            } else {
                                Err(vec![TypeMismatch {
                                    field_path: vec![name.to_owned()],
                                    range: value_offset..value_offset + value.len(),
                                    kind: TypeMismatchKind::IncompatibleType {
                                        actual: actual_type.clone(),
                                        expected: (*expected_value).to_owned(),
                                    },
                                }])
                            }
                        },
                    )
                });
                if let Some(Err(mut nested)) = value_result {
                    for mismatch in &mut nested {
                        if mismatch.field_path.is_empty() {
                            mismatch.field_path.push(name.to_owned());
                        }
                        mismatch.range.start += value_offset;
                        mismatch.range.end += value_offset;
                    }
                    mismatches.extend(nested);
                }
            } else if let Some(keys) = raw_key
                .trim()
                .strip_prefix('[')
                .and_then(|key| key.strip_suffix(']'))
                .and_then(|key| string_key_options(&infer(key.trim()), aliases))
                .filter(|keys| {
                    !keys.is_empty()
                        && keys
                            .iter()
                            .all(|key| expected_fields.iter().any(|(expected, _)| *expected == key))
                })
            {
                for key in keys {
                    let Some((_, expected_value)) = expected_fields
                        .iter()
                        .find(|(expected, _)| *expected == key)
                    else {
                        continue;
                    };
                    if let Some(Err(mut nested)) = check_field_value(
                        &value,
                        expected_value,
                        &format!("[{key}]"),
                        value_offset,
                        aliases,
                        infer,
                        depth + 1,
                    ) {
                        mismatches.append(&mut nested);
                    }
                }
            } else {
                mismatches.push(TypeMismatch {
                    field_path: vec![name.to_owned()],
                    range: key_offset..key_offset + raw_key.len(),
                    kind: TypeMismatchKind::UnexpectedField,
                });
            }
        }
    }
    Some(if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches)
    })
}

fn check_array_assignability(
    actual: &str,
    item_type: &str,
    aliases: &HashMap<String, TypeAlias>,
    infer: &dyn Fn(&str) -> String,
    depth: usize,
) -> Result<(), Vec<TypeMismatch>> {
    let Some(fields) = actual_table_fields(actual, infer) else {
        return Err(Vec::new());
    };
    let mut mismatches = Vec::new();
    let mut index = 0;
    for (name, raw_key, value, _, key_offset, value_offset) in fields {
        if raw_key.is_empty() {
            index += 1;
            check_array_item(
                &value,
                item_type,
                format!("[{index}]"),
                value_offset,
                aliases,
                infer,
                depth,
                &mut mismatches,
            );
            continue;
        }

        let Some(index_key) = raw_key
            .strip_prefix('[')
            .and_then(|key| key.strip_suffix(']'))
        else {
            mismatches.push(TypeMismatch {
                field_path: vec![name],
                range: key_offset..key_offset + raw_key.len(),
                kind: TypeMismatchKind::UnexpectedField,
            });
            continue;
        };
        let Ok(explicit_index) = index_key.trim().parse::<usize>() else {
            mismatches.push(TypeMismatch {
                field_path: vec![format!("[{index_key}]")],
                range: key_offset..key_offset + raw_key.len(),
                kind: TypeMismatchKind::UnexpectedField,
            });
            continue;
        };
        check_array_item(
            &value,
            item_type,
            format!("[{explicit_index}]"),
            value_offset,
            aliases,
            infer,
            depth,
            &mut mismatches,
        );
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches)
    }
}

#[allow(clippy::too_many_arguments)]
fn check_array_item(
    value: &str,
    expected: &str,
    index: String,
    offset: usize,
    aliases: &HashMap<String, TypeAlias>,
    infer: &dyn Fn(&str) -> String,
    depth: usize,
    mismatches: &mut Vec<TypeMismatch>,
) {
    let actual_type = infer(value);
    let result = if actual_type == "table" {
        check_table_assignability_at_depth(value, expected, aliases, infer, depth + 1)
    } else {
        None
    }
    .or_else(|| {
        is_assignable(&actual_type, expected, aliases, &mut Vec::new()).map(|okay| {
            if okay {
                Ok(())
            } else {
                Err(vec![TypeMismatch {
                    field_path: Vec::new(),
                    range: 0..value.len(),
                    kind: TypeMismatchKind::IncompatibleType {
                        actual: actual_type,
                        expected: expected.to_owned(),
                    },
                }])
            }
        })
    });
    if let Some(Err(mut nested)) = result {
        for mismatch in &mut nested {
            mismatch.field_path.insert(0, index.clone());
            mismatch.range.start += offset;
            mismatch.range.end += offset;
        }
        mismatches.extend(nested);
    }
}

fn actual_table_fields(
    actual: &str,
    infer: &dyn Fn(&str) -> String,
) -> Option<Vec<ActualTableField>> {
    let leading = actual.len() - actual.trim_start().len();
    let trimmed = actual.trim();
    if trimmed.starts_with('(')
        && matching_delimiter(trimmed, 0, '(', ')') == Some(trimmed.len() - 1)
    {
        let base = leading + 1;
        return actual_table_fields(&trimmed[1..trimmed.len() - 1], infer).map(|fields| {
            fields
                .into_iter()
                .map(|(name, raw_key, value, field, key, item)| {
                    (name, raw_key, value, field + base, key + base, item + base)
                })
                .collect()
        });
    }
    let prefix = "local __luat_table = ";
    let parsed = full_moon::parse_fallible(
        &format!("{prefix}{actual}"),
        LuaVersion::cfxlua().with_luau(),
    );
    if !parsed.errors().is_empty() {
        return None;
    }
    let mut initializer = InitializerCollector::default();
    initializer.visit_ast(&parsed.into_ast());
    let Some(Expression::TableConstructor(table)) = initializer.expression else {
        return None;
    };
    let mut fields = Vec::new();
    for field in table.fields() {
        let field_offset = relative_node_start(field, prefix.len())?;
        let (name, raw_key, value, key_offset, value_offset) = match field {
            full_moon::ast::Field::NameKey { key, value, .. } => (
                key.to_string().trim().to_owned(),
                key.to_string().trim().to_owned(),
                value.to_string(),
                relative_node_start(key, prefix.len())?,
                relative_node_start(value.as_ref(), prefix.len())?,
            ),
            full_moon::ast::Field::ExpressionKey { key, value, .. } => {
                let raw_key = format!("[{key}]");
                let name = table_field_name(&raw_key, infer).unwrap_or_else(|| raw_key.clone());
                (
                    name,
                    raw_key,
                    value.to_string(),
                    field_offset,
                    relative_node_start(value.as_ref(), prefix.len())?,
                )
            }
            full_moon::ast::Field::NoKey(value) => (
                String::new(),
                String::new(),
                value.to_string(),
                field_offset,
                relative_node_start(value, prefix.len())?,
            ),
            full_moon::ast::Field::SetConstructor { name, .. } => (
                name.token().to_string(),
                name.to_string(),
                "true".to_owned(),
                relative_node_start(name, prefix.len())?,
                relative_node_start(name, prefix.len())?,
            ),
            _ => continue,
        };
        fields.push((name, raw_key, value, field_offset, key_offset, value_offset));
    }
    Some(fields)
}

fn relative_node_start<T: Node>(node: &T, prefix_len: usize) -> Option<usize> {
    node.start_position()?.bytes().checked_sub(prefix_len)
}

fn check_field_value(
    value: &str,
    expected: &str,
    path: &str,
    offset: usize,
    aliases: &HashMap<String, TypeAlias>,
    infer: &dyn Fn(&str) -> String,
    depth: usize,
) -> Option<Result<(), Vec<TypeMismatch>>> {
    let actual_type = infer(value);
    let result = if actual_type == "table" {
        check_table_assignability_at_depth(value, expected, aliases, infer, depth)
    } else {
        None
    }
    .or_else(|| {
        is_assignable(&actual_type, expected, aliases, &mut Vec::new()).map(|okay| {
            if okay {
                Ok(())
            } else {
                Err(vec![TypeMismatch {
                    field_path: Vec::new(),
                    range: 0..value.len(),
                    kind: TypeMismatchKind::IncompatibleType {
                        actual: actual_type,
                        expected: expected.to_owned(),
                    },
                }])
            }
        })
    });
    result.map(|result| {
        result.map_err(|mut mismatches| {
            for mismatch in &mut mismatches {
                mismatch.field_path.insert(0, path.to_owned());
                mismatch.range.start += offset;
                mismatch.range.end += offset;
            }
            mismatches
        })
    })
}

fn string_key_options(
    type_name: &str,
    aliases: &HashMap<String, TypeAlias>,
) -> Option<Vec<String>> {
    fn resolve(
        type_name: &str,
        aliases: &HashMap<String, TypeAlias>,
        seen: &mut HashSet<String>,
    ) -> Option<Vec<String>> {
        let type_name = type_name.trim();
        let type_name = type_name
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .unwrap_or(type_name)
            .trim();
        let members = split_union(type_name);
        if members.len() > 1 {
            return members
                .into_iter()
                .try_fold(Vec::new(), |mut keys, member| {
                    let mut member_seen = seen.clone();
                    keys.extend(resolve(member, aliases, &mut member_seen)?);
                    Some(keys)
                });
        }
        if type_name.starts_with(['\'', '"']) {
            return Some(vec![lua_string_value(type_name)]);
        }
        if !seen.insert(type_name.to_owned()) {
            return None;
        }
        let alias = resolve_alias(type_name, aliases)?;
        resolve(&alias, aliases, seen)
    }

    resolve(type_name, aliases, &mut HashSet::new())
}

fn table_field_name(key: &str, infer: &dyn Fn(&str) -> String) -> Option<String> {
    let expression = key.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
    let key_type = infer(expression);
    key_type
        .starts_with(['\'', '"'])
        .then(|| lua_string_value(expression))
}

fn format_field_path(path: &[String]) -> String {
    let mut formatted = String::new();
    for part in path {
        if part.starts_with('[') {
            formatted.push_str(part);
        } else {
            if !formatted.is_empty() {
                formatted.push('.');
            }
            formatted.push_str(part);
        }
    }
    formatted
}

fn split_top_level(input: &str, separator: char) -> Vec<&str> {
    split_top_level_by(input, &[separator])
}

fn strip_lua_comments(input: &str) -> String {
    let mut output = input.as_bytes().to_vec();
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    while index < input.len() {
        let character = input[index..].chars().next().unwrap();
        let end = index + character.len_utf8();
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            index = end;
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            index = end;
            continue;
        }
        if input[index..].starts_with("--") {
            let comment_start = index + 2;
            let comment_end = long_bracket_end(input, comment_start).unwrap_or_else(|| {
                input[comment_start..]
                    .find('\n')
                    .map_or(input.len(), |offset| comment_start + offset)
            });
            for byte in &mut output[index..comment_end] {
                if !matches!(*byte, b'\r' | b'\n') {
                    *byte = b' ';
                }
            }
            index = comment_end;
            continue;
        }
        index = end;
    }
    String::from_utf8(output).unwrap()
}

fn split_table_type_fields(input: &str) -> Vec<&str> {
    split_top_level_by(input, &[',', ';'])
}

fn split_top_level_by<'a>(input: &'a str, separators: &[char]) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    let mut index = 0;
    while index < input.len() {
        if quote.is_none() && input[index..].starts_with("--") {
            let comment_start = index + 2;
            if let Some(end) = long_bracket_end(input, comment_start) {
                index = end;
                continue;
            }
            index = input[comment_start..]
                .find('\n')
                .map_or(input.len(), |offset| comment_start + offset + 1);
            continue;
        }
        let ch = input[index..].chars().next().unwrap();
        let end = index + ch.len_utf8();
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
            index = end;
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth -= 1,
            _ if separators.contains(&ch) && depth == 0 => {
                parts.push(input[start..index].trim());
                start = end;
            }
            _ => {}
        }
        index = end;
    }
    if !input[start..].trim().is_empty() {
        parts.push(input[start..].trim());
    }
    parts
}

fn long_bracket_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut cursor = start + 1;
    while bytes.get(cursor) == Some(&b'=') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'[') {
        return None;
    }
    let closing = format!("]{}]", "=".repeat(cursor - start - 1));
    input[cursor + 1..]
        .find(&closing)
        .map(|offset| cursor + 1 + offset + closing.len())
}

fn split_once_top_level(input: &str, separator: char) -> Option<(&str, &str)> {
    let mut depth = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    let mut index = 0;
    while index < input.len() {
        if quote.is_none() && input[index..].starts_with("--") {
            let comment_start = index + 2;
            if let Some(end) = long_bracket_end(input, comment_start) {
                index = end;
                continue;
            }
            index = input[comment_start..]
                .find('\n')
                .map_or(input.len(), |offset| comment_start + offset + 1);
            continue;
        }
        let ch = input[index..].chars().next().unwrap();
        let end = index + ch.len_utf8();
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
            index = end;
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth -= 1,
            _ if ch == separator && depth == 0 => {
                return Some((&input[..index], &input[end..]));
            }
            _ => {}
        }
        index = end;
    }
    None
}

pub(crate) fn check(
    source: &str,
    prepared: &PreparedSource,
    ast: &full_moon::ast::Ast,
    declarations: &[String],
) -> Vec<Diagnostic> {
    let mut consts = ConstChecker {
        source,
        prepared,
        scopes: vec![HashMap::new()],
        pending_bindings: Vec::new(),
        diagnostics: Vec::new(),
    };
    consts.visit_ast(ast);
    if !consts.diagnostics.is_empty() {
        return consts.diagnostics;
    }
    let mut aliases = AliasCollector {
        aliases: HashMap::new(),
    };
    aliases.aliases.insert(
        "vector2".to_owned(),
        TypeAlias {
            parameters: Vec::new(),
            definition: "{ x: number, y: number }".to_owned(),
        },
    );
    aliases.aliases.insert(
        "vector3".to_owned(),
        TypeAlias {
            parameters: Vec::new(),
            definition: "{ x: number, y: number, z: number }".to_owned(),
        },
    );
    aliases.aliases.insert(
        "vector4".to_owned(),
        TypeAlias {
            parameters: Vec::new(),
            definition: "{ x: number, y: number, z: number, w: number }".to_owned(),
        },
    );
    for declaration in declarations {
        let prepared_declaration = crate::compiler::syntax::prepare(declaration);
        let parsed = full_moon::parse_fallible(
            &prepared_declaration.text,
            full_moon::LuaVersion::cfxlua().with_luau(),
        );
        if parsed.errors().is_empty() {
            aliases.visit_ast(&parsed.into_ast());
        }
    }
    let checker_aliases = aliases.aliases.clone();
    aliases.visit_ast(ast);

    let alias_graph = unguarded_alias_graph(&aliases.aliases);
    let cycles = find_alias_cycles(&alias_graph);
    let deep_aliases = find_deep_aliases(&alias_graph);
    let mut validator = TypeReferenceValidator {
        source,
        prepared,
        alias_scopes: vec![checker_aliases.clone()],
        generic_scopes: Vec::new(),
        cycles,
        deep_aliases,
        reported_aliases: HashSet::new(),
        diagnostics: Vec::new(),
    };
    validator.visit_ast(ast);
    if !validator.diagnostics.is_empty() {
        return validator.diagnostics;
    }

    let mut checker = TypeChecker {
        source,
        prepared,
        aliases: checker_aliases,
        alias_scope_changes: Vec::new(),
        functions: {
            let mut functions = FunctionCollector::default();
            functions.add_vector_constructors();
            functions.visit_ast(ast);
            functions.functions
        },
        scopes: vec![HashMap::new()],
        block_scopes: Vec::new(),
        repeat_scope_pending: 0,
        return_types: Vec::new(),
        function_names: Vec::new(),
        function_scope_changes: Vec::new(),
        pending_local_bindings: Vec::new(),
        diagnostics: Vec::new(),
    };
    checker.visit_ast(ast);
    checker.diagnostics
}
