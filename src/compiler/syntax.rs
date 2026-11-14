use std::collections::HashSet;

use crate::{CompileError, Diagnostic};
use full_moon::{
    LuaVersion,
    ast::luau::{ExportedTypeDeclaration, TypeDeclaration, TypeField, TypeFieldKey, TypeInfo},
    node::Node,
    tokenizer::{Lexer, LexerResult, Symbol, Token, TokenType},
    visitors::Visitor,
};

#[derive(Clone)]
pub(crate) struct PreparedSource {
    pub text: String,
    pub coalesce_operators: HashSet<usize>,
    original_offsets: Vec<usize>,
}

impl PreparedSource {
    pub fn original_offset(&self, prepared_offset: usize) -> usize {
        self.original_offsets
            .get(prepared_offset)
            .copied()
            .unwrap_or_else(|| *self.original_offsets.last().unwrap_or(&0))
    }
}

pub(crate) fn prepare(source: &str) -> PreparedSource {
    let (expanded, template_offsets) = expand_templates(source);
    let typedefs = normalize_typedef_declarations(&expanded);
    let (arrays, array_offsets) = normalize_type_syntax(&typedefs, true);
    let (optional, optional_offsets) = normalize_optional_fields(&arrays);
    let (normalized, normalized_offsets) = normalize_type_syntax(&optional, false);
    let source_offsets = normalized_offsets
        .into_iter()
        .map(|offset| template_offsets[array_offsets[optional_offsets[offset]]])
        .collect::<Vec<_>>();
    let source = normalized.as_str();
    let version = LuaVersion::cfxlua().with_luau();
    let tokens = match Lexer::new(source, version).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => {
            return PreparedSource {
                text: source.to_owned(),
                coalesce_operators: HashSet::new(),
                original_offsets: source_offsets,
            };
        }
    };
    let operators = coalesce_spans(&tokens);
    let mut prepared = PreparedSource {
        text: String::with_capacity(source.len() + operators.len()),
        coalesce_operators: HashSet::with_capacity(operators.len()),
        original_offsets: Vec::with_capacity(source.len() + operators.len() + 1),
    };
    prepared.original_offsets.push(0);

    let mut cursor = 0;
    for (start, end) in operators {
        append_original(
            source,
            cursor..start,
            &mut prepared.text,
            &mut prepared.original_offsets,
            &source_offsets,
        );
        let needs_left_space = source[..start]
            .chars()
            .next_back()
            .is_some_and(is_identifier_character);
        let needs_right_space = source[end..]
            .chars()
            .next()
            .is_some_and(is_identifier_character);
        if needs_left_space {
            prepared.text.push(' ');
            prepared.original_offsets.push(source_offsets[start]);
        }
        prepared.coalesce_operators.insert(prepared.text.len());
        prepared.text.push_str("or");
        prepared.original_offsets.push(source_offsets[start]);
        prepared.original_offsets.push(source_offsets[end]);
        if needs_right_space {
            prepared.text.push(' ');
            prepared.original_offsets.push(source_offsets[end]);
        }
        cursor = end;
    }
    append_original(
        source,
        cursor..source.len(),
        &mut prepared.text,
        &mut prepared.original_offsets,
        &source_offsets,
    );
    prepared
}

#[derive(Default)]
struct TypeRanges {
    types: Vec<(usize, usize)>,
    integers: Vec<(usize, usize)>,
}

impl Visitor for TypeRanges {
    fn visit_type_info(&mut self, node: &TypeInfo) {
        let Some(range) = node
            .tokens()
            .fold(None, |range: Option<(usize, usize)>, token| {
                let start = token.token().start_position().bytes();
                let end = token.token().end_position().bytes();
                Some(range.map_or((start, end), |(a, b)| (a.min(start), b.max(end))))
            })
        else {
            return;
        };
        self.types.push(range);
        if matches!(node, TypeInfo::Basic(token) if token.token().to_string() == "integer") {
            self.integers.push(range);
        }
    }
}

fn replace_mapped(
    text: &mut String,
    offsets: &mut Vec<usize>,
    start: usize,
    end: usize,
    replacement: &str,
) {
    let anchor = offsets[start];
    let finish = offsets[end];
    text.replace_range(start..end, replacement);
    let mut mapping = vec![anchor; replacement.len()];
    if let Some(last) = mapping.last_mut() {
        *last = finish;
    }
    offsets.splice(start + 1..end + 1, mapping);
}

fn normalize_type_syntax(source: &str, arrays: bool) -> (String, Vec<usize>) {
    if (arrays
        && !source.match_indices('[').any(|(offset, _)| {
            let next = source[offset + 1..].trim_start();
            next.starts_with(']') || next.starts_with("--") || next.starts_with("/*")
        }))
        || (!arrays && !source.contains("integer"))
    {
        return (source.to_owned(), (0..=source.len()).collect());
    }
    let mut text = source.to_owned();
    let mut offsets = (0..=source.len()).collect::<Vec<_>>();
    loop {
        let tokens = significant_tokens(&text);
        if !arrays && !tokens.iter().any(|token| matches!(token.token_type(), TokenType::Identifier { identifier } if identifier.to_string() == "integer")) { break; }
        let candidates = tokens
            .windows(2)
            .enumerate()
            .filter(|&(index, pair)| {
                pair[0].to_string() == "[" && pair[1].to_string() == "]" && index > 0
            })
            .map(|(index, pair)| {
                (
                    tokens[index - 1].end_position().bytes(),
                    pair[0].start_position().bytes(),
                    pair[1].end_position().bytes(),
                )
            })
            .collect::<Vec<_>>();
        if arrays && candidates.is_empty() {
            break;
        }
        let mut masked = text.as_bytes().to_vec();
        if arrays {
            for &(_, start, end) in &candidates {
                for byte in &mut masked[start..end] {
                    if !matches!(*byte, b'\r' | b'\n') {
                        *byte = b' ';
                    }
                }
            }
            for offset in optional_field_markers(&tokens) {
                masked[offset] = b' ';
            }
        }
        let parsed = full_moon::parse_fallible(
            &String::from_utf8(masked).unwrap(),
            LuaVersion::cfxlua().with_luau(),
        );
        let mut ranges = TypeRanges::default();
        ranges.visit_ast(&parsed.into_ast());
        if !arrays {
            ranges.integers.sort_unstable();
            ranges.integers.dedup();
            for (start, end) in ranges.integers.into_iter().rev() {
                replace_mapped(&mut text, &mut offsets, start, end, "number");
            }
            break;
        }
        let next = candidates.into_iter().find_map(|(previous, start, end)| {
            ranges
                .types
                .iter()
                .filter(|(_, finish)| *finish == previous)
                .max_by_key(|(begin, _)| begin)
                .map(|(begin, _)| (*begin, start, end))
        });
        let Some((begin, start, end)) = next else {
            break;
        };
        // Keep trivia between the brackets, including comments and line breaks.
        replace_mapped(&mut text, &mut offsets, end - 1, end, "}");
        replace_mapped(&mut text, &mut offsets, start, start + 1, " ");
        replace_mapped(&mut text, &mut offsets, begin, begin, "{");
    }
    (text, offsets)
}

fn normalize_typedef_declarations(source: &str) -> String {
    if !source.contains("typedef") {
        return source.to_owned();
    }
    let declarations = type_alias_keyword_spans(source, "typedef");
    if declarations.is_empty() {
        return source.to_owned();
    }
    let mut bytes = source.as_bytes().to_vec();
    for (start, end) in declarations {
        bytes[start..end].copy_from_slice(b"type   ");
    }
    String::from_utf8(bytes).expect("keyword replacement preserves UTF-8")
}

fn type_alias_keyword_spans(source: &str, keyword: &str) -> Vec<(usize, usize)> {
    let tokens = significant_tokens(source);
    let candidates = tokens
        .windows(2)
        .enumerate()
        .filter_map(|(index, window)| {
            let is_keyword = matches!(window[0].token_type(), TokenType::Identifier { identifier } if identifier.to_string() == keyword);
            let has_name = matches!(window[1].token_type(), TokenType::Identifier { .. });
            if !is_keyword || !has_name {
                return None;
            }
            let after_name = index + 2;
            let has_equals = matches!(tokens.get(after_name).map(Token::token_type), Some(TokenType::Symbol { symbol: Symbol::Equal }))
                || (matches!(tokens.get(after_name).map(Token::token_type), Some(TokenType::Symbol { symbol: Symbol::LessThan }))
                    && tokens[after_name + 1..]
                        .iter()
                        .position(|token| matches!(token.token_type(), TokenType::Symbol { symbol: Symbol::GreaterThan }))
                        .is_some_and(|offset| matches!(tokens.get(after_name + offset + 2).map(Token::token_type), Some(TokenType::Symbol { symbol: Symbol::Equal }))));
            has_equals.then_some((
                window[0].start_position().bytes(),
                window[0].end_position().bytes(),
            ))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut normalized = source.as_bytes().to_vec();
    if keyword == "typedef" {
        for (start, end) in &candidates {
            normalized[*start..*end].copy_from_slice(b"type   ");
        }
    }
    // Recognize aliases before optional fields are rewritten, without moving offsets.
    for offset in optional_field_markers(&tokens) {
        normalized[offset] = b' ';
    }
    let Ok(normalized) = String::from_utf8(normalized) else {
        return Vec::new();
    };
    let mut declarations = TypeDeclarationSpans::default();
    declarations.visit_ast(
        &full_moon::parse_fallible(&normalized, LuaVersion::cfxlua().with_luau()).into_ast(),
    );
    candidates
        .into_iter()
        .filter(|(start, _)| {
            declarations
                .plain
                .iter()
                .any(|(declaration, _)| declaration == start)
        })
        .collect()
}

#[derive(Default)]
struct TypeDeclarationSpans {
    plain: Vec<(usize, usize)>,
    exported: Vec<(usize, usize, usize)>,
}

impl Visitor for TypeDeclarationSpans {
    fn visit_type_declaration(&mut self, declaration: &TypeDeclaration) {
        let token = declaration.type_token().token();
        self.plain
            .push((token.start_position().bytes(), token.end_position().bytes()));
    }

    fn visit_exported_type_declaration(&mut self, declaration: &ExportedTypeDeclaration) {
        let start = declaration.export_token().token().start_position().bytes();
        let type_token = declaration.type_declaration().type_token().token();
        let type_start = type_token.start_position().bytes();
        let end = type_token.end_position().bytes();
        self.exported.push((start, type_start, end));
    }
}

fn significant_tokens(source: &str) -> Vec<Token> {
    match Lexer::new(source, LuaVersion::cfxlua().with_luau()).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => Vec::new(),
    }
    .into_iter()
    .filter(|token| {
        !matches!(
            token.token_type(),
            TokenType::Whitespace { .. }
                | TokenType::SingleLineComment { .. }
                | TokenType::MultiLineComment { .. }
                | TokenType::CStyleComment { .. }
        )
    })
    .collect()
}

pub(crate) fn invalid_type_alias_error(source: &str) -> Option<CompileError> {
    if !source.contains("type") {
        return None;
    }
    let mut diagnostics = type_alias_keyword_spans(source, "type")
        .into_iter()
        .map(|(start, end)| {
            Diagnostic::range(
                source,
                start,
                end,
                "LT1001",
                "Invalid type alias syntax.".to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let typedef_candidates = type_alias_keyword_spans(source, "typedef");
    if !typedef_candidates.is_empty() {
        let mut normalized = source.as_bytes().to_vec();
        for (start, end) in &typedef_candidates {
            normalized[*start..*end].copy_from_slice(b"type   ");
        }
        if let Ok(normalized) = String::from_utf8(normalized) {
            let mut declarations = TypeDeclarationSpans::default();
            declarations.visit_ast(
                &full_moon::parse_fallible(&normalized, LuaVersion::cfxlua().with_luau())
                    .into_ast(),
            );
            for (start, type_start, end) in declarations.exported {
                if typedef_candidates
                    .iter()
                    .any(|(candidate, _)| *candidate == type_start)
                {
                    diagnostics.push(Diagnostic::range(
                        source,
                        start,
                        end,
                        "LT1002",
                        "Exported type declarations are not supported.".to_owned(),
                    ));
                }
            }
        }
    }
    (!diagnostics.is_empty()).then_some(CompileError { diagnostics })
}

fn expand_templates(source: &str) -> (String, Vec<usize>) {
    if !source.contains('`') {
        return (source.to_owned(), (0..=source.len()).collect());
    }
    let mut output = String::with_capacity(source.len());
    let mut offsets = vec![0];
    let mut cursor = 0;
    let mut i = 0;
    while i < source.len() {
        let bytes = source.as_bytes();
        if bytes[i] == b'\'' || bytes[i] == b'"' {
            let end = quoted_end(source, i, bytes[i]);
            i = end;
            continue;
        }
        if source[i..].starts_with("--") {
            let comment_start = i + 2;
            i = long_bracket_end(source, comment_start).unwrap_or_else(|| {
                source[comment_start..]
                    .find('\n')
                    .map_or(source.len(), |offset| comment_start + offset)
            });
            continue;
        }
        if source[i..].starts_with("/*") {
            i = c_style_comment_end(source, i);
            continue;
        }
        if let Some(end) = long_bracket_end(source, i) {
            i = end;
            continue;
        }
        if bytes[i] == b'`'
            && let Some(n) = source[i + 1..].find('`')
        {
            let end = i + 1 + n;
            let body = &source[i + 1..end];
            if body.contains('{') {
                append_source(source, cursor..i, 0, &mut output, &mut offsets);
                *offsets.last_mut().unwrap() = i;
                let (expanded, mut expanded_offsets) = expand_template_body(body, i + 1);
                output.push_str(&expanded);
                offsets.extend(expanded_offsets.drain(1..));
                *offsets.last_mut().unwrap() = end + 1;
                cursor = end + 1;
            }
            i = end + 1;
            continue;
        }
        let c = source[i..].chars().next().unwrap();
        i += c.len_utf8();
    }
    append_source(source, cursor..source.len(), 0, &mut output, &mut offsets);
    (output, offsets)
}

fn quoted_end(s: &str, start: usize, q: u8) -> usize {
    let mut i = start + 1;
    while i < s.len() {
        if s.as_bytes()[i] == b'\\' {
            i = (i + 2).min(s.len());
        } else if s.as_bytes()[i] == q {
            return i + 1;
        } else {
            i += 1
        }
    }
    s.len()
}

fn long_bracket_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
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
    Some(
        source[cursor + 1..]
            .find(&closing)
            .map_or(source.len(), |offset| cursor + 1 + offset + closing.len()),
    )
}

fn c_style_comment_end(source: &str, start: usize) -> usize {
    source[start + 2..]
        .find("*/")
        .map_or(source.len(), |offset| start + 2 + offset + 2)
}

fn expand_template_body(body: &str, source_start: usize) -> (String, Vec<usize>) {
    let mut output = String::new();
    let mut offsets = vec![source_start];
    push_generated(&mut output, &mut offsets, "(", source_start);
    let mut part_count = 0;
    let mut pos = 0;
    while let Some(n) = body[pos..].find('{') {
        let open = pos + n;
        let text_end = open;
        if text_end > pos {
            if part_count > 0 {
                push_generated(&mut output, &mut offsets, " .. ", source_start + pos);
            }
            append_template_literal(body, pos..text_end, source_start, &mut output, &mut offsets);
            part_count += 1;
        }
        let start = open + 1;
        let close = find_interpolation_end(body, start).unwrap_or(body.len());
        if start == close {
            if part_count > 0 {
                push_generated(&mut output, &mut offsets, " .. ", source_start + open);
            }
            push_generated(&mut output, &mut offsets, "tostring((", source_start + open);
            push_generated(&mut output, &mut offsets, "))", source_start + close);
            part_count += 1;
            pos = close.saturating_add(1);
            continue;
        }
        if part_count > 0 {
            push_generated(&mut output, &mut offsets, " .. ", source_start + open);
        }
        push_generated(&mut output, &mut offsets, "tostring((", source_start + open);
        *offsets.last_mut().unwrap() = source_start + start;
        append_source(body, start..close, source_start, &mut output, &mut offsets);
        push_generated(&mut output, &mut offsets, "))", source_start + close);
        part_count += 1;
        pos = close.saturating_add(1);
    }
    if pos < body.len() {
        if part_count > 0 {
            push_generated(&mut output, &mut offsets, " .. ", source_start + pos);
        }
        append_template_literal(
            body,
            pos..body.len(),
            source_start,
            &mut output,
            &mut offsets,
        );
        part_count += 1;
    }
    if part_count == 0 {
        push_generated(&mut output, &mut offsets, "''", source_start);
    }
    push_generated(&mut output, &mut offsets, ")", source_start + body.len());
    (output, offsets)
}

fn append_template_literal(
    body: &str,
    range: std::ops::Range<usize>,
    source_start: usize,
    output: &mut String,
    offsets: &mut Vec<usize>,
) {
    let start = source_start + range.start;
    let end = source_start + range.end;
    push_generated(output, offsets, "'", start);
    for (offset, character) in body[range.clone()].char_indices() {
        let source_character_start = source_start + range.start + offset;
        let source_character_end = source_character_start + character.len_utf8();
        let mut buffer = [0; 4];
        let escaped = match character {
            '\\' => "\\\\",
            '\'' => "\\'",
            '\n' => "\\n",
            '\r' => "\\r",
            _ => character.encode_utf8(&mut buffer),
        };
        output.push_str(escaped);
        offsets.extend(std::iter::repeat_n(
            source_character_start,
            escaped.len().saturating_sub(1),
        ));
        offsets.push(source_character_end);
    }
    push_generated(output, offsets, "'", end);
}

fn push_generated(output: &mut String, offsets: &mut Vec<usize>, text: &str, anchor: usize) {
    output.push_str(text);
    offsets.extend(std::iter::repeat_n(anchor, text.len()));
}

fn append_source(
    source: &str,
    range: std::ops::Range<usize>,
    source_offset: usize,
    output: &mut String,
    offsets: &mut Vec<usize>,
) {
    output.push_str(&source[range.clone()]);
    offsets.extend((range.start + 1..=range.end).map(|offset| source_offset + offset));
}

fn find_interpolation_end(s: &str, start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = start;
    while i < s.len() {
        let b = s.as_bytes()[i];
        if b == b'\'' || b == b'"' {
            i = quoted_end(s, i, b);
            continue;
        }
        if s[i..].starts_with("--") {
            let comment_start = i + 2;
            i = long_bracket_end(s, comment_start).unwrap_or_else(|| {
                s[comment_start..]
                    .find('\n')
                    .map_or(s.len(), |offset| comment_start + offset)
            });
            continue;
        }
        if s[i..].starts_with("/*") {
            i = c_style_comment_end(s, i);
            continue;
        }
        if let Some(end) = long_bracket_end(s, i) {
            i = end;
            continue;
        }
        match b {
            b'{' => depth += 1,
            b'}' if depth == 0 => return Some(i),
            b'}' => depth -= 1,
            _ => {}
        }
        i += s[i..].chars().next().unwrap().len_utf8();
    }
    None
}

fn append_original(
    source: &str,
    range: std::ops::Range<usize>,
    output: &mut String,
    offsets: &mut Vec<usize>,
    source_offsets: &[usize],
) {
    output.push_str(&source[range.clone()]);
    offsets.extend((range.start + 1..=range.end).map(|index| source_offsets[index]));
}

#[derive(Default)]
struct TypeFields(Vec<(usize, usize, usize)>);

impl Visitor for TypeFields {
    fn visit_type_field(&mut self, field: &TypeField) {
        if let TypeFieldKey::Name(key) = field.key() {
            let key_end = key.token().end_position().bytes();
            let value_range =
                field
                    .value()
                    .tokens()
                    .fold(None, |range: Option<(usize, usize)>, token| {
                        let start = token.token().start_position().bytes();
                        let end = token.token().end_position().bytes();
                        Some(range.map_or((start, end), |(a, b)| (a.min(start), b.max(end))))
                    });
            if let Some((start, end)) = value_range {
                self.0.push((key_end, start, end));
            }
        }
    }
}

fn optional_field_markers(tokens: &[Token]) -> Vec<usize> {
    tokens
        .windows(3)
        .filter(|window| {
            matches!(window[0].token_type(), TokenType::Identifier { .. })
                && is_question_mark(&window[1])
                && matches!(
                    window[2].token_type(),
                    TokenType::Symbol {
                        symbol: Symbol::Colon
                    }
                )
        })
        .map(|window| window[1].start_position().bytes())
        .collect()
}

fn normalize_optional_fields(source: &str) -> (String, Vec<usize>) {
    let identity = || (source.to_owned(), (0..=source.len()).collect::<Vec<_>>());
    let tokens = match Lexer::new(source, LuaVersion::cfxlua().with_luau()).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => return identity(),
    };
    let candidates = optional_field_markers(&tokens);
    if candidates.is_empty() {
        return identity();
    }
    let mut masked = source.as_bytes().to_vec();
    for &offset in &candidates {
        masked[offset] = b' ';
    }
    let parsed = full_moon::parse_fallible(
        &String::from_utf8(masked).unwrap(),
        LuaVersion::cfxlua().with_luau(),
    );
    let mut fields = TypeFields::default();
    fields.visit_ast(&parsed.into_ast());
    let fields = fields
        .0
        .into_iter()
        .filter_map(|(key_end, start, end)| {
            candidates
                .iter()
                .copied()
                .find(|offset| *offset >= key_end && *offset < start)
                .map(|question| (question, start, end))
        })
        .collect::<Vec<_>>();
    if fields.is_empty() {
        return identity();
    }
    let mut opens = HashSet::new();
    let mut closes = HashSet::new();
    let mut questions = HashSet::new();
    for (question, start, end) in fields {
        opens.insert(start);
        closes.insert(end);
        questions.insert(question);
    }
    let mut output = String::with_capacity(source.len() + opens.len() * 3);
    let mut offsets = vec![0];
    for (index, character) in source.char_indices() {
        if opens.contains(&index) {
            output.push('(');
            offsets.push(index);
        }
        let end = index + character.len_utf8();
        output.push(if questions.contains(&index) {
            ' '
        } else {
            character
        });
        offsets.extend(std::iter::repeat_n(end, character.len_utf8()));
        if closes.contains(&end) {
            output.push_str(")?");
            offsets.extend([end, end]);
        }
    }
    (output, offsets)
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric() || !character.is_ascii()
}

fn coalesce_spans(tokens: &[Token]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index + 1 < tokens.len() {
        let first = &tokens[index];
        let second = &tokens[index + 1];
        let first_start = first.start_position().bytes();
        let first_end = first.end_position().bytes();
        let second_start = second.start_position().bytes();
        if is_question_mark(first) && is_question_mark(second) && first_end == second_start {
            let extends_left = index > 0 && is_question_mark(&tokens[index - 1]);
            let extends_right = index + 2 < tokens.len() && is_question_mark(&tokens[index + 2]);
            if !extends_left && !extends_right {
                spans.push((first_start, second.end_position().bytes()));
                index += 2;
                continue;
            }
        }
        index += 1;
    }
    spans
}

fn is_question_mark(token: &Token) -> bool {
    matches!(
        token.token_type(),
        TokenType::Symbol {
            symbol: Symbol::QuestionMark
        }
    )
}

#[cfg(test)]
mod tests {
    use super::{expand_templates, invalid_type_alias_error, prepare};

    #[test]
    fn normalizes_typedef_declarations_without_moving_source_ranges() {
        let source = "typedef Player = { name: string? }\ntypedef List<T> = { T }";
        let prepared = prepare(source);

        assert!(prepared.text.contains("type    Player"));
        assert!(
            prepared.text.contains("type    List<T>"),
            "{}",
            prepared.text
        );
        for name in ["Player", "List"] {
            let offset = prepared.text.find(name).unwrap();
            assert_eq!(prepared.original_offset(offset), source.find(name).unwrap());
        }
    }

    #[test]
    fn normalizes_typedef_with_nested_optional_callback_fields() {
        let source = "typedef Api = { new: (options: { extend?: (d: number) -> any }) -> any }";
        let prepared = prepare(source);
        assert!(
            full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
                .errors()
                .is_empty(),
            "{}",
            prepared.text
        );
        let offset = prepared.text.find("extend").unwrap();
        assert_eq!(
            prepared.original_offset(offset),
            source.find("extend").unwrap()
        );
        assert!(invalid_type_alias_error(&source.replacen("typedef", "type", 1)).is_some());
    }

    #[test]
    fn does_not_treat_a_line_broken_type_call_as_a_type_alias() {
        assert!(invalid_type_alias_error("local f = type\nx = 1").is_none());
        assert!(invalid_type_alias_error("type Player = { name: string }").is_some());
        assert!(invalid_type_alias_error("type\nPlayer = { name: string }").is_some());
        assert!(invalid_type_alias_error("function f() type Player = number end").is_some());
        assert!(invalid_type_alias_error("local export = true\ntypedef Player = number").is_none());
        assert!(
            prepare("function f() typedef Player = number end")
                .text
                .contains("function f() type    Player = number end")
        );
    }

    #[test]
    fn expands_template_interpolation_without_dollar_prefix() {
        let prepared = prepare("local message = `hello {player.name}`");
        assert!(
            prepared
                .text
                .contains("'hello ' .. tostring((player.name))")
        );
    }

    #[test]
    fn treats_dollar_before_template_interpolation_as_literal_text() {
        let prepared = prepare("local message = `hello ${player.name}`");
        assert!(
            prepared
                .text
                .contains("'hello $' .. tostring((player.name))")
        );
    }

    #[test]
    fn normalizes_coalescing_without_touching_comments_or_strings() {
        let source = "local a=x??y\n-- x ?? y\nlocal b='??'\nlocal c=z?.value";
        let prepared = prepare(source);
        assert!(prepared.text.contains("x or y"));
        assert!(prepared.text.contains("-- x ?? y"));
        assert!(prepared.text.contains("'??'"));
        assert!(prepared.text.contains("z?.value"));
        assert_eq!(prepared.coalesce_operators.len(), 1);
    }

    #[test]
    fn maps_offsets_after_expanding_templates_to_the_original_source() {
        for source in [
            "typedef P = { name: string }\nlocal text = `hello {name}`\nlocal bad: P = {}\n-- trailing comment",
            "typedef P = { name: string, maybe?: number }\nlocal a = `ü {name}`\nlocal b = `value ${name}`\nlocal fallback = item ?? nil\nlocal bad: P = {}\n-- tail",
        ] {
            let (expanded, template_offsets) = expand_templates(source);
            assert_eq!(template_offsets.len(), expanded.len() + 1);
            let prepared = prepare(source);
            let prepared_record = prepared.text.rfind("{}").unwrap();
            let original_record = source.rfind("{}").unwrap();

            assert_eq!(prepared.original_offset(prepared_record), original_record);
        }
    }

    #[test]
    fn maps_coalescing_offsets_after_expanding_templates() {
        let source = "local text = `hello {name}`\nlocal value = a??b";
        let prepared = prepare(source);
        let operator = prepared.coalesce_operators.iter().next().unwrap();
        assert_eq!(
            prepared.original_offset(*operator),
            source.find("??").unwrap()
        );
        assert_eq!(
            prepared.original_offset(*operator + 2),
            source.find("??").unwrap() + 2
        );
        let right = prepared.text.rfind('b').unwrap();
        assert_eq!(prepared.original_offset(right), source.rfind('b').unwrap());
    }

    #[test]
    fn preserves_interpolation_and_multiline_template_boundaries() {
        let source =
            "local a = `ü {first} and $ {second}`\nlocal b = `line one\n{third}`\n-- trailing";
        let (expanded, offsets) = expand_templates(source);

        for value in ["first", "second", "third"] {
            let expanded_start = expanded.find(value).unwrap();
            let source_start = source.find(value).unwrap();
            assert_eq!(offsets[expanded_start], source_start, "{value}");
            assert_eq!(
                offsets[expanded_start + value.len()],
                source_start + value.len(),
                "{value}"
            );
        }
        let prepared = prepare(source);
        let expanded_comment = prepared.text.find("-- trailing").unwrap();
        assert_eq!(
            prepared.original_offset(expanded_comment),
            source.find("-- trailing").unwrap()
        );
    }

    #[test]
    fn does_not_expand_templates_inside_lua_long_strings_or_comments() {
        let source = "local text = [=[ `ignored {value}` ]=]\n--[==[ `ignored {value}` ]==]\nlocal actual = `hello {name}`";
        let (expanded, _) = expand_templates(source);

        assert!(expanded.contains("[=[ `ignored {value}` ]=]"));
        assert!(expanded.contains("--[==[ `ignored {value}` ]==]"));
        assert!(expanded.contains("'hello ' .. tostring((name))"));
    }

    #[test]
    fn does_not_expand_templates_inside_c_style_comments() {
        let source = "local ignored = 1 /* `ignored {value}` */\nlocal actual = `{(function() /* } ignored */ return 1 end)()}`";
        let (expanded, _) = expand_templates(source);

        assert!(expanded.contains("/* `ignored {value}` */"));
        assert!(expanded.contains("/* } ignored */ return 1 end)()"));
    }

    #[test]
    fn interpolation_end_skips_braces_inside_lua_comments_and_long_strings() {
        let source = "local text = `{(function() -- } ignored\n return [=[ } ]=] end)()}`";
        let (expanded, _) = expand_templates(source);

        assert!(expanded.contains("-- } ignored\n return [=[ } ]=] end)()"));
        assert!(expanded.ends_with(')'));
        assert!(expanded.contains("tostring(((function()"));
    }

    #[test]
    fn empty_interpolation_keeps_template_text_after_it() {
        let source = "local text = `prefix{}suffix`";
        let prepared = prepare(source);

        assert!(prepared.text.contains("prefix"));
        assert!(prepared.text.contains("suffix"));
        assert!(prepared.text.contains("tostring(()"));
        assert!(crate::compile_source(source).is_err());
    }
}
