use std::{collections::HashSet, ops::Range};

use full_moon::{
    LuaVersion,
    ast::luau::{
        ConstAssignment, GenericDeclaration, TypeAssertion, TypeDeclaration, TypeInstantiation,
        TypeSpecifier,
    },
    ast::{BinOp, Expression},
    node::Node,
    tokenizer::TokenType,
    visitors::Visitor,
};

use crate::compiler::syntax::PreparedSource;
use crate::diagnostic::{CompileError, parser_diagnostics};

mod conditional;

#[derive(Default)]
struct TypeEraser {
    ranges: Vec<Range<usize>>,
    const_keywords: Vec<Range<usize>>,
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

impl Visitor for TypeEraser {
    fn visit_const_assignment(&mut self, node: &ConstAssignment) {
        let keyword = node.const_token();
        let start = keyword.token().start_position().bytes();
        let end = keyword.token().end_position().bytes();
        self.ranges.push(start..end);
        self.const_keywords.push(start..end);
    }

    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        if let Some(range) = node_range(node) {
            self.ranges.push(range);
        }
    }

    fn visit_type_specifier(&mut self, node: &TypeSpecifier) {
        if let Some(range) = node_range(node) {
            self.ranges.push(range);
        }
    }

    fn visit_generic_declaration(&mut self, node: &GenericDeclaration) {
        if let Some(range) = node_range(node) {
            self.ranges.push(range);
        }
    }

    fn visit_type_instantiation(&mut self, node: &TypeInstantiation) {
        if let Some(range) = node_range(node) {
            self.ranges.push(range);
        }
    }

    fn visit_type_assertion(&mut self, node: &TypeAssertion) {
        if let Some(range) = node_range(node) {
            self.ranges.push(range);
        }
    }
}

pub(crate) fn lower(
    source: &str,
    prepared: &PreparedSource,
    ast: &full_moon::ast::Ast,
) -> Result<String, CompileError> {
    let mut eraser = TypeEraser::default();
    eraser.visit_ast(ast);
    let mut output = blank_ranges(&prepared.text, &eraser.ranges);
    for range in &eraser.const_keywords {
        output.replace_range(range.clone(), "local");
    }
    let (mut output, mut coalesce_offset_map, mut output_offsets) =
        conditional::lower_conditionals(source, prepared, ast, &output)?;
    let mut names = Vec::new();
    struct ConstNames<'a>(&'a mut Vec<usize>);
    impl Visitor for ConstNames<'_> {
        fn visit_const_assignment(&mut self, node: &ConstAssignment) {
            self.0.extend(
                node.names()
                    .iter()
                    .map(|name| name.token().end_position().bytes()),
            );
        }
    }
    ConstNames(&mut names).visit_ast(ast);
    let mut insertions = names
        .into_iter()
        .filter_map(|source_offset| {
            output_offsets
                .iter()
                .position(|offset| *offset == source_offset)
        })
        .map(|offset| (offset, " <const>"))
        .collect::<Vec<_>>();
    insertions.sort_by_key(|(offset, _)| std::cmp::Reverse(*offset));
    for (offset, insertion) in insertions {
        output.insert_str(offset, insertion);
        let anchor = output_offsets.get(offset).copied().unwrap_or_default();
        output_offsets.splice(offset..offset, std::iter::repeat_n(anchor, insertion.len()));
        coalesce_offset_map = coalesce_offset_map
            .into_iter()
            .map(|(operator, original)| {
                (
                    operator
                        + if operator >= offset {
                            insertion.len()
                        } else {
                            0
                        },
                    original,
                )
            })
            .collect();
    }
    let coalesce_operators = coalesce_offset_map.keys().copied().collect::<HashSet<_>>();
    let parsed = full_moon::parse_fallible(&output, LuaVersion::cfxlua());
    if !parsed.errors().is_empty() {
        let mut diagnostics = parser_diagnostics(
            parsed.errors(),
            "Generated CfxLua is invalid: ",
            source,
            |offset| {
                prepared.original_offset(
                    output_offsets
                        .get(offset)
                        .copied()
                        .unwrap_or_else(|| *output_offsets.last().unwrap_or(&0)),
                )
            },
        );
        for diagnostic in &mut diagnostics {
            diagnostic.code = "LT9002";
        }
        return Err(CompileError { diagnostics });
    }
    if coalesce_operators.is_empty() {
        return Ok(output);
    }

    let mut collector = CoalesceCollector {
        operators: &coalesce_operators,
        sites: Vec::new(),
        covered: HashSet::new(),
    };
    collector.visit_ast(&parsed.into_ast());
    if collector.covered.len() != coalesce_operators.len() {
        // shortcut: lift only whole-value statements until branch and evaluation ordering are modeled.
        let offset = coalesce_operators
            .iter()
            .find(|offset| !collector.covered.contains(*offset))
            .copied()
            .unwrap_or_default();
        return Err(CompileError {
            diagnostics: vec![crate::Diagnostic::at(
                source,
                prepared.original_offset(
                    coalesce_offset_map.get(&offset).copied().unwrap_or(offset),
                ),
                2,
                "LT3001",
                "`??` must be the complete value of a local initializer, simple assignment, or return".to_owned(),
            )],
        });
    }
    collector.sites.sort_by_key(|site| site.statement.start);
    let mut rebuilt = String::with_capacity(output.len() + collector.sites.len() * 64);
    let mut cursor = 0;
    let mut next_temporary = 0;
    for site in collector.sites {
        if site.statement.start < cursor
            || site.expression.start < site.statement.start
            || site.expression.end > site.statement.end
        {
            return Err(CompileError {
                diagnostics: vec![crate::Diagnostic::at(
                    source,
                    prepared.original_offset(
                        coalesce_offset_map
                            .get(&site.operator)
                            .copied()
                            .unwrap_or(site.operator),
                    ),
                    2,
                    "LT3002",
                    "Could not safely lower '??' in this assignment.".to_owned(),
                )],
            });
        }
        rebuilt.push_str(&output[cursor..site.statement.start]);
        let temporary = loop {
            let candidate = format!("__luat_{}", next_temporary);
            next_temporary += 1;
            if !source.contains(&candidate) && !output.contains(&candidate) {
                break candidate;
            }
        };
        rebuilt.push_str("local ");
        rebuilt.push_str(&temporary);
        rebuilt.push_str(" = ");
        rebuilt.push_str(&parenthesize_once(&site.lhs));
        rebuilt.push('\n');
        for (fallback, comments) in site.fallbacks {
            rebuilt.push_str("if ");
            rebuilt.push_str(&temporary);
            rebuilt.push_str(" == nil then\n");
            if !comments.is_empty() {
                rebuilt.push_str(&comments);
            }
            rebuilt.push_str("    ");
            rebuilt.push_str(&temporary);
            rebuilt.push_str(" = (");
            rebuilt.push_str(&fallback);
            rebuilt.push_str(")\nend\n");
        }
        rebuilt.push_str(&output[site.statement.start..site.expression.start]);
        rebuilt.push_str(&temporary);
        rebuilt.push_str(&output[site.expression.end..site.statement.end]);
        cursor = site.statement.end;
    }
    rebuilt.push_str(&output[cursor..]);
    output = rebuilt;
    let validated = full_moon::parse_fallible(&output, LuaVersion::cfxlua());
    if !validated.errors().is_empty() {
        let mut diagnostics = parser_diagnostics(
            validated.errors(),
            "Generated CfxLua is invalid: ",
            source,
            |_| 0,
        );
        for diagnostic in &mut diagnostics {
            diagnostic.code = "LT9003";
        }
        return Err(CompileError { diagnostics });
    }
    Ok(output)
}

fn parenthesize_once(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('(') && outer_parentheses_close(value) {
        value.to_owned()
    } else {
        format!("({value})")
    }
}

fn outer_parentheses_close(value: &str) -> bool {
    let mut depth = 0;
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return index == value.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

struct CoalesceSite {
    operator: usize,
    statement: Range<usize>,
    expression: Range<usize>,
    lhs: String,
    fallbacks: Vec<(String, String)>,
}

struct CoalesceCollector<'a> {
    operators: &'a HashSet<usize>,
    sites: Vec<CoalesceSite>,
    covered: HashSet<usize>,
}

impl Visitor for CoalesceCollector<'_> {
    fn visit_local_assignment(&mut self, node: &full_moon::ast::LocalAssignment) {
        if node.expressions().len() != 1 {
            return;
        }
        let Some(expression) = node.expressions().iter().next() else {
            return;
        };
        self.add_site(node, expression);
    }

    fn visit_assignment(&mut self, node: &full_moon::ast::Assignment) {
        if node.variables().len() != 1 || node.expressions().len() != 1 {
            return;
        }
        let Some(target) = node.variables().iter().next() else {
            return;
        };
        if !is_identifier(target.to_string().trim()) {
            return;
        }
        if let Some(expression) = node.expressions().iter().next() {
            self.add_site(node, expression);
        }
    }

    fn visit_return(&mut self, node: &full_moon::ast::Return) {
        if node.returns().len() == 1
            && let Some(expression) = node.returns().iter().next()
        {
            self.add_site(node, expression);
        }
    }
}

impl CoalesceCollector<'_> {
    fn add_site<T: Node>(&mut self, statement_node: &T, expression: &Expression) {
        let Some(chain) = coalesce_chain(expression, self.operators) else {
            return;
        };
        let (Some(statement), Some(expression_range)) =
            (node_range(statement_node), node_range(expression))
        else {
            return;
        };
        self.covered.extend(chain.operators.iter().copied());
        self.sites.push(CoalesceSite {
            operator: chain.operators[0],
            statement,
            expression: expression_range,
            lhs: chain.initial,
            fallbacks: chain.fallbacks,
        });
    }
}

struct CoalesceChain {
    initial: String,
    fallbacks: Vec<(String, String)>,
    operators: Vec<usize>,
}

fn coalesce_chain(expression: &Expression, operators: &HashSet<usize>) -> Option<CoalesceChain> {
    let Expression::BinaryOperator {
        lhs,
        binop: BinOp::Or(operator),
        rhs,
    } = expression
    else {
        return None;
    };
    let offset = operator.start_position().map(|position| position.bytes())?;
    if !operators.contains(&offset) {
        return None;
    }

    let mut chain = coalesce_chain(lhs, operators).unwrap_or_else(|| CoalesceChain {
        initial: lhs.to_string().trim().to_owned(),
        fallbacks: Vec::new(),
        operators: Vec::new(),
    });
    let right = coalesce_chain(rhs, operators);
    let fallback = right.as_ref().map_or_else(
        || rhs.to_string().trim().to_owned(),
        |right| right.initial.clone(),
    );
    let comments = operator
        .leading_trivia()
        .chain(operator.trailing_trivia())
        .filter(|token| {
            matches!(
                token.token_type(),
                TokenType::SingleLineComment { .. }
                    | TokenType::MultiLineComment { .. }
                    | TokenType::CStyleComment { .. }
            )
        })
        .map(|token| format!("{token}\n"))
        .collect::<String>();
    chain.fallbacks.push((fallback, comments));
    chain.operators.push(offset);
    if let Some(right) = right {
        chain.fallbacks.extend(right.fallbacks);
        chain.operators.extend(right.operators);
    }
    Some(chain)
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn blank_ranges(source: &str, ranges: &[Range<usize>]) -> String {
    let mut output = source.as_bytes().to_vec();
    let mut ordered = ranges.to_vec();
    ordered.sort_by_key(|range| (range.start, std::cmp::Reverse(range.end)));

    let mut end = 0;
    for range in ordered {
        let start = range.start.max(end).min(output.len());
        let stop = range.end.min(output.len());
        if start >= stop || !source.is_char_boundary(start) || !source.is_char_boundary(stop) {
            continue;
        }
        for byte in &mut output[start..stop] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
        end = stop;
    }
    String::from_utf8(output).expect("blanking bytes preserves UTF-8 validity")
}

#[cfg(test)]
mod tests {
    use super::lower;
    use full_moon::LuaVersion;

    #[test]
    fn erases_nested_type_syntax_without_moving_source_lines() {
        let source = "typedef Player = { name: string }\nlocal player: Player = { name = 'Ada' }";
        let prepared = crate::compiler::syntax::prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
                .into_ast();
        let output = lower(source, &prepared, &ast).unwrap();
        assert!(!output.contains("type Player"));
        assert!(!output.contains(": Player"));
        assert_eq!(output.lines().count(), source.lines().count());
    }

    #[test]
    fn lowers_nil_coalescing_without_iifes_or_repeated_left_evaluation() {
        let source = "local value = get_value() ?? false";
        let prepared = crate::compiler::syntax::prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau()).into_ast();
        let output = lower(source, &prepared, &ast).unwrap();
        assert!(output.contains("__luat_0 == nil then"));
        assert!(output.contains("__luat_0 = (false)"));
        assert_eq!(output.matches("get_value()").count(), 1);
        assert!(!output.contains("??"));
        assert!(full_moon::parse(&output).is_ok());
    }

    #[test]
    fn lowers_outer_varargs_without_adding_a_function_scope() {
        let source = "local value = ... ?? nil";
        let prepared = crate::compiler::syntax::prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau()).into_ast();
        let output = lower(source, &prepared, &ast).unwrap();
        assert!(output.contains("local __luat_0 = (...)"));
        assert!(!output.contains("function(__luat"));
    }

    #[test]
    fn lowers_coalescing_in_return_assignment_and_multi_local_declaration() {
        let source = "local first, second = get_value() ?? false\nvalue = get_value() ?? fallback()\nreturn get_value() ?? nil";
        let prepared = crate::compiler::syntax::prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau()).into_ast();
        let output = lower(source, &prepared, &ast).unwrap();
        assert_eq!(output.matches("if __luat_").count(), 3, "{output}");
        assert!(
            output.contains("local first, second = __luat_0"),
            "{output}"
        );
        assert!(output.contains("value = __luat_1"), "{output}");
        assert!(output.contains("return __luat_2"), "{output}");
    }

    #[test]
    fn lowers_chained_coalescing_in_order_without_repeated_evaluation() {
        let source = "local value = first() ?? second() ?? third()";
        let prepared = crate::compiler::syntax::prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau()).into_ast();
        let output = lower(source, &prepared, &ast).unwrap();
        assert_eq!(
            output.matches("if __luat_0 == nil then").count(),
            2,
            "{output}"
        );
        assert_eq!(output.matches("first()").count(), 1, "{output}");
        assert_eq!(output.matches("second()").count(), 1, "{output}");
        assert_eq!(output.matches("third()").count(), 1, "{output}");
    }

    #[test]
    fn rejects_coalescing_inside_an_expression_without_changing_evaluation_order() {
        let source = "local value = wrap(get_value() ?? fallback())";
        let prepared = crate::compiler::syntax::prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, LuaVersion::cfxlua().with_luau()).into_ast();
        let error = lower(source, &prepared, &ast).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "LT3001");
    }
}
