use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Documentation {
    pub markdown: String,
    pub parameters: BTreeMap<String, String>,
    pub returns: Vec<String>,
    pub fields: BTreeMap<String, String>,
    pub deprecated: Option<String>,
}

impl Documentation {
    pub fn parse(text: &str) -> Self {
        let mut result = Self::default();
        let mut prose = Vec::new();
        let mut fenced = false;
        for line in text.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") {
                fenced = !fenced;
            }
            if !fenced {
                if let Some(rest) = trimmed.strip_prefix("@param ") {
                    if let Some((name, description)) = rest.split_once(char::is_whitespace) {
                        result.parameters.insert(
                            name.trim_end_matches('?').to_owned(),
                            description.trim().to_owned(),
                        );
                    }
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix("@field ") {
                    if let Some((name, description)) = rest.split_once(char::is_whitespace) {
                        result.fields.insert(
                            name.trim_end_matches('?').to_owned(),
                            description.trim().to_owned(),
                        );
                    }
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix("@return ") {
                    result.returns.push(rest.trim().to_owned());
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix("@deprecated") {
                    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                        result.deprecated = Some(rest.trim().to_owned());
                        continue;
                    }
                }
            }
            prose.push(line);
        }
        result.markdown = prose.join("\n").trim_matches('\n').to_owned();
        result
    }

    pub fn render(&self) -> String {
        let mut sections = Vec::new();
        if !self.markdown.trim().is_empty() {
            sections.push(self.markdown.clone());
        }
        for (name, description) in &self.parameters {
            sections.push(format!("**{name}** — {description}"));
        }
        for description in &self.returns {
            sections.push(format!("**Returns** — {description}"));
        }
        if let Some(description) = &self.deprecated {
            sections.push(format!(
                "**Deprecated**{}",
                if description.is_empty() {
                    String::new()
                } else {
                    format!(" — {description}")
                }
            ));
        }
        sections.join("\n\n")
    }

    pub fn before(source: &str, offset: usize) -> Self {
        Self::parse(&raw_before(source, offset))
    }
}

pub(super) fn raw_before(source: &str, offset: usize) -> String {
    let line_start = source[..offset]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    source[..line_start]
        .lines()
        .rev()
        .take_while(|line| line.trim_start().starts_with("---"))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| {
            line.trim_start()
                .strip_prefix("---")
                .unwrap_or(line)
                .strip_prefix(' ')
                .unwrap_or_else(|| line.trim_start().strip_prefix("---").unwrap_or(line))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl super::AnalysisSnapshot {
    pub(super) fn documentation_completions(
        &self,
        path: &std::path::Path,
        offset: usize,
    ) -> Option<Vec<super::CompletionItem>> {
        use super::{CompletionItem, CompletionKind};
        let file = self.files.get(path)?;
        let offset = super::clamp_offset(&file.source, offset);
        let line_start = file.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let line = file.source[line_start..offset].trim_start();
        let comment = line.strip_prefix("---")?;
        let line_end = file.source[offset..]
            .find('\n')
            .map_or(file.source.len(), |i| offset + i);
        if super::non_code_at(&file.source, line_start) {
            return None;
        }
        let target = file
            .symbols
            .iter()
            .filter(|symbol| {
                symbol.range.start > line_end
                    && (symbol.signature.is_some() || symbol.kind == super::SymbolKind::TypeAlias)
            })
            .min_by_key(|symbol| symbol.range.start);
        let target = target.filter(|symbol| {
            file.source[line_end..symbol.range.start]
                .lines()
                .skip(1)
                .all(|line| {
                    line.trim().is_empty()
                        || line.trim_start().starts_with("---")
                        || line.trim_start().starts_with("local function ")
                        || line.trim_start().starts_with("function ")
                        || line.trim_start().starts_with("typedef ")
                })
        });
        if let Some(tag) = comment.trim_start().strip_prefix('@') {
            if let Some((kind, prefix)) = tag.split_once(char::is_whitespace) {
                let prefix = prefix.trim_start();
                if prefix.contains(char::is_whitespace) {
                    return Some(Vec::new());
                }
                let Some(target) = target else {
                    return Some(Vec::new());
                };
                let existing = Documentation::before(&file.source, line_start);
                let names = match kind {
                    "param" => target
                        .signature
                        .as_ref()
                        .map(|signature| {
                            signature
                                .parameters
                                .iter()
                                .filter_map(|parameter| {
                                    parameter
                                        .split_once(':')
                                        .map(|(name, _)| name.trim().to_owned())
                                })
                                .filter(|name| !existing.parameters.contains_key(name))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                    "field" => self
                        .record_fields(path, target.visible.start, &target.type_name)
                        .into_iter()
                        .map(|field| field.name)
                        .filter(|name| !existing.fields.contains_key(name))
                        .collect(),
                    _ => return Some(Vec::new()),
                };
                return Some(
                    names
                        .into_iter()
                        .filter(|name| name.starts_with(prefix))
                        .map(|name| CompletionItem {
                            label: name,
                            kind: CompletionKind::Variable,
                            detail: "Documentation name".to_owned(),
                            ..Default::default()
                        })
                        .collect(),
                );
            }
            return Some(
                ["param", "return", "field", "deprecated"]
                    .into_iter()
                    .filter(|name| name.starts_with(tag))
                    .map(|name| CompletionItem {
                        label: name.to_owned(),
                        detail: "Documentation tag".to_owned(),
                        ..Default::default()
                    })
                    .collect(),
            );
        }
        if !comment.trim().is_empty() {
            return Some(Vec::new());
        }
        let Some(target) = target else {
            return Some(Vec::new());
        };
        let Some(signature) = &target.signature else {
            return Some(Vec::new());
        };
        let existing = Documentation::before(&file.source, target.range.start);
        let mut text = " ${1:Description.}".to_owned();
        let mut tab = 2;
        for parameter in &signature.parameters {
            let name = parameter.split(':').next().unwrap_or(parameter).trim();
            if !existing.parameters.contains_key(name) {
                text.push_str(&format!("\n---@param {name} ${{{tab}:Description.}}"));
                tab += 1;
            }
        }
        if existing.returns.is_empty()
            && signature
                .label
                .rsplit_once("): ")
                .is_some_and(|(_, returns)| returns != "()")
        {
            text.push_str(&format!("\n---@return ${{{tab}:Description.}}"));
        }
        Some(vec![CompletionItem {
            label: "Document function".to_owned(),
            detail: "Add descriptions without types".to_owned(),
            kind: CompletionKind::Snippet,
            insert_text: Some(text),
            ..Default::default()
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typeless_tags_preserve_words_and_fenced_markdown() {
        let doc = Documentation::parse(
            "[Link](https://example.com)  \nnext\n@param value string text\n@param second Açıklama\n@field enabled Enabled.\n@return Sonuç\n@deprecated Use other.\n```luat\n@param literal Keep\n```",
        );
        assert_eq!(doc.parameters["value"], "string text");
        assert_eq!(doc.parameters["second"], "Açıklama");
        assert_eq!(doc.fields["enabled"], "Enabled.");
        assert_eq!(doc.returns, ["Sonuç"]);
        assert!(doc.render().contains("@param literal Keep"));
    }
}
