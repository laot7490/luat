use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use full_moon::{
    ast::{self, Call, Expression, FunctionArgs, Prefix, Suffix},
    node::Node,
    tokenizer::{StringLiteralQuoteType, TokenReference, TokenType},
    visitors::Visitor,
};

use super::syntax::PreparedSource;
use crate::{CompileError, Diagnostic};

#[derive(Clone, Debug)]
pub(crate) struct ImportCall {
    pub source_range: Range<usize>,
    pub specifier: String,
    name_range: Range<usize>,
    argument_range: Range<usize>,
}

fn range(token: &TokenReference) -> Range<usize> {
    token.token().start_position().bytes()..token.token().end_position().bytes()
}

fn string_value(token: &TokenReference) -> Option<String> {
    let TokenType::StringLiteral {
        literal,
        quote_type,
        ..
    } = token.token().token_type()
    else {
        return None;
    };
    if *quote_type == StringLiteralQuoteType::Brackets {
        let value = literal.replace("\r\n", "\n").replace('\r', "\n");
        return Some(value.strip_prefix('\n').unwrap_or(&value).to_owned());
    }
    if !matches!(
        quote_type,
        StringLiteralQuoteType::Single | StringLiteralQuoteType::Double
    ) {
        return None;
    }
    let mut bytes = literal.bytes().peekable();
    let mut result = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            result.push(byte);
            continue;
        }
        match bytes.next()? {
            b'a' => result.push(7),
            b'b' => result.push(8),
            b'f' => result.push(12),
            b'n' => result.push(b'\n'),
            b'r' => result.push(b'\r'),
            b't' => result.push(b'\t'),
            b'v' => result.push(11),
            byte @ (b'\\' | b'\'' | b'"') => result.push(byte),
            b'\n' => result.push(b'\n'),
            b'\r' => {
                if bytes.peek() == Some(&b'\n') {
                    bytes.next();
                }
                result.push(b'\n');
            }
            b'z' => {
                while bytes.peek().is_some_and(u8::is_ascii_whitespace) {
                    bytes.next();
                }
            }
            b'x' => {
                let high = (bytes.next()? as char).to_digit(16)?;
                let low = (bytes.next()? as char).to_digit(16)?;
                result.push((high * 16 + low) as u8);
            }
            b'u' => {
                if bytes.next()? != b'{' {
                    return None;
                }
                let mut value = 0u32;
                let mut digits = 0;
                loop {
                    let byte = bytes.next()?;
                    if byte == b'}' {
                        break;
                    }
                    value = value
                        .checked_mul(16)?
                        .checked_add((byte as char).to_digit(16)?)?;
                    digits += 1;
                }
                if digits == 0 {
                    return None;
                }
                let character = char::from_u32(value)?;
                result.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
            }
            byte @ b'0'..=b'9' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    if !bytes.peek().is_some_and(u8::is_ascii_digit) {
                        break;
                    }
                    value = value * 10 + u16::from(bytes.next()? - b'0');
                }
                result.push(u8::try_from(value).ok()?);
            }
            _ => return None,
        }
    }
    String::from_utf8(result).ok()
}

#[derive(Default)]
struct Collector {
    calls: Vec<ImportCall>,
    allowed_names: HashSet<usize>,
}

impl Collector {
    fn call(&mut self, prefix: &Prefix, first_suffix: Option<&Suffix>) {
        let Prefix::Name(name) = prefix else { return };
        if name.token().to_string() != "import" {
            return;
        }
        let Some(Suffix::Call(Call::AnonymousCall(args))) = first_suffix else {
            return;
        };
        let FunctionArgs::Parentheses { arguments, .. } = args.as_ref() else {
            return;
        };
        if arguments.len() != 1 {
            return;
        }
        let Some(Expression::String(token)) = arguments.iter().next() else {
            return;
        };
        let Some(specifier) = string_value(token) else {
            return;
        };
        self.allowed_names.insert(range(name).start);
        self.calls.push(ImportCall {
            source_range: range(name).start..args.end_position().unwrap().bytes(),
            specifier,
            name_range: range(name),
            argument_range: range(token),
        });
    }
}

impl Visitor for Collector {
    fn visit_field(&mut self, field: &ast::Field) {
        if let ast::Field::NameKey { key, .. } = field {
            self.allowed_names.insert(range(key).start);
        }
    }

    fn visit_function_call(&mut self, call: &ast::FunctionCall) {
        self.call(call.prefix(), call.suffixes().next());
    }

    fn visit_var_expression(&mut self, expression: &ast::VarExpression) {
        self.call(expression.prefix(), expression.suffixes().next());
    }
}

pub(crate) fn analyze(
    source: &str,
    prepared: &PreparedSource,
    ast: &ast::Ast,
    project_context: bool,
) -> Result<Vec<ImportCall>, CompileError> {
    let mut collector = Collector::default();
    collector.visit_ast(ast);
    let mut diagnostics = Vec::new();
    let mut previous = String::new();
    for token in ast.tokens() {
        let text = token.token().to_string();
        if matches!(token.token().token_type(), TokenType::Identifier { .. })
            && matches!(text.as_str(), "import" | "__luat")
            && !matches!(previous.as_str(), "." | ":" | "?.")
            && !collector.allowed_names.contains(&range(token).start)
        {
            let message = if text == "import" {
                "'import' is reserved; use a direct import('literal') call with exactly one argument"
            } else {
                "'__luat' is reserved for the LuaT module runtime"
            };
            diagnostics.push(Diagnostic::range(
                source,
                prepared.original_offset(range(token).start),
                prepared.original_offset(range(token).end),
                "LT4001",
                message.to_owned(),
            ));
        }
        previous = text;
    }
    collector.calls.sort_by_key(|call| call.source_range.start);
    for call in &mut collector.calls {
        for span in [
            &mut call.source_range,
            &mut call.name_range,
            &mut call.argument_range,
        ] {
            *span = prepared.original_offset(span.start)..prepared.original_offset(span.end);
        }
        if !project_context {
            diagnostics.push(Diagnostic::range(
                source,
                call.name_range.start,
                call.name_range.end,
                "LT4007",
                "LuaT imports require a project; use Project::check/build or the LuaT CLI"
                    .to_owned(),
            ));
        }
    }
    diagnostics.sort_by_key(|diagnostic| diagnostic.source_range.start);
    if diagnostics.is_empty() {
        Ok(collector.calls)
    } else {
        Err(CompileError { diagnostics })
    }
}

pub(crate) fn rewrite(
    source: &str,
    resolved: &HashMap<String, String>,
) -> Result<String, CompileError> {
    let prepared = super::syntax::prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let calls = analyze(source, &prepared, &ast, true)?;
    let mut edits = Vec::new();
    for call in calls {
        let Some(id) = resolved.get(&call.specifier) else {
            return Err(CompileError {
                diagnostics: vec![Diagnostic::range(
                    source,
                    call.source_range.start,
                    call.source_range.end,
                    "LT4003",
                    "Import target was not resolved".to_owned(),
                )],
            });
        };
        edits.push((call.name_range, "__luat.import".to_owned()));
        edits.push((call.argument_range, lua_string(id)));
    }
    edits.sort_by_key(|(span, _)| std::cmp::Reverse(span.start));
    let mut output = source.to_owned();
    for (span, replacement) in edits {
        output.replace_range(span, &replacement);
    }
    Ok(output)
}

pub(crate) fn lua_string(value: &str) -> String {
    let mut result = String::from("'");
    for byte in value.bytes() {
        match byte {
            b'\'' => result.push_str("\\'"),
            b'\\' => result.push_str("\\\\"),
            32..=126 => result.push(byte as char),
            _ => result.push_str(&format!("\\{byte:03}")),
        }
    }
    result.push('\'');
    result
}
