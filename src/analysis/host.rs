use super::*;
use crate::ExecutionContext;

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
    scoped_declarations: crate::project::ScopedDeclarations,
}

impl AnalysisHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_file(&mut self, path: PathBuf, source: String) {
        if self.files.get(&path).map(AsRef::as_ref) == Some(source.as_str()) {
            return;
        }
        if is_configuration(&path) {
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
        if path.file_name().is_some_and(|name| name == "luat.lock")
            || path.components().any(|part| part.as_os_str() == ".luat")
            || is_configuration(path)
        {
            self.filesystem_dirty.insert(path.to_path_buf());
        }
        if is_configuration(path) {
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

    pub fn editor_project_context_current(&self, path: &Path) -> bool {
        let Some(root) = self
            .roots
            .get(path)
            .cloned()
            .unwrap_or_else(|| nearest_project_root(path))
        else {
            return true;
        };
        let Some((revision, snapshot)) = &self.cached else {
            return false;
        };
        if *revision == self.revision {
            return true;
        }
        if snapshot.project_root(path).as_ref() != Some(&root)
            || self
                .dirty_paths
                .iter()
                .any(|dirty| dirty != path && dirty.starts_with(&root))
            || self.filesystem_dirty.contains(path)
            || path.to_string_lossy().ends_with(".d.luat")
        {
            return false;
        }
        let Some(source) = self.files.get(path) else {
            return false;
        };
        let file = self.inputs.file(path, source);
        let Ok(calls) = file.parsed.editor_import_calls() else {
            return false;
        };
        let actual: HashSet<_> = calls.iter().map(|call| call.specifier.as_str()).collect();
        let cached: HashSet<_> = snapshot
            .resolved_imports
            .get(path)
            .into_iter()
            .flat_map(|imports| imports.keys().map(String::as_str))
            .collect();
        actual == cached
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
            if path
                .extension()
                .is_some_and(|extension| extension == "luat" || extension == "lua")
            {
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
                            context: None,
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
            if path
                .extension()
                .is_some_and(|extension| extension == "luat" || extension == "lua")
            {
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
            let errors = if declaration
                && (old.is_none() || self.dirty_paths.iter().any(|path| is_configuration(path)))
            {
                DeclarationSet::from_sources([crate::compiler::declarations::DeclarationSource {
                    path: Some(path.clone()),
                    context: None,
                    text: file.source.to_string(),
                }])
                .err()
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
        let mut symbol_visibility = HashMap::new();
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
            let config_path = configuration_path(&root);
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
            if reload {
                self.scoped_declarations.clear();
                changed.insert(config_path.clone());
            }
            let current_load =
                if root.join("luat.toml").is_file() && root.join("luat-package.toml").is_file() {
                    Project::load(&root)
                } else if !reload {
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
                    for path in project
                        .source_paths()
                        .chain(project.declaration_files().iter().map(PathBuf::as_path))
                    {
                        if let Some(visible) = project.editor_visible_paths(path) {
                            symbol_visibility.insert(path.to_path_buf(), visible);
                        }
                    }
                    let declarations_changed = reload
                        || changed.iter().any(|path| {
                            path == &configuration_path(&root)
                                || path == &root.join("luat.lock")
                                || path == &root.join(".luat/install.lock")
                                || project.declaration_files().contains(path)
                        });
                    if declarations_changed {
                        self.scoped_declarations.clear();
                    }
                    let mut declaration_error = None;
                    let declarations =
                        if declarations_changed || !self.project_declarations.contains_key(&root) {
                            match project
                                .read_editor_declarations(&overlays, &mut self.scoped_declarations)
                            {
                                Ok(declarations) => {
                                    self.project_declarations.insert(root.clone(), declarations);
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
                            .filter(|_| !reload && !self.dirty_paths.contains(path))
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
                            diagnostics.insert(path.clone(), Vec::new());
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
                        &mut self.scoped_declarations,
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
                    if let Some(declarations) = &declarations {
                        for file in &declarations.files {
                            if let Some(path) = &file.path {
                                let sides = match file.context {
                                    Some(side) => vec![side],
                                    None => {
                                        vec![ExecutionContext::Client, ExecutionContext::Server]
                                    }
                                };
                                contexts.insert(path.clone(), sides);
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
                            let declaration_context = declarations
                                .as_ref()
                                .and_then(|declarations| {
                                    declarations
                                        .files
                                        .iter()
                                        .find(|declaration| declaration.path.as_ref() == Some(path))
                                })
                                .and_then(|declaration| declaration.context);
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
                                    context: declaration_context,
                                });
                        }
                    }
                    if let Some(declarations) = &declarations {
                        for global in declarations.globals.values().chain(
                            declarations
                                .scoped_globals
                                .values()
                                .flat_map(|globals| globals.values()),
                        ) {
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
                                    context: global.context,
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
            symbol_visibility: Arc::new(symbol_visibility),
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

pub(super) fn nearest_project_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| {
            ancestor.join("luat.toml").is_file()
                || ancestor.join("luat-package.toml").is_file()
                    && !ancestor
                        .components()
                        .any(|part| part.as_os_str() == ".luat")
        })
        .map(Path::to_path_buf)
}

pub(super) fn is_configuration(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == "luat.toml" || name == "luat-package.toml")
}

pub(super) fn configuration_path(root: &Path) -> PathBuf {
    let project = root.join("luat.toml");
    if project.is_file() {
        project
    } else {
        root.join("luat-package.toml")
    }
}

#[test]
fn discovers_standalone_package_roots() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/tests")
        .join(format!("package-discovery-{}", std::process::id()));
    fs::create_dir_all(root.join("modules")).unwrap();
    fs::write(root.join("luat-package.toml"), "name='example'\n").unwrap();
    assert_eq!(
        nearest_project_root(&root.join("modules/api.lua")),
        Some(root.clone())
    );
    assert_eq!(
        nearest_project_root(&root.join("luat-package.toml")),
        Some(root.clone())
    );
    fs::remove_dir_all(root).unwrap();
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
