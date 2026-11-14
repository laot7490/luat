use super::aliases::*;
use super::*;

pub(super) fn check_if_expression_assignability(
    actual: &str,
    expected: &str,
    aliases: &HashMap<String, TypeAlias>,
    infer: &dyn Fn(&str) -> String,
    depth: usize,
) -> Option<Result<(), Vec<TypeMismatch>>> {
    let mut candidate = actual.trim();
    while candidate.starts_with('(')
        && matching_delimiter(candidate, 0, '(', ')') == Some(candidate.len() - 1)
    {
        candidate = candidate[1..candidate.len() - 1].trim();
    }
    if !candidate.starts_with("if ") {
        return None;
    }
    let prefix = "local __luat_if = ";
    let parsed = full_moon::parse_fallible(
        &format!("{prefix}{actual}"),
        LuaVersion::cfxlua().with_luau(),
    );
    if !parsed.errors().is_empty() {
        return None;
    }
    let mut initializer = InitializerCollector::default();
    initializer.visit_ast(&parsed.into_ast());
    let expression = unwrap_parentheses(initializer.expression?);
    let Expression::IfExpression(value) = expression else {
        return None;
    };
    if depth > 128 {
        return Some(Err(vec![TypeMismatch {
            field_path: Vec::new(),
            range: 0..1,
            kind: TypeMismatchKind::TooDeep,
        }]));
    }

    let mut branches = vec![value.if_expression()];
    if let Some(else_ifs) = value.else_if_expressions() {
        branches.extend(else_ifs.iter().map(|branch| branch.expression()));
    }
    branches.push(value.else_expression());
    let mut mismatches = Vec::new();
    for branch in branches {
        let Some((branch_start, branch_end)) = relative_node_range(branch, prefix.len()) else {
            continue;
        };
        let branch_source = branch.to_string();
        let branch_type = infer(&branch_source);
        let nested_if = matches!(branch, Expression::IfExpression(_))
            || matches!(branch, Expression::Parentheses { expression, .. } if matches!(expression.as_ref(), Expression::IfExpression(_)));
        let result = if nested_if {
            check_if_expression_assignability(&branch_source, expected, aliases, infer, depth + 1)
        } else if branch_type == "table" {
            check_table_assignability_at_depth(&branch_source, expected, aliases, infer, depth + 1)
        } else {
            is_assignable(&branch_type, expected, aliases, &mut Vec::new()).map(|okay| {
                if okay {
                    Ok(())
                } else {
                    Err(vec![TypeMismatch {
                        field_path: Vec::new(),
                        range: branch_start..branch_end,
                        kind: TypeMismatchKind::IncompatibleType {
                            actual: branch_type.clone(),
                            expected: expected.to_owned(),
                        },
                    }])
                }
            })
        };
        if let Some(Err(mut nested)) = result {
            if branch_type == "table" || nested_if {
                for mismatch in &mut nested {
                    mismatch.range.start += branch_start;
                    mismatch.range.end += branch_start;
                }
            }
            mismatches.extend(nested);
        }
    }
    Some(if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches)
    })
}

pub(super) fn unwrap_parentheses(mut expression: Expression) -> Expression {
    loop {
        match expression {
            Expression::Parentheses {
                expression: inner, ..
            } => expression = *inner,
            _ => return expression,
        }
    }
}

pub(super) fn relative_node_range<T: Node>(node: &T, prefix_len: usize) -> Option<(usize, usize)> {
    let (start, end) = node
        .tokens()
        .fold(None, |range: Option<(usize, usize)>, token| {
            let start = token.token().start_position().bytes();
            let end = token.token().end_position().bytes();
            Some(range.map_or((start, end), |(first, last)| {
                (first.min(start), last.max(end))
            }))
        })?;
    Some((start.checked_sub(prefix_len)?, end.checked_sub(prefix_len)?))
}

pub(super) fn matching_delimiter(
    input: &str,
    open: usize,
    left: char,
    right: char,
) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    for (index, ch) in input.char_indices().filter(|(index, _)| *index >= open) {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch == left => depth += 1,
            ch if ch == right => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn infer_literal_type(expression: &str) -> Option<&'static str> {
    let expression = expression.trim();
    match expression {
        "nil" => Some("nil"),
        "true" | "false" => Some("boolean"),
        _ if expression.starts_with(['\'', '"']) => Some("string"),
        _ if expression.starts_with('{') => Some("table"),
        _ if expression.parse::<i64>().is_ok() => Some("number"),
        _ if expression.parse::<f64>().is_ok() => Some("number"),
        _ if expression.starts_with("function") => Some("function"),
        _ => None,
    }
}

pub(super) fn widen_inferred_type(type_name: &str) -> String {
    match type_name {
        "true" | "false" => "boolean".to_owned(),
        "integer" | "number" => "number".to_owned(),
        value if value.starts_with(['\'', '"']) => "string".to_owned(),
        value => value.to_owned(),
    }
}

pub(super) fn is_assignable(
    actual: &str,
    expected: &str,
    aliases: &HashMap<String, TypeAlias>,
    seen: &mut Vec<String>,
) -> Option<bool> {
    let expected = expected.trim();
    let expected = strip_wrapping_parentheses(expected);
    let actual = actual.trim();
    let actual_owned;
    let actual = if let Some(inner) = actual
        .strip_suffix('?')
        .filter(|_| split_once_top_level(actual, '-').is_none())
    {
        actual_owned = format!("{inner} | nil");
        actual_owned.as_str()
    } else {
        actual
    };
    let actual_union = split_union(actual);
    if actual_union.len() > 1 {
        let mut uncertain = false;
        for member in actual_union {
            match is_assignable(member, expected, aliases, &mut seen.clone()) {
                Some(false) => return Some(false),
                None => uncertain = true,
                Some(true) => {}
            }
        }
        return if uncertain { None } else { Some(true) };
    }
    if let Some(inner) = expected
        .strip_suffix('?')
        .filter(|_| split_once_top_level(expected, '-').is_none())
    {
        return if actual == "nil" {
            Some(true)
        } else {
            is_assignable(actual, inner, aliases, seen)
        };
    }
    if split_union(expected).len() == 1
        && (invalid_generic_arity(actual, aliases) || invalid_generic_arity(expected, aliases))
    {
        return Some(false);
    }
    if let Some(actual_vector) = vector_dimension(actual, aliases)
        && let Some(expected_vector) = vector_dimension(expected, aliases)
    {
        return Some(actual_vector == expected_vector);
    }
    if actual == "any" || expected == "any" || expected == "unknown" || expected == actual {
        return Some(true);
    }
    if actual == "nil" && expected.starts_with(['\'', '"']) {
        return Some(false);
    }
    if actual.starts_with(['\'', '"']) {
        if expected == "string" {
            return Some(true);
        }
        if expected.starts_with(['\'', '"']) && split_union(expected).len() == 1 {
            return Some(lua_string_value(actual) == lua_string_value(expected));
        }
    }
    if expected.starts_with(['\'', '"'])
        && matches!(actual, "nil" | "string" | "number" | "integer" | "boolean")
    {
        return Some(false);
    }
    let union = split_union(expected);
    if union.len() > 1 {
        if union.iter().all(|member| member.starts_with(['\'', '"'])) {
            return if actual.starts_with(['\'', '"']) {
                Some(union.iter().any(|member| {
                    is_assignable(actual, member, aliases, &mut seen.clone()) == Some(true)
                }))
            } else if matches!(actual, "nil" | "string" | "number" | "integer" | "boolean") {
                Some(false)
            } else {
                None
            };
        }
        let mut uncertain = false;
        for member in union {
            match is_assignable(actual, member, aliases, &mut seen.clone()) {
                Some(true) => return Some(true),
                Some(false) => {}
                None => uncertain = true,
            }
        }
        return if uncertain { None } else { Some(false) };
    }
    let intersections = split_top_level(expected, '&');
    if intersections.len() > 1 {
        let definitions = intersections
            .iter()
            .map(|member| {
                let definition =
                    resolve_alias_chain(member, aliases).unwrap_or_else(|| (*member).to_owned());
                let definition = definition.trim();
                definition
                    .strip_prefix('(')
                    .and_then(|definition| definition.strip_suffix(')'))
                    .unwrap_or(definition)
                    .to_owned()
            })
            .collect::<Vec<_>>();
        if definitions
            .iter()
            .all(|definition| definition.starts_with('{') && definition.ends_with('}'))
        {
            let fields = definitions
                .iter()
                .flat_map(|definition| {
                    split_table_type_fields(&definition[1..definition.len() - 1])
                        .into_iter()
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>();
            let combined = format!("{{ {} }}", fields.join(", "));
            return is_assignable(actual, &combined, aliases, seen);
        }

        let mut uncertain = false;
        for member in intersections {
            match is_assignable(actual, member, aliases, &mut seen.clone()) {
                Some(false) => return Some(false),
                None => uncertain = true,
                Some(true) => {}
            }
        }
        return if uncertain { None } else { Some(true) };
    }

    if expected == "number" {
        return Some(matches!(actual, "number" | "integer"));
    }
    if matches!(
        expected,
        "integer" | "string" | "boolean" | "nil" | "table" | "function"
    ) {
        return Some(expected == actual || (expected == "table" && actual.starts_with('{')));
    }
    if expected.starts_with('{') {
        if actual.starts_with('{') {
            return record_type_assignable(actual, expected, aliases, seen);
        }
        if let Some(alias) = resolve_alias(actual, aliases) {
            let name = actual.split('<').next().unwrap_or(actual).trim();
            if seen.iter().any(|seen_name| seen_name == name) {
                return None;
            }
            seen.push(name.to_owned());
            return is_assignable(&alias, expected, aliases, seen);
        }
        return Some(false);
    }
    if let Some(alias) = resolve_alias(actual, aliases) {
        let name = actual.split('<').next().unwrap_or(actual).trim();
        if seen.iter().any(|seen_name| seen_name == name) {
            return None;
        }
        seen.push(name.to_owned());
        return is_assignable(&alias, expected, aliases, seen);
    }
    if expected.starts_with(['\'', '"']) || seen.iter().any(|name| name == expected) {
        return None;
    }
    if let Some(alias) = resolve_alias(expected, aliases) {
        let name = expected.split('<').next().unwrap_or(expected).trim();
        if seen.iter().any(|seen_name| seen_name == name) {
            return None;
        }
        seen.push(name.to_owned());
        return is_assignable(actual, &alias, aliases, seen);
    }
    if actual == "nil" && parse_function_type(expected).is_some() {
        return Some(false);
    }
    None
}

pub(super) fn strip_wrapping_parentheses(input: &str) -> &str {
    input
        .strip_prefix('(')
        .filter(|_| matching_delimiter(input, 0, '(', ')') == Some(input.len() - 1))
        .and_then(|value| value.strip_suffix(')'))
        .unwrap_or(input)
}

pub(super) fn invalid_generic_arity(type_name: &str, aliases: &HashMap<String, TypeAlias>) -> bool {
    let type_name = type_name.trim();
    let Some(start) = type_name.find('<') else {
        return aliases
            .get(type_name)
            .is_some_and(|alias| !alias.parameters.is_empty());
    };
    let name = type_name[..start].trim();
    let Some(alias) = aliases.get(name) else {
        return false;
    };
    let Some(end) = matching_delimiter(type_name, start, '<', '>') else {
        return true;
    };
    !type_name[end + 1..].trim().is_empty()
        || split_top_level(&type_name[start + 1..end], ',').len() != alias.parameters.len()
}

pub(super) fn record_type_assignable(
    actual: &str,
    expected: &str,
    aliases: &HashMap<String, TypeAlias>,
    seen: &mut Vec<String>,
) -> Option<bool> {
    let actual_body = actual.trim().strip_prefix('{')?.strip_suffix('}')?;
    let expected_body = expected.trim().strip_prefix('{')?.strip_suffix('}')?;
    let actual_array = split_table_type_fields(actual_body);
    let expected_array = split_table_type_fields(expected_body);
    if let ([actual_item], [expected_item]) = (actual_array.as_slice(), expected_array.as_slice())
        && !actual_item.contains(':')
        && !expected_item.contains(':')
    {
        return is_assignable(actual_item.trim(), expected_item.trim(), aliases, seen);
    }
    let actual_fields = split_table_type_fields(actual_body)
        .into_iter()
        .filter_map(|field| {
            let (name, ty) = split_once_top_level(field, ':')?;
            Some((name.trim().trim_end_matches('?'), ty.trim()))
        })
        .collect::<Vec<_>>();
    let expected_fields = split_table_type_fields(expected_body)
        .into_iter()
        .filter_map(|field| {
            let (name, ty) = split_once_top_level(field, ':')?;
            let name = name.trim();
            Some((name.trim_end_matches('?'), name.ends_with('?'), ty.trim()))
        })
        .collect::<Vec<_>>();
    if actual_fields.is_empty() || expected_fields.is_empty() {
        return None;
    }
    let mut uncertain = false;
    for (name, optional, expected_type) in expected_fields {
        let Some((_, actual_type)) = actual_fields.iter().find(|(actual, _)| *actual == name)
        else {
            if optional || is_optional_type(expected_type, aliases) {
                continue;
            }
            return Some(false);
        };
        match is_assignable(actual_type, expected_type, aliases, &mut seen.clone()) {
            Some(false) => return Some(false),
            None => uncertain = true,
            Some(true) => {}
        }
    }
    if uncertain { None } else { Some(true) }
}

pub(super) fn vector_dimension<'a>(
    name: &'a str,
    aliases: &'a HashMap<String, TypeAlias>,
) -> Option<&'a str> {
    let mut current = name.trim();
    for _ in 0..aliases.len() + 1 {
        match current {
            "vector2" | "vector3" | "vector4" => return Some(current),
            _ => current = aliases.get(current)?.definition.trim(),
        }
    }
    None
}

pub(super) fn lua_string_value(value: &str) -> String {
    let quote = value.chars().next().unwrap_or('\'');
    value[1..value.len().saturating_sub(1)]
        .replace("\\\\", "\\")
        .replace(&format!("\\{quote}"), &quote.to_string())
}

pub(super) fn split_union(type_name: &str) -> Vec<&str> {
    split_top_level(type_name, '|')
}
