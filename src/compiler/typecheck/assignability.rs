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

    if actual.starts_with("$package")
        && matches!(
            expected,
            "number" | "integer" | "string" | "boolean" | "nil" | "table" | "function"
        )
        && let Some(resolved) = resolve_alias_chain(actual, aliases)
    {
        return is_assignable(&resolved, expected, aliases, seen);
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

pub(super) fn check_table_assignability(
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
                    range: 0..value.trim_end().len(),
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

pub(super) fn actual_table_fields(
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

pub(super) fn string_key_options(
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

pub(super) fn format_field_path(path: &[String]) -> String {
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
