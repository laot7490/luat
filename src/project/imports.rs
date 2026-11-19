use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use super::*;
use crate::ExecutionContext;
use crate::compiler::imports::{ImportCall, lua_string};

pub(crate) type SourceParser<'a> =
    dyn Fn(&Path, &str) -> Result<std::sync::Arc<crate::compiler::ParsedSource>, BuildError> + 'a;

#[derive(Clone, Default)]
pub(crate) struct ImportPlan {
    pub(super) declaration_sets: HashMap<Option<PathBuf>, Arc<DeclarationSet>>,
    contextual_exports: HashMap<(PathBuf, Option<ExecutionContext>), String>,
    pub texts: HashMap<PathBuf, String>,
    pub parsed: HashMap<PathBuf, std::sync::Arc<crate::compiler::ParsedSource>>,
    pub resolved: HashMap<PathBuf, HashMap<String, String>>,
    pub import_types: HashMap<PathBuf, HashMap<String, String>>,
    pub contextual_import_types:
        HashMap<(PathBuf, Option<ExecutionContext>), HashMap<String, String>>,
    pub wrapped: HashSet<PathBuf>,
    pub scripts: Option<[Vec<String>; 3]>,
    pub entries: Vec<(PathBuf, String)>,
    pub execution_contexts: HashMap<PathBuf, Vec<ExecutionContext>>,
    pub exports: HashMap<PathBuf, String>,
    pub calls: HashMap<PathBuf, Vec<ImportCall>>,
    pub order: Vec<PathBuf>,
    pub originals: [Vec<String>; 3],
    pub dependents: HashMap<PathBuf, BTreeSet<PathBuf>>,
}

struct Module {
    path: PathBuf,
    source: String,
    calls: Vec<ImportCall>,
    edges: Vec<(ImportCall, String)>,
}

fn diagnostic(
    module: &Module,
    call: Option<&ImportCall>,
    code: &'static str,
    message: String,
) -> BuildError {
    let span = call.map(|call| call.source_range.clone()).unwrap_or(0..0);
    BuildError::Compile {
        path: module.path.clone(),
        error: CompileError {
            diagnostics: vec![Diagnostic::range(
                &module.source,
                span.start,
                span.end,
                code,
                message,
            )],
        },
    }
}

fn resolve_project(
    module: &Module,
    call: &ImportCall,
    modules: &BTreeMap<String, Module>,
    root: &Path,
) -> Result<String, BuildError> {
    let specifier = &call.specifier;
    let invalid = || {
        diagnostic(
            module,
            Some(call),
            "LT4002",
            format!(
                "Invalid import path '{specifier}'; use ./, ../ or @/ within src/ and target a .luat module"
            ),
        )
    };
    if specifier.contains(['\\', '\0', '\r', '\n', ':']) || specifier.ends_with('/') {
        return Err(invalid());
    }
    let (base, rest) = if let Some(rest) = specifier.strip_prefix("@/") {
        (Vec::new(), rest)
    } else if specifier.starts_with("./") || specifier.starts_with("../") {
        let relative = module.path.strip_prefix(root.join("src")).unwrap();
        (
            relative
                .parent()
                .unwrap()
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            specifier.as_str(),
        )
    } else {
        return Err(invalid());
    };
    let mut parts = base;
    for part in rest.split('/') {
        match part {
            "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(invalid());
                }
            }
            "" => return Err(invalid()),
            _ => parts.push(part.to_owned()),
        }
    }
    let path = parts.join("/");
    if path.is_empty() || path.ends_with(".d.luat") || path.split('/').next() == Some("_luat") {
        return Err(invalid());
    }
    let explicit = path.ends_with(".luat");
    if !explicit && Path::new(&path).extension().is_some() {
        return Err(invalid());
    }
    let candidates = if explicit {
        vec![path.strip_suffix(".luat").unwrap().to_owned()]
    } else {
        vec![path.clone(), format!("{path}/init")]
    };
    let found = candidates
        .into_iter()
        .filter(|id| modules.contains_key(id))
        .collect::<Vec<_>>();
    match found.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(diagnostic(
            module,
            Some(call),
            "LT4003",
            format!("Import '{specifier}' was not found with exact filename case under src/"),
        )),
        _ => Err(diagnostic(
            module,
            Some(call),
            "LT4004",
            format!(
                "Import '{specifier}' is ambiguous: both {path}.luat and {path}/init.luat exist"
            ),
        )),
    }
}

fn visit_cycle<'a>(
    id: &'a str,
    modules: &'a BTreeMap<String, Module>,
    complete: &mut HashSet<String>,
    order: &mut Vec<String>,
) -> Result<(), BuildError> {
    if complete.contains(id) {
        return Ok(());
    }
    let mut stack = vec![(id, 0)];
    let mut active = HashMap::from([(id, 0)]);
    while let Some(&(current, next)) = stack.last() {
        let module = &modules[current];
        if next == module.edges.len() {
            complete.insert(current.to_owned());
            order.push(current.to_owned());
            active.remove(current);
            stack.pop();
            continue;
        }
        stack.last_mut().unwrap().1 += 1;
        let (call, target) = &module.edges[next];
        if let Some(&start) = active.get(target.as_str()) {
            let mut cycle = stack[start..]
                .iter()
                .map(|(id, _)| (*id).to_owned())
                .collect::<Vec<_>>();
            cycle.push(target.clone());
            return Err(diagnostic(
                module,
                Some(call),
                "LT4005",
                format!("Import cycle: {}", cycle.join(" -> ")),
            ));
        }
        if !complete.contains(target) {
            active.insert(target.as_str(), stack.len());
            stack.push((target.as_str(), 0));
        }
    }
    Ok(())
}

fn side_name(side: usize) -> &'static str {
    ["shared", "client", "server"][side]
}

fn collect_side<'a>(
    id: &'a str,
    side: usize,
    modules: &'a BTreeMap<String, Module>,
    declared: &HashMap<String, u8>,
    collected: &mut BTreeSet<String>,
) -> Result<(), BuildError> {
    if !collected.insert(id.to_owned()) {
        return Ok(());
    }
    let mut stack = vec![(id, 0)];
    while let Some(&(current, next)) = stack.last() {
        let module = &modules[current];
        if next == module.edges.len() {
            stack.pop();
            continue;
        }
        stack.last_mut().unwrap().1 += 1;
        let (call, target) = &module.edges[next];
        let directory = target.split('/').next().unwrap();
        let private_violation = match directory {
            "server" => side != 2,
            "client" => side != 1,
            _ => false,
        };
        let declared_violation = declared.get(target).is_some_and(|mask| {
            let allowed = if side == 0 { 1 } else { 1 | (1 << side) };
            mask & allowed == 0
        });
        if private_violation || declared_violation {
            let mut path = stack
                .iter()
                .map(|(id, _)| (*id).to_owned())
                .collect::<Vec<_>>();
            path.push(target.clone());
            return Err(diagnostic(
                module,
                Some(call),
                "LT4006",
                format!(
                    "{} entry cannot import '{target}': {}",
                    side_name(side),
                    path.join(" -> ")
                ),
            ));
        }
        if collected.insert(target.clone()) {
            stack.push((target.as_str(), 0));
        }
    }
    Ok(())
}

fn output_path_for(id: &str, out_dir: &Path) -> String {
    out_dir
        .join(format!("{id}.lua"))
        .to_string_lossy()
        .replace('\\', "/")
}

fn ordered_entries<'a>(
    paths: &'a [String],
    modules: &BTreeMap<String, Module>,
) -> Result<Vec<&'a str>, BuildError> {
    let explicit: HashMap<_, _> = paths
        .iter()
        .filter_map(|path| entry_id(path).map(|id| (id, path.as_str())))
        .collect();
    let mut complete = HashSet::new();
    let mut dependencies = Vec::new();
    let mut entries = Vec::new();
    for path in paths {
        if let Some(id) = entry_id(path).filter(|id| modules.contains_key(*id)) {
            visit_cycle(id, modules, &mut complete, &mut dependencies)?;
            entries.extend(
                dependencies
                    .drain(..)
                    .filter_map(|id| explicit.get(id.as_str()).copied()),
            );
        } else {
            entries.push(path);
        }
    }
    Ok(entries)
}

impl Project {
    #[cfg(test)]
    pub(super) fn analyze_imports(&self) -> Result<ImportPlan, BuildError> {
        self.analyze_imports_with_overlays(&HashMap::new())
    }

    #[cfg(test)]
    pub(super) fn analyze_imports_with_overlays(
        &self,
        overlays: &HashMap<PathBuf, String>,
    ) -> Result<ImportPlan, BuildError> {
        self.analyze_imports_incremental(
            overlays,
            None,
            None,
            None,
            &mut HashMap::new(),
            &mut |_| {},
        )
    }

    pub(crate) fn analyze_imports_with_parser(
        &self,
        overlays: &HashMap<PathBuf, String>,
        parse: Option<&SourceParser<'_>>,
        previous: Option<&ImportPlan>,
        declarations: &mut ScopedDeclarations,
    ) -> Result<ImportPlan, BuildError> {
        self.analyze_imports_incremental(
            overlays,
            parse,
            previous,
            previous,
            declarations,
            &mut |_| {},
        )
    }

    pub(super) fn analyze_imports_incremental(
        &self,
        overlays: &HashMap<PathBuf, String>,
        parse: Option<&SourceParser<'_>>,
        previous: Option<&ImportPlan>,
        reuse: Option<&ImportPlan>,
        declarations: &mut ScopedDeclarations,
        progress: &mut dyn FnMut(&Path),
    ) -> Result<ImportPlan, BuildError> {
        let mut modules = BTreeMap::new();
        let mut plan = ImportPlan::default();
        let mut paths = HashMap::new();
        for file in &self.sources {
            if file.output.is_none() {
                let source = match overlays.get(&file.input) {
                    Some(source) => source.clone(),
                    None => fs::read_to_string(&file.input).map_err(|source| BuildError::Io {
                        path: file.input.clone(),
                        source,
                    })?,
                };
                plan.texts.insert(file.input.clone(), source);
                continue;
            }
            let id = file
                .output
                .as_ref()
                .unwrap()
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            paths.insert(id, file.input.clone());
        }
        for (id, path) in &paths {
            modules.insert(
                id.clone(),
                Module {
                    path: path.clone(),
                    source: String::new(),
                    calls: Vec::new(),
                    edges: Vec::new(),
                },
            );
        }
        let f = &self.config.fivem;
        let mut originals: [Vec<String>; 3] = previous
            .map(|plan| plan.originals.clone())
            .unwrap_or_default();
        if previous.is_none() {
            for (side, entries) in [&f.shared, &f.client, &f.server].into_iter().enumerate() {
                originals[side] =
                    manifest::expanded_paths(entries, &self.root, false).map_err(|message| {
                        BuildError::Configuration {
                            path: self.config_path.clone(),
                            message,
                        }
                    })?;
                originals[side].retain(|path| !manifest::is_declaration_path(path));
            }
        }
        if self.package_root {
            for (path, source) in &self.package_sources {
                if source.root != self.root {
                    continue;
                }
                for (side, entries) in originals.iter_mut().enumerate() {
                    if source.sides & (1 << side) != 0 {
                        entries.push(format!("@module:{}", self.module_id(path).unwrap()));
                    }
                }
            }
            for entries in &mut originals {
                entries.sort();
                entries.dedup();
            }
        }
        plan.originals = originals.clone();
        let mut pending = originals
            .iter()
            .flatten()
            .filter_map(|path| entry_id(path).map(str::to_owned))
            .collect::<Vec<_>>();
        let roots = originals
            .iter()
            .enumerate()
            .flat_map(|(side, paths)| {
                let contexts: &[ExecutionContext] = match side {
                    0 => &[ExecutionContext::Client, ExecutionContext::Server],
                    1 => &[ExecutionContext::Client],
                    _ => &[ExecutionContext::Server],
                };
                paths
                    .iter()
                    .filter_map(|path| {
                        entry_id(path).map(|id| {
                            contexts
                                .iter()
                                .map(move |context| (id.to_owned(), *context))
                        })
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let mut visited = HashSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            let Some(path) = paths.get(&id).cloned() else {
                continue;
            };
            progress(&path);
            let source = match overlays.get(&path) {
                Some(source) => source.clone(),
                None => fs::read_to_string(&path).map_err(|source| BuildError::Io {
                    path: path.clone(),
                    source,
                })?,
            };
            let parsed = if let Some(parse) = parse {
                parse(&path, &source)?
            } else {
                reuse
                    .and_then(|old| old.parsed.get(&path))
                    .filter(|old| old.source == source)
                    .cloned()
                    .unwrap_or_else(|| {
                        Arc::new(crate::compiler::ParsedSource::new_prepared(
                            &source,
                            crate::compiler::syntax::prepare(&source),
                        ))
                    })
            };
            self.validate_package_return(&path, &parsed)?;
            let calls = if parse.is_some() {
                parsed.editor_import_calls()
            } else {
                parsed.import_calls()
            }
            .map_err(|error| BuildError::Compile {
                path: path.clone(),
                error,
            })?;
            plan.parsed
                .insert(path.clone(), std::sync::Arc::clone(&parsed));
            plan.texts.insert(path.clone(), source.clone());
            plan.calls.insert(path.clone(), calls.clone());
            let mut module = Module {
                path: path.clone(),
                source,
                calls,
                edges: Vec::new(),
            };
            let mut resolved = HashMap::new();
            let mut dependencies = Vec::new();
            for call in &module.calls {
                let target = self.resolve_module(&module, call, &modules)?;
                resolved.insert(call.specifier.clone(), target.clone());
                plan.wrapped.insert(module.path.clone());
                plan.wrapped.insert(modules[&target].path.clone());
                dependencies.push((call.clone(), target));
            }
            plan.resolved.insert(module.path.clone(), resolved);
            pending.extend(dependencies.iter().map(|(_, target)| target.clone()));
            module.edges = dependencies;
            modules.insert(id, module);
        }
        modules.retain(|id, _| visited.contains(id));
        for module in modules.values() {
            for (_, target) in &module.edges {
                plan.dependents
                    .entry(modules[target].path.clone())
                    .or_default()
                    .insert(module.path.clone());
            }
        }
        let mut context_stack = roots;
        let mut visited_contexts = HashSet::new();
        while let Some((id, context)) = context_stack.pop() {
            if !visited_contexts.insert((id.clone(), context)) {
                continue;
            }
            let Some(module) = modules.get(&id) else {
                continue;
            };
            let contexts = plan
                .execution_contexts
                .entry(module.path.clone())
                .or_default();
            if !contexts.contains(&context) {
                contexts.push(context);
            }
            context_stack.extend(
                module
                    .edges
                    .iter()
                    .map(|(_, target)| (target.clone(), context)),
            );
        }
        for contexts in plan.execution_contexts.values_mut() {
            contexts.sort_by_key(|context| match context {
                ExecutionContext::Client => 0,
                ExecutionContext::Server => 1,
            });
        }
        for module in modules.values() {
            if module
                .path
                .strip_prefix(self.root.join("src"))
                .unwrap_or(Path::new(""))
                .components()
                .next()
                .is_some_and(|component| {
                    component
                        .as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case("_luat")
                })
            {
                return Err(diagnostic(
                    module,
                    None,
                    "LT4002",
                    "src/_luat/ is reserved for generated runtime files".to_owned(),
                ));
            }
        }
        if !self.package_root && modules.values().all(|module| module.calls.is_empty()) {
            return Ok(plan);
        }
        let mut complete = HashSet::new();
        let mut dependency_order = Vec::with_capacity(modules.len());
        for id in modules.keys() {
            visit_cycle(id, &modules, &mut complete, &mut dependency_order)?;
        }

        let mut export_types = HashMap::<(String, Option<ExecutionContext>), String>::new();

        for id in dependency_order {
            let module = &modules[&id];
            plan.order.push(module.path.clone());
            let owner_root = self.declaration_owner(Some(&module.path));
            if !plan.declaration_sets.contains_key(&owner_root) {
                let set =
                    self.cached_declarations(Some(&module.path), &plan.texts, declarations)?;
                plan.declaration_sets.insert(owner_root.clone(), set);
            }
            let declarations = &plan.declaration_sets[&owner_root];
            let contexts = plan
                .execution_contexts
                .get(&module.path)
                .map(|contexts| contexts.iter().copied().map(Some).collect::<Vec<_>>())
                .filter(|contexts| !contexts.is_empty())
                .unwrap_or_else(|| vec![None]);
            let parsed = std::sync::Arc::clone(&plan.parsed[&module.path]);
            let mut exports = Vec::new();
            let mut imports = HashMap::<String, Vec<String>>::new();
            for context in contexts {
                let import_types = module
                    .edges
                    .iter()
                    .map(|(call, target)| {
                        (
                            call.specifier.clone(),
                            export_types[&(target.clone(), context)].clone(),
                        )
                    })
                    .collect::<HashMap<String, String>>();
                let key = (module.path.clone(), context);
                let export_type = reuse
                    .filter(|old| {
                        old.parsed
                            .get(&module.path)
                            .is_some_and(|old| Arc::ptr_eq(old, &parsed))
                            && old
                                .declaration_sets
                                .get(&owner_root)
                                .is_some_and(|old| Arc::ptr_eq(old, declarations))
                            && old.contextual_import_types.get(&key) == Some(&import_types)
                    })
                    .and_then(|old| old.contextual_exports.get(&key))
                    .cloned()
                    .unwrap_or_else(|| {
                        parsed.contextual_export_type(&import_types, declarations, context)
                    });
                plan.contextual_exports.insert(key, export_type.clone());
                for (specifier, ty) in &import_types {
                    imports
                        .entry(specifier.clone())
                        .or_default()
                        .push(ty.clone());
                }
                plan.contextual_import_types
                    .insert((module.path.clone(), context), import_types);
                exports.push(export_type.clone());
                export_types.insert((id.clone(), context), export_type);
            }
            let union = |mut types: Vec<String>| {
                types.sort();
                types.dedup();
                if types.len() == 1 {
                    types.pop().unwrap()
                } else {
                    types
                        .into_iter()
                        .map(|ty| format!("({ty})"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                }
            };
            plan.import_types.insert(
                module.path.clone(),
                imports
                    .into_iter()
                    .map(|(specifier, types)| (specifier, union(types)))
                    .collect(),
            );
            plan.exports.insert(module.path.clone(), union(exports));
        }

        let mut declared = HashMap::<String, u8>::new();
        for (path, source) in &self.package_sources {
            declared.insert(self.module_id(path).unwrap(), source.sides);
        }
        for (side, original_paths) in originals.iter().enumerate() {
            for path in original_paths {
                if self.package_root {
                    continue;
                }
                if let Some(id) = entry_id(path) {
                    *declared.entry(id.to_owned()).or_default() |= 1 << side;
                }
            }
        }
        let mut closures: [BTreeSet<String>; 3] = Default::default();
        for (side, original_paths) in originals.iter().enumerate() {
            for path in original_paths {
                let Some(id) = entry_id(path) else {
                    continue;
                };
                let Some(module) = modules.get(id) else {
                    continue;
                };
                let private = id.split('/').next().unwrap();
                if private == "server" && side != 2 || private == "client" && side != 1 {
                    return Err(diagnostic(
                        module,
                        None,
                        "LT4006",
                        format!("{} entry cannot execute '{id}'", side_name(side)),
                    ));
                }
                collect_side(id, side, &modules, &declared, &mut closures[side])?;
            }
        }
        let out_dir = &self.config.compiler.out_dir;
        let mut scripts: [Vec<String>; 3] = Default::default();
        scripts[0].push(output_path_for("_luat/runtime", out_dir));
        for (side, original_paths) in originals.iter().enumerate() {
            // Package roots are analysis roots; only imports should become runtime entries.
            if self.package_root {
                continue;
            }
            for id in &closures[side] {
                if plan.wrapped.contains(&modules[id].path)
                    && (side == 0 || !closures[0].contains(id))
                {
                    scripts[side].push(output_path_for(id, out_dir));
                }
            }
            for path in ordered_entries(original_paths, &modules)? {
                if let Some(id) = entry_id(path) {
                    if modules
                        .get(id)
                        .is_some_and(|module| plan.wrapped.contains(&module.path))
                    {
                        let stub = format!("_luat/entries/{id}");
                        scripts[side].push(output_path_for(&stub, out_dir));
                        let entry = (
                            PathBuf::from(format!("{stub}.lua")),
                            format!("__luat.import({})\n", lua_string(id)),
                        );
                        if !plan.entries.iter().any(|(path, _)| *path == entry.0) {
                            plan.entries.push(entry);
                        }
                    } else {
                        scripts[side].push(output_path_for(id, out_dir));
                    }
                } else {
                    scripts[side].push(path.to_owned());
                }
            }
        }
        plan.scripts = Some(scripts);
        Ok(plan)
    }
}

fn entry_id(path: &str) -> Option<&str> {
    path.strip_prefix("@module:")
        .or_else(|| path.strip_prefix("src/")?.strip_suffix(".luat"))
}

impl Project {
    fn resolve_module(
        &self,
        module: &Module,
        call: &ImportCall,
        modules: &BTreeMap<String, Module>,
    ) -> Result<String, BuildError> {
        let specifier = &call.specifier;
        let owner = self.package_sources.get(&module.path);
        let local = specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier.starts_with("@/");
        if owner.is_none() && local {
            return resolve_project(module, call, modules, &self.root);
        }
        let bad = |message: String| diagnostic(module, Some(call), "LT4002", message);
        if specifier.contains(['\\', '\0', '\r', '\n', ':']) || specifier.ends_with('/') {
            return Err(bad(format!("Invalid package import '{specifier}'")));
        }
        let (name, root, path) = if local {
            let owner = owner.unwrap();
            let relative = module.path.strip_prefix(&owner.root).unwrap();
            let path = if let Some(rest) = specifier.strip_prefix("@/") {
                PathBuf::from(rest)
            } else {
                relative.parent().unwrap().join(specifier)
            };
            (owner.manifest.name.clone(), owner.root.clone(), path)
        } else {
            let (name, rest) = specifier.split_once('/').unwrap_or((specifier, "init"));
            let dependencies = owner
                .map(|owner| &owner.manifest.dependencies)
                .unwrap_or(&self.config.dependencies);
            if !dependencies.contains_key(name) {
                return Err(bad(format!("Package '{name}' is not a direct dependency")));
            }
            let Some(target) = self
                .package_sources
                .values()
                .find(|source| source.manifest.name == name)
            else {
                return Err(bad(format!("Package '{name}' has no runtime modules")));
            };
            (name.to_owned(), target.root.clone(), PathBuf::from(rest))
        };
        let _ = root;
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(part) => {
                    parts.push(part.to_string_lossy().into_owned())
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if parts.pop().is_none() {
                        return Err(bad("Import escapes package root".into()));
                    }
                }
                _ => return Err(bad("Invalid package import path".into())),
            }
        }
        let relative = parts.join("/");
        if relative.ends_with(".d.luat") || relative.is_empty() {
            return Err(bad("Import must target a runtime module".into()));
        }
        let prefix = format!("_luat/packages/{name}/");
        let candidates = if relative.ends_with(".lua") || relative.ends_with(".luat") {
            vec![relative.clone()]
        } else if Path::new(&relative).extension().is_some() {
            return Err(bad("Unsupported module extension".into()));
        } else {
            vec![
                format!("{relative}.luat"),
                format!("{relative}.lua"),
                format!("{relative}/init.luat"),
                format!("{relative}/init.lua"),
            ]
        };
        let found = candidates
            .into_iter()
            .map(|path| format!("{prefix}{path}"))
            .filter(|id| modules.contains_key(id))
            .collect::<Vec<_>>();
        match found.as_slice() {
            [id] => Ok(id.clone()),
            [] => Err(diagnostic(
                module,
                Some(call),
                "LT4003",
                format!("Package module '{specifier}' was not found"),
            )),
            _ => Err(diagnostic(
                module,
                Some(call),
                "LT4004",
                format!("Package import '{specifier}' is ambiguous"),
            )),
        }
    }
}
