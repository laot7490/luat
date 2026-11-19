mod builtins;
mod host;
mod symbols;
use builtins::*;
pub use host::AnalysisHost;
use symbols::*;
pub mod documentation;
mod editor;
mod inputs;
mod records;
use documentation::Documentation;
pub use editor::{EditorDiagnostic, EditorInlayHint, EditorSymbol, EditorSymbolKind};

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use full_moon::{
    LuaVersion,
    ast::{self, luau::TypeDeclaration},
    node::Node,
    tokenizer::{Lexer, LexerResult, Token, TokenType},
    visitors::Visitor,
};

use crate::{
    BuildError, Diagnostic, Project,
    compiler::{ParsedSource, declarations::DeclarationSet, syntax},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRange {
    pub path: PathBuf,
    pub range: std::ops::Range<usize>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompletionItem {
    pub documentation: Option<String>,
    pub deprecated: bool,
    pub insert_text: Option<String>,
    pub label: String,
    pub detail: String,
    pub kind: CompletionKind,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CompletionKind {
    #[default]
    Variable,
    Property,
    Snippet,
    Function,
    Type,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HoverResult {
    pub range: std::ops::Range<usize>,
    pub contents: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureInformation {
    pub label: String,
    pub parameters: Vec<String>,
    pub parameter_documentation: Vec<Option<String>>,
    pub documentation: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureHelp {
    pub signatures: Vec<SignatureInformation>,
    pub active_signature: usize,
    pub active_parameter: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub range: std::ops::Range<usize>,
    pub new_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceEdit {
    pub changes: BTreeMap<PathBuf, Vec<TextEdit>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenameError {
    MissingFile,
    InvalidOffset,
    InvalidName,
    NotRenameable,
    NameConflict,
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::MissingFile => "the file is not in this analysis snapshot",
            Self::InvalidOffset => "the position is not on a known symbol",
            Self::InvalidName => "the new name is not a valid Lua identifier",
            Self::NotRenameable => "the symbol cannot be renamed safely",
            Self::NameConflict => "the new name would shadow or conflict with another binding",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RenameError {}

type SemanticTokenRange = (std::ops::Range<usize>, u32, u32);

#[derive(Clone, Default)]
pub struct AnalysisSnapshot {
    contexts: Arc<HashMap<PathBuf, Vec<crate::ExecutionContext>>>,
    files: Arc<BTreeMap<PathBuf, Arc<AnalyzedFile>>>,
    project_symbols: Arc<HashMap<PathBuf, HashMap<String, Vec<ProjectSymbol>>>>,
    symbol_visibility: Arc<HashMap<PathBuf, HashSet<PathBuf>>>,
    project_imports: Arc<HashMap<PathBuf, Vec<ImportBinding>>>,
    roots: Arc<HashMap<PathBuf, Option<PathBuf>>>,
    semantic_cache: Arc<std::sync::Mutex<HashMap<PathBuf, Vec<SemanticTokenRange>>>>,
    resolved_imports: Arc<crate::project::ResolvedImports>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProjectSymbolKind {
    Function,
    TypeAlias,
    Global,
}

#[derive(Clone)]
struct ProjectSymbol {
    name: String,
    kind: ProjectSymbolKind,
    path: PathBuf,
    range: std::ops::Range<usize>,
    type_name: String,
    signature: Option<SignatureInformation>,
    documentation: String,
    context: Option<crate::ExecutionContext>,
}

#[derive(Clone)]
struct ImportBinding {
    symbol: usize,
    target_path: PathBuf,
    type_name: String,
    members: HashMap<String, ImportMember>,
}

#[derive(Clone)]
struct ImportMember {
    range: std::ops::Range<usize>,
    type_name: String,
    signature: Option<SignatureInformation>,
}

impl AnalysisSnapshot {
    fn project_root(&self, path: &Path) -> Option<PathBuf> {
        self.roots.get(path).cloned().flatten()
    }

    fn project_group(
        &self,
        path: &Path,
        name: &str,
        kind: ProjectSymbolKind,
    ) -> Option<Vec<&ProjectSymbol>> {
        let root = self.project_root(path)?;
        let candidates = self.project_symbols.get(&root)?.get(name)?;
        let selected = candidates
            .iter()
            .filter(|symbol| symbol.kind == kind && self.project_symbol_available(path, symbol))
            .collect::<Vec<_>>();
        let count = selected.len();
        (count > 0 && (kind == ProjectSymbolKind::Function || count == 1)).then_some(selected)
    }

    fn project_symbol_available(&self, path: &Path, symbol: &ProjectSymbol) -> bool {
        if self
            .symbol_visibility
            .get(path)
            .is_some_and(|visible| !visible.contains(&symbol.path))
        {
            return false;
        }
        symbol.context.is_none_or(|context| {
            if let Some(declaration) = self
                .project_root(path)
                .and_then(|root| self.project_symbols.get(&root))
                .and_then(|index| index.get(&symbol.name))
                .and_then(|group| {
                    group.iter().find(|candidate| {
                        candidate.path == path && path.to_string_lossy().ends_with(".d.luat")
                    })
                })
            {
                return declaration.context.is_none_or(|side| side == context);
            }
            self.contexts.get(path).is_none_or(|contexts| {
                contexts.iter().all(|side| *side == context)
                    || contexts.len() > 1
                        && contexts.iter().all(|side| {
                            self.project_root(path)
                                .and_then(|root| self.project_symbols.get(&root))
                                .and_then(|index| index.get(&symbol.name))
                                .is_some_and(|group| {
                                    group.iter().any(|candidate| {
                                        candidate.kind == symbol.kind
                                            && self.symbol_visibility.get(path).is_none_or(
                                                |visible| visible.contains(&candidate.path),
                                            )
                                            && (candidate.context.is_none()
                                                || candidate.context == Some(*side))
                                    })
                                })
                        })
            })
        })
    }

    fn project_symbol_at(&self, path: &Path, byte_offset: usize) -> Option<&ProjectSymbol> {
        let file = self.files.get(path)?;
        let range = file.identifier_range_at(byte_offset)?;
        let name = &file.source[range.clone()];
        let root = self.project_root(path)?;
        let candidates = self.project_symbols.get(&root)?.get(name)?;
        if let Some(symbol) = candidates.iter().find(|symbol| {
            symbol.path == path
                && symbol.range == range
                && self.project_symbol_available(path, symbol)
        }) {
            return Some(symbol);
        }
        let is_type = is_type_reference_position(&file.source, range.start);
        if !is_type && is_field_or_type_position(&file.source, range.start, range.end) {
            return None;
        }
        let kind = if is_type {
            ProjectSymbolKind::TypeAlias
        } else if candidates
            .iter()
            .any(|symbol| symbol.kind == ProjectSymbolKind::Function)
        {
            if candidates
                .iter()
                .any(|symbol| symbol.kind == ProjectSymbolKind::Global)
            {
                return None;
            }
            ProjectSymbolKind::Function
        } else {
            ProjectSymbolKind::Global
        };
        let group = self.project_group(path, name, kind)?;
        if let Some(occurrence) = file.occurrence_at(range.start) {
            let local = &file.symbols[occurrence.symbol];
            if local.kind == SymbolKind::Local
                || local.kind == SymbolKind::Parameter
                || local.kind == SymbolKind::TypeAlias
                || local.kind == SymbolKind::Function && local.scope_depth != 0
            {
                return None;
            }
        }
        group
            .into_iter()
            .find(|symbol| symbol.kind == kind && self.project_symbol_available(path, symbol))
    }

    fn imported_member_at<'a>(
        &'a self,
        path: &Path,
        byte_offset: usize,
    ) -> Option<(&'a PathBuf, &'a str, &'a ImportMember)> {
        let file = self.files.get(path)?;
        let range = file.identifier_range_at(byte_offset)?;
        let name = &file.source[range.clone()];
        if let Some(receiver) = member_receiver_range(&file.source, &range)
            && let Some(occurrence) = file.occurrence_at(receiver.start)
            && occurrence.range == receiver
            && let Some(binding) = self
                .project_imports
                .get(path)
                .into_iter()
                .flatten()
                .find(|binding| binding.symbol == occurrence.symbol)
            && let Some(member) = binding.members.get(name)
        {
            return Some((&binding.target_path, name, member));
        }
        self.project_imports.values().flatten().find_map(|binding| {
            if binding.target_path != path {
                return None;
            }
            binding
                .members
                .iter()
                .find(|(member_name, member)| member_name.as_str() == name && member.range == range)
                .map(|(member_name, member)| (&binding.target_path, member_name.as_str(), member))
        })
    }

    fn import_member_references(
        &self,
        target_path: &Path,
        name: &str,
        include_declaration: bool,
    ) -> Vec<FileRange> {
        let mut references = Vec::new();
        for (importer_path, bindings) in self.project_imports.iter() {
            let Some(file) = self.files.get(importer_path) else {
                continue;
            };
            for binding in bindings.iter().filter(|binding| {
                binding.target_path == target_path && binding.members.contains_key(name)
            }) {
                for token in scan_identifiers(&file.prepared.text) {
                    if token.name != name {
                        continue;
                    }
                    let start = file.prepared.original_offset(token.range.start);
                    let end = file.prepared.original_offset(token.range.end);
                    let range = start..end;
                    let Some(receiver) = member_receiver_range(&file.source, &range) else {
                        continue;
                    };
                    if file
                        .occurrence_at(receiver.start)
                        .is_some_and(|occurrence| {
                            occurrence.range == receiver && occurrence.symbol == binding.symbol
                        })
                    {
                        references.push(FileRange {
                            path: importer_path.clone(),
                            range,
                        });
                    }
                }
            }
        }
        if include_declaration
            && let Some(binding) = self
                .project_imports
                .values()
                .flatten()
                .find(|binding| binding.target_path == target_path)
            && let Some(member) = binding.members.get(name)
        {
            references.push(FileRange {
                path: target_path.to_path_buf(),
                range: member.range.clone(),
            });
        }
        references.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.range.start.cmp(&right.range.start))
        });
        references.dedup_by(|left, right| left.path == right.path && left.range == right.range);
        references
    }

    fn rename_import_member(
        &self,
        target_path: &Path,
        name: &str,
        new_name: &str,
    ) -> Result<WorkspaceEdit, RenameError> {
        let root = self
            .project_root(target_path)
            .ok_or(RenameError::NotRenameable)?;
        if self.files.iter().any(|(path, file)| {
            self.project_root(path).as_ref() == Some(&root) && !file.syntax_valid
        }) {
            return Err(RenameError::NotRenameable);
        }
        if new_name != name
            && self.project_imports.values().flatten().any(|binding| {
                binding.target_path == target_path && binding.members.contains_key(new_name)
            })
        {
            return Err(RenameError::NameConflict);
        }
        let references = self.import_member_references(target_path, name, true);
        if references.is_empty() {
            return Err(RenameError::NotRenameable);
        }
        let mut changes = BTreeMap::<PathBuf, Vec<TextEdit>>::new();
        for reference in references {
            changes.entry(reference.path).or_default().push(TextEdit {
                range: reference.range,
                new_text: new_name.to_owned(),
            });
        }
        Ok(WorkspaceEdit { changes })
    }

    fn imported_member_completions(
        &self,
        path: &Path,
        byte_offset: usize,
    ) -> Option<Vec<CompletionItem>> {
        let file = self.files.get(path)?;
        let receiver = receiver_before_dot(&file.source, byte_offset)?;
        let symbol = file
            .occurrence_at(receiver.start)
            .filter(|occurrence| occurrence.range == receiver)
            .map(|occurrence| occurrence.symbol)
            .or_else(|| {
                if file.syntax_valid || file.source[..receiver.start].trim_end().ends_with('.') {
                    return None;
                }
                file.visible_symbol(&file.source[receiver.clone()], receiver.start)
                    .map(|symbol| symbol.id)
            })?;
        let binding = self
            .project_imports
            .get(path)?
            .iter()
            .find(|binding| binding.symbol == symbol)?;
        let target = self.files.get(&binding.target_path)?;
        let prefix = completion_prefix(&file.source, byte_offset);
        Some(
            binding
                .members
                .iter()
                .filter(|(name, _)| name.starts_with(prefix))
                .map(|(name, member)| {
                    let doc = Documentation::before(&target.source, member.range.start);
                    CompletionItem {
                        label: name.clone(),
                        detail: member.type_name.clone(),
                        kind: if member.signature.is_some() {
                            CompletionKind::Function
                        } else {
                            CompletionKind::Variable
                        },
                        documentation: nonempty(doc.render()),
                        deprecated: doc.deprecated.is_some(),
                        ..Default::default()
                    }
                })
                .collect(),
        )
    }

    fn project_references(
        &self,
        path: &Path,
        name: &str,
        kind: ProjectSymbolKind,
        include_declaration: bool,
    ) -> Vec<FileRange> {
        let Some(root) = self.project_root(path) else {
            return Vec::new();
        };
        let Some(group) = self.project_group(path, name, kind) else {
            return Vec::new();
        };
        let mut references = Vec::new();
        for (candidate_path, file) in self.files.iter() {
            if self.project_root(candidate_path).as_ref() != Some(&root) {
                continue;
            }
            if !group
                .iter()
                .any(|symbol| self.project_symbol_available(candidate_path, symbol))
            {
                continue;
            }
            for token in scan_identifiers(&file.prepared.text) {
                if token.name != name {
                    continue;
                }
                let start = file.prepared.original_offset(token.range.start);
                let end = file.prepared.original_offset(token.range.end);
                let range = start..end;
                let is_type = is_type_reference_position(&file.source, start);
                let declaration = group.iter().any(|symbol| {
                    symbol.path == *candidate_path
                        && symbol.range == range
                        && self.project_symbol_available(candidate_path, symbol)
                });
                if (kind == ProjectSymbolKind::TypeAlias) != is_type
                    || !is_type
                        && !declaration
                        && is_field_or_type_position(&file.source, start, end)
                {
                    continue;
                }
                if !declaration && let Some(occurrence) = file.occurrence_at(start) {
                    let local = &file.symbols[occurrence.symbol];
                    if local.kind == SymbolKind::Local
                        || local.kind == SymbolKind::Parameter
                        || local.kind == SymbolKind::TypeAlias
                        || local.kind == SymbolKind::Function && local.scope_depth != 0
                    {
                        continue;
                    }
                }
                if include_declaration || !declaration {
                    references.push(FileRange {
                        path: candidate_path.clone(),
                        range,
                    });
                }
            }
        }
        references
    }

    fn rename_project_symbol(
        &self,
        path: &Path,
        symbol: &ProjectSymbol,
        new_name: &str,
    ) -> Result<WorkspaceEdit, RenameError> {
        if symbol
            .path
            .components()
            .any(|component| component.as_os_str() == ".luat")
        {
            return Err(RenameError::NotRenameable);
        }
        let root = self.project_root(path).ok_or(RenameError::NotRenameable)?;
        let group = self
            .project_group(path, &symbol.name, symbol.kind)
            .ok_or(RenameError::NotRenameable)?;
        if !builtin_by_short_name(&symbol.name).is_empty() {
            return Err(RenameError::NotRenameable);
        }
        for (candidate_path, file) in self.files.iter() {
            if self.project_root(candidate_path).as_ref() == Some(&root)
                && (!file.syntax_valid
                    || file.symbols.iter().any(|other| {
                        other.name == new_name
                            && !group.iter().any(|candidate| {
                                candidate.path == *candidate_path && candidate.range == other.range
                            })
                    }))
            {
                return Err(if file.syntax_valid {
                    RenameError::NameConflict
                } else {
                    RenameError::NotRenameable
                });
            }
        }
        if let Some(candidates) = self
            .project_symbols
            .get(&root)
            .and_then(|symbols| symbols.get(new_name))
            && candidates.iter().any(|candidate| {
                !group.iter().any(|current| {
                    current.path == candidate.path && current.range == candidate.range
                })
            })
        {
            return Err(RenameError::NameConflict);
        }
        if symbol.kind == ProjectSymbolKind::TypeAlias
            && self
                .project_symbols
                .get(&root)
                .is_some_and(|symbols| symbols.contains_key(new_name))
        {
            return Err(RenameError::NameConflict);
        }
        let references = self.project_references(path, &symbol.name, symbol.kind, true);
        if references.is_empty() {
            return Err(RenameError::NotRenameable);
        }
        let mut changes = BTreeMap::<PathBuf, Vec<TextEdit>>::new();
        for reference in references {
            if self
                .contexts
                .get(&reference.path)
                .is_some_and(|contexts| contexts.len() > 1)
                && self
                    .project_symbols
                    .get(&root)
                    .and_then(|index| index.get(&symbol.name))
                    .is_some_and(|candidates| {
                        candidates.iter().any(|candidate| {
                            candidate.kind == symbol.kind
                                && !group.iter().any(|selected| {
                                    selected.path == candidate.path
                                        && selected.range == candidate.range
                                })
                        })
                    })
            {
                return Err(RenameError::NotRenameable);
            }
            changes.entry(reference.path).or_default().push(TextEdit {
                range: reference.range,
                new_text: new_name.to_owned(),
            });
        }
        Ok(WorkspaceEdit { changes })
    }

    pub fn source(&self, path: &Path) -> Option<&str> {
        self.files.get(path).map(|file| file.source.as_str())
    }

    pub fn semantic_tokens(&self, path: &Path) -> Vec<(std::ops::Range<usize>, u32, u32)> {
        if let Some(tokens) = self.semantic_cache.lock().unwrap().get(path) {
            return tokens.clone();
        }
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        let mut result = BTreeMap::<(usize, usize), (u32, u32)>::new();
        let mut deprecated = HashMap::new();
        for occurrence in &file.occurrences {
            let symbol = &file.symbols[occurrence.symbol];
            let kind = match symbol.kind {
                SymbolKind::Parameter => 1,
                SymbolKind::TypeAlias => 2,
                SymbolKind::Function => 3,
                SymbolKind::Local if symbol.signature.is_some() => 3,
                SymbolKind::Local => 0,
            };
            let is_deprecated = *deprecated
                .entry(occurrence.symbol)
                .or_insert_with(|| self.deprecated_at(path, occurrence.range.start).is_some());
            result.insert(
                (occurrence.range.start, occurrence.range.end),
                (
                    kind,
                    u32::from(symbol.readonly)
                        | (u32::from(occurrence.declaration) << 1)
                        | (u32::from(is_deprecated) << 3),
                ),
            );
        }
        let mut builtin_deprecated = HashMap::new();
        for token in scan_identifiers(&file.prepared.text) {
            let range = file.prepared.original_offset(token.range.start)
                ..file.prepared.original_offset(token.range.end);
            let key = (range.start, range.end);
            // Normalization can turn typedef into a partial identifier named type.
            if file.identifier_range_at(range.start).as_ref() != Some(&range)
                || result.contains_key(&key)
            {
                continue;
            }
            let before = file.source[..range.start].trim_end();
            let after = file.source[range.end..].trim_start();
            if let Some((_, symbol)) = self.builtin_in_range(path, range.clone()) {
                let kind = match symbol.kind {
                    CompletionKind::Variable => 0,
                    CompletionKind::Function => 3,
                    CompletionKind::Type => 2,
                    CompletionKind::Property | CompletionKind::Snippet => 4,
                };
                let is_deprecated = *builtin_deprecated
                    .entry((&symbol.file, &symbol.name))
                    .or_insert_with(|| {
                        Documentation::parse(&symbol.documentation)
                            .deprecated
                            .is_some()
                    });
                result.insert(key, (kind, 4 | (u32::from(is_deprecated) << 3)));
                continue;
            }
            if before.ends_with('.') || (before.ends_with(':') && after.starts_with('(')) {
                let callable = self
                    .imported_member_at(path, range.start)
                    .map(|(_, _, member)| member.signature.is_some())
                    .unwrap_or_else(|| after.starts_with('(') || after.starts_with('{'));
                let modifiers = if self.deprecated_at(path, range.start).is_some() {
                    8
                } else {
                    0
                };
                result.insert(key, (if callable { 3 } else { 4 }, modifiers));
            }
        }
        let mut line_start = 0;
        for line in file.source.split_inclusive('\n') {
            let line_text = line.trim_end_matches(['\r', '\n']);
            let annotation = line_text.trim();
            if ["---@client", "---@server", "---@shared"].contains(&annotation) {
                let start = line_start + line_text.len() - line_text.trim_start().len();
                result.insert((start, start + annotation.len()), (6, 0));
            }
            line_start += line.len();
        }
        for (range, kind, modifiers) in &file.semantic_roles {
            let entry = result.entry((range.start, range.end)).or_insert((*kind, 0));
            entry.0 = *kind;
            entry.1 |= modifiers;
        }
        for token in file.tokens.iter() {
            if is_double_slash_comment(&file.prepared.text, token.start_position().bytes()) {
                continue;
            }
            let text = token.to_string();
            if !matches!(
                text.as_str(),
                "and"
                    | "or"
                    | "not"
                    | "~="
                    | "=="
                    | "<="
                    | ">="
                    | "+="
                    | "-="
                    | "*="
                    | "/="
                    | "//="
                    | "%="
                    | "^="
                    | "..="
                    | "<<="
                    | ">>="
                    | "&="
                    | "|="
                    | "??"
                    | "?."
                    | "::"
                    | "->"
                    | "<<"
                    | ">>"
                    | ".."
                    | "+"
                    | "-"
                    | "*"
                    | "/"
                    | "//"
                    | "%"
                    | "^"
                    | "#"
                    | "="
                    | "<"
                    | ">"
                    | "&"
                    | "|"
                    | "~"
                    | "!"
            ) {
                continue;
            }
            let start = file
                .prepared
                .original_offset(token.start_position().bytes());
            let end = file.prepared.original_offset(token.end_position().bytes());
            if start < end {
                result
                    .entry((start, end))
                    .and_modify(|entry| entry.0 = 5)
                    .or_insert((5, 0));
            }
        }
        let mut end = 0;
        let tokens = result
            .into_iter()
            .filter_map(|((start, stop), (kind, modifiers))| {
                if start < end || stop <= start {
                    return None;
                }
                end = stop;
                Some((start..stop, kind, modifiers))
            })
            .collect::<Vec<_>>();
        self.semantic_cache
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), tokens.clone());
        tokens
    }

    pub fn diagnostics(&self, path: &Path) -> Option<&[Diagnostic]> {
        Some(&self.files.get(path)?.diagnostics)
    }

    pub fn completions(&self, path: &Path, byte_offset: usize) -> Vec<CompletionItem> {
        if let Some(items) = self.documentation_completions(path, byte_offset) {
            return items;
        }
        if let Some(file) = self.files.get(path) {
            if non_code_at(&file.source, clamp_offset(&file.source, byte_offset)) {
                return Vec::new();
            }
            let offset = clamp_offset(&file.source, byte_offset);
            let prefix = completion_prefix(&file.source, offset);
            let line_start = file.source[..offset]
                .rfind('\n')
                .map_or(0, |index| index + 1);
            if prefix == "typedef"
                && file.source[line_start..offset - prefix.len()]
                    .trim()
                    .is_empty()
            {
                return vec![CompletionItem {
                    label: "typedef".to_owned(),
                    detail: "Type alias declaration".to_owned(),
                    kind: CompletionKind::Snippet,
                    insert_text: Some("typedef ${1:Name} = ${2:any}".to_owned()),
                    ..Default::default()
                }];
            }
        }
        if let Some(items) = self.imported_member_completions(path, byte_offset) {
            return items;
        }
        if let Some(items) = self.record_completions(path, byte_offset) {
            return items;
        }
        let mut items = self.base_completions(path, byte_offset);
        let Some(file) = self.files.get(path) else {
            return items;
        };
        let offset = clamp_offset(&file.source, byte_offset);
        for item in &mut items {
            let raw = self.completion_documentation(path, offset, &item.label);
            let doc = Documentation::parse(&raw);
            item.documentation = nonempty(doc.render());
            item.deprecated = doc.deprecated.is_some();
        }
        items.retain(|item| {
            let prefix = completion_prefix(&file.source, offset);
            let before = file.source[..offset - prefix.len()].trim_end();
            let receiver = before.strip_suffix('.').and_then(|text| {
                identifier_range_at(text, text.len().saturating_sub(1)).map(|r| &text[r])
            });
            let name =
                receiver.map_or_else(|| item.label.clone(), |r| format!("{r}.{}", item.label));
            builtin_index().get(&name).is_none_or(|symbols| {
                symbols
                    .iter()
                    .any(|symbol| self.builtin_available(path, symbol))
            }) || file.visible_symbol(&item.label, offset).is_some()
        });
        items
    }

    fn builtin_available(&self, path: &Path, symbol: &BuiltinSymbol) -> bool {
        symbol.file != "lua-server.d.luat"
            || self
                .contexts
                .get(path)
                .is_none_or(|contexts| !contexts.contains(&crate::ExecutionContext::Client))
    }

    fn completion_documentation(&self, path: &Path, offset: usize, label: &str) -> String {
        let Some(file) = self.files.get(path) else {
            return String::new();
        };
        let prefix = completion_prefix(&file.source, offset);
        let before = file.source[..offset - prefix.len()].trim_end();
        if let Some(receiver_end) = before.strip_suffix('.') {
            if let Some(range) =
                identifier_range_at(receiver_end, receiver_end.len().saturating_sub(1))
            {
                let receiver = &receiver_end[range.clone()];
                if let Some(binding) =
                    self.project_imports
                        .get(path)
                        .into_iter()
                        .flatten()
                        .find(|binding| {
                            file.visible_symbol(receiver, offset)
                                .is_some_and(|symbol| symbol.id == binding.symbol)
                        })
                    && let Some(member) = binding.members.get(label)
                {
                    return self
                        .files
                        .get(&binding.target_path)
                        .map_or_else(String::new, |target| {
                            documentation_before(&target.source, member.range.start)
                        });
                }
                if let Some(symbol) = builtin_index()
                    .get(&format!("{receiver}.{label}"))
                    .and_then(|group| group.first())
                {
                    return symbol.documentation.clone();
                }
            }
            return String::new();
        }
        if let Some(symbol) = file.visible_symbol(label, offset) {
            return documentation_before(&file.source, symbol.range.start);
        }
        if let Some(root) = self.project_root(path)
            && let Some(symbol) = self
                .project_symbols
                .get(&root)
                .and_then(|symbols| symbols.get(label))
                .and_then(|group| {
                    group
                        .iter()
                        .find(|symbol| self.project_symbol_available(path, symbol))
                })
        {
            return symbol.documentation.clone();
        }
        builtin_index()
            .get(label)
            .and_then(|group| group.first())
            .map_or_else(String::new, |symbol| symbol.documentation.clone())
    }

    fn base_completions(&self, path: &Path, byte_offset: usize) -> Vec<CompletionItem> {
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        let offset = clamp_offset(&file.source, byte_offset);
        if let Some(fields) = file.member_completions(offset) {
            return fields;
        }
        let mut items = BTreeMap::new();
        let prefix = completion_prefix(&file.source, offset);
        let type_position = is_type_reference_position(&file.source, offset - prefix.len());
        for symbol in &file.symbols {
            if symbol.visible.start <= offset
                && symbol.scope.start <= offset
                && offset <= symbol.scope.end
            {
                items.insert(
                    symbol.name.clone(),
                    CompletionItem {
                        label: symbol.name.clone(),
                        detail: symbol.signature.as_ref().map_or_else(
                            || symbol.type_name.clone(),
                            |signature| signature.label.clone(),
                        ),
                        kind: if symbol.kind == SymbolKind::TypeAlias {
                            CompletionKind::Type
                        } else if symbol.signature.is_some() {
                            CompletionKind::Function
                        } else {
                            CompletionKind::Variable
                        },
                        ..Default::default()
                    },
                );
            }
        }
        if let Some(root) = self.project_root(path)
            && let Some(symbols) = self.project_symbols.get(&root)
        {
            for symbols in symbols.values() {
                for symbol in symbols {
                    if symbol.name.starts_with(prefix)
                        && self.project_symbol_available(path, symbol)
                    {
                        items
                            .entry(symbol.name.clone())
                            .or_insert_with(|| CompletionItem {
                                label: symbol.name.clone(),
                                detail: symbol.signature.as_ref().map_or_else(
                                    || format!("{}: {}", symbol.name, symbol.type_name),
                                    |signature| signature.label.clone(),
                                ),
                                kind: if symbol.kind == ProjectSymbolKind::TypeAlias {
                                    CompletionKind::Type
                                } else if symbol.signature.is_some() {
                                    CompletionKind::Function
                                } else {
                                    CompletionKind::Variable
                                },
                                ..Default::default()
                            });
                    }
                }
            }
        }
        for symbols in builtin_index().values() {
            for symbol in symbols {
                if !symbol.name.contains('.')
                    && symbol.name.starts_with(prefix)
                    && (symbol.kind == CompletionKind::Type) == type_position
                {
                    items
                        .entry(symbol.name.clone())
                        .or_insert_with(|| CompletionItem {
                            label: symbol.name.clone(),
                            detail: format!("{} [{}]", symbol.detail(), symbol.context),
                            kind: symbol.kind,
                            ..Default::default()
                        });
                }
            }
        }
        items
            .into_values()
            .map(|mut item| {
                item.detail = crate::compiler::typecheck::display_type(&item.detail);
                item
            })
            .collect()
    }

    fn builtin_at(
        &self,
        path: &Path,
        byte_offset: usize,
    ) -> Option<(std::ops::Range<usize>, &'static BuiltinSymbol)> {
        let file = self.files.get(path)?;
        let range = file.identifier_range_at(byte_offset)?;
        self.builtin_in_range(path, range)
    }

    fn builtin_in_range(
        &self,
        path: &Path,
        range: std::ops::Range<usize>,
    ) -> Option<(std::ops::Range<usize>, &'static BuiltinSymbol)> {
        let file = self.files.get(path)?;
        let name = if let Some(receiver) = member_receiver_range(&file.source, &range) {
            let receiver_name = &file.source[receiver.clone()];
            if file.visible_symbol(receiver_name, receiver.start).is_some()
                || self.project_symbol_at(path, receiver.start).is_some()
                || file.source[..receiver.start].trim_end().ends_with('.')
            {
                return None;
            }
            format!("{receiver_name}.{}", &file.source[range.clone()])
        } else {
            if is_field_or_type_position(&file.source, range.start, range.end)
                && !is_type_reference_position(&file.source, range.start)
                || file
                    .visible_symbol(&file.source[range.clone()], range.start)
                    .is_some()
            {
                return None;
            }
            file.source[range.clone()].to_owned()
        };
        let type_position = is_type_reference_position(&file.source, range.start);
        let symbol = builtin_index().get(&name)?.iter().find(|symbol| {
            (symbol.kind == CompletionKind::Type) == type_position
                && self.builtin_available(path, symbol)
        })?;
        Some((range, symbol))
    }

    pub fn hover(&self, path: &Path, byte_offset: usize) -> Option<HoverResult> {
        let file = self.files.get(path)?;
        if let Some(hover) = self.record_hover(path, byte_offset) {
            return Some(hover);
        }
        if let Some((target_path, name, member)) = self.imported_member_at(path, byte_offset) {
            return Some(HoverResult {
                range: if target_path == path {
                    member.range.clone()
                } else {
                    file.identifier_range_at(byte_offset)?
                },
                contents: hover_markdown(
                    &member.signature.as_ref().map_or_else(
                        || format!("{name}: {}", member.type_name),
                        |signature| signature.label.clone(),
                    ),
                    &self
                        .files
                        .get(target_path)
                        .map_or_else(String::new, |file| {
                            documentation_before(&file.source, member.range.start)
                        }),
                ),
            });
        }
        if let Some(symbol) = self.project_symbol_at(path, byte_offset) {
            let detail = symbol
                .signature
                .as_ref()
                .map(|signature| signature.label.clone())
                .unwrap_or_else(|| format!("{}: {}", symbol.name, symbol.type_name));
            return Some(HoverResult {
                range: file.identifier_range_at(byte_offset)?,
                contents: hover_markdown(&detail, &symbol.documentation),
            });
        }
        if let Some(occurrence) = file.occurrence_at(byte_offset) {
            let symbol = &file.symbols[occurrence.symbol];
            let detail = symbol
                .signature
                .as_ref()
                .map(|signature| signature.label.clone())
                .unwrap_or_else(|| format!("{}: {}", symbol.name, symbol.type_name));
            return Some(HoverResult {
                range: occurrence.range.clone(),
                contents: hover_markdown(
                    &detail,
                    &documentation_before(&file.source, symbol.range.start),
                ),
            });
        }
        let (range, symbol) = self.builtin_at(path, byte_offset)?;
        Some(HoverResult {
            range,
            contents: hover_markdown(&symbol.detail(), &symbol.documentation),
        })
    }

    pub fn signature_help(&self, path: &Path, byte_offset: usize) -> Option<SignatureHelp> {
        let file = self.files.get(path)?;
        let offset = clamp_offset(&file.source, byte_offset);
        let tokens = &file.tokens;
        let mut opens = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            let start = token.start_position().bytes();
            if file.prepared.original_offset(start) >= offset {
                break;
            }
            match token.to_string().trim() {
                "(" => opens.push(index),
                ")" => {
                    opens.pop();
                }
                _ => {}
            }
        }
        let open = opens.into_iter().rev().find(|&open| {
            open > 0 && matches!(tokens[open - 1].token_type(), TokenType::Identifier { .. })
        })?;
        let name_token = &tokens[open - 1];
        let name = name_token.to_string().trim().to_owned();
        let mut active_parameter = 0;
        let mut nested = 0usize;
        for token in &tokens[open + 1..] {
            if file
                .prepared
                .original_offset(token.start_position().bytes())
                >= offset
            {
                break;
            }
            match token.to_string().trim() {
                "(" | "{" | "[" => nested += 1,
                ")" | "}" | "]" => nested = nested.saturating_sub(1),
                "," if nested == 0 => active_parameter += 1,
                _ => {}
            }
        }
        let name_offset = file
            .prepared
            .original_offset(name_token.start_position().bytes());
        let member = file
            .identifier_range_at(name_offset)
            .and_then(|range| member_receiver_range(&file.source, &range));
        let signatures = if let Some(signature) = self.member_signature(path, name_offset) {
            vec![signature]
        } else if let Some(signatures) = file
            .visible_symbol(&name, name_offset)
            .filter(|_| member.is_none())
            .and_then(|symbol| symbol.signature.clone())
            .map(|signature| vec![signature])
            .or_else(|| {
                let offset = file
                    .prepared
                    .original_offset(name_token.start_position().bytes());
                self.imported_member_at(path, offset)
                    .and_then(|(_, _, member)| member.signature.clone())
                    .map(|signature| vec![signature])
            })
        {
            signatures
        } else if let Some(group) = self
            .project_group(path, &name, ProjectSymbolKind::Function)
            .filter(|_| member.is_none())
        {
            group
                .iter()
                .filter(|symbol| symbol.kind == ProjectSymbolKind::Function)
                .filter_map(|symbol| symbol.signature.clone())
                .collect()
        } else {
            self.builtin_at(path, name_offset)
                .and_then(|(_, symbol)| builtin_index().get(&symbol.name))
                .into_iter()
                .flatten()
                .filter(|symbol| self.builtin_available(path, symbol))
                .filter_map(|symbol| symbol.signature.clone())
                .collect()
        };
        if signatures.is_empty() {
            return None;
        }
        let active_signature = signatures
            .iter()
            .position(|signature| {
                signature.parameters.len() > active_parameter
                    || signature
                        .parameters
                        .last()
                        .is_some_and(|parameter| parameter.starts_with("..."))
            })
            .unwrap_or(0);
        active_parameter = active_parameter.min(
            signatures[active_signature]
                .parameters
                .len()
                .saturating_sub(1),
        );
        Some(SignatureHelp {
            signatures: signatures
                .into_iter()
                .map(|mut signature| {
                    signature.label = crate::compiler::typecheck::display_type(&signature.label);
                    signature.parameters = signature
                        .parameters
                        .into_iter()
                        .map(|ty| crate::compiler::typecheck::display_type(&ty))
                        .collect();
                    signature
                })
                .collect(),
            active_signature,
            active_parameter,
        })
    }

    pub fn definition(&self, path: &Path, byte_offset: usize) -> Option<FileRange> {
        let file = self.files.get(path)?;
        if let Some((target_path, _, member)) = self.imported_member_at(path, byte_offset) {
            return Some(FileRange {
                path: target_path.clone(),
                range: member.range.clone(),
            });
        }
        if let Some(symbol) = self.project_symbol_at(path, byte_offset) {
            return Some(FileRange {
                path: symbol.path.clone(),
                range: symbol.range.clone(),
            });
        }
        if let Some(occurrence) = file.occurrence_at(byte_offset) {
            let symbol = &file.symbols[occurrence.symbol];
            return Some(FileRange {
                path: path.to_path_buf(),
                range: symbol.range.clone(),
            });
        }
        self.builtin_at(path, byte_offset)
            .map(|(_, symbol)| FileRange {
                path: builtin_source_path(&symbol.file),
                range: symbol.range.clone(),
            })
    }

    pub fn references(
        &self,
        path: &Path,
        byte_offset: usize,
        include_declaration: bool,
    ) -> Vec<FileRange> {
        let Some(file) = self.files.get(path) else {
            return Vec::new();
        };
        if let Some((target_path, name, _)) = self.imported_member_at(path, byte_offset) {
            return self.import_member_references(target_path, name, include_declaration);
        }
        if let Some(symbol) = self.project_symbol_at(path, byte_offset) {
            return self.project_references(path, &symbol.name, symbol.kind, include_declaration);
        }
        let Some(occurrence) = file.occurrence_at(byte_offset) else {
            return Vec::new();
        };
        file.occurrences
            .iter()
            .filter(|candidate| {
                candidate.symbol == occurrence.symbol
                    && (include_declaration || !candidate.declaration)
            })
            .map(|candidate| FileRange {
                path: path.to_path_buf(),
                range: candidate.range.clone(),
            })
            .collect()
    }

    pub fn rename(
        &self,
        path: &Path,
        byte_offset: usize,
        new_name: &str,
    ) -> Result<WorkspaceEdit, RenameError> {
        if !valid_identifier(new_name) {
            return Err(RenameError::InvalidName);
        }
        let file = self.files.get(path).ok_or(RenameError::MissingFile)?;
        if !file.syntax_valid {
            return Err(RenameError::NotRenameable);
        }
        if let Some((target_path, name, _)) = self.imported_member_at(path, byte_offset) {
            return self.rename_import_member(target_path, name, new_name);
        }
        let local_function = file.occurrence_at(byte_offset).is_some_and(|occurrence| {
            file.editor.declarations.iter().any(|declaration| {
                declaration.local
                    && declaration.kind == EditorSymbolKind::Function
                    && declaration.selection == file.symbols[occurrence.symbol].range
            })
        });
        if !local_function && let Some(symbol) = self.project_symbol_at(path, byte_offset) {
            return self.rename_project_symbol(path, symbol, new_name);
        }
        let Some(occurrence) = file.occurrence_at(byte_offset) else {
            if self.builtin_at(path, byte_offset).is_some() {
                return Err(RenameError::NotRenameable);
            }
            return Err(RenameError::InvalidOffset);
        };
        let symbol = &file.symbols[occurrence.symbol];
        if !matches!(
            symbol.kind,
            SymbolKind::Local | SymbolKind::Parameter | SymbolKind::TypeAlias
        ) && !local_function
        {
            return Err(RenameError::NotRenameable);
        }
        if file.symbols.iter().any(|other| {
            other.name == new_name
                && other.id != symbol.id
                && ranges_overlap(&other.visible, &symbol.visible)
        }) {
            return Err(RenameError::NameConflict);
        }
        if scan_identifiers(&file.prepared.text).iter().any(|token| {
            let start = file.prepared.original_offset(token.range.start);
            let end = file.prepared.original_offset(token.range.end);
            token.name == new_name
                && symbol.visible.contains(&start)
                && !is_field_or_type_position(&file.source, start, end)
                && file.occurrence_at(start).is_none()
        }) {
            return Err(RenameError::NameConflict);
        }
        let edits = file
            .occurrences
            .iter()
            .filter(|candidate| candidate.symbol == symbol.id)
            .map(|candidate| TextEdit {
                range: candidate.range.clone(),
                new_text: new_name.to_owned(),
            })
            .collect();
        Ok(WorkspaceEdit {
            changes: BTreeMap::from([(path.to_path_buf(), edits)]),
        })
    }
}

fn collect_import_bindings(
    file: &AnalyzedFile,
    imports: &HashMap<String, crate::project::ResolvedImport>,
    files: &BTreeMap<PathBuf, Arc<AnalyzedFile>>,
) -> Vec<ImportBinding> {
    let calls = file.parsed.editor_import_calls();
    let Ok(calls) = calls else {
        return Vec::new();
    };
    let calls = calls
        .into_iter()
        .filter_map(|call| {
            imports
                .get(&call.specifier)
                .map(|import| (call.source_range.start, import))
        })
        .collect::<HashMap<_, _>>();
    if calls.is_empty() {
        return Vec::new();
    }
    let mut collector = ImportBindingCollector {
        prepared: &file.prepared,
        calls,
        bindings: Vec::new(),
    };
    collector.visit_ast(&file.parsed.ast);
    collector
        .bindings
        .into_iter()
        .filter_map(|(name, range, import)| {
            let symbol = file.symbols.iter().position(|symbol| {
                symbol.kind == SymbolKind::Local && symbol.name == name && symbol.range == range
            })?;
            let target = files.get(&import.target_path)?;
            Some(ImportBinding {
                symbol,
                target_path: import.target_path.clone(),
                type_name: import.type_name.clone(),
                members: exported_table_members(target, &import.type_name),
            })
        })
        .collect()
}

struct ImportBindingCollector<'a> {
    prepared: &'a syntax::PreparedSource,
    calls: HashMap<usize, &'a crate::project::ResolvedImport>,
    bindings: Vec<(
        String,
        std::ops::Range<usize>,
        &'a crate::project::ResolvedImport,
    )>,
}

impl ImportBindingCollector<'_> {
    fn assignment(
        &mut self,
        names: impl Iterator<Item = (String, std::ops::Range<usize>)>,
        expressions: impl Iterator<Item = Option<usize>>,
    ) {
        for ((name, range), expression) in names.zip(expressions) {
            let Some(offset) = expression else { continue };
            let Some(import) = self.calls.get(&offset) else {
                continue;
            };
            self.bindings.push((name, range, *import));
        }
    }
}

impl Visitor for ImportBindingCollector<'_> {
    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        let names = node.names().iter().map(|name| {
            let token = name.token();
            let start = self
                .prepared
                .original_offset(token.start_position().bytes());
            let end = self.prepared.original_offset(token.end_position().bytes());
            (token.to_string(), start..end)
        });
        let expressions = node.expressions().iter().map(|expression| {
            expression
                .start_position()
                .map(|position| self.prepared.original_offset(position.bytes()))
        });
        self.assignment(names, expressions);
    }

    fn visit_const_assignment(&mut self, node: &ast::luau::ConstAssignment) {
        let names = node.names().iter().map(|name| {
            let token = name.token();
            let start = self
                .prepared
                .original_offset(token.start_position().bytes());
            let end = self.prepared.original_offset(token.end_position().bytes());
            (token.to_string(), start..end)
        });
        let expressions = node.expressions().iter().map(|expression| {
            expression
                .start_position()
                .map(|position| self.prepared.original_offset(position.bytes()))
        });
        self.assignment(names, expressions);
    }
}

fn exported_table_members(file: &AnalyzedFile, export_type: &str) -> HashMap<String, ImportMember> {
    let export_type = crate::compiler::typecheck::editor_unwrap_type(export_type);
    let variants = crate::compiler::typecheck::editor_type_parts(export_type, '|');
    if variants.len() > 1 {
        let mut common = exported_table_members(file, variants[0]);
        for variant in &variants[1..] {
            let other = exported_table_members(file, variant);
            common.retain(|name, member| {
                let Some(candidate) = other.get(name) else {
                    return false;
                };
                if member.type_name != candidate.type_name {
                    member.type_name =
                        format!("({}) | ({})", member.type_name, candidate.type_name);
                    member.signature = None;
                }
                true
            });
        }
        return common;
    }
    let source = &file.source;
    let prepared = &file.prepared;
    let ast = &file.parsed.ast;
    let Some(body) = export_type
        .trim()
        .strip_prefix('{')
        .and_then(|ty| ty.strip_suffix('}'))
    else {
        return HashMap::new();
    };
    let field_types = split_type_fields(body)
        .into_iter()
        .filter_map(|field| {
            let (name, type_name) = field.split_once(':')?;
            Some((name.trim().to_owned(), type_name.trim().to_owned()))
        })
        .collect::<HashMap<_, _>>();
    if field_types.is_empty() {
        return HashMap::new();
    }
    // shortcut: only direct returned table constructors, trace local exports when module flow supports it.
    let Some(ast::LastStmt::Return(return_statement)) = ast.nodes().last_stmt() else {
        return HashMap::new();
    };
    let Some(ast::Expression::TableConstructor(table)) = return_statement.returns().iter().next()
    else {
        return HashMap::new();
    };
    let signatures = SemanticCollector::new(source, prepared);
    table
        .fields()
        .iter()
        .filter_map(|field| {
            let ast::Field::NameKey { key, value, .. } = field else {
                return None;
            };
            let name = key.token().to_string();
            let type_name = field_types.get(&name)?.clone();
            let start = prepared.original_offset(key.start_position()?.bytes());
            let end = prepared.original_offset(key.end_position()?.bytes());
            let signature = match value.as_ref() {
                ast::Expression::Function(function) => {
                    Some(signatures.signature(function.body(), &name))
                }
                _ => None,
            };
            Some((
                name,
                ImportMember {
                    range: start..end,
                    type_name,
                    signature,
                },
            ))
        })
        .collect()
}

fn lex(source: &str) -> Vec<Token> {
    raw_tokens(source)
        .into_iter()
        .filter(|token| {
            !token.token_type().is_trivia() && !matches!(token.token_type(), TokenType::Eof)
        })
        .collect()
}

fn raw_tokens(source: &str) -> Vec<Token> {
    match Lexer::new(source, LuaVersion::cfxlua().with_luau()).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => Vec::new(),
    }
}

fn non_code_at(source: &str, offset: usize) -> bool {
    let mut index = 0;
    while index < source.len() && index < offset {
        let rest = &source[index..];
        let comment = rest.starts_with("--");
        let opening = if comment { index + 2 } else { index };
        let mut bracket = opening + 1;
        if source.as_bytes().get(opening) == Some(&b'[') {
            while source.as_bytes().get(bracket) == Some(&b'=') {
                bracket += 1;
            }
            if source.as_bytes().get(bracket) == Some(&b'[') {
                let closing = format!("]{}]", "=".repeat(bracket - opening - 1));
                let end = source[bracket + 1..]
                    .find(&closing)
                    .map(|end| bracket + 1 + end + closing.len());
                if end.is_none_or(|end| offset < end) {
                    return true;
                }
                index = end.unwrap();
                continue;
            }
        }
        if comment {
            let end = rest.find('\n').map_or(source.len(), |end| index + end);
            if offset <= end {
                return true;
            }
            index = end + 1;
            continue;
        }
        if let Some(comment) = rest.strip_prefix("/*") {
            let end = comment.find("*/").map(|end| index + end + 4);
            if end.is_none_or(|end| offset < end) {
                return true;
            }
            index = end.unwrap();
            continue;
        }
        let quote = rest.chars().next().unwrap();
        if matches!(quote, '\'' | '"') {
            let mut end = index + 1;
            let mut escaped = false;
            let mut closed = false;
            for character in source[end..].chars() {
                end += character.len_utf8();
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == quote {
                    closed = true;
                    break;
                }
            }
            if offset < end || !closed && offset == end {
                return true;
            }
            index = end;
            continue;
        }
        index += quote.len_utf8();
    }
    false
}

fn is_field_or_type_position(source: &str, start: usize, end: usize) -> bool {
    let before = source[..start].trim_end();
    let after = source[end..].trim_start();
    if before.ends_with(['.', ':']) || before.ends_with("typedef") || before.ends_with("typeof") {
        return true;
    }
    if after.starts_with(':') {
        return true;
    }
    if after.starts_with('=') && (before.ends_with('{') || before.ends_with(',')) {
        return true;
    }
    false
}

fn is_type_reference_position(source: &str, start: usize) -> bool {
    let before = source[..start].trim_end();
    before.ends_with(':')
        || before.ends_with("typeof")
        || source[..start]
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .trim_start()
            .starts_with("typedef ")
}

fn split_type_fields(source: &str) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut closing = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in source.char_indices() {
        if let Some(current) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == current {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '{' => closing.push('}'),
            '(' => closing.push(')'),
            '[' => closing.push(']'),
            '<' => closing.push('>'),
            '}' | ')' | ']' | '>' if closing.last() == Some(&character) => {
                closing.pop();
            }
            ',' if closing.is_empty() => {
                fields.push(source[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if start < source.len() {
        fields.push(source[start..].trim());
    }
    fields
}

fn is_double_slash_comment(source: &str, offset: usize) -> bool {
    let line_start = source[..offset]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    source[line_start..]
        .trim_start()
        .strip_prefix("//")
        .is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace))
}

fn scan_identifiers(source: &str) -> Vec<IdentifierToken> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let character = source[index..].chars().next().unwrap();
        if character == '-' && source[index..].starts_with("--") {
            if let Some(end) = syntax::long_bracket_end(source, index + 2, None) {
                index = end;
            } else {
                index = source[index..]
                    .find('\n')
                    .map_or(source.len(), |end| index + end + 1);
            }
            continue;
        }
        if character == '\'' || character == '"' {
            index = quoted_end(source, index, character);
            continue;
        }
        if character == '['
            && let Some(end) = syntax::long_bracket_end(source, index, None)
        {
            index = end;
            continue;
        }
        if character == '_' || character.is_ascii_alphabetic() {
            let start = index;
            index += character.len_utf8();
            while index < source.len() {
                let next = source[index..].chars().next().unwrap();
                if next == '_' || next.is_ascii_alphanumeric() {
                    index += next.len_utf8();
                } else {
                    break;
                }
            }
            result.push(IdentifierToken {
                name: source[start..index].to_owned(),
                range: start..index,
            });
            continue;
        }
        index += character.len_utf8();
    }
    result
}

#[derive(Clone)]
struct IdentifierToken {
    name: String,
    range: std::ops::Range<usize>,
}

fn completion_prefix(source: &str, byte_offset: usize) -> &str {
    let prefix = &source[..clamp_offset(source, byte_offset)];
    let start = prefix
        .char_indices()
        .rev()
        .take_while(|(_, character)| *character == '_' || character.is_ascii_alphanumeric())
        .last()
        .map_or(prefix.len(), |(index, _)| index);
    &prefix[start..]
}

fn identifier_range_at(source: &str, byte_offset: usize) -> Option<std::ops::Range<usize>> {
    let offset = clamp_offset(source, byte_offset);
    scan_identifiers(source)
        .into_iter()
        .find(|token| {
            token.range.contains(&offset) || token.range.end == offset && token.range.start < offset
        })
        .map(|token| token.range)
}

fn quoted_end(source: &str, start: usize, quote: char) -> usize {
    let mut index = start + quote.len_utf8();
    let mut escaped = false;
    while index < source.len() {
        let character = source[index..].chars().next().unwrap();
        index += character.len_utf8();
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == quote {
            break;
        }
    }
    index
}

fn clamp_offset(source: &str, byte_offset: usize) -> usize {
    let mut offset = byte_offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset = offset.saturating_sub(1);
    }
    offset
}

fn ranges_overlap(left: &std::ops::Range<usize>, right: &std::ops::Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn member_receiver_range(
    source: &str,
    member: &std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    let before_member = source.get(..member.start)?.trim_end();
    let dot = before_member.len().checked_sub(1)?;
    if !before_member.ends_with('.') {
        return None;
    }
    receiver_range(source, before_member[..dot].trim_end().len())
}

fn receiver_before_dot(source: &str, byte_offset: usize) -> Option<std::ops::Range<usize>> {
    let offset = clamp_offset(source, byte_offset);
    let member_prefix = completion_prefix(source, offset);
    let prefix = source.get(..offset - member_prefix.len())?.trim_end();
    if !prefix.ends_with('.') {
        return None;
    }
    let dot = prefix.len().checked_sub(1)?;
    receiver_range(source, prefix[..dot].trim_end().len())
}

fn receiver_range(source: &str, receiver_end: usize) -> Option<std::ops::Range<usize>> {
    let prefix = source.get(..receiver_end)?;
    let start = prefix
        .char_indices()
        .rev()
        .take_while(|(_, character)| *character == '_' || character.is_ascii_alphanumeric())
        .last()
        .map(|(index, _)| index)?;
    Some(start..receiver_end)
}

fn valid_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod runtime_hover_tests {
    use super::*;

    fn analyze(source: &str) -> (AnalysisSnapshot, PathBuf) {
        let path = PathBuf::from("target/tests/runtime-hover/main.luat");
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.into());
        (host.snapshot(), path)
    }

    #[test]
    fn runtime_globals_types_and_partial_members_complete() {
        for name in ["json", "Citizen", "msgpack", "promise", "GlobalState"] {
            let (snapshot, path) = analyze(name);
            assert!(
                snapshot
                    .completions(&path, name.len())
                    .iter()
                    .any(|item| item.label == name),
                "{name}"
            );
        }
        let source = "local state: json_encode_st";
        let (snapshot, path) = analyze(source);
        assert!(
            snapshot
                .completions(&path, source.len())
                .iter()
                .any(|item| item.label == "json_encode_state")
        );
        for (source, expected) in [
            ("json.", "encode"),
            ("json.en", "encode"),
            ("Citizen.", "CreateThread"),
            ("msgpack.pa", "pack"),
        ] {
            let (snapshot, path) = analyze(source);
            let items = snapshot.completions(&path, source.len());
            assert!(
                items.iter().any(|item| item.label == expected),
                "{source}: {items:?}"
            );
            assert!(items.iter().all(|item| !item.label.contains('.')));
        }
    }

    #[test]
    fn runtime_members_resolve_qualified_names_and_preserve_ranges() {
        let source = "local title = 'ş😀'\njson.encode({})";
        let (snapshot, path) = analyze(source);
        let offset = source.find("encode").unwrap();
        let hover = snapshot.hover(&path, offset).unwrap();
        assert_eq!(&source[hover.range], "encode");
        assert!(
            hover
                .contents
                .contains("function json.encode(object: any, state: json_encode_state?): string"),
            "{}",
            hover.contents
        );
        let definition = snapshot.definition(&path, offset).unwrap();
        assert!(definition.path.ends_with("runtime-shared.d.luat"));
        assert_eq!(
            snapshot.rename(&path, offset, "encodeNew"),
            Err(RenameError::NotRenameable)
        );
        let help = snapshot
            .signature_help(&path, source.find("({").unwrap() + 1)
            .unwrap();
        assert_eq!(
            help.signatures[0].label,
            "function json.encode(object: any, state: json_encode_state?): string"
        );
        let (snapshot, path) = analyze("unknown.encode({})");
        assert!(snapshot.hover(&path, 10).is_none());
        assert!(snapshot.signature_help(&path, 15).is_none());
    }

    #[test]
    fn local_receivers_shadow_runtime_members() {
        for declaration in ["local json = { custom = 1 }", "local json: any = {}"] {
            let source = format!("{declaration}\njson.en");
            let (snapshot, path) = analyze(&source);
            assert!(
                !snapshot
                    .completions(&path, source.len())
                    .iter()
                    .any(|item| item.label == "encode")
            );
            let source = format!("{declaration}\njson.encode({{}})");
            let (snapshot, path) = analyze(&source);
            assert!(
                snapshot
                    .hover(&path, source.find("encode").unwrap())
                    .is_none()
            );
            assert!(
                snapshot
                    .signature_help(&path, source.rfind('(').unwrap() + 1)
                    .is_none()
            );
        }
        let (snapshot, path) = analyze("other.json.en");
        assert!(snapshot.completions(&path, 13).is_empty());
    }

    #[test]
    fn runtime_type_and_value_names_resolve_in_separate_namespaces() {
        let source = "local pending: promise = nil\nlocal value = promise.new()";
        let (snapshot, path) = analyze(source);
        let type_offset = source.find("promise").unwrap();
        let type_hover = snapshot.hover(&path, type_offset).unwrap();
        assert!(
            type_hover.contents.contains("promise: {"),
            "{}",
            type_hover.contents
        );
        let definition = snapshot.definition(&path, type_offset).unwrap();
        let runtime = crate::definitions::RUNTIME_SHARED;
        assert_eq!(
            definition.range.start,
            runtime.find("typedef promise =").unwrap() + "typedef ".len()
        );
        let type_items = snapshot.completions(&path, type_offset + "promise".len());
        assert!(
            type_items
                .iter()
                .any(|item| item.label == "promise" && item.kind == CompletionKind::Type)
        );
        let value_offset = source.rfind("promise").unwrap();
        assert!(
            snapshot
                .hover(&path, value_offset)
                .unwrap()
                .contents
                .contains("promise: PromiseApi")
        );
    }

    #[test]
    fn completion_kinds_distinguish_callable_values_from_callback_tables() {
        let source = "local config: { handlers: { run: () -> () }, run: () -> () } = {}\nconfig.";
        let (snapshot, path) = analyze(source);
        let items = snapshot.completions(&path, source.len());
        assert_eq!(
            items
                .iter()
                .find(|item| item.label == "handlers")
                .unwrap()
                .kind,
            CompletionKind::Property
        );
        assert_eq!(
            items.iter().find(|item| item.label == "run").unwrap().kind,
            CompletionKind::Function
        );
        let (snapshot, path) = analyze("exports");
        assert_eq!(
            snapshot
                .completions(&path, 7)
                .iter()
                .find(|item| item.label == "exports")
                .unwrap()
                .kind,
            CompletionKind::Function
        );
    }

    #[test]
    fn hover_has_highlighted_signature_and_optional_documentation() {
        let (snapshot, path) = analyze("SendNUIMessage({})");
        let contents = snapshot.hover(&path, 2).unwrap().contents;
        assert!(
            contents.starts_with("```luat\nfunction SendNUIMessage(data: table)"),
            "{contents}"
        );
        assert!(!contents.contains("**Documentation**"));
        assert!(contents.contains("Sends a message to the NUI frame."));
        let source = "---Finds a value.\nlocal function find(value: string?, ...: number): (string?, number) return value, 1 end\nfind(nil)";
        let (snapshot, path) = analyze(source);
        let contents = snapshot
            .hover(&path, source.rfind("find").unwrap())
            .unwrap()
            .contents;
        assert!(
            contents.contains("function find(value: string?, ...: number): (string?, number)"),
            "{contents}"
        );
        assert!(contents.contains("Finds a value."));
        let (snapshot, path) = analyze("local function plain(value) return value end\nplain(1)");
        assert_eq!(
            snapshot.hover(&path, 46).unwrap().contents,
            "```luat\nfunction plain(value: any)\n```"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{AnalysisHost, RenameError};
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn completion_and_hover_use_the_latest_buffer_and_keep_old_snapshots() {
        let path = PathBuf::from("src/main.luat");
        let mut host = AnalysisHost::new();
        host.set_file(
            path.clone(),
            "local amount: number = 1\nprint(amount)".to_owned(),
        );
        let old = host.snapshot();
        host.set_file(
            path.clone(),
            "local title: string = 'ready'\nprint(title)".to_owned(),
        );
        let current = host.snapshot();

        assert!(
            old.completions(&path, 30)
                .iter()
                .any(|item| item.label == "amount")
        );
        assert!(
            !current
                .completions(&path, 30)
                .iter()
                .any(|item| item.label == "amount")
        );
        let offset = "local title: string = 'ready'\nprint(ti".len();
        assert_eq!(
            current.hover(&path, offset).unwrap().contents,
            "```luat\ntitle: string\n```"
        );
    }

    #[test]
    fn rename_tracks_shadowing_and_closure_references() {
        let path = PathBuf::from("src/main.luat");
        let source = "local value: number = 1\ndo\n local value: string = 'inner'\n print(value)\nend\nlocal count: number = 0\nlocal read = function() return value end";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.to_owned());
        let snapshot = host.snapshot();
        let outer = source.find("value").unwrap();
        let edit = snapshot.rename(&path, outer, "renamed").unwrap();
        let changes = &edit.changes[&path];
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|edit| &source[edit.range.clone()] == "value")
        );
        let inner = source[outer + 1..].find("value").unwrap() + outer + 1;
        assert_eq!(
            snapshot.rename(&path, inner, "renamed").unwrap().changes[&path].len(),
            2
        );
        assert_eq!(
            snapshot.rename(&path, outer, "count").unwrap_err(),
            RenameError::NameConflict
        );
    }

    #[test]
    fn signature_help_counts_nested_call_arguments() {
        let path = PathBuf::from("src/main.luat");
        let source = "---@param left The first operand.\n---@param right The second operand.\nlocal function combine(left: string, right: number): string return left end\ncombine('x', 2)";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.to_owned());
        let snapshot = host.snapshot();
        let offset = source.rfind("2)").unwrap();
        let help = snapshot.signature_help(&path, offset).unwrap();
        assert_eq!(help.active_parameter, 1);
        assert_eq!(help.signatures[0].parameters.len(), 2);
        assert_eq!(
            help.signatures[0].parameter_documentation,
            [
                Some("The first operand.".to_owned()),
                Some("The second operand.".to_owned())
            ]
        );

        let native_source = "SetPedArmour(1, 100)";
        host.set_file(path.clone(), native_source.to_owned());
        let native = host.snapshot();
        let native_help = native
            .signature_help(&path, native_source.rfind("100").unwrap())
            .unwrap();
        assert!(native_help.signatures.iter().any(|signature| {
            signature
                .parameter_documentation
                .get(1)
                .is_some_and(|documentation| {
                    documentation
                        .as_deref()
                        .is_some_and(|text| text.contains("value between 0 and 100"))
                })
        }));
    }

    #[test]
    fn invalid_or_builtin_renames_are_rejected() {
        let path = PathBuf::from("src/main.luat");
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), "print('unchanged')".to_owned());
        let snapshot = host.snapshot();
        assert_eq!(
            snapshot.rename(&path, 0, "newName"),
            Err(RenameError::NotRenameable)
        );
        assert_eq!(
            snapshot.rename(&path, 0, "not-valid"),
            Err(RenameError::InvalidName)
        );
        let source = "GetEntityCoords(1)";
        host.set_file(path.clone(), source.to_owned());
        let snapshot = host.snapshot();
        assert_eq!(
            snapshot.rename(&path, source.find("GetEntityCoords").unwrap(), "coords"),
            Err(RenameError::NotRenameable)
        );
        assert!(
            snapshot
                .hover(&path, source.find("GetEntityCoords").unwrap())
                .unwrap()
                .contents
                .contains("Native Documentation")
        );
        let definition = snapshot
            .definition(&path, source.find("GetEntityCoords").unwrap())
            .unwrap();
        let declaration = fs::read_to_string(definition.path).unwrap();
        assert_eq!(&declaration[definition.range], "GetEntityCoords");
        assert!(
            snapshot
                .completions(&path, "GetEnt".len())
                .iter()
                .any(|item| item.label == "GetEntityCoords")
        );
        assert!(snapshot.signature_help(&path, source.len() - 1).is_some());
    }

    #[test]
    fn type_alias_references_are_resolved_and_renamed() {
        let path = PathBuf::from("src/main.luat");
        let source = "typedef UserId = integer\nlocal id: UserId = 1\nprint(id)";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.to_owned());
        let snapshot = host.snapshot();
        let use_offset = source.rfind("UserId").unwrap();
        assert_eq!(snapshot.definition(&path, use_offset).unwrap().range, 8..14);
        assert_eq!(snapshot.references(&path, use_offset, true).len(), 2);
        assert_eq!(
            snapshot
                .rename(&path, use_offset, "AccountId")
                .unwrap()
                .changes[&path]
                .len(),
            2
        );
    }

    #[test]
    fn partial_code_keeps_member_completions_and_disables_rename() {
        let path = PathBuf::from("src/main.luat");
        let source = "local object: { nested: { count: number }, title: string } = { nested = { count = 1 }, title = 'ready' }\nif then\nobject.";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.to_owned());
        let snapshot = host.snapshot();

        let items = snapshot.completions(&path, source.len());
        assert!(items.iter().any(|item| item.label == "nested"), "{items:?}");
        assert!(items.iter().any(|item| item.label == "title"));
        assert!(
            snapshot
                .diagnostics(&path)
                .unwrap()
                .iter()
                .any(|item| item.code == "LT1001")
        );
        assert_eq!(
            snapshot.rename(&path, source.find("object").unwrap(), "entity"),
            Err(RenameError::NotRenameable)
        );
    }

    #[test]
    fn semantic_ranges_remain_byte_accurate_after_multibyte_text() {
        let path = PathBuf::from("src/main.luat");
        let source = "local value: string = '🚀 Türkçe'\nprint(value)";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.to_owned());
        let snapshot = host.snapshot();
        let reference = source.rfind("value").unwrap();
        let hover = snapshot.hover(&path, reference).unwrap();
        assert_eq!(&source[hover.range], "value");
        assert_eq!(snapshot.references(&path, reference, true).len(), 2);
    }

    #[test]
    fn semantic_ranges_map_template_interpolations_to_original_source() {
        let path = PathBuf::from("src/main.luat");
        let source =
            "local name: string = 'LuaT'\nlocal message: string = `Hello {name}`\nprint(message)";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.to_owned());
        let snapshot = host.snapshot();
        let reference = source.rfind("name").unwrap();
        let hover = snapshot.hover(&path, reference).unwrap();
        assert_eq!(&source[hover.range.clone()], "name");
        assert_eq!(snapshot.definition(&path, reference).unwrap().range, 6..10);
        assert_eq!(snapshot.references(&path, reference, true).len(), 2);
    }

    #[test]
    fn workspace_snapshot_checks_unsaved_imported_buffers() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-workspace-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        let api_path = root.join("src/shared/api.luat");
        let main_path = root.join("src/client/main.luat");
        fs::write(
            &api_path,
            "return { get = function(): string return 'ready' end }",
        )
        .unwrap();
        let main_source = "local api = import('@/shared/api')\nlocal value: string = api.get()";
        fs::write(&main_path, main_source).unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(main_path.clone(), main_source.to_owned());
        host.set_file(
            api_path.clone(),
            "return { get = function(): number return 1 end }".to_owned(),
        );
        let snapshot = host.snapshot();
        assert_eq!(snapshot.diagnostics(&main_path).unwrap()[0].code, "LT2001");
        assert!(
            fs::read_to_string(api_path)
                .unwrap()
                .contains("return 'ready'")
        );
        host.set_file(
            root.join("src/shared/api.luat"),
            "return { get = function(): string return 'updated' end }".to_owned(),
        );
        let updated = host.snapshot();
        assert!(
            updated.diagnostics(&main_path).unwrap().is_empty(),
            "{:?}",
            updated.diagnostics(&main_path)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_snapshot_rechecks_open_dependents_after_disk_changes() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-disk-import-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        let api_path = root.join("src/shared/api.luat");
        let main_path = root.join("src/client/main.luat");
        fs::write(
            &api_path,
            "return { get = function(): string return 'ready' end }",
        )
        .unwrap();
        fs::write(
            &main_path,
            "local api = import('@/shared/api')\nlocal value: string = api.get()",
        )
        .unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(main_path.clone(), fs::read_to_string(&main_path).unwrap());
        assert!(host.snapshot().diagnostics(&main_path).unwrap().is_empty());

        fs::write(
            &api_path,
            "return { get = function(): number return 1 end }",
        )
        .unwrap();
        host.invalidate_path(&api_path);
        let updated = host.snapshot();
        assert_eq!(updated.diagnostics(&main_path).unwrap()[0].code, "LT2001");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workspace_snapshot_reports_deleted_imports_and_cycles() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-import-lifecycle-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        let main_path = root.join("src/client/main.luat");
        let api_path = root.join("src/shared/api.luat");
        let nested_path = root.join("src/shared/nested.luat");
        let main_source = "local api = import('@/shared/api')\napi.get()";
        fs::write(&main_path, main_source).unwrap();
        fs::write(
            &api_path,
            "return { get = function(): string return 'ready' end }",
        )
        .unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(main_path.clone(), main_source.to_owned());
        assert!(host.snapshot().diagnostics(&main_path).unwrap().is_empty());

        fs::remove_file(&api_path).unwrap();
        host.invalidate_path(&api_path);
        assert!(
            host.snapshot()
                .diagnostics(&main_path)
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic.code == "LT4003")
        );

        fs::write(&api_path, "return import('./nested')").unwrap();
        fs::write(&nested_path, "return import('./api')").unwrap();
        host.invalidate_path(&api_path);
        host.invalidate_path(&nested_path);
        let cyclic = host.snapshot();
        assert!(
            [&main_path, &api_path, &nested_path]
                .into_iter()
                .filter_map(|path| cyclic.diagnostics(path))
                .flatten()
                .any(|diagnostic| diagnostic.code == "LT4005")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_symbols_support_cross_file_definitions_references_and_rename() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-project-symbols-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        fs::create_dir_all(root.join("types")).unwrap();
        let main_path = root.join("src/client/main.luat");
        let api_path = root.join("src/shared/api.luat");
        let declaration_path = root.join("types/api.d.luat");
        let main_source =
            "local id: UserId = 1\nprint(read_config(), NativeConfig(), GlobalCount, id)";
        fs::write(&main_path, main_source).unwrap();
        fs::write(
            &api_path,
            "---Reads the configuration.\nfunction read_config(): number return 1 end",
        )
        .unwrap();
        fs::write(
            &declaration_path,
            "typedef UserId = integer\ndeclare GlobalCount: number\nfunction NativeConfig(): string end",
        )
        .unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[compiler]\ndeclarations=['types/*.d.luat']\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(main_path.clone(), main_source.to_owned());
        let snapshot = host.snapshot();
        let read_config = main_source.find("read_config").unwrap();
        let hover = snapshot.hover(&main_path, read_config).unwrap();
        assert_eq!(&main_source[hover.range], "read_config");
        assert!(
            hover
                .contents
                .starts_with("```luat\nfunction read_config(): number\n```")
        );
        assert!(hover.contents.contains("Reads the configuration."));
        assert_eq!(
            snapshot.definition(&main_path, read_config).unwrap().path,
            api_path
        );
        assert_eq!(snapshot.references(&main_path, read_config, true).len(), 2);
        let renamed = snapshot
            .rename(&main_path, read_config, "load_config")
            .unwrap();
        assert_eq!(renamed.changes[&api_path].len(), 1);
        assert_eq!(renamed.changes[&main_path].len(), 1);

        let alias_use = main_source.find("UserId").unwrap();
        assert_eq!(
            snapshot.definition(&main_path, alias_use).unwrap().path,
            declaration_path
        );
        assert_eq!(snapshot.references(&main_path, alias_use, true).len(), 2);
        let alias_rename = snapshot.rename(&main_path, alias_use, "AccountId").unwrap();
        assert_eq!(alias_rename.changes[&declaration_path].len(), 1);
        assert_eq!(alias_rename.changes[&main_path].len(), 1);

        let global_use = main_source.find("GlobalCount").unwrap();
        assert_eq!(
            snapshot.definition(&main_path, global_use).unwrap().path,
            declaration_path
        );
        let global_references = snapshot.references(&main_path, global_use, true);
        assert_eq!(global_references.len(), 2, "{global_references:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn context_declarations_keep_editor_queries_and_renames_separate() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!(
                "context-review-{}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("types")).unwrap();
        fs::write(root.join("luat.toml"), "[project]\nname='test'\n[compiler]\ndeclarations=['types/*.d.luat']\n[fivem]\nclient=['src/client.luat']\nserver=['src/server.luat']\nshared=['src/shared.luat']\n").unwrap();
        fs::write(root.join("types/client.d.luat"), "---@client\nfunction ContextQuery(value: string): string end\nfunction ClientOnly() end").unwrap();
        fs::write(
            root.join("types/server.d.luat"),
            "---@server\nfunction ContextQuery(value: number): number end",
        )
        .unwrap();
        let client = root.join("src/client.luat");
        let server = root.join("src/server.luat");
        let shared = root.join("src/shared.luat");
        let source = "ContextQuery('value')";
        fs::write(&client, source).unwrap();
        fs::write(&server, "ContextQuery(1)").unwrap();
        fs::write(&shared, "").unwrap();
        let mut host = AnalysisHost::new();
        host.set_file(client.clone(), source.into());
        host.set_file(shared.clone(), String::new());
        let snapshot = host.snapshot();
        let signatures = snapshot
            .signature_help(&client, source.find("'value'").unwrap())
            .unwrap();
        assert_eq!(signatures.signatures.len(), 1);
        let edits = snapshot.rename(&client, 1, "RenamedQuery").unwrap();
        assert!(!edits.changes.contains_key(&server));
        assert!(
            !edits
                .changes
                .contains_key(&root.join("types/server.d.luat"))
        );
        assert!(
            !snapshot
                .completions(&shared, 0)
                .iter()
                .any(|item| item.label == "ClientOnly")
        );
        host.set_file(shared.clone(), "local callback = ContextQuery".into());
        assert_eq!(
            host.snapshot().rename(&client, 1, "RenamedQuery"),
            Err(RenameError::NotRenameable)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_projects_keep_import_symbols_isolated() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-nested-projects-{id}"));
        let inner = root.join("inner");
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        fs::create_dir_all(inner.join("src/client")).unwrap();
        fs::create_dir_all(inner.join("src/shared")).unwrap();
        let outer_main = root.join("src/client/main.luat");
        let outer_api = root.join("src/shared/api.luat");
        let inner_main = inner.join("src/client/main.luat");
        let inner_api = inner.join("src/shared/api.luat");
        let main_source = "local api = import('@/shared/api')\napi.read()";
        let api_source = "return { read = function(): string return 'outer' end }";
        fs::write(&outer_main, main_source).unwrap();
        fs::write(&outer_api, api_source).unwrap();
        fs::write(&inner_main, main_source).unwrap();
        fs::write(
            &inner_api,
            "return { read = function(): string return 'inner' end }",
        )
        .unwrap();
        let config = "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n";
        fs::write(root.join("luat.toml"), config).unwrap();
        fs::write(inner.join("luat.toml"), config).unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(outer_main.clone(), main_source.to_owned());
        host.set_file(inner_main.clone(), main_source.to_owned());
        let snapshot = host.snapshot();
        let read = main_source.find("read").unwrap();
        assert_eq!(
            snapshot.definition(&outer_main, read).unwrap().path,
            outer_api
        );
        assert_eq!(
            snapshot.definition(&inner_main, read).unwrap().path,
            inner_api
        );
        let edit = snapshot.rename(&outer_main, read, "fetch").unwrap();
        assert!(edit.changes.contains_key(&outer_api));
        assert!(edit.changes.contains_key(&outer_main));
        assert!(!edit.changes.contains_key(&inner_api));
        assert!(!edit.changes.contains_key(&inner_main));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imported_table_members_link_to_their_source_and_rename_all_references() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-import-members-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        let main_path = root.join("src/client/main.luat");
        let api_path = root.join("src/shared/api.luat");
        let api_source = "return {\n---[Get docs](https://example.com)\nget = function(key: string): string return key end, find = function(key: string): boolean return true end }";
        let main_source = "local api = import('@/shared/api')\napi.get('x')\napi.get('y')";
        fs::write(&main_path, main_source).unwrap();
        fs::write(&api_path, api_source).unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(main_path.clone(), main_source.to_owned());
        let snapshot = host.snapshot();
        let first_member = main_source.find("get").unwrap();
        let definition = snapshot.definition(&main_path, first_member).unwrap();
        assert_eq!(definition.path, api_path);
        assert_eq!(&api_source[definition.range], "get");
        assert!(
            snapshot
                .hover(&main_path, first_member)
                .unwrap()
                .contents
                .contains("string")
        );
        assert_eq!(snapshot.references(&main_path, first_member, true).len(), 3);
        assert!(
            snapshot
                .completions(&main_path, "local api = import('@/shared/api')\napi.".len())
                .iter()
                .any(|item| item.label == "get")
        );
        let imported_items =
            snapshot.completions(&main_path, "local api = import('@/shared/api')\napi.".len());
        assert!(
            imported_items
                .iter()
                .find(|item| item.label == "get")
                .unwrap()
                .documentation
                .as_deref()
                .is_some_and(|doc| doc.contains("[Get docs](https://example.com)"))
        );
        let signature_offset = main_source.find("api.get(").unwrap() + "api.get(".len();
        assert_eq!(
            snapshot
                .signature_help(&main_path, signature_offset)
                .unwrap()
                .signatures[0]
                .label,
            "function get(key: string): string"
        );
        assert_eq!(
            snapshot.rename(&main_path, first_member, "find"),
            Err(RenameError::NameConflict)
        );
        let edit = snapshot.rename(&main_path, first_member, "fetch").unwrap();
        assert_eq!(edit.changes[&api_path].len(), 1);
        assert_eq!(edit.changes[&main_path].len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsaved_project_config_changes_execution_context() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-config-overlay-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        let config_path = root.join("luat.toml");
        let source_path = root.join("src/client/main.luat");
        let source = "MumbleCreateChannel(1)";
        fs::write(
            &config_path,
            "[project]\nname='test'\n[fivem]\nserver=['src/client/main.luat']\n",
        )
        .unwrap();
        fs::write(&source_path, source).unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(source_path.clone(), source.to_owned());
        assert!(
            host.snapshot()
                .diagnostics(&source_path)
                .unwrap()
                .is_empty()
        );

        host.set_file(
            config_path.clone(),
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n".to_owned(),
        );
        let updated = host.snapshot();
        assert!(
            updated
                .diagnostics(&source_path)
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic.code == "LT2010")
        );
        assert!(updated.diagnostics(&config_path).is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_config_retains_project_symbols_for_changed_buffers() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-config-context-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        let config_path = root.join("luat.toml");
        let main_path = root.join("src/client/main.luat");
        let api_path = root.join("src/shared/api.luat");
        let main_source = "print(read_config())";
        fs::write(
            &config_path,
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();
        fs::write(&main_path, main_source).unwrap();
        fs::write(&api_path, "function read_config(): number return 1 end").unwrap();

        let mut host = AnalysisHost::new();
        host.set_file(main_path.clone(), main_source.to_owned());
        let valid = host.snapshot();
        let use_offset = main_source.find("read_config").unwrap();
        assert_eq!(
            valid.definition(&main_path, use_offset).unwrap().path,
            api_path
        );

        fs::write(&config_path, "not valid toml = [").unwrap();
        host.invalidate_path(&config_path);
        let invalid = host.snapshot();
        assert!(
            invalid
                .diagnostics(&main_path)
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic.code == "LT9001")
        );
        let changed_source = "local value = read_config()\nprint(value)";
        host.set_file(main_path.clone(), changed_source.to_owned());
        let changed = host.snapshot();
        assert!(
            changed
                .diagnostics(&main_path)
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic.code == "LT9001")
        );
        let changed_use = changed_source.find("read_config").unwrap();
        assert_eq!(
            changed.definition(&main_path, changed_use).unwrap().path,
            api_path
        );
        let rename = changed
            .rename(&main_path, changed_use, "load_config")
            .unwrap();
        assert_eq!(rename.changes[&main_path].len(), 1);
        assert_eq!(rename.changes[&api_path].len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_project_config_keeps_file_diagnostics() {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("analysis-config-{id}"));
        fs::create_dir_all(root.join("src")).unwrap();
        let config_path = root.join("luat.toml");
        fs::write(&config_path, "not valid toml = [").unwrap();
        let path = root.join("src/main.luat");
        fs::write(&path, "local =").unwrap();
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), "local =".to_owned());
        let snapshot = host.snapshot();
        let codes = snapshot
            .diagnostics(&path)
            .unwrap()
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        assert!(codes.contains(&"LT1001"), "{codes:?}");
        assert!(codes.contains(&"LT9001"), "{codes:?}");
        fs::write(
            &config_path,
            "[project]\nname='test'\n[fivem]\nclient=['src/main.luat']\n",
        )
        .unwrap();
        host.invalidate_path(&config_path);
        let recovered = host.snapshot();
        assert!(
            !recovered
                .diagnostics(&path)
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic.code == "LT9001")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod editor_correction_tests {
    use super::*;
    #[test]
    fn declaration_documents_validate_without_source_parser_errors() {
        let path = PathBuf::from("api.d.luat");
        let mut host = AnalysisHost::new();
        host.set_file(
            path.clone(),
            "typedef MsgpackApi = { pack: (...any) -> string }\ndeclare msgpack: MsgpackApi".into(),
        );
        assert!(host.snapshot().diagnostics(&path).unwrap().is_empty());
        host.set_file(path.clone(), "declare msgpack: number = 1".into());
        assert!(!host.snapshot().diagnostics(&path).unwrap().is_empty());
    }
    #[test]
    fn semantic_tokens_classify_names_and_references() {
        let path = PathBuf::from("semantic.luat");
        let source = "const cfg = {}\ncfg.print('info')\ntypedef Selam = { id: number }\nlocal function SELAM(status: number)\n print(status)\nend\nSELAM(15)";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.into());
        let snapshot = host.snapshot();
        let tokens = snapshot.semantic_tokens(&path);
        for (name, kind) in [
            ("cfg", 0),
            ("print", 3),
            ("Selam", 2),
            ("status", 1),
            ("SELAM", 3),
        ] {
            assert!(
                tokens
                    .iter()
                    .any(|(range, k, _)| &source[range.clone()] == name && *k == kind),
                "{name}"
            );
        }
        assert_eq!(
            tokens
                .iter()
                .filter(|(r, _, m)| &source[r.clone()] == "cfg" && m & 1 != 0)
                .count(),
            2
        );
        assert!(
            tokens
                .iter()
                .filter(|(r, _, _)| &source[r.clone()] == "number")
                .all(|(_, kind, modifiers)| *kind == 2 && modifiers & 4 != 0)
        );
    }

    #[test]
    fn semantic_tokens_highlight_execution_context_annotations() {
        let path = PathBuf::from("contexts.luat");
        let source = "---@client\n---@server\n---@shared\n-- @client";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.into());

        let tokens = host.snapshot().semantic_tokens(&path);
        for annotation in ["---@client", "---@server", "---@shared"] {
            assert!(
                tokens
                    .iter()
                    .any(|(range, kind, _)| { &source[range.clone()] == annotation && *kind == 6 })
            );
        }
        assert!(
            !tokens
                .iter()
                .any(|(range, kind, _)| { &source[range.clone()] == "-- @client" && *kind == 6 })
        );
    }

    #[test]
    fn semantic_tokens_mark_operators_and_map_coalescing_ranges() {
        let path = PathBuf::from("operators.luat");
        let source = "local value = left ?? right\nlocal sum = a + b\n-- ~=\n// comment with +=\nlocal quotient = a // b\nlocal text = '=='";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.into());
        let tokens = host.snapshot().semantic_tokens(&path);

        for operator in ["??", "+"] {
            assert!(
                tokens
                    .iter()
                    .any(|(range, kind, _)| { &source[range.clone()] == operator && *kind == 5 }),
                "missing {operator}"
            );
        }
        let comment = source.find("// comment").unwrap()..source.find("\nlocal quotient").unwrap();
        assert!(!tokens.iter().any(|(range, kind, _)| {
            *kind == 5 && range.start >= comment.start && range.end <= comment.end
        }));
        assert!(!tokens.iter().any(|(range, kind, _)| {
            *kind == 5 && ["~=", "==", "+="].contains(&&source[range.clone()])
        }));
        assert!(
            tokens
                .iter()
                .any(|(range, kind, _)| { &source[range.clone()] == "//" && *kind == 5 })
        );
    }

    #[test]
    fn semantic_tokens_cover_compound_operator_matrix() {
        let path = PathBuf::from("operator-matrix.luat");
        let operators = [
            "~=", "==", "<=", ">=", "+=", "-=", "*=", "/=", "//=", "%=", "^=", "..=", "??", "?.",
            "<<=", ">>=", "&=", "|=", "::", "->", "<<", ">>", "..", "+", "-", "*", "/", "%", "^",
            "#", "=", "<", ">", "&", "|", "~", "and", "or", "not",
        ];
        let source = operators.join(" ");
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.clone());
        let tokens = host.snapshot().semantic_tokens(&path);

        for operator in operators {
            assert!(
                tokens
                    .iter()
                    .any(|(range, kind, _)| &source[range.clone()] == operator && *kind == 5),
                "missing exact operator token {operator}"
            );
        }
    }

    #[test]
    fn offers_typedef_snippet_without_hijacking_lua_type() {
        let path = PathBuf::from("snippet.luat");
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), "typedef".into());
        let items = host.snapshot().completions(&path, "typedef".len());
        assert_eq!(items[0].label, "typedef");
        assert_eq!(
            items[0].insert_text.as_deref(),
            Some("typedef ${1:Name} = ${2:any}")
        );

        host.set_file(path.clone(), "type".into());
        assert!(
            !host
                .snapshot()
                .completions(&path, 4)
                .iter()
                .any(|item| item.kind == CompletionKind::Snippet)
        );
    }
}

#[cfg(test)]
mod nui_definition_tests {
    use super::*;
    #[test]
    fn lua_nui_helper_and_raw_native_are_both_indexed() {
        let helper = builtin_by_short_name("SendNUIMessage");
        let raw = builtin_by_short_name("SendNuiMessage");
        assert!(!helper.is_empty());
        assert!(!raw.is_empty());
        assert!(helper[0].detail().contains("table"));
        assert!(raw[0].detail().contains("string"));
        assert!(raw[0].documentation.contains("SendNUIMessage"));
    }
}

#[cfg(test)]
mod semantic_edge_tests {
    use super::*;

    #[test]
    fn typedef_keyword_has_no_partial_function_token() {
        let path = PathBuf::from("keyword.luat");
        let mut host = AnalysisHost::new();
        host.set_file(
            path.clone(),
            "typedef User = { id: number }\nlocal kind = type(1)".into(),
        );
        let snapshot = host.editor_snapshot();
        assert!(
            !snapshot
                .semantic_tokens(&path)
                .iter()
                .any(|(range, _, _)| range.start < 7)
        );
        let start = snapshot.source(&path).unwrap().find("type(1)").unwrap();
        assert!(
            snapshot
                .semantic_tokens(&path)
                .iter()
                .any(|(range, kind, _)| *range == (start..start + 4) && *kind == 3)
        );
    }

    #[test]
    fn editor_snapshots_share_parsing_and_preserve_old_buffers() {
        let path = PathBuf::from("cached.luat");
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), "local value = 1\nprint(value)".into());
        let first = host.editor_snapshot();
        let second = host.editor_snapshot();
        assert!(Arc::ptr_eq(&first.files[&path], &second.files[&path]));
        let checked = host.snapshot();
        assert!(Arc::ptr_eq(
            &first.files[&path].parsed,
            &checked.files[&path].parsed
        ));
        host.set_file(path.clone(), "local value = 2\nprint(value)".into());
        let changed = host.editor_snapshot();
        assert_ne!(first.source(&path), changed.source(&path));
        assert!(first.source(&path).unwrap().contains("= 1"));
    }
    #[test]
    fn readonly_respects_same_line_declarations_shadowing_and_unicode() {
        let path = PathBuf::from("edges.luat");
        let source = "-- Türkçe 😀\nlocal first = 0; const cfg = {}; local mutable = 1\ndo local cfg = 3; print(cfg) end\nprint(cfg)";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.into());
        let snapshot = host.snapshot();
        let tokens = snapshot.semantic_tokens(&path);
        assert_eq!(
            tokens
                .iter()
                .filter(|(r, _, m)| &source[r.clone()] == "cfg" && m & 1 != 0)
                .count(),
            2
        );
        assert!(
            tokens
                .iter()
                .filter(|(r, _, _)| &source[r.clone()] == "mutable")
                .all(|(_, _, m)| m & 1 == 0)
        );
        assert!(tokens.windows(2).all(|w| w[0].0.end <= w[1].0.start));
    }
}

#[cfg(test)]
mod token_assignment_tests {
    use super::*;
    #[test]
    fn every_token_has_the_expected_role() {
        let path = PathBuf::from("roles.luat");
        let source = "const cfg = {}\nlocal function run(status: number, ok: boolean)\nprint(status, ok)\ncfg.print(status)\nend\nrun(15, true)\nGetEntityCoords(1)\nlocal cb = function(arg: number) return arg end";
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), source.into());
        let snap = host.snapshot();
        let tokens = snap.semantic_tokens(&path);
        for (name, kind, modifier, count) in [
            ("cfg", 0, 1, 2),
            ("status", 1, 0, 3),
            ("ok", 1, 0, 2),
            ("number", 2, 4, 2),
            ("boolean", 2, 4, 1),
            ("run", 3, 0, 2),
            ("print", 3, 0, 2),
            ("GetEntityCoords", 3, 0, 1),
            ("arg", 1, 0, 2),
        ] {
            let expected = source
                .match_indices(name)
                .filter(|(i, _)| {
                    source[..*i]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
                        && source[*i + name.len()..]
                            .chars()
                            .next()
                            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
                })
                .map(|(i, _)| i..i + name.len())
                .collect::<Vec<_>>();
            assert_eq!(expected.len(), count, "{name}");
            for range in expected {
                assert!(
                    tokens
                        .iter()
                        .any(|(r, k, m)| *r == range && *k == kind && m & modifier == modifier),
                    "{name} at {range:?}: {tokens:?}"
                );
            }
        }
    }
    #[test]
    fn real_runtime_declaration_has_no_parser_diagnostic() {
        let path = std::env::current_dir()
            .unwrap()
            .join("src/definitions/runtime-client.d.luat");
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), fs::read_to_string(&path).unwrap());
        let snapshot = host.snapshot();
        assert!(
            snapshot.diagnostics(&path).unwrap().is_empty(),
            "{:?}",
            snapshot.diagnostics(&path)
        );
    }
}

#[cfg(test)]
mod assistance_tests;
