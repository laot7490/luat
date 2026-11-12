use std::{
    collections::{BTreeMap, HashMap},
    ops::Range,
};

use full_moon::{
    ast::{self, Call, Expression, Prefix, Stmt, Suffix, luau::IfExpression},
    node::Node,
    tokenizer::TokenType,
    visitors::Visitor,
};

use crate::{compiler::syntax::PreparedSource, diagnostic::CompileError};

#[derive(Clone)]
struct Site {
    statement: Range<usize>,
    expression: Range<usize>,
    value: IfExpression,
}

#[derive(Default)]
struct Collector {
    sites: Vec<Site>,
    roots: Vec<Range<usize>>,
    unsupported: Option<Range<usize>>,
}

type ConditionalOutput = (String, HashMap<usize, usize>, Vec<usize>);

// shortcut: lift root values and simple call statements only, until a statement-aware pass can preserve evaluation order in every expression.
impl Visitor for Collector {
    fn visit_stmt(&mut self, node: &Stmt) {
        let Stmt::FunctionCall(call) = node else {
            return;
        };
        if !matches!(call.prefix(), Prefix::Name(_)) {
            return;
        }
        let Some(value) = call_if_argument(call) else {
            return;
        };
        self.add(node, value);
    }

    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        if node.names().len() != 1 || node.expressions().len() != 1 {
            return;
        }
        let Some(expression) = node.expressions().iter().next() else {
            return;
        };
        if let Some(value) = if_expression(expression) {
            self.add(node, value);
        } else if let Expression::FunctionCall(call) = expression
            && let Some(value) = call_if_argument(call)
        {
            self.add(node, value);
        }
    }

    fn visit_return(&mut self, node: &ast::Return) {
        if node.returns().len() != 1 {
            return;
        }
        let Some(value) = node.returns().iter().next().and_then(if_expression) else {
            return;
        };
        self.add(node, value);
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        if node.variables().len() != 1 || node.expressions().len() != 1 {
            return;
        }
        let Some(target) = node.variables().iter().next() else {
            return;
        };
        let target = target.to_string();
        if !is_identifier(target.trim()) {
            return;
        }
        let Some(value) = node.expressions().iter().next().and_then(if_expression) else {
            return;
        };
        self.add(node, value);
    }

    fn visit_if_expression(&mut self, node: &IfExpression) {
        let Some(range) = node_range(node) else {
            return;
        };
        if !self
            .roots
            .iter()
            .any(|root| root.start <= range.start && root.end >= range.end)
        {
            self.unsupported.get_or_insert(range);
        }
    }
}

impl Collector {
    fn add<T: Node>(&mut self, statement: &T, expression: &IfExpression) {
        let (Some(statement), Some(range)) = (node_range(statement), node_range(expression)) else {
            return;
        };
        self.roots.push(range.clone());
        self.sites.push(Site {
            statement,
            expression: range,
            value: expression.clone(),
        });
    }
}

pub(super) fn lower_conditionals(
    source: &str,
    prepared: &PreparedSource,
    ast: &full_moon::ast::Ast,
    output: &str,
) -> Result<ConditionalOutput, CompileError> {
    let mut collector = Collector::default();
    collector.visit_ast(ast);
    if let Some(range) = collector.unsupported {
        return Err(unsupported_error(source, prepared, range.start));
    }
    if collector.sites.is_empty() {
        return Ok((
            output.to_owned(),
            prepared
                .coalesce_operators
                .iter()
                .map(|offset| (*offset, *offset))
                .collect(),
            (0..=output.len()).collect(),
        ));
    }

    collector.sites.sort_by_key(|site| site.statement.start);
    let mut result = String::with_capacity(output.len() + collector.sites.len() * 96);
    let mut result_offsets = vec![0];
    let mut cursor = 0;
    let mut next_temporary = 0;
    let mut shifts = Vec::new();
    for site in collector.sites {
        if site.statement.start < cursor
            || site.expression.start < site.statement.start
            || site.expression.end > site.statement.end
        {
            return Err(unsupported_error(source, prepared, site.expression.start));
        }
        if prepared
            .coalesce_operators
            .iter()
            .any(|offset| site.statement.contains(offset))
        {
            let offset = prepared
                .coalesce_operators
                .iter()
                .find(|offset| site.statement.contains(offset))
                .copied()
                .unwrap_or(site.statement.start);
            return Err(CompileError {
                diagnostics: vec![crate::Diagnostic::at(
                    source,
                    prepared.original_offset(offset),
                    2,
                    "LT3001",
                    "`??` must be the complete value of a local initializer, simple assignment, or return".to_owned(),
                )],
            });
        }
        let temporary = loop {
            let candidate = format!("__luat_{next_temporary}");
            next_temporary += 1;
            if !source.contains(&candidate) {
                break candidate;
            }
        };
        append_source(
            output,
            cursor..site.statement.start,
            &mut result,
            &mut result_offsets,
        );
        let start = result.len();
        append_generated(
            &comments(&site.value),
            site.expression.start,
            &mut result,
            &mut result_offsets,
        );
        append_generated(
            &format!("local {temporary}\n"),
            site.statement.start,
            &mut result,
            &mut result_offsets,
        );
        let mut lowered_expression = String::new();
        emit_if(&site.value, &temporary, output, &mut lowered_expression, 0);
        append_generated(
            &lowered_expression,
            site.expression.start,
            &mut result,
            &mut result_offsets,
        );
        append_source(
            output,
            site.statement.start..site.expression.start,
            &mut result,
            &mut result_offsets,
        );
        append_generated(
            &temporary,
            site.expression.start,
            &mut result,
            &mut result_offsets,
        );
        append_source(
            output,
            site.expression.end..site.statement.end,
            &mut result,
            &mut result_offsets,
        );
        shifts.push((
            site.statement.clone(),
            result.len() - start - site.statement.len(),
        ));
        cursor = site.statement.end;
    }
    append_source(
        output,
        cursor..output.len(),
        &mut result,
        &mut result_offsets,
    );
    let operators = prepared
        .coalesce_operators
        .iter()
        .map(|offset| {
            let shift = shifts
                .iter()
                .filter(|(statement, _)| statement.start <= *offset)
                .map(|(_, shift)| *shift)
                .sum::<usize>();
            (offset.saturating_add(shift), *offset)
        })
        .collect();
    Ok((result, operators, result_offsets))
}

fn append_source(source: &str, range: Range<usize>, output: &mut String, offsets: &mut Vec<usize>) {
    *offsets.last_mut().unwrap() = range.start;
    output.push_str(&source[range.clone()]);
    offsets.extend(range.start + 1..=range.end);
}

fn append_generated(text: &str, anchor: usize, output: &mut String, offsets: &mut Vec<usize>) {
    output.push_str(text);
    offsets.extend(std::iter::repeat_n(anchor, text.len()));
}

fn comments<T: Node>(node: &T) -> String {
    let mut comments = BTreeMap::new();
    for token in node.tokens() {
        for trivia in token.leading_trivia().chain(token.trailing_trivia()) {
            if matches!(
                trivia.token_type(),
                TokenType::SingleLineComment { .. }
                    | TokenType::MultiLineComment { .. }
                    | TokenType::CStyleComment { .. }
            ) {
                let start = trivia.start_position().bytes();
                let end = trivia.end_position().bytes();
                comments
                    .entry((start, end))
                    .or_insert_with(|| format!("{trivia}"));
            }
        }
    }
    comments
        .into_values()
        .map(|comment| format!("{comment}\n"))
        .collect()
}

fn emit_if(value: &IfExpression, target: &str, source: &str, output: &mut String, indent: usize) {
    let pad = " ".repeat(indent);
    output.push_str(&pad);
    output.push_str("if ");
    output.push_str(&node_text(value.condition(), source));
    output.push_str(" then\n");
    emit_branch(value.if_expression(), target, source, output, indent + 4);
    if let Some(else_ifs) = value.else_if_expressions() {
        for else_if in else_ifs {
            output.push_str(&pad);
            output.push_str("elseif ");
            output.push_str(&node_text(else_if.condition(), source));
            output.push_str(" then\n");
            emit_branch(else_if.expression(), target, source, output, indent + 4);
        }
    }
    output.push_str(&pad);
    output.push_str("else\n");
    emit_branch(value.else_expression(), target, source, output, indent + 4);
    output.push_str(&pad);
    output.push_str("end\n");
}

fn emit_branch(
    expression: &Expression,
    target: &str,
    source: &str,
    output: &mut String,
    indent: usize,
) {
    if let Some(nested) = if_expression(expression) {
        emit_if(nested, target, source, output, indent);
    } else {
        output.push_str(&" ".repeat(indent));
        output.push_str(target);
        output.push_str(" = ");
        output.push_str(&node_text(expression, source));
        output.push('\n');
    }
}

fn if_expression(expression: &Expression) -> Option<&IfExpression> {
    match expression {
        Expression::IfExpression(value) => Some(value),
        Expression::Parentheses { expression, .. } => if_expression(expression),
        _ => None,
    }
}

fn call_if_argument(call: &ast::FunctionCall) -> Option<&IfExpression> {
    if !matches!(call.prefix(), Prefix::Name(_)) {
        return None;
    }
    let mut suffixes = call.suffixes();
    let Some(Suffix::Call(Call::AnonymousCall(arguments))) = suffixes.next() else {
        return None;
    };
    if suffixes.next().is_some() {
        return None;
    }
    let ast::FunctionArgs::Parentheses { arguments, .. } = arguments.as_ref() else {
        return None;
    };
    (arguments.len() == 1)
        .then(|| arguments.iter().next().and_then(if_expression))
        .flatten()
}

fn node_text<T: Node + std::fmt::Display>(node: &T, source: &str) -> String {
    node_range(node)
        .and_then(|range| source.get(range).map(str::to_owned))
        .unwrap_or_else(|| node.to_string())
        .trim()
        .to_owned()
}

fn node_range<T: Node>(node: &T) -> Option<Range<usize>> {
    let (start, end) = node
        .tokens()
        .fold(None, |range: Option<(usize, usize)>, token| {
            let start = token.token().start_position().bytes();
            let end = token.token().end_position().bytes();
            Some(match range {
                Some((first, last)) => (first.min(start), last.max(end)),
                None => (start, end),
            })
        })?;
    Some(start..end)
}

fn unsupported_error(source: &str, prepared: &PreparedSource, offset: usize) -> CompileError {
    CompileError {
        diagnostics: vec![crate::Diagnostic::at(
            source,
            prepared.original_offset(offset),
            2,
            "LT3003",
            "`if` expressions in this position cannot yet be safely lowered".to_owned(),
        )],
    }
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::lower_conditionals;
    use crate::compiler::syntax::prepare;
    use full_moon::LuaVersion;

    #[test]
    fn maps_source_after_lowered_conditionals_through_template_expansion() {
        let source = "local greeting = `Hi {name}`\nlocal value = if true then 1 else 2\nlocal after = candidate ?? 3";
        let prepared = prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau()).into_ast();
        let (lowered, _, offsets) =
            lower_conditionals(source, &prepared, &ast, &prepared.text).unwrap();
        let lowered_after = lowered.find("local after").unwrap();
        let original_after = source.find("local after").unwrap();
        let lowered_operator = lowered.find("or 3").unwrap();

        assert_eq!(offsets.len(), lowered.len() + 1);
        assert_eq!(
            prepared.original_offset(offsets[lowered_after]),
            original_after
        );
        assert_eq!(
            prepared.original_offset(offsets[lowered_operator]),
            source.find("??").unwrap()
        );
    }
}
