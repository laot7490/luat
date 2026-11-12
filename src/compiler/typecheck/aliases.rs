use super::*;

pub(super) fn resolve_alias(
    expected: &str,
    aliases: &HashMap<String, TypeAlias>,
) -> Option<String> {
    let expected = expected.trim();
    let generic_start = expected.find('<');
    let name = generic_start.map_or(expected, |index| expected[..index].trim());
    let alias = aliases.get(name)?;
    let Some(start) = generic_start else {
        return alias
            .parameters
            .is_empty()
            .then(|| alias.definition.clone());
    };
    let end = matching_delimiter(expected, start, '<', '>')?;
    if !expected[end + 1..].trim().is_empty() {
        return None;
    }
    let arguments = split_top_level(&expected[start + 1..end], ',');
    if arguments.len() != alias.parameters.len() {
        return None;
    }
    let mut definition = alias.definition.clone();
    for (parameter, argument) in alias.parameters.iter().zip(arguments) {
        definition = substitute_type_name(&definition, parameter, argument);
    }
    Some(definition)
}

pub(super) fn resolve_alias_chain(
    type_name: &str,
    aliases: &HashMap<String, TypeAlias>,
) -> Option<String> {
    let mut current = strip_wrapping_parentheses(type_name.trim())
        .trim()
        .to_owned();
    let mut seen = HashSet::new();
    let mut changed = false;
    for _ in 0..128 {
        let optional = current.ends_with('?');
        let base = current.strip_suffix('?').unwrap_or(&current).trim();
        let Some(next) = resolve_alias(base, aliases) else {
            return changed.then(|| {
                if optional && !base.ends_with('?') {
                    format!("{base}?")
                } else {
                    base.to_owned()
                }
            });
        };
        if !seen.insert(base.to_owned()) {
            return None;
        }
        current = next.trim().to_owned();
        if optional && !current.ends_with('?') {
            current.push('?');
        }
        changed = true;
    }
    None
}

pub(super) fn is_optional_type(type_name: &str, aliases: &HashMap<String, TypeAlias>) -> bool {
    let mut current = type_name.trim().to_owned();
    let mut seen = Vec::new();
    loop {
        if current.ends_with('?') || split_union(&current).contains(&"nil") {
            return true;
        }
        let name = current.split('<').next().unwrap_or(&current).trim();
        if seen.iter().any(|seen_name| seen_name == name) {
            return false;
        }
        let Some(resolved) = resolve_alias(&current, aliases) else {
            return false;
        };
        seen.push(name.to_owned());
        current = resolved;
    }
}

pub(super) fn resolve_optional_base_type(
    type_name: &str,
    aliases: &HashMap<String, TypeAlias>,
) -> (String, bool) {
    let mut resolved =
        resolve_alias_chain(type_name, aliases).unwrap_or_else(|| type_name.trim().to_owned());
    let mut optional = is_optional_type(type_name, aliases) || is_optional_type(&resolved, aliases);
    if let Some(base) = resolved.strip_suffix('?') {
        resolved = base.trim().to_owned();
        optional = true;
    }
    let members = split_union(&resolved);
    if members.contains(&"nil") {
        let non_nil = members
            .into_iter()
            .filter(|member| *member != "nil")
            .collect::<Vec<_>>();
        if non_nil.len() == 1 {
            resolved = non_nil[0].to_owned();
        }
        optional = true;
    }
    if let Some(expanded) = resolve_alias_chain(&resolved, aliases) {
        resolved = expanded;
    }
    if let Some(base) = resolved.strip_suffix('?') {
        resolved = base.trim().to_owned();
        optional = true;
    }
    (resolved, optional)
}

pub(super) fn substitute_type_name(input: &str, name: &str, replacement: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    let flush = |token: &mut String, output: &mut String| {
        if token == name {
            output.push_str(replacement);
        } else {
            output.push_str(token);
        }
        token.clear();
    };
    for character in input.chars() {
        if let Some(active) = quote {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            continue;
        }
        if character == '\'' || character == '"' {
            flush(&mut token, &mut output);
            quote = Some(character);
            output.push(character);
        } else if character == '_' || character.is_alphanumeric() {
            token.push(character);
        } else {
            flush(&mut token, &mut output);
            output.push(character);
        }
    }
    flush(&mut token, &mut output);
    output
}

pub(super) fn unguarded_alias_graph(
    aliases: &HashMap<String, TypeAlias>,
) -> HashMap<String, Vec<String>> {
    aliases
        .iter()
        .map(|(name, alias)| {
            let mut references = Vec::new();
            let mut index = 0;
            let mut record_depth: usize = 0;
            let mut quote = None;
            while index < alias.definition.len() {
                let character = alias.definition[index..].chars().next().unwrap();
                if let Some(active) = quote {
                    if character == '\\' {
                        index += character.len_utf8();
                        if index < alias.definition.len() {
                            index += alias.definition[index..].chars().next().unwrap().len_utf8();
                        }
                        continue;
                    }
                    if character == active {
                        quote = None;
                    }
                    index += character.len_utf8();
                    continue;
                }
                if matches!(character, '\'' | '"') {
                    quote = Some(character);
                    index += character.len_utf8();
                    continue;
                }
                if character == '{' {
                    record_depth += 1;
                    index += 1;
                    continue;
                }
                if character == '}' {
                    record_depth = record_depth.saturating_sub(1);
                    index += 1;
                    continue;
                }
                if character == '_' || character.is_alphabetic() {
                    let start = index;
                    index += character.len_utf8();
                    while index < alias.definition.len() {
                        let next = alias.definition[index..].chars().next().unwrap();
                        if next == '_' || next.is_alphanumeric() {
                            index += next.len_utf8();
                        } else {
                            break;
                        }
                    }
                    if record_depth == 0 {
                        let identifier = &alias.definition[start..index];
                        if aliases.contains_key(identifier)
                            && !references.iter().any(|reference| reference == identifier)
                        {
                            references.push(identifier.to_owned());
                        }
                    }
                    continue;
                }
                index += character.len_utf8();
            }
            (name.clone(), references)
        })
        .collect()
}

pub(super) fn find_alias_cycles(graph: &HashMap<String, Vec<String>>) -> HashSet<String> {
    pub(super) fn visit(
        name: &str,
        graph: &HashMap<String, Vec<String>>,
        stack: &mut Vec<String>,
        visited: &mut HashSet<String>,
        cycles: &mut HashSet<String>,
    ) {
        if stack.len() > 128 {
            return;
        }
        if let Some(start) = stack.iter().position(|current| current == name) {
            cycles.extend(stack[start..].iter().cloned());
            return;
        }
        if !visited.insert(name.to_owned()) {
            return;
        }
        stack.push(name.to_owned());
        if let Some(references) = graph.get(name) {
            for reference in references {
                visit(reference, graph, stack, visited, cycles);
            }
        }
        stack.pop();
    }

    let mut cycles = HashSet::new();
    let mut visited = HashSet::new();
    for name in graph.keys() {
        visit(name, graph, &mut Vec::new(), &mut visited, &mut cycles);
    }
    cycles
}

pub(super) fn find_deep_aliases(graph: &HashMap<String, Vec<String>>) -> HashSet<String> {
    pub(super) fn depth(
        name: &str,
        graph: &HashMap<String, Vec<String>>,
        path: &mut HashSet<String>,
        memo: &mut HashMap<String, usize>,
    ) -> usize {
        if let Some(depth) = memo.get(name) {
            return *depth;
        }
        if !path.insert(name.to_owned()) {
            return 0;
        }
        if path.len() > 128 {
            path.remove(name);
            return 129;
        }
        let max_child = graph
            .get(name)
            .into_iter()
            .flatten()
            .map(|child| depth(child, graph, path, memo))
            .max()
            .unwrap_or_default();
        path.remove(name);
        let depth = max_child.saturating_add(1);
        memo.insert(name.to_owned(), depth);
        depth
    }

    let mut deep = HashSet::new();
    let mut memo = HashMap::new();
    for name in graph.keys() {
        if depth(name, graph, &mut HashSet::new(), &mut memo) > 128 {
            deep.insert(name.clone());
        }
    }
    deep
}
