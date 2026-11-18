mod config;
pub(crate) mod imports;
mod manifest;
mod packages;

use config::FiveMConfig;
pub use config::ProjectConfig;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::compiler::declarations::{DeclarationSet, DeclarationSource};
use crate::{CompileError, Diagnostic};

type DeclarationInputs = Vec<(PathBuf, String)>;
type ScopedDeclarations = HashMap<DeclarationInputs, std::sync::Arc<DeclarationSet>>;
type Outputs = Vec<(PathBuf, String)>;
type CompiledFiles = HashMap<PathBuf, (PathBuf, String)>;

#[derive(Default)]
pub struct BuildSession {
    previous: Option<imports::ImportPlan>,
    declarations: ScopedDeclarations,
    compiled: CompiledFiles,
    published: Option<(PathBuf, Outputs, String)>,
}

#[derive(Debug)]
pub enum BuildError {
    Io { path: PathBuf, source: io::Error },
    Configuration { path: PathBuf, message: String },
    Compile { path: PathBuf, error: CompileError },
    CompileMany(Vec<(PathBuf, CompileError)>),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Configuration { path, message } => {
                write!(f, "{}: {message}", path.display())
            }
            Self::Compile { path, error } => write!(f, "{}: {error}", path.display()),
            Self::CompileMany(errors) => {
                for (index, (path, error)) in errors.iter().enumerate() {
                    if index > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{}: {error}", path.display())?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for BuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Compile { error, .. } => Some(error),
            Self::CompileMany(_) => None,
            Self::Configuration { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
struct SourceFile {
    input: PathBuf,
    output: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Project {
    config_path: PathBuf,
    config: ProjectConfig,
    output_dir: PathBuf,
    root: PathBuf,
    manifest: String,
    warnings: Vec<String>,
    declarations: Vec<PathBuf>,
    sources: Vec<SourceFile>,
    package_sources: HashMap<PathBuf, packages::PackageSource>,
    package_declaration_roots: HashMap<PathBuf, PathBuf>,
    package_manifests: HashMap<PathBuf, crate::packages::PackageManifest>,
    package_root: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedImport {
    pub target_path: PathBuf,
    pub type_name: String,
}

pub(crate) type ResolvedImports = HashMap<PathBuf, HashMap<String, ResolvedImport>>;

#[derive(Debug, Default, Eq, PartialEq)]
pub struct BuildResult {
    pub compiled_files: usize,
    pub declaration_files: usize,
    pub output_dir: Option<PathBuf>,
}

impl Project {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, BuildError> {
        let requested = path.as_ref();
        let requested = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|source| BuildError::Io {
                    path: requested.to_path_buf(),
                    source,
                })?
                .join(requested)
        };
        let config_path = if requested.is_dir() {
            if requested.join("luat.toml").is_file()
                && requested.join("luat-package.toml").is_file()
            {
                return Err(BuildError::Configuration { path: requested.to_path_buf(), message: "both luat.toml and luat-package.toml exist; select a configuration file explicitly".into() });
            }
            if requested.join("luat.toml").is_file() {
                requested.join("luat.toml")
            } else if requested.join("luat-package.toml").is_file() {
                requested.join("luat-package.toml")
            } else {
                requested.join("luat.toml")
            }
        } else {
            requested
        };
        if config_path
            .file_name()
            .is_none_or(|n| n != "luat.toml" && n != "luat-package.toml")
        {
            return Err(BuildError::Configuration {
                path: config_path,
                message: "use luat.toml or luat-package.toml".into(),
            });
        }
        let text = fs::read_to_string(&config_path).map_err(|source| BuildError::Io {
            path: config_path.clone(),
            source,
        })?;
        Self::load_with_config_text(&config_path, &text)
    }

    pub(crate) fn load_with_config_text(
        config_path: &Path,
        text: &str,
    ) -> Result<Self, BuildError> {
        if config_path
            .file_name()
            .is_some_and(|name| name == "luat-package.toml")
        {
            return Self::load_package(config_path, text);
        }
        let config_path = config_path.to_path_buf();
        let config: ProjectConfig =
            toml::from_str(text).map_err(|error| BuildError::Configuration {
                path: config_path.clone(),
                message: error.to_string(),
            })?;

        let out_dir = &config.compiler.out_dir;
        if out_dir.as_os_str().is_empty()
            || out_dir.is_absolute()
            || !out_dir
                .components()
                .any(|component| matches!(component, std::path::Component::Normal(_)))
            || out_dir.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::Prefix(_)
                        | std::path::Component::RootDir
                )
            })
        {
            return Err(BuildError::Configuration {
                path: config_path,
                message: "out_dir must be a non-empty path inside the project".to_owned(),
            });
        }

        if config.project.name.trim().is_empty()
            || config.project.name.contains(['/', '\\'])
            || config.fivem.game.is_some() && !config.fivem.games.is_empty()
            || config
                .fivem
                .node_version
                .is_some_and(|version| !matches!(version, 16 | 22))
            || config
                .fivem
                .entries
                .iter()
                .any(|entry| !manifest::is_manifest_identifier(&entry.key))
            || config
                .fivem
                .entries
                .iter()
                .any(|entry| manifest::is_builtin_manifest_key(&entry.key))
            || config
                .fivem
                .data_file
                .iter()
                .any(|entry| entry.r#type.trim().is_empty())
        {
            return Err(BuildError::Configuration {
                path: config_path,
                message: "invalid project name, fivem.entries key, data_file type, or conflicting fivem.game and fivem.games".to_owned(),
            });
        }
        let root = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let installed = crate::packages::installed_packages(&root, &config.dependencies).map_err(
            |message| BuildError::Configuration {
                path: config_path.clone(),
                message,
            },
        )?;
        let src_root = root.join("src");
        if fs::symlink_metadata(&src_root).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(BuildError::Configuration {
                path: config_path,
                message: "src/ must not be a symlink or junction".to_owned(),
            });
        }
        let mut sources = Vec::new();
        let mut outputs = HashSet::new();
        let mut declaration_paths = HashSet::new();

        for pattern in &config.compiler.declarations {
            validate_project_path(pattern, &config_path)?;
            let absolute_pattern = root.join(pattern);
            let pattern = absolute_pattern.to_string_lossy().replace('\\', "/");
            let mut matches = glob::glob(&pattern)
                .map_err(|error| BuildError::Configuration {
                    path: config_path.clone(),
                    message: error.to_string(),
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| BuildError::Configuration {
                    path: config_path.clone(),
                    message: error.to_string(),
                })?;
            matches.sort();
            let matched = !matches.is_empty();
            for path in matches {
                let canonical_root = fs::canonicalize(&root).map_err(|source| BuildError::Io {
                    path: root.clone(),
                    source,
                })?;
                let canonical_path = fs::canonicalize(&path).map_err(|source| BuildError::Io {
                    path: path.clone(),
                    source,
                })?;
                reject_symlink_path(&root, &path, &config_path)?;
                if !canonical_path.starts_with(&canonical_root) {
                    return Err(BuildError::Configuration {
                        path: config_path.clone(),
                        message: format!(
                            "declaration path resolves outside the project: {}",
                            path.display()
                        ),
                    });
                }
                if !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".d.luat"))
                {
                    return Err(BuildError::Configuration {
                        path: config_path.clone(),
                        message: format!(
                            "declaration files must use the .d.luat extension: {}",
                            path.display()
                        ),
                    });
                }
                declaration_paths.insert(path);
            }
            if !matched {
                return Err(BuildError::Configuration {
                    path: config_path.clone(),
                    message: format!("declaration pattern matched no files: {}", pattern),
                });
            }
        }
        let mut runtime_paths = Vec::new();
        collect_luat(&src_root, &mut runtime_paths).map_err(|source| BuildError::Io {
            path: src_root,
            source,
        })?;
        runtime_paths.sort();
        for path in runtime_paths {
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".d.luat"))
            {
                continue;
            }
            let relative = path.strip_prefix(&root).unwrap_or(&path);
            let output = output_path(relative, &config_path)?;
            if !outputs.insert(output.clone()) {
                return Err(BuildError::Configuration {
                    path: config_path.clone(),
                    message: format!("multiple source files map to {}", output.display()),
                });
            }
            sources.push(SourceFile {
                input: path,
                output: Some(output),
            });
        }
        let mut declarations = declaration_paths.into_iter().collect::<Vec<_>>();
        declarations.sort();
        for path in &declarations {
            sources.push(SourceFile {
                input: path.clone(),
                output: None,
            });
        }
        manifest::validate_manifest_paths(&config.fivem, &root, &config_path)?;
        let output_dir = root.join(&config.compiler.out_dir);
        let manifest_path = root.join("fxmanifest.lua");
        reject_symlink_path(&root, &manifest_path, &config_path)?;
        if output_dir.exists() && !output_dir.is_dir() {
            return Err(BuildError::Configuration {
                path: config_path.clone(),
                message: "out_dir exists but is not a directory".to_owned(),
            });
        }
        if manifest_path.is_dir() {
            return Err(BuildError::Configuration {
                path: config_path.clone(),
                message: "fxmanifest.lua exists but is not a file".to_owned(),
            });
        }
        let output_conflicts = std::iter::once(&config_path)
            .chain(std::iter::once(&manifest_path))
            .chain(sources.iter().map(|source| &source.input))
            .any(|protected| {
                protected.starts_with(&output_dir) || output_dir.starts_with(protected)
            });
        if output_conflicts {
            return Err(BuildError::Configuration {
                path: config_path,
                message: "out_dir must not overlap the project config or source files".to_owned(),
            });
        }
        let canonical_root = fs::canonicalize(&root).map_err(|source| BuildError::Io {
            path: root.clone(),
            source,
        })?;
        reject_symlink_path(&root, &output_dir, &config_path)?;
        let mut existing = output_dir.as_path();
        while !existing.exists() {
            existing = existing.parent().ok_or_else(|| BuildError::Configuration {
                path: config_path.clone(),
                message: "could not resolve output directory safely".to_owned(),
            })?;
        }
        let canonical_output_parent =
            fs::canonicalize(existing).map_err(|source| BuildError::Io {
                path: existing.to_path_buf(),
                source,
            })?;
        if !canonical_output_parent.starts_with(&canonical_root) {
            return Err(BuildError::Configuration {
                path: config_path,
                message: "out_dir resolves outside the project through a symlink".to_owned(),
            });
        }
        let mut warnings = Vec::new();
        if config.fivem.lua54 {
            warnings
                .push("fivem.lua54 is deprecated; FiveM now runs Lua 5.4 by default".to_owned());
        }
        if config.fivem.resource_manifest_version.is_some() {
            warnings.push(
                "fivem.resource_manifest_version is deprecated; use fx_version instead".to_owned(),
            );
        }
        for key in ["before_level_meta", "after_level_meta"] {
            let configured = match key {
                "before_level_meta" => config.fivem.before_level_meta.is_some(),
                _ => config.fivem.after_level_meta.is_some(),
            };
            if configured {
                warnings.push(format!(
                    "fivem.{key} is deprecated; prefer data_file where possible"
                ));
            }
        }
        let manifest = manifest::render_manifest(&config, &root).map_err(|message| {
            BuildError::Configuration {
                path: config_path.clone(),
                message,
            }
        })?;
        let mut project = Self {
            config_path,
            config,
            output_dir,
            root,
            manifest,
            warnings,
            declarations,
            sources,
            package_sources: HashMap::new(),
            package_declaration_roots: HashMap::new(),
            package_manifests: HashMap::new(),
            package_root: false,
        };
        project.attach_packages(installed)?;
        Ok(project)
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn declaration_files(&self) -> &[PathBuf] {
        &self.declarations
    }

    pub(crate) fn source_paths(&self) -> impl Iterator<Item = &Path> {
        self.sources
            .iter()
            .filter(|file| file.output.is_some())
            .map(|file| file.input.as_path())
    }

    pub fn check(&self) -> Result<BuildResult, BuildError> {
        Self::load(&self.config_path)?.check_current()
    }

    pub fn check_with_overlays(
        &self,
        overlays: &std::collections::HashMap<PathBuf, String>,
    ) -> Result<BuildResult, BuildError> {
        self.check_current_with_overlays(overlays)
    }

    fn check_current(&self) -> Result<BuildResult, BuildError> {
        self.check_current_with_overlays(&std::collections::HashMap::new())
    }

    pub(crate) fn check_editor_with_plan(
        &self,
        plan: &imports::ImportPlan,
        previous: Option<&imports::ImportPlan>,
        changed: &BTreeSet<PathBuf>,
        declarations: &DeclarationSet,
        parse: &imports::SourceParser<'_>,
    ) -> (BTreeSet<PathBuf>, Result<(), BuildError>, ResolvedImports) {
        let source_paths = self
            .source_paths()
            .map(Path::to_path_buf)
            .collect::<BTreeSet<_>>();
        let interfaces = changed
            .iter()
            .filter(|path| {
                previous.is_none_or(|old| {
                    old.exports.get(*path) != plan.exports.get(*path)
                        || old.resolved.get(*path) != plan.resolved.get(*path)
                }) || *path == &self.config_path
                    || self.declarations.contains(path)
            })
            .cloned()
            .collect();
        let mut checked = if previous.is_none() {
            self.source_paths().map(Path::to_path_buf).collect()
        } else {
            self.dependent_source_paths(plan, &interfaces)
        };
        checked.extend(changed.intersection(&source_paths).cloned());
        if let Some(old) = previous {
            for path in plan.texts.keys().chain(old.texts.keys()) {
                if !old.texts.contains_key(path)
                    || !plan.texts.contains_key(path)
                    || old.execution_contexts.get(path) != plan.execution_contexts.get(path)
                    || old.import_types.get(path) != plan.import_types.get(path)
                    || [
                        None,
                        Some(crate::ExecutionContext::Client),
                        Some(crate::ExecutionContext::Server),
                    ]
                    .into_iter()
                    .any(|context| {
                        let key = (path.clone(), context);
                        old.contextual_import_types.get(&key)
                            != plan.contextual_import_types.get(&key)
                    })
                {
                    checked.insert(path.clone());
                }
            }
        }
        checked.retain(|path| source_paths.contains(path));
        let mut errors = Vec::new();
        for file in &self.sources {
            if file.output.is_none() || !checked.contains(&file.input) {
                continue;
            }
            let Some(source) = plan.texts.get(&file.input) else {
                continue;
            };
            let parsed = match parse(&file.input, source) {
                Ok(parsed) => parsed,
                Err(error) => return (checked, Err(error), self.resolved_imports(plan)),
            };
            let contexts = plan
                .execution_contexts
                .get(&file.input)
                .map(|contexts| contexts.iter().copied().map(Some).collect::<Vec<_>>())
                .filter(|contexts| !contexts.is_empty())
                .unwrap_or_else(|| vec![None]);
            let owned_declarations = if self.package_sources.contains_key(&file.input) {
                match self.read_declarations_for(Some(&file.input), &HashMap::new()) {
                    Ok(value) => Some(value),
                    Err(error) => return (checked, Err(error), self.resolved_imports(plan)),
                }
            } else {
                None
            };
            let declarations = owned_declarations.as_ref().unwrap_or(declarations);
            let mut diagnostics = Vec::new();
            for context in contexts {
                if let Err(error) = parsed.check(
                    declarations,
                    true,
                    plan.contextual_import_types
                        .get(&(file.input.clone(), context)),
                    context,
                ) {
                    for diagnostic in error.diagnostics {
                        if !diagnostics.contains(&diagnostic) {
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
            if !diagnostics.is_empty() {
                errors.push((file.input.clone(), CompileError { diagnostics }));
            }
        }
        let result = if errors.is_empty() {
            Ok(())
        } else {
            Err(compile_errors(errors))
        };
        (checked, result, self.resolved_imports(plan))
    }

    fn check_current_with_overlays(
        &self,
        overlays: &std::collections::HashMap<PathBuf, String>,
    ) -> Result<BuildResult, BuildError> {
        let mut cache = HashMap::new();
        let mut plan = self.analyze_imports_incremental(overlays, None, None, None, &mut cache)?;
        self.populate_declarations(&mut plan, &mut cache)?;
        let mut errors = Vec::new();
        for file in &self.sources {
            let Some(parsed) = plan.parsed.get(&file.input) else {
                continue;
            };
            let contexts = plan
                .execution_contexts
                .get(&file.input)
                .map(|contexts| contexts.iter().copied().map(Some).collect::<Vec<_>>())
                .filter(|contexts| !contexts.is_empty())
                .unwrap_or_else(|| vec![None]);
            let declarations = plan.declarations_for(self, Some(&file.input));
            let mut diagnostics = Vec::new();
            for context in contexts {
                if let Err(error) = parsed.check(
                    declarations,
                    true,
                    plan.contextual_import_types
                        .get(&(file.input.clone(), context)),
                    context,
                ) {
                    for diagnostic in error.diagnostics {
                        if !diagnostics.contains(&diagnostic) {
                            diagnostics.push(diagnostic);
                        }
                    }
                }
            }
            if !diagnostics.is_empty() {
                errors.push((file.input.clone(), CompileError { diagnostics }));
            }
        }
        if !errors.is_empty() {
            return Err(compile_errors(errors));
        }
        Ok(self.result(None, &plan))
    }

    pub(crate) fn resolved_imports(&self, plan: &imports::ImportPlan) -> ResolvedImports {
        let paths = self
            .sources
            .iter()
            .filter_map(|file| {
                let output = file.output.as_ref()?;
                let id = output
                    .with_extension("")
                    .to_string_lossy()
                    .replace('\\', "/");
                Some((id, file.input.clone()))
            })
            .collect::<HashMap<_, _>>();
        plan.resolved
            .iter()
            .filter_map(|(importer, imports)| {
                let types = plan.import_types.get(importer)?;
                let resolved = imports
                    .iter()
                    .filter_map(|(specifier, id)| {
                        Some((
                            specifier.clone(),
                            ResolvedImport {
                                target_path: paths.get(id)?.clone(),
                                type_name: types.get(specifier)?.clone(),
                            },
                        ))
                    })
                    .collect::<HashMap<_, _>>();
                Some((importer.clone(), resolved))
            })
            .collect()
    }

    fn dependent_source_paths(
        &self,
        plan: &imports::ImportPlan,
        changed_paths: &BTreeSet<PathBuf>,
    ) -> BTreeSet<PathBuf> {
        let all_paths = self
            .sources
            .iter()
            .filter(|file| file.output.is_some())
            .map(|file| file.input.clone())
            .collect::<BTreeSet<_>>();
        if changed_paths.contains(&self.config_path)
            || self
                .declarations
                .iter()
                .any(|path| changed_paths.contains(path))
        {
            return all_paths;
        }
        let mut affected = changed_paths.clone();
        let mut pending = changed_paths.iter().cloned().collect::<Vec<_>>();
        while let Some(path) = pending.pop() {
            if let Some(dependents) = plan.dependents.get(&path) {
                for dependent in dependents {
                    if affected.insert(dependent.clone()) {
                        pending.push(dependent.clone());
                    }
                }
            }
        }
        affected.retain(|path| all_paths.contains(path));
        affected
    }

    pub fn build(&self) -> Result<BuildResult, BuildError> {
        Self::load(&self.config_path)?.build_with_session(&mut BuildSession::default())
    }

    #[cfg(test)]
    fn build_current(&self) -> Result<BuildResult, BuildError> {
        self.build_with_session(&mut BuildSession::default())
    }

    #[doc(hidden)]
    pub fn build_with_session(
        &self,
        session: &mut BuildSession,
    ) -> Result<BuildResult, BuildError> {
        let mut plan = self.analyze_imports_incremental(
            &HashMap::new(),
            None,
            None,
            session.previous.as_ref(),
            &mut session.declarations,
        )?;
        self.populate_declarations(&mut plan, &mut session.declarations)?;
        session.declarations.retain(|_, declarations| {
            plan.declaration_sets
                .values()
                .any(|set| std::sync::Arc::ptr_eq(set, declarations))
        });
        let (outputs, compiled) = self.compile_all(&plan, session)?;
        let mut package_ignores = Vec::new();
        for file in &self.sources {
            if self.package_sources.contains_key(&file.input)
                && plan.texts.contains_key(&file.input)
                && let Some(output) = &file.output
            {
                let path = self
                    .config
                    .compiler
                    .out_dir
                    .join(output)
                    .to_string_lossy()
                    .replace('\\', "/");
                if !package_ignores.contains(&path) {
                    package_ignores.push(path);
                }
            }
        }
        let manifest = if let Some(scripts) = &plan.scripts {
            manifest::render_manifest_with_scripts(
                &self.config,
                &self.root,
                Some(scripts),
                &package_ignores,
            )
            .map_err(|message| BuildError::Configuration {
                path: self.config_path.clone(),
                message,
            })?
        } else {
            self.manifest.clone()
        };
        let published = (self.output_dir.clone(), outputs, manifest);
        if session.published.as_ref() != Some(&published)
            || !self.output_dir.is_dir()
            || !self.root.join("fxmanifest.lua").is_file()
        {
            self.publish(&published.1, &published.2)?;
        }
        let result = self.result(Some(self.output_dir.clone()), &plan);
        session.published = Some(published);
        session.previous = Some(plan);
        session.compiled = compiled;
        Ok(result)
    }

    fn compile_all(
        &self,
        plan: &imports::ImportPlan,
        session: &BuildSession,
    ) -> Result<(Outputs, CompiledFiles), BuildError> {
        let mut outputs = Vec::new();
        let mut compiled = HashMap::new();
        let mut errors = Vec::new();
        for file in &self.sources {
            let Some(relative) = file
                .output
                .as_ref()
                .filter(|_| plan.parsed.contains_key(&file.input))
            else {
                continue;
            };
            let reused = session
                .previous
                .as_ref()
                .filter(|old| plan.same_compile_inputs(old, self, &file.input))
                .and_then(|_| session.compiled.get(&file.input))
                .filter(|(path, _)| path == relative);
            let result = match reused {
                Some((_, output)) => Ok(output.clone()),
                None => self.compile(file, plan.declarations_for(self, Some(&file.input)), plan),
            };
            match result {
                Ok(contents) => {
                    compiled.insert(file.input.clone(), (relative.clone(), contents.clone()));
                    outputs.push((relative.clone(), contents));
                }
                Err(BuildError::Compile { path, error }) => errors.push((path, error)),
                Err(BuildError::CompileMany(mut compile_errors)) => {
                    errors.append(&mut compile_errors)
                }
                Err(error) => return Err(error),
            }
        }
        if errors.is_empty() {
            if plan.scripts.is_some() {
                outputs.push((
                    PathBuf::from("_luat/runtime.lua"),
                    include_str!("runtime.lua").to_owned(),
                ));
                outputs.extend(plan.entries.iter().cloned());
            }
            Ok((outputs, compiled))
        } else {
            Err(compile_errors(errors))
        }
    }

    #[cfg(test)]
    fn read_declarations(&self) -> Result<DeclarationSet, BuildError> {
        self.read_declarations_with_overlays(&HashMap::new())
    }

    pub(crate) fn read_declarations_with_overlays(
        &self,
        overlays: &std::collections::HashMap<PathBuf, String>,
    ) -> Result<DeclarationSet, BuildError> {
        let mut checked = HashSet::new();
        for (path, root) in &self.package_declaration_roots {
            if !(self.package_root && root == &self.root) && checked.insert(root) {
                self.read_declarations_for(Some(path), overlays)?;
            }
        }
        self.read_declarations_for(None, overlays)
    }

    fn read_declarations_for(
        &self,
        path: Option<&Path>,
        overlays: &HashMap<PathBuf, String>,
    ) -> Result<DeclarationSet, BuildError> {
        self.parse_declarations(&self.declaration_inputs(path, overlays)?)
    }

    fn populate_declarations(
        &self,
        plan: &mut imports::ImportPlan,
        cache: &mut ScopedDeclarations,
    ) -> Result<(), BuildError> {
        for path in self
            .package_declaration_roots
            .keys()
            .chain(
                plan.parsed
                    .keys()
                    .filter(|path| self.package_sources.contains_key(*path)),
            )
            .map(|path| Some(path.as_path()))
            .chain(std::iter::once(None))
            .collect::<Vec<_>>()
        {
            let owner = self.declaration_owner(path);
            if !plan.declaration_sets.contains_key(&owner) {
                let declarations = self.cached_declarations(path, &plan.texts, cache)?;
                plan.declaration_sets.insert(owner, declarations);
            }
        }
        Ok(())
    }

    fn declaration_owner(&self, path: Option<&Path>) -> Option<PathBuf> {
        path.and_then(|path| {
            self.package_sources
                .get(path)
                .map(|source| &source.root)
                .or_else(|| self.package_declaration_roots.get(path))
        })
        .or_else(|| self.package_root.then_some(&self.root))
        .cloned()
    }

    fn cached_declarations(
        &self,
        path: Option<&Path>,
        texts: &HashMap<PathBuf, String>,
        cache: &mut ScopedDeclarations,
    ) -> Result<std::sync::Arc<DeclarationSet>, BuildError> {
        let inputs = self.declaration_inputs(path, texts)?;
        if let Some(set) = cache.get(&inputs) {
            return Ok(set.clone());
        }
        let set = std::sync::Arc::new(self.parse_declarations(&inputs)?);
        cache.insert(inputs, set.clone());
        Ok(set)
    }

    fn declaration_inputs(
        &self,
        path: Option<&Path>,
        overlays: &HashMap<PathBuf, String>,
    ) -> Result<DeclarationInputs, BuildError> {
        let package_scope = self.package_declaration_scope(path);
        self.declarations
            .iter()
            .filter(|path| {
                package_scope.as_ref().is_none_or(|roots| {
                    self.package_declaration_roots
                        .get(*path)
                        .is_some_and(|root| roots.contains(root))
                })
            })
            .map(|path| {
                let text = overlays
                    .get(path)
                    .cloned()
                    .map(Ok)
                    .unwrap_or_else(|| fs::read_to_string(path))
                    .map_err(|source| BuildError::Io {
                        path: path.clone(),
                        source,
                    })?;
                Ok((path.clone(), text))
            })
            .collect()
    }

    fn parse_declarations(&self, inputs: &DeclarationInputs) -> Result<DeclarationSet, BuildError> {
        let mut sources = vec![
            DeclarationSource {
                path: None,
                context: Some(crate::ExecutionContext::Client),
                text: include_str!("../../lsp/definitions/fivem-client.d.luat").to_owned(),
            },
            DeclarationSource {
                path: None,
                context: Some(crate::ExecutionContext::Server),
                text: include_str!("../../lsp/definitions/fivem-server.d.luat").to_owned(),
            },
            DeclarationSource {
                path: None,
                context: None,
                text: include_str!("../../lsp/definitions/fivem-shared.d.luat").to_owned(),
            },
            DeclarationSource {
                path: None,
                context: None,
                text: include_str!("../../lsp/definitions/runtime-shared.d.luat").to_owned(),
            },
            DeclarationSource {
                path: None,
                context: Some(crate::ExecutionContext::Client),
                text: include_str!("../../lsp/definitions/runtime-client.d.luat").to_owned(),
            },
            DeclarationSource {
                path: None,
                context: Some(crate::ExecutionContext::Server),
                text: include_str!("../../lsp/definitions/runtime-server.d.luat").to_owned(),
            },
        ];
        sources.extend(inputs.iter().map(|(path, text)| DeclarationSource {
            path: Some(path.clone()),
            context: None,
            text: text.clone(),
        }));
        DeclarationSet::from_sources(sources).map_err(|(path, error)| BuildError::Compile {
            path: path.unwrap_or_else(|| self.config_path.clone()),
            error,
        })
    }

    fn compile(
        &self,
        file: &SourceFile,
        declarations: &DeclarationSet,
        plan: &imports::ImportPlan,
    ) -> Result<String, BuildError> {
        let parsed = &plan.parsed[&file.input];
        let source = &parsed.source;
        let contexts = plan
            .execution_contexts
            .get(&file.input)
            .map(|contexts| contexts.iter().copied().map(Some).collect::<Vec<_>>())
            .filter(|contexts| !contexts.is_empty())
            .unwrap_or_else(|| vec![None]);
        let mut errors = Vec::new();
        let imports = plan.resolved.get(&file.input);
        let mut compiled = None;
        for context in contexts {
            let result = parsed.check(
                declarations,
                true,
                plan.contextual_import_types
                    .get(&(file.input.clone(), context)),
                context,
            );
            let result = result.and_then(|()| {
                compiled
                    .get_or_insert_with(|| crate::compiler::lower_parsed_source(parsed, imports))
                    .as_ref()
                    .map(|_| ())
                    .map_err(Clone::clone)
            });
            if let Err(error) = result {
                errors.push((file.input.clone(), error));
            }
        }
        if !errors.is_empty() {
            return Err(compile_errors(errors));
        }
        let compiled = if file
            .input
            .extension()
            .is_some_and(|extension| extension == "lua")
            && self.package_sources.contains_key(&file.input)
        {
            crate::compiler::imports::rewrite(
                source,
                plan.resolved.get(&file.input).unwrap_or(&HashMap::new()),
            )
            .map_err(|error| BuildError::Compile {
                path: file.input.clone(),
                error,
            })?
        } else {
            compiled
                .expect("checked at least one context")
                .expect("lowering succeeded")
        };
        let generated = if plan.wrapped.contains(&file.input) {
            let id = file
                .output
                .as_ref()
                .unwrap()
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            let generated = format!(
                "__luat.register({}, function(...)\n{compiled}\nend)\n",
                crate::compiler::imports::lua_string(&id)
            );
            generated
        } else {
            compiled
        };
        let output = crate::compiler::format_output(source, &generated).map_err(|error| {
            BuildError::Compile {
                path: file.input.clone(),
                error,
            }
        })?;
        Ok(output)
    }

    fn result(&self, output_dir: Option<PathBuf>, plan: &imports::ImportPlan) -> BuildResult {
        BuildResult {
            compiled_files: self
                .sources
                .iter()
                .filter(|file| file.output.is_some() && plan.texts.contains_key(&file.input))
                .count(),
            declaration_files: self
                .sources
                .iter()
                .filter(|file| file.output.is_none() && plan.texts.contains_key(&file.input))
                .count(),
            output_dir,
        }
    }

    fn publish(&self, outputs: &[(PathBuf, String)], manifest: &str) -> Result<(), BuildError> {
        let parent = self.output_dir.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| BuildError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let staging = unique_sibling(&self.output_dir, "staging")?;
        fs::create_dir(&staging).map_err(|source| BuildError::Io {
            path: staging.clone(),
            source,
        })?;

        let write_result = outputs.iter().try_for_each(|(relative, contents)| {
            let path = staging.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| BuildError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::write(&path, contents).map_err(|source| BuildError::Io { path, source })
        });
        if let Err(error) = write_result {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }

        let manifest_path = self.root.join("fxmanifest.lua");
        let manifest_temp = match unique_sibling(&manifest_path, "staging") {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        let mut manifest_file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest_temp)
        {
            Ok(file) => file,
            Err(source) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(BuildError::Io {
                    path: manifest_temp,
                    source,
                });
            }
        };
        if let Err(source) = manifest_file.write_all(manifest.as_bytes()) {
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_file(&manifest_temp);
            return Err(BuildError::Io {
                path: manifest_temp,
                source,
            });
        }
        drop(manifest_file);

        let output_backup = if self.output_dir.exists() {
            let backup = match unique_sibling(&self.output_dir, "backup") {
                Ok(path) => path,
                Err(error) => {
                    let _ = fs::remove_dir_all(&staging);
                    let _ = fs::remove_file(&manifest_temp);
                    return Err(error);
                }
            };
            if let Err(source) = fs::rename(&self.output_dir, &backup) {
                let _ = fs::remove_dir_all(&staging);
                let _ = fs::remove_file(&manifest_temp);
                return Err(BuildError::Io {
                    path: self.output_dir.clone(),
                    source,
                });
            }
            Some(backup)
        } else {
            None
        };

        let manifest_backup = if manifest_path.exists() {
            let manifest_backup_path = match unique_sibling(&manifest_path, "backup") {
                Ok(path) => path,
                Err(error) => {
                    let rollback = output_backup.as_ref().and_then(|previous| {
                        fs::rename(previous, &self.output_dir)
                            .err()
                            .map(|error| (previous, error))
                    });
                    let _ = fs::remove_dir_all(&staging);
                    let _ = fs::remove_file(&manifest_temp);
                    if let Some((previous, rollback_error)) = rollback {
                        return Err(BuildError::Configuration {
                            path: self.output_dir.clone(),
                            message: format!(
                                "could not prepare manifest backup ({error}); previous output remains at {} because rollback failed ({rollback_error})",
                                previous.display()
                            ),
                        });
                    }
                    return Err(error);
                }
            };
            if let Err(source) = fs::rename(&manifest_path, &manifest_backup_path) {
                let rollback = output_backup.as_ref().and_then(|previous| {
                    fs::rename(previous, &self.output_dir)
                        .err()
                        .map(|error| (previous, error))
                });
                let _ = fs::remove_dir_all(&staging);
                let _ = fs::remove_file(&manifest_temp);
                if let Some((previous, rollback_error)) = rollback {
                    return Err(BuildError::Configuration {
                        path: self.output_dir.clone(),
                        message: format!(
                            "could not stage manifest ({source}); previous output remains at {} because rollback failed ({rollback_error})",
                            previous.display()
                        ),
                    });
                }
                return Err(BuildError::Io {
                    path: manifest_path,
                    source,
                });
            }
            Some(manifest_backup_path)
        } else {
            None
        };

        if let Err(source) = fs::rename(&staging, &self.output_dir) {
            let manifest_rollback = manifest_backup.as_ref().and_then(|backup| {
                fs::rename(backup, &manifest_path)
                    .err()
                    .map(|error| (backup, error))
            });
            let output_rollback = output_backup.as_ref().and_then(|backup| {
                fs::rename(backup, &self.output_dir)
                    .err()
                    .map(|error| (backup, error))
            });
            if let Some((backup, rollback_error)) = manifest_rollback.or(output_rollback) {
                let _ = fs::remove_dir_all(&staging);
                let _ = fs::remove_file(&manifest_temp);
                return Err(BuildError::Configuration {
                    path: backup.clone(),
                    message: format!(
                        "could not publish output ({source}); previous data remains at {} because rollback failed ({rollback_error})",
                        backup.display()
                    ),
                });
            }
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_file(&manifest_temp);
            return Err(BuildError::Io {
                path: self.output_dir.clone(),
                source,
            });
        }
        if let Err(source) = fs::rename(&manifest_temp, &manifest_path) {
            let remove_error = fs::remove_dir_all(&self.output_dir).err();
            let output_rollback = output_backup.as_ref().and_then(|backup| {
                fs::rename(backup, &self.output_dir)
                    .err()
                    .map(|error| (backup, error))
            });
            let manifest_rollback = manifest_backup.as_ref().and_then(|backup| {
                fs::rename(backup, &manifest_path)
                    .err()
                    .map(|error| (backup, error))
            });
            let _ = fs::remove_file(&manifest_temp);
            if let Some((backup, rollback_error)) = output_rollback.or(manifest_rollback) {
                return Err(BuildError::Configuration {
                    path: backup.clone(),
                    message: format!(
                        "could not publish manifest ({source}); previous data remains at {} because rollback failed ({rollback_error})",
                        backup.display()
                    ),
                });
            }
            if let Some(remove_error) = remove_error {
                return Err(BuildError::Configuration {
                    path: self.output_dir.clone(),
                    message: format!(
                        "could not publish manifest ({source}) or remove the new output during rollback ({remove_error})"
                    ),
                });
            }
            return Err(BuildError::Io {
                path: manifest_path,
                source,
            });
        }
        if let Some(backup) = output_backup
            && let Err(error) = fs::remove_dir_all(&backup)
        {
            crate::output::warning(&format!(
                "Build succeeded, but the previous output backup remains at {} ({error}).",
                backup.display()
            ));
        }
        if let Some(backup) = manifest_backup
            && let Err(error) = fs::remove_file(&backup)
        {
            crate::output::warning(&format!(
                "Build succeeded, but the previous manifest backup remains at {} ({error}).",
                backup.display()
            ));
        }
        Ok(())
    }
}

fn compile_errors(mut errors: Vec<(PathBuf, CompileError)>) -> BuildError {
    if errors.len() == 1 {
        let (path, error) = errors.pop().expect("length checked");
        BuildError::Compile { path, error }
    } else {
        BuildError::CompileMany(errors)
    }
}

fn output_path(source: &Path, config: &Path) -> Result<PathBuf, BuildError> {
    if source
        .extension()
        .is_none_or(|extension| extension != "luat")
    {
        return Err(BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!("source must use the .luat extension: {}", source.display()),
        });
    }
    let relative = source
        .strip_prefix("src")
        .map_err(|_| BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!("source must be inside src/: {}", source.display()),
        })?;
    let mut output = relative.to_path_buf();
    output.set_extension("lua");
    Ok(output)
}

fn collect_luat(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "symlink or junction is not allowed under src/: {}",
                    entry.path().display()
                ),
            ));
        }
        if kind.is_dir() {
            collect_luat(&entry.path(), files)?;
        } else if kind.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "luat")
        {
            files.push(entry.path());
        }
    }
    Ok(())
}

fn validate_project_path(path: &Path, config: &Path) -> Result<(), BuildError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err(BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!(
                "source path must stay inside the project: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

fn reject_symlink_path(root: &Path, path: &Path, config: &Path) -> Result<(), BuildError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!("path must stay inside the project: {}", path.display()),
        })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(BuildError::Configuration {
                    path: config.to_path_buf(),
                    message: format!(
                        "symlink or junction is not allowed in project paths: {}",
                        current.display()
                    ),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(source) => {
                return Err(BuildError::Io {
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn unique_sibling(target: &Path, kind: &str) -> Result<PathBuf, BuildError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    for suffix in 0..1000 {
        let path = parent.join(format!(".{name}.{kind}.{}.{suffix}", std::process::id()));
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(path),
            Ok(_) | Err(_) => continue,
        }
    }
    Err(BuildError::Configuration {
        path: target.to_path_buf(),
        message: "could not allocate a temporary output directory".to_owned(),
    })
}

#[cfg(test)]
mod tests;

impl imports::ImportPlan {
    fn declarations_for(
        &self,
        project: &Project,
        path: Option<&Path>,
    ) -> &std::sync::Arc<DeclarationSet> {
        &self.declaration_sets[&project.declaration_owner(path)]
    }

    fn same_compile_inputs(&self, old: &Self, project: &Project, path: &Path) -> bool {
        let owner = project.declaration_owner(Some(path));
        old.parsed
            .get(path)
            .is_some_and(|parsed| std::sync::Arc::ptr_eq(parsed, &self.parsed[path]))
            && old
                .declaration_sets
                .get(&owner)
                .is_some_and(|set| std::sync::Arc::ptr_eq(set, &self.declaration_sets[&owner]))
            && self.resolved.get(path) == old.resolved.get(path)
            && self.wrapped.contains(path) == old.wrapped.contains(path)
            && self.execution_contexts.get(path) == old.execution_contexts.get(path)
            && [
                None,
                Some(crate::ExecutionContext::Client),
                Some(crate::ExecutionContext::Server),
            ]
            .into_iter()
            .all(|context| {
                let key = (path.to_path_buf(), context);
                self.contextual_import_types.get(&key) == old.contextual_import_types.get(&key)
            })
    }
}
