use super::*;
use full_moon::node::Node;

pub(super) fn validate_declaration(source: &str) -> Result<(), CompileError> {
    let prepared = crate::compiler::syntax::prepare(source);
    let parsed =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau());
    if !parsed.errors().is_empty() {
        return Err(CompileError {
            diagnostics: crate::diagnostic::parser_diagnostics(
                parsed.errors(),
                "",
                source,
                |offset| prepared.original_offset(offset),
            ),
        });
    }
    let ast = parsed.into_ast();
    let mut eraser = DeclarationEraser::default();
    full_moon::visitors::Visitor::visit_ast(&mut eraser, &ast);
    let erased = erase_ranges(&prepared.text, &eraser.ranges);
    let erased_ast = full_moon::parse_fallible(&erased, full_moon::LuaVersion::cfxlua());
    let erased_ast = erased_ast.into_ast();
    if let Some(statement) = erased_ast.nodes().stmts().next() {
        let byte = full_moon::node::Node::start_position(statement)
            .map(|position| position.bytes())
            .unwrap_or_default();
        let original = prepared.original_offset(byte);
        return Err(CompileError {
            diagnostics: vec![Diagnostic::at(
                source,
                original,
                1,
                "LT1002",
                "Declaration files may contain type declarations only.".to_owned(),
            )],
        });
    }
    Ok(())
}

#[derive(Default)]
struct DeclarationEraser {
    ranges: Vec<std::ops::Range<usize>>,
}

impl full_moon::visitors::Visitor for DeclarationEraser {
    fn visit_type_declaration(&mut self, node: &full_moon::ast::luau::TypeDeclaration) {
        let (start, end) = node.tokens().fold((None, None), |(start, end), token| {
            let start_byte = token.token().start_position().bytes();
            let end_byte = token.token().end_position().bytes();
            (
                Some(start.map_or(start_byte, |value: usize| value.min(start_byte))),
                Some(end.map_or(end_byte, |value: usize| value.max(end_byte))),
            )
        });
        if let (Some(start), Some(end)) = (start, end) {
            self.ranges.push(start..end);
        }
    }
}

fn erase_ranges(source: &str, ranges: &[std::ops::Range<usize>]) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for range in ranges {
        if range.end > bytes.len()
            || !source.is_char_boundary(range.start)
            || !source.is_char_boundary(range.end)
        {
            continue;
        }
        for byte in &mut bytes[range.clone()] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).expect("erasing source syntax preserves UTF-8")
}
