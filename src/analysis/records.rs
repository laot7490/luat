use super::*;
use crate::compiler::{
    declarations::TypeAlias,
    typecheck::{editor_resolve_alias, editor_type_fields, editor_type_parts},
};

#[derive(Clone)]
pub(super) struct RecordField {
    pub name: String,
    pub type_name: String,
    pub documentation: Option<String>,
}

impl AnalysisSnapshot {
    pub(super) fn record_member_signature(
        &self,
        path: &Path,
        offset: usize,
        receiver: &str,
        name: &str,
        method: bool,
    ) -> Option<SignatureInformation> {
        let (context, ty) = self.receiver_type(path, offset, receiver)?;
        let field = self
            .record_fields(&context, offset, &ty)
            .into_iter()
            .find(|field| field.name == name)?;
        let mut ty = field.type_name.trim();
        let (parameters, returns) = loop {
            let mut depth = 0usize;
            let close = ty.char_indices().find_map(|(index, character)| {
                if character == '(' {
                    depth += 1;
                } else if character == ')' {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(index);
                    }
                }
                None
            })?;
            if !ty.starts_with('(') {
                return None;
            }
            if let Some(returns) = ty[close + 1..].trim_start().strip_prefix("->") {
                break (&ty[1..close], returns.trim());
            }
            if close != ty.len() - 1 {
                return None;
            }
            ty = ty[1..close].trim();
        };
        let mut parameters = editor_type_parts(parameters, ',')
            .into_iter()
            .filter(|ty| !ty.trim().is_empty())
            .enumerate()
            .map(|(index, ty)| {
                if ty.contains(':') {
                    ty.trim().to_owned()
                } else {
                    format!("arg{}: {}", index + 1, ty.trim())
                }
            })
            .collect::<Vec<_>>();
        if method && !parameters.is_empty() {
            parameters.remove(0);
        }
        Some(SignatureInformation {
            label: format!(
                "function {receiver}{}{}({}): {}",
                if method { ':' } else { '.' },
                name,
                parameters.join(", "),
                returns.trim()
            ),
            parameter_documentation: vec![None; parameters.len()],
            parameters,
            documentation: field.documentation,
        })
    }

    fn editor_aliases(
        &self,
        path: &Path,
        offset: usize,
    ) -> (HashMap<String, TypeAlias>, HashMap<String, Documentation>) {
        let mut aliases = HashMap::new();
        let mut docs = HashMap::new();
        for group in builtin_index().values() {
            for symbol in group.iter().filter(|symbol| {
                symbol.kind == CompletionKind::Type && self.builtin_available(path, symbol)
            }) {
                aliases.insert(
                    symbol.name.clone(),
                    TypeAlias {
                        parameters: Vec::new(),
                        definition: symbol.type_name.clone(),
                    },
                );
                docs.insert(
                    symbol.name.clone(),
                    Documentation::parse(&symbol.documentation),
                );
            }
        }
        let mut add = |file: &AnalyzedFile, symbol: &Symbol| {
            let tail = file.source[symbol.range.end..].trim_start();
            let parameters = tail
                .strip_prefix('<')
                .and_then(|tail| tail.split_once('>'))
                .map_or_else(Vec::new, |(parameters, _)| {
                    parameters
                        .split(',')
                        .map(|name| name.trim().to_owned())
                        .collect()
                });
            aliases.insert(
                symbol.name.clone(),
                TypeAlias {
                    parameters,
                    definition: symbol.type_name.clone(),
                },
            );
            docs.insert(
                symbol.name.clone(),
                Documentation::before(&file.source, symbol.range.start),
            );
        };
        if let Some(root) = self.project_root(path)
            && let Some(index) = self.project_symbols.get(&root)
        {
            for group in index.values() {
                if let Some(project_symbol) = group
                    .iter()
                    .find(|s| s.kind == ProjectSymbolKind::TypeAlias)
                    && let Some(file) = self.files.get(&project_symbol.path)
                    && let Some(symbol) = file
                        .symbols
                        .iter()
                        .find(|s| s.range == project_symbol.range)
                {
                    add(file, symbol);
                }
            }
        }
        if let Some(file) = self.files.get(path) {
            let mut visible = file
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.kind == SymbolKind::TypeAlias
                        && symbol.visible.start <= offset
                        && symbol.scope.start <= offset
                        && offset <= symbol.scope.end
                })
                .collect::<Vec<_>>();
            visible.sort_by_key(|symbol| (symbol.scope_depth, symbol.visible.start));
            for symbol in visible {
                add(file, symbol);
            }
        }
        (aliases, docs)
    }

    pub(super) fn record_fields(
        &self,
        path: &Path,
        offset: usize,
        type_name: &str,
    ) -> Vec<RecordField> {
        let (aliases, docs) = self.editor_aliases(path, offset);
        fn fields(
            ty: &str,
            aliases: &HashMap<String, TypeAlias>,
            docs: &HashMap<String, Documentation>,
            seen: &mut HashSet<String>,
            depth: usize,
        ) -> Vec<RecordField> {
            if depth > 32 {
                return Vec::new();
            }
            let mut ty = ty.trim();
            if ty.starts_with('(') && ty.ends_with(')') {
                ty = &ty[1..ty.len() - 1];
            }
            let union = editor_type_parts(ty, '|');
            if union.len() > 1 {
                let mut common = fields(union[0], aliases, docs, &mut seen.clone(), depth + 1);
                for member in &union[1..] {
                    let other = fields(member, aliases, docs, &mut seen.clone(), depth + 1);
                    common.retain_mut(|field| {
                        if let Some(candidate) =
                            other.iter().find(|candidate| candidate.name == field.name)
                        {
                            if candidate.type_name != field.type_name {
                                field.type_name =
                                    format!("{} | {}", field.type_name, candidate.type_name);
                            }
                            true
                        } else {
                            false
                        }
                    });
                }
                return common;
            }
            let name = ty.split('<').next().unwrap_or(ty).trim();
            if aliases.contains_key(name) {
                if !seen.insert(ty.to_owned()) {
                    return Vec::new();
                }
                let Some(expanded) = editor_resolve_alias(ty, aliases) else {
                    return Vec::new();
                };
                let mut result = fields(&expanded, aliases, docs, seen, depth + 1);
                if let Some(documentation) = docs.get(name) {
                    for field in &mut result {
                        if let Some(text) = documentation.fields.get(&field.name) {
                            field.documentation = Some(text.clone());
                        }
                    }
                }
                return result;
            }
            let Some(body) = ty.strip_prefix('{').and_then(|ty| ty.strip_suffix('}')) else {
                return Vec::new();
            };
            editor_type_fields(body)
                .into_iter()
                .filter_map(|field| {
                    let (name, ty) = field.split_once(':')?;
                    let name = name.trim().trim_end_matches('?');
                    if name.is_empty()
                        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return None;
                    }
                    Some(RecordField {
                        name: name.to_owned(),
                        type_name: ty.trim().to_owned(),
                        documentation: None,
                    })
                })
                .collect()
        }
        fields(type_name, &aliases, &docs, &mut HashSet::new(), 0)
    }

    fn receiver_type(
        &self,
        path: &Path,
        offset: usize,
        expression: &str,
    ) -> Option<(PathBuf, String)> {
        let file = self.files.get(path)?;
        let mut names = expression.split('.');
        let root = names.next()?;
        let mut context = path.to_path_buf();
        let mut ty = if let Some(symbol) = file.visible_symbol(root, offset) {
            if let Some(binding) = self
                .project_imports
                .get(path)
                .into_iter()
                .flatten()
                .find(|binding| binding.symbol == symbol.id)
            {
                context = binding.target_path.clone();
                binding.type_name.clone()
            } else {
                symbol.type_name.clone()
            }
        } else if let Some(symbol) = self
            .project_group(path, root, ProjectSymbolKind::Global)
            .and_then(|group| group.first())
        {
            symbol.type_name.clone()
        } else {
            builtin_index()
                .get(root)?
                .iter()
                .find(|symbol| {
                    symbol.kind != CompletionKind::Type && self.builtin_available(path, symbol)
                })?
                .type_name
                .clone()
        };
        for name in names {
            ty = self
                .record_fields(&context, offset, &ty)
                .into_iter()
                .find(|field| field.name == name)?
                .type_name;
        }
        Some((context, ty))
    }

    fn receiver_expression<'a>(&self, source: &'a str, end: usize) -> Option<&'a str> {
        let text = source[..end].trim_end();
        let start = text
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .map_or(0, |index| index + 1);
        let expression = &text[start..];
        (!expression.is_empty()).then_some(expression)
    }

    pub(super) fn record_completions(
        &self,
        path: &Path,
        offset: usize,
    ) -> Option<Vec<CompletionItem>> {
        let file = self.files.get(path)?;
        let offset = clamp_offset(&file.source, offset);
        let prefix = completion_prefix(&file.source, offset);
        let before = file.source[..offset - prefix.len()].trim_end();
        let (fields, written) = if let Some(before) = before.strip_suffix('.') {
            if self.imported_member_completions(path, offset).is_some() {
                return None;
            }
            let expression = self.receiver_expression(&file.source, before.len())?;
            let (context, ty) = self.receiver_type(path, offset, expression)?;
            let fields = self.record_fields(&context, offset, &ty);
            // Builtin namespace functions live in the qualified symbol index.
            if fields.is_empty()
                || builtin_index().contains_key(expression)
                    && self
                        .files
                        .get(path)?
                        .visible_symbol(expression, offset)
                        .is_none()
            {
                return None;
            }
            (fields, HashSet::new())
        } else {
            self.contextual_fields(path, offset)?
        };
        Some(
            fields
                .into_iter()
                .filter(|field| field.name.starts_with(prefix) && !written.contains(&field.name))
                .map(|field| CompletionItem {
                    label: field.name,
                    detail: field.type_name.clone(),
                    kind: if field.type_name.trim_start().starts_with('(')
                        && field.type_name.contains("->")
                    {
                        CompletionKind::Function
                    } else {
                        CompletionKind::Property
                    },
                    documentation: field.documentation,
                    ..Default::default()
                })
                .collect(),
        )
    }

    fn contextual_fields(
        &self,
        path: &Path,
        offset: usize,
    ) -> Option<(Vec<RecordField>, HashSet<String>)> {
        let file = self.files.get(path)?;
        let tokens = &file.tokens;
        let mut stack = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            if file
                .prepared
                .original_offset(token.start_position().bytes())
                >= offset
            {
                break;
            }
            match token.to_string().trim() {
                "{" | "(" | "[" => stack.push(index),
                "}" | ")" | "]" => {
                    stack.pop();
                }
                _ => {}
            }
        }
        let &open = stack.last()?;
        if tokens[open].to_string().trim() != "{" {
            return None;
        }
        let tables = stack
            .iter()
            .copied()
            .filter(|&index| tokens[index].to_string().trim() == "{")
            .collect::<Vec<_>>();
        let brace = file
            .prepared
            .original_offset(tokens[*tables.first()?].start_position().bytes());
        let help = self.signature_help(path, brace)?;
        if help.signatures.len() != 1 {
            return None;
        }
        let parameter = help.signatures[0].parameters.get(help.active_parameter)?;
        let ty = parameter.split_once(':')?.1.trim();
        let mut fields = self.record_fields(path, brace, ty);
        for &nested in tables.iter().skip(1) {
            if nested < 2 || tokens[nested - 1].to_string().trim() != "=" {
                return Some((Vec::new(), HashSet::new()));
            }
            let field_name = tokens[nested - 2].to_string();
            let Some(field) = fields.iter().find(|field| field.name == field_name.trim()) else {
                return Some((Vec::new(), HashSet::new()));
            };
            fields = self.record_fields(path, brace, &field.type_name);
        }
        // Only key positions receive field suggestions; values use ordinary expression completion.
        let mut segment = Vec::new();
        let mut nesting = 0usize;
        for token in &tokens[open + 1..] {
            if file
                .prepared
                .original_offset(token.start_position().bytes())
                >= offset
            {
                break;
            }
            let text = token.to_string();
            if nesting == 0 && matches!(text.trim(), "," | ";") {
                segment.clear();
                continue;
            }
            if nesting == 0 {
                segment.push(token);
            }
            match text.trim() {
                "(" | "{" | "[" => nesting += 1,
                ")" | "}" | "]" => nesting = nesting.saturating_sub(1),
                _ => {}
            }
        }
        if !(segment.is_empty()
            || segment.len() == 1
                && matches!(segment[0].token_type(), TokenType::Identifier { .. }))
        {
            return None;
        }
        let mut written = HashSet::new();
        let mut depth = 0usize;
        let mut field_start = true;
        for (index, token) in tokens.iter().enumerate().skip(open + 1) {
            let position = file
                .prepared
                .original_offset(token.start_position().bytes());
            if position >= offset {
                break;
            }
            let text = token.to_string();
            let text = text.trim();
            if depth == 0
                && field_start
                && matches!(token.token_type(), TokenType::Identifier { .. })
                && tokens
                    .get(index + 1)
                    .is_some_and(|next| next.to_string().trim() == "=")
            {
                written.insert(text.to_owned());
            }
            if depth == 0 && !text.is_empty() {
                field_start = text == "," || text == ";";
            }
            match text {
                "(" | "{" | "[" => depth += 1,
                ")" | "}" | "]" => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        Some((fields, written))
    }

    pub(super) fn record_hover(&self, path: &Path, offset: usize) -> Option<HoverResult> {
        let file = self.files.get(path)?;
        if self.imported_member_at(path, offset).is_some()
            || self.builtin_at(path, offset).is_some()
        {
            return None;
        }
        let range = file.identifier_range_at(offset)?;
        let before = file.source[..range.start].trim_end().strip_suffix('.')?;
        let expression = self.receiver_expression(&file.source, before.len())?;
        let (context, ty) = self.receiver_type(path, range.start, expression)?;
        let field = self
            .record_fields(&context, range.start, &ty)
            .into_iter()
            .find(|field| field.name == file.source[range.clone()])?;
        Some(HoverResult {
            range,
            contents: hover_markdown(
                &format!("{}: {}", field.name, field.type_name),
                field.documentation.as_deref().unwrap_or_default(),
            ),
        })
    }
}
