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

fn declaration_sources(
    path: &Path,
    source: &str,
    overlays: &BTreeMap<PathBuf, Arc<str>>,
) -> Vec<crate::compiler::declarations::DeclarationSource> {
    let mut texts = BTreeMap::new();
    if let Some(parent) = path.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            for entry in entries.flatten() {
                let sibling = entry.path();
                if sibling.to_string_lossy().ends_with(".d.luat")
                    && let Ok(text) = fs::read_to_string(&sibling)
                {
                    texts.insert(sibling, text);
                }
            }
        }
        for (file, text) in overlays {
            if file.parent() == Some(parent) && file.to_string_lossy().ends_with(".d.luat") {
                texts.insert(file.clone(), text.to_string());
            }
        }
    }
    texts.insert(path.to_path_buf(), source.to_owned());
    texts
        .into_iter()
        .map(
            |(path, text)| crate::compiler::declarations::DeclarationSource {
                path: Some(path),
                context: None,
                text,
            },
        )
        .collect()
}

/// Owns the latest editor buffers used to create analysis snapshots.
#[derive(Clone, Default)]
pub struct AnalysisHost {
    files: Arc<BTreeMap<PathBuf, Arc<str>>>,
    revision: u64,
    dirty_paths: BTreeSet<PathBuf>,
    cached: Option<(u64, AnalysisSnapshot)>,
    project_declarations: HashMap<PathBuf, Arc<DeclarationSet>>,
    project_configs: HashMap<PathBuf, String>,
    project_contexts: HashMap<PathBuf, Arc<Project>>,
    filesystem_dirty: BTreeSet<PathBuf>,
    invalid_configs: HashSet<PathBuf>,
    inputs: inputs::Inputs,
    roots: HashMap<PathBuf, Option<PathBuf>>,
    editor_cached: Option<(u64, AnalysisSnapshot)>,
    import_plans: HashMap<PathBuf, Arc<crate::project::imports::ImportPlan>>,
    project_epoch: u64,
}

impl AnalysisHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file(&mut self, path: PathBuf, source: String) {
        if self.files.get(&path).map(AsRef::as_ref) == Some(source.as_str()) {
            return;
        }
        if path.file_name().is_some_and(|name| name == "luat.toml") {
            self.roots.clear();
        }
        Arc::make_mut(&mut self.files).insert(path.clone(), source.into());
        self.revision += 1;
        self.dirty_paths.insert(path);
    }

    pub fn remove_file(&mut self, path: &Path) {
        if Arc::make_mut(&mut self.files).remove(path).is_some() {
            self.revision += 1;
            self.dirty_paths.insert(path.to_path_buf());
        }
    }

    pub fn invalidate_path(&mut self, path: &Path) {
        self.invalidate_file(path);
        self.filesystem_dirty.insert(path.to_path_buf());
    }

    pub fn invalidate_file(&mut self, path: &Path) {
        self.revision += 1;
        self.dirty_paths.insert(path.to_path_buf());
        if path.file_name().is_some_and(|name| name == "luat.toml") {
            self.roots.clear();
        }
    }

    pub fn warm_builtins() {
        let _ = builtin_index();
    }

    pub fn adopt_editor_cache(&mut self, other: &Self) {
        if self.revision == other.revision && self.project_epoch == other.project_epoch {
            self.editor_cached = other.editor_cached.clone();
        }
    }

    pub fn editor_snapshot(&mut self) -> AnalysisSnapshot {
        if let Some((revision, snapshot)) = &self.editor_cached
            && *revision == self.revision
        {
            return snapshot.clone();
        }
        let mut snapshot = self
            .cached
            .as_ref()
            .map(|(_, s)| s.clone())
            .unwrap_or_default();
        let mut changed = Vec::new();
        for (path, source) in self.files.iter() {
            if path.to_string_lossy().ends_with(".luat") {
                let file = self.inputs.file(path, source);
                if !snapshot
                    .files
                    .get(path)
                    .is_some_and(|old| Arc::ptr_eq(&old.parsed, &file.parsed))
                {
                    Arc::make_mut(&mut snapshot.files).insert(path.clone(), Arc::clone(&file));
                    changed.push((path.clone(), file));
                }
            }
        }
        Arc::make_mut(&mut snapshot.files).retain(|path, _| {
            self.files.contains_key(path) || self.roots.get(path).is_some_and(|root| root.is_some())
        });
        for (path, file) in changed {
            if let Some(imports) = snapshot.resolved_imports.get(&path) {
                let bindings = collect_import_bindings(&file, imports, &snapshot.files);
                let mut file = Arc::clone(&file);
                for binding in &bindings {
                    if let Some(symbol) = Arc::make_mut(&mut file).symbols.get_mut(binding.symbol) {
                        symbol.type_name = binding.type_name.clone();
                    }
                }
                Arc::make_mut(&mut snapshot.files).insert(path.clone(), file);
                Arc::make_mut(&mut snapshot.project_imports).insert(path.clone(), bindings);
            }
            if let Some(root) = snapshot.project_root(&path) {
                let index = Arc::make_mut(&mut snapshot.project_symbols)
                    .entry(root)
                    .or_default();
                index.retain(|_, group| {
                    group.retain(|symbol| symbol.path != path);
                    !group.is_empty()
                });
                for symbol in &file.symbols {
                    if symbol.scope_depth != 0 || symbol.kind != SymbolKind::Function {
                        continue;
                    }
                    index
                        .entry(symbol.name.clone())
                        .or_default()
                        .push(ProjectSymbol {
                            name: symbol.name.clone(),
                            kind: ProjectSymbolKind::Function,
                            path: path.clone(),
                            range: symbol.range.clone(),
                            type_name: symbol.type_name.clone(),
                            signature: symbol.signature.clone(),
                            documentation: documentation_before(&file.source, symbol.range.start),
                        });
                }
            }
        }
        snapshot.semantic_cache = Default::default();
        self.editor_cached = Some((self.revision, snapshot.clone()));
        snapshot
    }

    pub fn snapshot(&mut self) -> AnalysisSnapshot {
        self.snapshot_cancellable(&|| false)
            .expect("uncancelled analysis")
    }

    pub fn snapshot_cancellable(
        &mut self,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<AnalysisSnapshot> {
        if let Some((revision, snapshot)) = &self.cached
            && *revision == self.revision
        {
            return Some(snapshot.clone());
        }
        for path in &self.dirty_paths {
            self.roots
                .entry(path.clone())
                .or_insert_with(|| nearest_project_root(path));
        }
        let previous_snapshot = self.cached.as_ref().map(|(_, snapshot)| snapshot.clone());
        let previous = previous_snapshot.as_ref();
        if cancelled() {
            return None;
        }
        let mut files = BTreeMap::new();
        for (path, source) in self.files.iter() {
            if cancelled() {
                return None;
            }
            if path.to_string_lossy().ends_with(".luat") {
                files.insert(path.clone(), self.inputs.file(path, source));
            }
        }
        let mut projects = BTreeMap::<PathBuf, HashMap<PathBuf, String>>::new();
        for (path, source) in self.files.iter() {
            if let Some(root) = self
                .roots
                .entry(path.clone())
                .or_insert_with(|| nearest_project_root(path))
                .clone()
            {
                projects
                    .entry(root)
                    .or_default()
                    .insert(path.clone(), source.to_string());
            }
        }
        let mut diagnostics = HashMap::new();
        for (path, file) in &files {
            if cancelled() {
                return None;
            }
            let old = previous
                .and_then(|snapshot| snapshot.files.get(path))
                .filter(|old| Arc::ptr_eq(&old.parsed, &file.parsed));
            let declaration = path.to_string_lossy().ends_with(".d.luat");
            let siblings_changed = declaration
                && self.dirty_paths.iter().any(|changed| {
                    changed.parent() == path.parent()
                        && changed.to_string_lossy().ends_with(".d.luat")
                });
            let errors = if declaration && (old.is_none() || siblings_changed) {
                DeclarationSet::from_sources(declaration_sources(path, &file.source, &self.files))
                    .err()
                    .filter(|(error_path, _)| error_path.as_ref().is_none_or(|p| p == path))
                    .map_or_else(|| file.diagnostics.clone(), |(_, error)| error.diagnostics)
            } else if let Some(old) = old {
                old.diagnostics.clone()
            } else if self.roots.get(path).is_some_and(|root| root.is_some()) {
                file.base_diagnostics.clone()
            } else {
                file.standalone_diagnostics
                    .get_or_init(|| {
                        file.parsed
                            .check(&DeclarationSet::default(), false, None, None)
                            .err()
                            .map_or_else(Vec::new, |error| error.diagnostics)
                    })
                    .clone()
            };
            diagnostics.insert(path.clone(), errors);
        }
        let mut contexts = previous
            .map(|snapshot| (*snapshot.contexts).clone())
            .unwrap_or_default();
        let mut project_symbols = HashMap::new();
        let mut project_imports = HashMap::new();
        let mut all_resolved_imports = HashMap::new();
        let mut project_files = HashSet::new();
        for (root, mut overlays) in projects {
            if cancelled() {
                return None;
            }
            let mut changed = self
                .dirty_paths
                .iter()
                .filter(|path| {
                    self.roots
                        .get(path.as_path())
                        .and_then(|root| root.as_ref())
                        == Some(&root)
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            let config_path = root.join("luat.toml");
            let config_text = overlays
                .get(&config_path)
                .cloned()
                .or_else(|| {
                    if !self.dirty_paths.contains(&config_path)
                        && !self.invalid_configs.contains(&root)
                    {
                        self.project_configs.get(&root).cloned()
                    } else {
                        None
                    }
                })
                .or_else(|| fs::read_to_string(&config_path).ok());
            let reload = !self.project_contexts.contains_key(&root)
                || self.invalid_configs.contains(&root)
                || changed
                    .iter()
                    .any(|path| path == &config_path || self.filesystem_dirty.contains(path));
            let current_load = if !reload {
                Ok((*self.project_contexts[&root]).clone())
            } else {
                match config_text.as_deref() {
                    Some(source) => Project::load_with_config_text(&config_path, source),
                    None => Project::load(&root),
                }
            };
            let (loaded, config_error) = match current_load {
                Ok(project) => {
                    self.invalid_configs.remove(&root);
                    if let Some(source) = config_text {
                        self.project_configs.insert(root.clone(), source);
                    }
                    (Ok(project), None)
                }
                Err(error) => {
                    self.invalid_configs.insert(root.clone());
                    let fallback = self
                        .project_configs
                        .get(&root)
                        .and_then(|source| {
                            Project::load_with_config_text(&config_path, source).ok()
                        })
                        .or_else(|| {
                            self.project_contexts
                                .get(&root)
                                .map(|project| (**project).clone())
                        });
                    match fallback {
                        Some(project) => (Ok(project), Some(error)),
                        None => (Err(error), None),
                    }
                }
            };
            if let Ok(project) = &loaded {
                self.project_contexts
                    .insert(root.clone(), Arc::new(project.clone()));
            }
            match loaded {
                Ok(project) => {
                    let declarations_changed = changed.iter().any(|path| {
                        path == &root.join("luat.toml")
                            || project.declaration_files().contains(path)
                    });
                    let mut declaration_error = None;
                    let declarations =
                        if declarations_changed || !self.project_declarations.contains_key(&root) {
                            match project.read_declarations_with_overlays(&overlays) {
                                Ok(declarations) => {
                                    self.project_declarations
                                        .insert(root.clone(), Arc::new(declarations));
                                    self.project_declarations.get(&root).cloned()
                                }
                                Err(error) => {
                                    self.project_declarations.remove(&root);
                                    declaration_error = Some(error);
                                    None
                                }
                            }
                        } else {
                            self.project_declarations.get(&root).cloned()
                        };
                    for path in project.source_paths() {
                        if files.contains_key(path) {
                            continue;
                        }
                        if cancelled() {
                            return None;
                        }
                        let source = previous
                            .and_then(|s| s.files.get(path))
                            .filter(|_| !self.dirty_paths.contains(path))
                            .map(|file| file.source.clone())
                            .or_else(|| fs::read_to_string(path).ok());
                        let Some(source) = source else {
                            continue;
                        };
                        let parsed = self.inputs.file(path, &source);
                        let file = previous
                            .and_then(|s| s.files.get(path))
                            .filter(|old| Arc::ptr_eq(&old.parsed, &parsed.parsed))
                            .cloned()
                            .unwrap_or(parsed);
                        diagnostics.insert(path.to_path_buf(), file.diagnostics.clone());
                        files.insert(path.to_path_buf(), file);
                        self.roots.insert(path.to_path_buf(), Some(root.clone()));
                    }
                    if let Some(declarations) = &declarations {
                        for declaration in
                            declarations.files.iter().filter(|file| file.path.is_some())
                        {
                            let path = declaration.path.as_ref().unwrap();
                            self.roots.insert(path.clone(), Some(root.clone()));
                            let file = previous
                                .and_then(|snapshot| snapshot.files.get(path))
                                .filter(|file| {
                                    file.source == declaration.source
                                        && !self.dirty_paths.contains(path)
                                })
                                .cloned()
                                .unwrap_or_else(|| {
                                    Arc::new(AnalyzedFile::from_ast(
                                        &declaration.source,
                                        declaration.prepared.clone(),
                                        &declaration.ast,
                                        Vec::new(),
                                        true,
                                    ))
                                });
                            diagnostics.insert(path.clone(), file.diagnostics.clone());
                            files.insert(path.clone(), file);
                        }
                    }
                    for (path, file) in &files {
                        if self.roots.get(path).and_then(|r| r.as_ref()) == Some(&root) {
                            overlays.insert(path.clone(), file.source.clone());
                        }
                    }
                    // Global declarations affect callers even without an explicit import.
                    if changed.iter().any(|path| {
                        let globals = |file: &AnalyzedFile| {
                            file.symbols
                                .iter()
                                .filter(|symbol| {
                                    symbol.scope_depth == 0 && symbol.kind == SymbolKind::Function
                                })
                                .map(|symbol| (symbol.name.clone(), symbol.signature.clone()))
                                .collect::<Vec<_>>()
                        };
                        files.get(path).map(|f| globals(f))
                            != previous.and_then(|s| s.files.get(path)).map(|f| globals(f))
                    }) {
                        changed.insert(config_path.clone());
                    }
                    let parse = |path: &Path, source: &str| {
                        if cancelled() {
                            return Err(BuildError::Io {
                                path: path.to_path_buf(),
                                source: std::io::Error::from(std::io::ErrorKind::Interrupted),
                            });
                        }
                        Ok(Arc::clone(&self.inputs.file(path, source).parsed))
                    };
                    let plan = project.analyze_imports_with_parser(
                        &overlays,
                        Some(&parse),
                        if reload {
                            None
                        } else {
                            self.import_plans.get(&root).map(AsRef::as_ref)
                        },
                    );
                    let (checked, result, resolved_imports) = match plan {
                        Ok(plan) => {
                            project_files.extend(plan.texts.keys().cloned());
                            contexts.retain(|path, _| {
                                self.roots.get(path).and_then(|root| root.as_ref()) != Some(&root)
                            });
                            contexts.extend(plan.execution_contexts.clone());
                            let (checked, result, imports) =
                                if let Some(declarations) = &declarations {
                                    project.check_editor_with_plan(
                                        &plan,
                                        self.import_plans.get(&root).map(AsRef::as_ref),
                                        &changed,
                                        declarations,
                                        &parse,
                                    )
                                } else {
                                    (
                                        overlays.keys().cloned().collect(),
                                        Ok(()),
                                        project.resolved_imports(&plan),
                                    )
                                };
                            self.import_plans.insert(root.clone(), Arc::new(plan));
                            (checked, result, Some(imports))
                        }
                        Err(error) => (overlays.keys().cloned().collect(), Err(error), None),
                    };
                    if cancelled() {
                        return None;
                    }
                    for path in checked {
                        if path.to_string_lossy().ends_with(".d.luat") {
                            continue;
                        }
                        if let Some(file) = files.get(&path) {
                            let mut base = file.base_diagnostics.clone();
                            base.retain(|diagnostic| diagnostic.code != "LT4007");
                            diagnostics.insert(path, base);
                        }
                    }
                    if let Err(error) = result {
                        apply_project_error(error, &overlays, &mut diagnostics);
                    }
                    if let Some(error) = declaration_error {
                        apply_project_error(error, &overlays, &mut diagnostics);
                    }
                    if let Some(resolved_imports) = resolved_imports {
                        all_resolved_imports.extend(resolved_imports.clone());
                        for (path, imports) in resolved_imports {
                            let Some(file) = files.get(&path) else {
                                continue;
                            };
                            let bindings = collect_import_bindings(file, &imports, &files);
                            for binding in &bindings {
                                if let Some(file) = files.get_mut(&path)
                                    && let Some(symbol) =
                                        Arc::make_mut(file).symbols.get_mut(binding.symbol)
                                {
                                    symbol.type_name = binding.type_name.clone();
                                }
                            }
                            if !bindings.is_empty() {
                                project_imports.insert(path, bindings);
                            }
                        }
                    }
                    let source_paths = project
                        .source_paths()
                        .map(Path::to_path_buf)
                        .collect::<HashSet<_>>();
                    let declaration_paths = project
                        .declaration_files()
                        .iter()
                        .cloned()
                        .collect::<HashSet<_>>();
                    let mut index = HashMap::<String, Vec<ProjectSymbol>>::new();
                    for (path, file) in &files {
                        let declaration_file = declaration_paths.contains(path);
                        if !source_paths.contains(path) && !declaration_file {
                            continue;
                        }
                        for symbol in &file.symbols {
                            let kind = match symbol.kind {
                                SymbolKind::Function => ProjectSymbolKind::Function,
                                SymbolKind::TypeAlias if declaration_file => {
                                    ProjectSymbolKind::TypeAlias
                                }
                                _ => continue,
                            };
                            if symbol.scope_depth != 0 {
                                continue;
                            }
                            let documentation = declarations
                                .as_ref()
                                .and_then(|declarations| declarations.functions.get(&symbol.name))
                                .into_iter()
                                .flatten()
                                .find(|function| {
                                    function.location.path.as_ref() == Some(path)
                                        && function.location.range == symbol.range
                                })
                                .map_or_else(
                                    || documentation_before(&file.source, symbol.range.start),
                                    |function| function.documentation.clone(),
                                );
                            index
                                .entry(symbol.name.clone())
                                .or_default()
                                .push(ProjectSymbol {
                                    name: symbol.name.clone(),
                                    kind,
                                    path: path.clone(),
                                    range: symbol.range.clone(),
                                    type_name: symbol.type_name.clone(),
                                    signature: symbol.signature.clone(),
                                    documentation,
                                });
                        }
                    }
                    if let Some(declarations) = &declarations {
                        for global in declarations.globals.values() {
                            let Some(path) = &global.location.path else {
                                continue;
                            };
                            index
                                .entry(global.name.clone())
                                .or_default()
                                .push(ProjectSymbol {
                                    name: global.name.clone(),
                                    kind: ProjectSymbolKind::Global,
                                    path: path.clone(),
                                    range: global.location.range.clone(),
                                    type_name: global.type_name.clone(),
                                    signature: None,
                                    documentation: global.documentation.clone(),
                                });
                        }
                    }
                    for symbols in index.values_mut() {
                        symbols.sort_by(|left, right| {
                            left.path
                                .cmp(&right.path)
                                .then(left.range.start.cmp(&right.range.start))
                        });
                        symbols.dedup_by(|left, right| {
                            left.path == right.path
                                && left.range == right.range
                                && left.kind == right.kind
                        });
                    }
                    project_symbols.insert(root.clone(), index);
                    if let Some(error) = config_error {
                        apply_project_error(error, &overlays, &mut diagnostics);
                    }
                }
                Err(error) => {
                    if let Some(previous) = previous {
                        let project_files = previous
                            .files
                            .iter()
                            .filter(|(path, _)| {
                                self.roots
                                    .get(path.as_path())
                                    .and_then(|root| root.as_ref())
                                    == Some(&root)
                            })
                            .collect::<Vec<_>>();
                        let context_unchanged = !project_files.is_empty()
                            && project_files.iter().all(|(path, file)| {
                                let current = overlays
                                    .get(*path)
                                    .cloned()
                                    .or_else(|| fs::read_to_string(path).ok());
                                current.as_deref() == Some(file.source.as_str())
                            });
                        if context_unchanged {
                            if let Some(symbols) = previous.project_symbols.get(&root) {
                                project_symbols.insert(root.clone(), symbols.clone());
                            }
                            for (path, file) in project_files {
                                files.insert(path.clone(), file.clone());
                                diagnostics.insert(path.clone(), file.diagnostics.clone());
                            }
                            for (path, imports) in previous.project_imports.iter() {
                                if self.roots.get(path).and_then(|root| root.as_ref())
                                    == Some(&root)
                                {
                                    project_imports.insert(path.clone(), imports.clone());
                                }
                            }
                        }
                    }
                    for path in overlays.keys() {
                        if let Some(file) = files.get(path) {
                            diagnostics.insert(path.clone(), file.base_diagnostics.clone());
                        }
                    }
                    apply_project_error(error, &overlays, &mut diagnostics);
                }
            }
        }
        for (path, file) in &files {
            if path.to_string_lossy().ends_with(".d.luat")
                || project_files.contains(path)
                || self.roots.get(path).is_none_or(|root| root.is_none())
            {
                continue;
            }
            let standalone = file.standalone_diagnostics.get_or_init(|| {
                file.parsed
                    .check(&DeclarationSet::default(), false, None, None)
                    .err()
                    .map_or_else(Vec::new, |error| error.diagnostics)
            });
            let errors = diagnostics.entry(path.clone()).or_default();
            for error in standalone {
                if error.code != "LT4007" && !errors.contains(error) {
                    errors.push(error.clone());
                }
            }
        }
        for (path, file) in &mut files {
            let diagnostics = diagnostics.remove(path).unwrap_or_default();
            if file.diagnostics != diagnostics {
                Arc::make_mut(file).diagnostics = diagnostics;
            }
        }
        if cancelled() {
            return None;
        }
        let snapshot = AnalysisSnapshot {
            contexts: Arc::new(contexts),
            files: Arc::new(files),
            project_symbols: Arc::new(project_symbols),
            project_imports: Arc::new(project_imports),
            roots: Arc::new(self.roots.clone()),
            semantic_cache: Default::default(),
            resolved_imports: Arc::new(all_resolved_imports),
        };
        self.dirty_paths.clear();
        self.filesystem_dirty.clear();
        self.cached = Some((self.revision, snapshot.clone()));
        self.editor_cached = None;
        self.project_epoch += 1;
        Some(snapshot)
    }
}

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

#[derive(Clone, Default)]
pub struct AnalysisSnapshot {
    contexts: Arc<HashMap<PathBuf, Vec<crate::ExecutionContext>>>,
    files: Arc<BTreeMap<PathBuf, Arc<AnalyzedFile>>>,
    project_symbols: Arc<HashMap<PathBuf, HashMap<String, Vec<ProjectSymbol>>>>,
    project_imports: Arc<HashMap<PathBuf, Vec<ImportBinding>>>,
    roots: Arc<HashMap<PathBuf, Option<PathBuf>>>,
    semantic_cache:
        Arc<std::sync::Mutex<HashMap<PathBuf, Vec<(std::ops::Range<usize>, u32, u32)>>>>,
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
    ) -> Option<&[ProjectSymbol]> {
        let root = self.project_root(path)?;
        let candidates = self.project_symbols.get(&root)?.get(name)?;
        let count = candidates
            .iter()
            .filter(|symbol| symbol.kind == kind)
            .count();
        (count > 0 && (kind == ProjectSymbolKind::Function || count == 1))
            .then_some(candidates.as_slice())
    }

    fn project_symbol_at(&self, path: &Path, byte_offset: usize) -> Option<&ProjectSymbol> {
        let file = self.files.get(path)?;
        let range = file.identifier_range_at(byte_offset)?;
        let name = &file.source[range.clone()];
        let root = self.project_root(path)?;
        let candidates = self.project_symbols.get(&root)?.get(name)?;
        if let Some(symbol) = candidates
            .iter()
            .find(|symbol| symbol.path == path && symbol.range == range)
        {
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
        group.iter().find(|symbol| symbol.kind == kind)
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
        let occurrence = file.occurrence_at(receiver.start)?;
        if occurrence.range != receiver {
            return None;
        }
        let binding = self
            .project_imports
            .get(path)?
            .iter()
            .find(|binding| binding.symbol == occurrence.symbol)?;
        Some(
            binding
                .members
                .iter()
                .map(|(name, member)| CompletionItem {
                    label: name.clone(),
                    detail: member.type_name.clone(),
                    kind: if member.signature.is_some() {
                        CompletionKind::Function
                    } else {
                        CompletionKind::Variable
                    },
                    ..Default::default()
                })
                .filter(|item| {
                    item.label
                        .starts_with(completion_prefix(&file.source, byte_offset))
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
            for token in scan_identifiers(&file.prepared.text) {
                if token.name != name {
                    continue;
                }
                let start = file.prepared.original_offset(token.range.start);
                let end = file.prepared.original_offset(token.range.end);
                let range = start..end;
                let is_type = is_type_reference_position(&file.source, start);
                let declaration = group
                    .iter()
                    .any(|symbol| symbol.path == *candidate_path && symbol.range == range);
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
                .and_then(|group| group.first())
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
        if let Some(fields) = self.imported_member_completions(path, offset) {
            return fields;
        }
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
                    if symbol.name.starts_with(prefix) {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SymbolKind {
    Local,
    Parameter,
    TypeAlias,
    Function,
}

#[derive(Clone)]
struct Symbol {
    id: usize,
    name: String,
    kind: SymbolKind,
    range: std::ops::Range<usize>,
    visible: std::ops::Range<usize>,
    scope: std::ops::Range<usize>,
    scope_depth: usize,
    type_name: String,
    signature: Option<SignatureInformation>,
    readonly: bool,
}

#[derive(Clone)]
struct Occurrence {
    symbol: usize,
    range: std::ops::Range<usize>,
    declaration: bool,
}

#[derive(Clone)]
struct AnalyzedFile {
    parsed: Arc<ParsedSource>,
    tokens: Arc<Vec<Token>>,
    identifiers: Arc<Vec<IdentifierToken>>,
    symbols_by_name: Arc<HashMap<String, Vec<usize>>>,
    standalone_diagnostics: OnceLock<Vec<Diagnostic>>,
    base_diagnostics: Vec<Diagnostic>,
    diagnostics: Vec<Diagnostic>,
    symbols: Vec<Symbol>,
    occurrences: Vec<Occurrence>,
    syntax_valid: bool,
    semantic_roles: Vec<(std::ops::Range<usize>, u32, u32)>,
    editor: editor::FileAssistance,
}

impl std::ops::Deref for AnalyzedFile {
    type Target = ParsedSource;
    fn deref(&self) -> &Self::Target {
        &self.parsed
    }
}

impl AnalyzedFile {
    fn new(source: &str, diagnostics: Vec<Diagnostic>) -> Self {
        let prepared = syntax::prepare(source);
        Self::from_prepared(source, prepared, diagnostics)
    }

    fn from_prepared(
        source: &str,
        prepared: syntax::PreparedSource,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self::from_parsed(
            Arc::new(ParsedSource::new_prepared(source, prepared)),
            diagnostics,
        )
    }

    fn from_ast(
        source: &str,
        prepared: syntax::PreparedSource,
        ast: &ast::Ast,
        diagnostics: Vec<Diagnostic>,
        syntax_valid: bool,
    ) -> Self {
        Self::from_parsed(
            Arc::new(ParsedSource {
                source: source.to_owned(),
                prepared,
                ast: ast.clone(),
                diagnostics: diagnostics.clone(),
                syntax_valid,
                exports: Default::default(),
                imports: Default::default(),
            }),
            diagnostics,
        )
    }

    fn from_parsed(parsed: Arc<ParsedSource>, mut diagnostics: Vec<Diagnostic>) -> Self {
        for diagnostic in &parsed.diagnostics {
            if !diagnostics.contains(diagnostic) {
                diagnostics.push(diagnostic.clone());
            }
        }
        let source = &parsed.source;
        let prepared = &parsed.prepared;
        let ast = &parsed.ast;
        let mut collector = SemanticCollector::new(source, prepared);
        collector.visit_ast(ast);
        let semantic_roles = collector.tokens.clone();
        let (mut symbols, mut occurrences) = collector.finish(source);
        if !parsed.syntax_valid {
            recover_partial_locals(source, prepared, &mut symbols, &mut occurrences);
        }
        occurrences.sort_by_key(|occurrence| occurrence.range.start);
        let mut symbols_by_name = HashMap::<String, Vec<usize>>::new();
        for symbol in &symbols {
            symbols_by_name
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.id);
        }
        let editor = editor::FileAssistance::collect(source, prepared, ast);
        let tokens = Arc::new(lex(&prepared.text));
        let identifiers = Arc::new(scan_identifiers(source));
        Self {
            syntax_valid: parsed.syntax_valid,
            parsed,
            tokens,
            identifiers,
            symbols_by_name: Arc::new(symbols_by_name),
            standalone_diagnostics: OnceLock::new(),
            base_diagnostics: diagnostics.clone(),
            diagnostics,
            symbols,
            occurrences,
            semantic_roles,
            editor,
        }
    }

    fn occurrence_at(&self, byte_offset: usize) -> Option<&Occurrence> {
        let offset = clamp_offset(&self.source, byte_offset);
        let index = self
            .occurrences
            .partition_point(|occurrence| occurrence.range.end < offset);
        self.occurrences.get(index).filter(|occurrence| {
            occurrence.range.contains(&offset)
                || (occurrence.range.end == offset && occurrence.range.start < offset)
        })
    }

    fn identifier_range_at(&self, offset: usize) -> Option<std::ops::Range<usize>> {
        let index = self
            .identifiers
            .partition_point(|token| token.range.end < offset);
        self.identifiers
            .get(index)
            .filter(|token| token.range.start <= offset)
            .map(|token| token.range.clone())
    }

    fn member_completions(&self, offset: usize) -> Option<Vec<CompletionItem>> {
        let receiver_range = receiver_before_dot(&self.source, offset)?;
        if self.source[..receiver_range.start]
            .trim_end()
            .ends_with('.')
        {
            return Some(Vec::new());
        }
        let receiver = &self.source[receiver_range];
        let prefix = completion_prefix(&self.source, offset);
        let symbol = self.visible_symbol(receiver, offset);
        if let Some(symbol) = symbol {
            let mut type_name = symbol.type_name.as_str();
            if let Some(alias) = self.symbols.iter().find(|candidate| {
                candidate.kind == SymbolKind::TypeAlias && candidate.name == type_name
            }) {
                type_name = &alias.type_name;
            }
            if let Some(body) = type_name
                .trim()
                .strip_prefix('{')
                .and_then(|ty| ty.strip_suffix('}'))
            {
                return Some(
                    split_type_fields(body)
                        .into_iter()
                        .filter_map(|field| {
                            let (name, ty) = field.split_once(':')?;
                            let name = name.trim();
                            (!name.is_empty() && !name.starts_with('[')).then(|| CompletionItem {
                                label: name.to_owned(),
                                detail: ty.trim().to_owned(),
                                kind: if ty.trim_start().starts_with('(') && ty.contains("->") {
                                    CompletionKind::Function
                                } else {
                                    CompletionKind::Variable
                                },
                                ..Default::default()
                            })
                        })
                        .filter(|item| item.label.starts_with(prefix))
                        .collect(),
                );
            }
            return Some(Vec::new());
        }
        let mut items = builtin_index()
            .iter()
            .flat_map(|(name, symbols)| symbols.iter().map(move |symbol| (name, symbol)))
            .filter_map(|(name, symbol)| {
                name.strip_prefix(&format!("{receiver}."))
                    .map(|field| (field, symbol))
            })
            .map(|(field, symbol)| CompletionItem {
                label: field.to_owned(),
                detail: format!("{} [{}]", symbol.detail(), symbol.context),
                kind: symbol.kind,
                ..Default::default()
            })
            .filter(|item| !item.label.contains('.') && item.label.starts_with(prefix))
            .map(|item| (item.label.clone(), item))
            .collect::<BTreeMap<_, _>>();
        if let Some(global) = builtin_index()
            .get(receiver)
            .and_then(|symbols| symbols.first())
            && let Some(alias) = builtin_index()
                .get(&global.type_name)
                .and_then(|symbols| symbols.first())
            && let Some(body) = alias
                .type_name
                .strip_prefix('{')
                .and_then(|ty| ty.strip_suffix('}'))
        {
            for field in split_type_fields(body) {
                if let Some((name, ty)) = field.split_once(':') {
                    let name = name.trim();
                    if name.starts_with(prefix) && !name.starts_with('[') {
                        items
                            .entry(name.to_owned())
                            .or_insert_with(|| CompletionItem {
                                label: name.to_owned(),
                                detail: ty.trim().to_owned(),
                                kind: if ty.trim_start().starts_with('(') && ty.contains("->") {
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
        Some(items.into_values().collect())
    }

    fn visible_symbol(&self, name: &str, offset: usize) -> Option<&Symbol> {
        self.symbols_by_name
            .get(name)
            .into_iter()
            .flatten()
            .map(|&id| &self.symbols[id])
            .filter(|symbol| {
                symbol.name == name
                    && symbol.visible.start <= offset
                    && symbol.scope.start <= offset
                    && offset <= symbol.scope.end
            })
            .max_by_key(|symbol| (symbol.scope_depth, symbol.visible.start))
    }
}

fn recover_partial_locals(
    source: &str,
    prepared: &syntax::PreparedSource,
    symbols: &mut Vec<Symbol>,
    occurrences: &mut Vec<Occurrence>,
) {
    let mut line_start = 0;
    for line in prepared.text.split_inclusive('\n') {
        let is_declaration =
            line.trim_start().starts_with("local ") || line.trim_start().starts_with("const ");
        if is_declaration
            && !lex(&prepared.text[..line_start]).iter().any(|token| {
                matches!(
                    token.to_string().trim(),
                    "do" | "then" | "repeat" | "function"
                )
            })
        {
            let line_prepared = syntax::prepare(line);
            let parsed =
                full_moon::parse_fallible(&line_prepared.text, LuaVersion::cfxlua().with_luau());
            if parsed.errors().is_empty() {
                let mut collector = SemanticCollector::new(line, &line_prepared);
                collector.visit_ast(&parsed.into_ast());
                let (line_symbols, _) = collector.finish(line);
                for symbol in line_symbols {
                    let start = prepared.original_offset(line_start + symbol.range.start);
                    let end = prepared.original_offset(line_start + symbol.range.end);
                    let visible_start = prepared.original_offset(line_start + symbol.visible.start);
                    if let Some(existing) = symbols
                        .iter_mut()
                        .find(|existing| existing.range.start == start && existing.range.end == end)
                    {
                        existing.visible.start = visible_start;
                        existing.visible.end = source.len();
                        existing.scope = 0..source.len();
                        existing.scope_depth = 0;
                        continue;
                    }
                    let id = symbols.len();
                    symbols.push(Symbol {
                        id,
                        name: symbol.name,
                        kind: symbol.kind,
                        range: start..end,
                        visible: visible_start..source.len(),
                        scope: 0..source.len(),
                        scope_depth: 0,
                        type_name: symbol.type_name,
                        signature: symbol.signature,
                        readonly: symbol.readonly,
                    });
                    occurrences.push(Occurrence {
                        symbol: id,
                        range: start..end,
                        declaration: true,
                    });
                }
            }
        }
        line_start += line.len();
    }
}

fn collect_import_bindings(
    file: &AnalyzedFile,
    imports: &HashMap<String, crate::project::ResolvedImport>,
    files: &BTreeMap<PathBuf, Arc<AnalyzedFile>>,
) -> Vec<ImportBinding> {
    let calls = if file.syntax_valid {
        file.parsed.import_calls()
    } else {
        crate::compiler::imports::analyze(&file.source, &file.prepared, &file.parsed.ast, true)
    };
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
    let signatures = SemanticCollector::new(source, &prepared);
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

fn nearest_project_root(path: &Path) -> Option<PathBuf> {
    if path.file_name().is_some_and(|name| name == "luat.toml") {
        return path.parent().map(Path::to_path_buf);
    }
    path.ancestors()
        .find(|ancestor| ancestor.join("luat.toml").is_file())
        .map(Path::to_path_buf)
}

fn apply_project_error(
    error: BuildError,
    overlays: &HashMap<PathBuf, String>,
    diagnostics: &mut HashMap<PathBuf, Vec<Diagnostic>>,
) {
    match error {
        BuildError::Compile { path, error } => {
            if let Some(existing) = diagnostics.get_mut(&path) {
                *existing = error.diagnostics;
            }
        }
        BuildError::CompileMany(errors) => {
            for (path, error) in errors {
                if let Some(existing) = diagnostics.get_mut(&path) {
                    *existing = error.diagnostics;
                }
            }
        }
        BuildError::Io { path, source } => {
            for (open_path, text) in overlays {
                diagnostics
                    .entry(open_path.clone())
                    .or_default()
                    .push(Diagnostic::at(
                        text,
                        0,
                        0,
                        "LT9001",
                        format!("{}: {source}", path.display()),
                    ));
            }
        }
        BuildError::Configuration { path, message } => {
            for (open_path, text) in overlays {
                diagnostics
                    .entry(open_path.clone())
                    .or_default()
                    .push(Diagnostic::at(
                        text,
                        0,
                        0,
                        "LT9001",
                        format!("{}: {message}", path.display()),
                    ));
            }
        }
    }
}

struct Scope {
    range: std::ops::Range<usize>,
    depth: usize,
}

struct PendingSymbol {
    name: String,
    kind: SymbolKind,
    range: std::ops::Range<usize>,
    visible_start: usize,
    scope_id: usize,
    type_name: String,
    signature: Option<SignatureInformation>,
    readonly: bool,
}

struct SemanticCollector<'a> {
    source: &'a str,
    prepared: &'a syntax::PreparedSource,
    scopes: Vec<Scope>,
    scope_stack: Vec<usize>,
    pending_parameters: Vec<Vec<(String, std::ops::Range<usize>, String)>>,
    symbols: Vec<PendingSymbol>,
    tokens: Vec<(std::ops::Range<usize>, u32, u32)>,
}

impl<'a> SemanticCollector<'a> {
    fn new(source: &'a str, prepared: &'a syntax::PreparedSource) -> Self {
        Self {
            source,
            prepared,
            scopes: vec![Scope {
                range: 0..source.len(),
                depth: 0,
            }],
            scope_stack: vec![0],
            pending_parameters: Vec::new(),
            symbols: Vec::new(),
            tokens: Vec::new(),
        }
    }

    fn add_symbol(&mut self, symbol: PendingSymbol) {
        self.symbols.push(symbol);
    }

    fn offset(&self, prepared: usize) -> usize {
        self.prepared.original_offset(prepared)
    }

    fn identifier_range(&self, token: &Token) -> Option<std::ops::Range<usize>> {
        let start = token.start_position().bytes();
        let end = token.end_position().bytes();
        Some(self.offset(start)..self.offset(end))
    }

    fn parameters(
        &self,
        body: &ast::FunctionBody,
    ) -> Vec<(String, std::ops::Range<usize>, String)> {
        body.parameters()
            .iter()
            .enumerate()
            .filter_map(|(index, parameter)| {
                let ast::Parameter::Name(name) = parameter else {
                    return None;
                };
                let token = name.token();
                let range = token.start_position().bytes()..token.end_position().bytes();
                let range = self.offset(range.start)..self.offset(range.end);
                let type_name = body.type_specifiers().nth(index).flatten().map_or_else(
                    || "any".to_owned(),
                    |specifier| specifier.type_info().to_string().trim().to_owned(),
                );
                Some((token.to_string(), range, type_name))
            })
            .collect()
    }

    fn signature(&self, body: &ast::FunctionBody, name: &str) -> SignatureInformation {
        let parameters = body
            .parameters()
            .iter()
            .enumerate()
            .map(|(index, parameter)| match parameter {
                ast::Parameter::Name(token) => {
                    let name = token.token().to_string();
                    let ty = body.type_specifiers().nth(index).flatten().map_or_else(
                        || "any".to_owned(),
                        |specifier| specifier.type_info().to_string().trim().to_owned(),
                    );
                    format!("{name}: {ty}")
                }
                ast::Parameter::Ellipsis(_) => {
                    body.type_specifiers().nth(index).flatten().map_or_else(
                        || "...".to_owned(),
                        |specifier| format!("...: {}", specifier.type_info().to_string().trim()),
                    )
                }
                _ => String::new(),
            })
            .filter(|parameter| !parameter.is_empty())
            .collect::<Vec<_>>();
        let offset = body
            .start_position()
            .map(|position| self.offset(position.bytes()))
            .unwrap_or_default();
        SignatureInformation {
            label: format!(
                "function {name}({}){}",
                parameters.join(", "),
                body.return_type()
                    .map_or_else(String::new, |specifier| format!(
                        ": {}",
                        specifier.type_info().to_string().trim()
                    ))
            ),
            documentation: nonempty(Documentation::before(self.source, offset).render()),
            parameter_documentation: parameter_documentation(self.source, offset, &parameters),
            parameters,
        }
    }

    fn finish(self, source: &str) -> (Vec<Symbol>, Vec<Occurrence>) {
        let symbols = self
            .symbols
            .into_iter()
            .enumerate()
            .map(|(id, pending)| {
                let scope = &self.scopes[pending.scope_id];
                Symbol {
                    id,
                    name: pending.name,
                    kind: pending.kind,
                    range: pending.range,
                    visible: pending.visible_start..scope.range.end,
                    scope: scope.range.clone(),
                    scope_depth: scope.depth,
                    type_name: pending.type_name,
                    signature: pending.signature,
                    readonly: pending.readonly,
                }
            })
            .collect::<Vec<_>>();
        let prepared = self.prepared;
        let declarations = symbols
            .iter()
            .map(|symbol| ((symbol.range.start, symbol.range.end), symbol.id))
            .collect::<HashMap<_, _>>();
        let mut names = HashMap::<&str, Vec<&Symbol>>::new();
        for symbol in &symbols {
            names.entry(&symbol.name).or_default().push(symbol);
        }
        let mut occurrences = Vec::new();
        for token in scan_identifiers(&prepared.text) {
            let start = prepared.original_offset(token.range.start);
            let end = prepared.original_offset(token.range.end);
            let name = token.name;
            let declaration = declarations.get(&(start, end)).copied();
            if let Some(symbol) = declaration {
                occurrences.push(Occurrence {
                    symbol,
                    range: start..end,
                    declaration: true,
                });
                continue;
            }
            let type_position = is_type_reference_position(source, start);
            if is_field_or_type_position(source, start, end) && !type_position {
                continue;
            }
            if type_position
                && !names
                    .get(name.as_str())
                    .into_iter()
                    .flatten()
                    .any(|symbol| {
                        symbol.kind == SymbolKind::TypeAlias
                            && symbol.name == name
                            && symbol.visible.start <= start
                            && symbol.scope.contains(&start)
                    })
            {
                continue;
            }
            let resolved = names
                .get(name.as_str())
                .into_iter()
                .flatten()
                .filter(|symbol| {
                    symbol.name == name
                        && if type_position {
                            symbol.kind == SymbolKind::TypeAlias
                        } else {
                            symbol.kind != SymbolKind::TypeAlias
                        }
                        && symbol.visible.start <= start
                        && symbol.scope.contains(&start)
                })
                .max_by_key(|symbol| (symbol.scope_depth, symbol.visible.start));
            if let Some(symbol) = resolved {
                occurrences.push(Occurrence {
                    symbol: symbol.id,
                    range: start..end,
                    declaration: false,
                });
            }
        }
        (symbols, occurrences)
    }
}

impl Visitor for SemanticCollector<'_> {
    fn visit_function_body(&mut self, body: &ast::FunctionBody) {
        self.pending_parameters.push(self.parameters(body));
    }
    fn visit_type_info(&mut self, info: &ast::luau::TypeInfo) {
        if let ast::luau::TypeInfo::Basic(name) = info
            && let Some(range) = self.identifier_range(name.token())
        {
            let builtin = matches!(
                name.token().to_string().as_str(),
                "any"
                    | "boolean"
                    | "integer"
                    | "never"
                    | "number"
                    | "string"
                    | "table"
                    | "thread"
                    | "unknown"
                    | "userdata"
                    | "vector2"
                    | "vector3"
                    | "vector4"
            );
            self.tokens.push((range, 2, if builtin { 4 } else { 0 }));
        }
    }
    fn visit_function_call(&mut self, call: &ast::FunctionCall) {
        let mut target = match call.prefix() {
            ast::Prefix::Name(name) => Some(name),
            _ => None,
        };
        for suffix in call.suffixes() {
            match suffix {
                ast::Suffix::Index(ast::Index::Dot { name, .. }) => target = Some(name),
                ast::Suffix::Call(ast::Call::MethodCall(method)) => {
                    if let Some(range) = self.identifier_range(method.name().token()) {
                        self.tokens.push((range, 3, 0));
                    }
                    target = None;
                }
                ast::Suffix::Call(_) => {
                    if let Some(name) = target.take()
                        && let Some(range) = self.identifier_range(name.token())
                    {
                        self.tokens.push((range, 3, 0));
                    }
                }
                _ => target = None,
            }
        }
    }

    fn visit_block(&mut self, node: &ast::Block) {
        let start = node
            .start_position()
            .map_or(0, |position| self.offset(position.bytes()));
        let end = node
            .end_position()
            .map_or(self.source.len(), |position| self.offset(position.bytes()));
        let id = self.scopes.len();
        let depth = self.scope_stack.len();
        self.scopes.push(Scope {
            range: start..end.max(start),
            depth,
        });
        self.scope_stack.push(id);
        if let Some(parameters) = self.pending_parameters.pop() {
            for (name, range, type_name) in parameters {
                self.add_symbol(PendingSymbol {
                    name,
                    kind: SymbolKind::Parameter,
                    range,
                    visible_start: start,
                    scope_id: id,
                    type_name,
                    signature: None,
                    readonly: false,
                });
            }
        }
    }

    fn visit_block_end(&mut self, _: &ast::Block) {
        if self.scope_stack.len() > 1 {
            self.scope_stack.pop();
        }
    }

    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        let visible_start = node.end_position().map_or_else(
            || node.start_position().map_or(0, |position| position.bytes()),
            |position| position.bytes(),
        );
        for (index, name) in node.names().iter().enumerate() {
            let Some(range) = self.identifier_range(name.token()) else {
                continue;
            };
            let type_name = node.type_specifiers().nth(index).flatten().map_or_else(
                || {
                    node.expressions()
                        .iter()
                        .nth(index)
                        .map_or_else(|| "any".to_owned(), infer_literal)
                },
                |specifier| specifier.type_info().to_string().trim().to_owned(),
            );
            let signature =
                node.expressions()
                    .iter()
                    .nth(index)
                    .and_then(|expression| match expression {
                        ast::Expression::Function(function) => {
                            Some(self.signature(function.body(), &name.token().to_string()))
                        }
                        _ => None,
                    });
            self.add_symbol(PendingSymbol {
                name: name.token().to_string(),
                kind: SymbolKind::Local,
                range,
                visible_start: self.offset(visible_start),
                scope_id: *self.scope_stack.last().unwrap_or(&0),
                type_name,
                signature,
                readonly: false,
            });
        }
    }

    fn visit_const_assignment(&mut self, node: &ast::luau::ConstAssignment) {
        let visible_start = node.end_position().map_or_else(
            || node.start_position().map_or(0, |position| position.bytes()),
            |position| position.bytes(),
        );
        for (index, name) in node.names().iter().enumerate() {
            let Some(range) = self.identifier_range(name.token()) else {
                continue;
            };
            let type_name = node.type_specifiers().nth(index).flatten().map_or_else(
                || {
                    node.expressions()
                        .iter()
                        .nth(index)
                        .map_or_else(|| "any".to_owned(), infer_literal)
                },
                |specifier| specifier.type_info().to_string().trim().to_owned(),
            );
            self.add_symbol(PendingSymbol {
                name: name.token().to_string(),
                kind: SymbolKind::Local,
                range,
                visible_start: self.offset(visible_start),
                scope_id: *self.scope_stack.last().unwrap_or(&0),
                type_name,
                signature: None,
                readonly: true,
            });
        }
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        let name = node.name().token().to_string();
        let Some(range) = self.identifier_range(node.name().token()) else {
            return;
        };
        let visible_start = node.start_position().map_or(0, |position| position.bytes());
        let signature = self.signature(node.body(), &name);
        self.add_symbol(PendingSymbol {
            name,
            kind: SymbolKind::Function,
            range,
            visible_start: self.offset(visible_start),
            scope_id: *self.scope_stack.last().unwrap_or(&0),
            type_name: "function".to_owned(),
            signature: Some(signature),
            readonly: false,
        });
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        if node.name().names().len() == 1 && node.name().method_name().is_none() {
            let token = node.name().names().iter().next().unwrap();
            let name = token.token().to_string();
            if let Some(range) = self.identifier_range(token.token()) {
                self.add_symbol(PendingSymbol {
                    name: name.clone(),
                    kind: SymbolKind::Function,
                    range,
                    visible_start: 0,
                    scope_id: 0,
                    type_name: "function".to_owned(),
                    signature: Some(self.signature(node.body(), &name)),
                    readonly: false,
                });
            }
        }
    }

    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        let token = node.type_name();
        let Some(range) = self.identifier_range(token.token()) else {
            return;
        };
        let Some(visible_start) = token.end_position().map(|position| position.bytes()) else {
            return;
        };
        self.add_symbol(PendingSymbol {
            name: token.token().to_string(),
            kind: SymbolKind::TypeAlias,
            range,
            visible_start: self.offset(visible_start),
            scope_id: if self.scope_stack.len() == 2 {
                0
            } else {
                *self.scope_stack.last().unwrap_or(&0)
            },
            type_name: node.type_definition().to_string().trim().to_owned(),
            signature: None,
            readonly: false,
        });
    }
}

fn infer_literal(expression: &ast::Expression) -> String {
    let value = expression.to_string();
    let value = value.trim();
    if value.starts_with(['\'', '"']) {
        "string".to_owned()
    } else if value.parse::<f64>().is_ok() {
        "number".to_owned()
    } else if matches!(value, "true" | "false") {
        "boolean".to_owned()
    } else if value.starts_with('{') {
        "table".to_owned()
    } else if value.starts_with("function") {
        "function".to_owned()
    } else {
        "any".to_owned()
    }
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

struct BuiltinSymbol {
    name: String,
    file: String,
    range: std::ops::Range<usize>,
    signature: Option<SignatureInformation>,
    kind: CompletionKind,
    type_name: String,
    documentation: String,
    context: &'static str,
}

fn hover_markdown(detail: &str, documentation: &str) -> String {
    let detail = crate::compiler::typecheck::display_type(detail);
    let mut contents = format!("```luat\n{detail}\n```");
    let documentation = Documentation::parse(documentation).render();
    if !documentation.trim().is_empty() {
        contents.push_str("\n\n");
        contents.push_str(&documentation);
    }
    contents
}

impl BuiltinSymbol {
    fn detail(&self) -> String {
        self.signature.as_ref().map_or_else(
            || format!("{}: {}", self.name, self.type_name),
            |signature| signature.label.clone(),
        )
    }
}

fn documentation_before(source: &str, offset: usize) -> String {
    documentation::raw_before(source, offset)
}

fn nonempty(text: String) -> Option<String> {
    (!text.trim().is_empty()).then_some(text)
}

fn parameter_documentation(
    source: &str,
    offset: usize,
    parameters: &[String],
) -> Vec<Option<String>> {
    let documentation = Documentation::before(source, offset);
    parameters
        .iter()
        .map(|parameter| {
            let name = parameter
                .split(':')
                .next()
                .unwrap_or(parameter)
                .trim()
                .trim_end_matches('?');
            documentation.parameters.get(name).cloned().or_else(|| {
                documentation.markdown.lines().find_map(|line| {
                    let marker = format!("**{name}**");
                    line.split_once(&marker)
                        .and_then(|(_, text)| text.strip_prefix(':'))
                        .map(|text| text.trim().to_owned())
                        .or_else(|| {
                            line.split_once(':')
                                .filter(|(documented, _)| documented.trim() == name)
                                .map(|(_, text)| text.trim().to_owned())
                        })
                })
            })
        })
        .collect()
}

fn builtin_index() -> &'static BTreeMap<String, Vec<BuiltinSymbol>> {
    static INDEX: OnceLock<BTreeMap<String, Vec<BuiltinSymbol>>> = OnceLock::new();
    INDEX.get_or_init(|| {
        [
            (
                "lua-shared.d.luat",
                include_str!("../lsp/definitions/lua-shared.d.luat"),
            ),
            (
                "lua-server.d.luat",
                include_str!("../lsp/definitions/lua-server.d.luat"),
            ),
            (
                "fivem-client.d.luat",
                include_str!("../lsp/definitions/fivem-client.d.luat"),
            ),
            (
                "fivem-server.d.luat",
                include_str!("../lsp/definitions/fivem-server.d.luat"),
            ),
            (
                "fivem-shared.d.luat",
                include_str!("../lsp/definitions/fivem-shared.d.luat"),
            ),
            (
                "runtime-client.d.luat",
                include_str!("../lsp/definitions/runtime-client.d.luat"),
            ),
            (
                "runtime-server.d.luat",
                include_str!("../lsp/definitions/runtime-server.d.luat"),
            ),
            (
                "runtime-shared.d.luat",
                include_str!("../lsp/definitions/runtime-shared.d.luat"),
            ),
        ]
        .into_iter()
        .fold(
            BTreeMap::<String, Vec<BuiltinSymbol>>::new(),
            |mut index, (file, source)| {
                let context = if file.contains("client") {
                    "client"
                } else if file.contains("server") {
                    "server"
                } else {
                    "shared"
                };
                if let Ok((prepared, ast, globals)) =
                    crate::compiler::declarations::parse_declaration_file(source)
                {
                    let collector = SemanticCollector::new(source, &prepared);
                    for global in globals {
                        index
                            .entry(global.name.clone())
                            .or_default()
                            .push(BuiltinSymbol {
                                name: global.name,
                                file: file.to_owned(),
                                range: global.location.range,
                                signature: None,
                                kind: if global.type_name.trim_start().starts_with('(')
                                    && global.type_name.contains("->")
                                {
                                    CompletionKind::Function
                                } else {
                                    CompletionKind::Variable
                                },
                                type_name: global.type_name,
                                documentation: global.documentation,
                                context,
                            });
                    }
                    for statement in ast.nodes().stmts() {
                        if let ast::Stmt::TypeDeclaration(alias) = statement {
                            let token = alias.type_name().token();
                            let name = token.to_string();
                            let range = prepared.original_offset(token.start_position().bytes())
                                ..prepared.original_offset(token.end_position().bytes());
                            index.entry(name.clone()).or_default().push(BuiltinSymbol {
                                name,
                                file: file.to_owned(),
                                range: range.clone(),
                                signature: None,
                                kind: CompletionKind::Type,
                                type_name: alias.type_definition().to_string().trim().to_owned(),
                                documentation: documentation_before(source, range.start),
                                context,
                            });
                        }
                        let ast::Stmt::FunctionDeclaration(function) = statement else {
                            continue;
                        };
                        let name = function.name().to_string().trim().to_owned();
                        let range = prepared
                            .original_offset(function.name().start_position().unwrap().bytes())
                            ..prepared
                                .original_offset(function.name().end_position().unwrap().bytes());
                        index.entry(name.clone()).or_default().push(BuiltinSymbol {
                            name: name.clone(),
                            file: file.to_owned(),
                            range: range.clone(),
                            signature: Some(collector.signature(function.body(), &name)),
                            kind: CompletionKind::Function,
                            type_name: "function".to_owned(),
                            documentation: documentation_before(source, range.start),
                            context,
                        });
                    }
                }
                index
            },
        )
    })
}

fn builtin_by_short_name(name: &str) -> Vec<&'static BuiltinSymbol> {
    builtin_index()
        .values()
        .flatten()
        .filter(|symbol| symbol.name == name)
        .collect()
}

fn builtin_source_path(file: &str) -> PathBuf {
    if let Some(path) = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()
            .map(|parent| parent.join("definitions").join(file))
    }) && path.is_file()
    {
        return path;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("lsp")
        .join("definitions")
        .join(file)
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
            if let Some(end) = long_bracket_end(source, index + 2) {
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
            && let Some(end) = long_bracket_end(source, index)
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

fn long_bracket_end(source: &str, start: usize) -> Option<usize> {
    let suffix = source.get(start..)?;
    if !suffix.starts_with('[') {
        return None;
    }
    let opening = suffix[1..].find('[')? + 1;
    let level = opening - 1;
    if !suffix[1..opening].chars().all(|character| character == '=') {
        return None;
    }
    let close = format!("]{}]", "=".repeat(level));
    let content_start = start + opening + 1;
    source[content_start..]
        .find(&close)
        .map(|offset| content_start + offset + close.len())
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
        let runtime = include_str!("../lsp/definitions/runtime-shared.d.luat");
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
            .join("lsp/definitions/runtime-client.d.luat");
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
