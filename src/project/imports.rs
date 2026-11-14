use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::*;
use crate::ExecutionContext;
use crate::compiler::{
    collect_imports,
    imports::{ImportCall, lua_string},
};

pub(crate) type SourceParser<'a> =
    dyn Fn(&Path, &str) -> Result<std::sync::Arc<crate::compiler::ParsedSource>, BuildError> + 'a;

#[derive(Clone, Default)]
pub(crate) struct ImportPlan {
    pub texts: HashMap<PathBuf, String>,
    pub resolved: HashMap<PathBuf, HashMap<String, String>>,
    pub import_types: HashMap<PathBuf, HashMap<String, String>>,
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

fn resolve(
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

impl Project {
    pub(super) fn analyze_imports(&self) -> Result<ImportPlan, BuildError> {
        self.analyze_imports_with_overlays(&HashMap::new())
    }

    pub(super) fn analyze_imports_with_overlays(
        &self,
        overlays: &HashMap<PathBuf, String>,
    ) -> Result<ImportPlan, BuildError> {
        self.analyze_imports_with_parser(overlays, None, None)
    }

    pub(crate) fn analyze_imports_with_parser(
        &self,
        overlays: &HashMap<PathBuf, String>,
        parse: Option<&SourceParser<'_>>,
        previous: Option<&ImportPlan>,
    ) -> Result<ImportPlan, BuildError> {
        if let (Some(parse), Some(old)) = (parse, previous) {
            let mut plan = old.clone();
            let mut same_graph = true;
            for (path, old_calls) in &old.calls {
                let Some(source) = overlays.get(path) else {
                    same_graph = false;
                    break;
                };
                if old.texts.get(path) == Some(source) {
                    continue;
                }
                let calls =
                    parse(path, source)?
                        .import_calls()
                        .map_err(|error| BuildError::Compile {
                            path: path.clone(),
                            error,
                        })?;
                if !calls
                    .iter()
                    .map(|call| &call.specifier)
                    .eq(old_calls.iter().map(|call| &call.specifier))
                {
                    same_graph = false;
                    break;
                }
                plan.calls.insert(path.clone(), calls);
            }
            if same_graph {
                for (path, source) in &mut plan.texts {
                    if let Some(current) = overlays.get(path) {
                        *source = current.clone();
                    }
                }
                let targets = self.resolved_imports(old);
                for path in &old.order {
                    let imports = targets
                        .get(path)
                        .into_iter()
                        .flatten()
                        .map(|(specifier, target)| {
                            (
                                specifier.clone(),
                                plan.exports
                                    .get(&target.target_path)
                                    .cloned()
                                    .unwrap_or_else(|| "any".into()),
                            )
                        })
                        .collect::<HashMap<_, _>>();
                    if old.texts.get(path) != plan.texts.get(path)
                        || old.import_types.get(path) != Some(&imports)
                    {
                        let parsed = parse(path, &plan.texts[path])?;
                        plan.exports
                            .insert(path.clone(), parsed.export_type(&imports));
                    }
                    plan.import_types.insert(path.clone(), imports);
                }
                return Ok(plan);
            }
        }
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
            }
        }
        plan.originals = originals.clone();
        let mut pending = originals
            .iter()
            .flatten()
            .filter_map(|path| {
                path.strip_prefix("src/")
                    .and_then(|path| path.strip_suffix(".luat"))
                    .map(str::to_owned)
            })
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
                        path.strip_prefix("src/")
                            .and_then(|path| path.strip_suffix(".luat"))
                            .map(|id| {
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
            let source = match overlays.get(&path) {
                Some(source) => source.clone(),
                None => fs::read_to_string(&path).map_err(|source| BuildError::Io {
                    path: path.clone(),
                    source,
                })?,
            };
            let calls = (if let Some(parse) = parse {
                let parsed = parse(&path, &source)?;
                parsed.import_calls()
            } else {
                collect_imports(&source)
            })
            .map_err(|error| BuildError::Compile {
                path: path.clone(),
                error,
            })?;
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
                let target = resolve(&module, call, &modules, &self.root)?;
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
                .unwrap()
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
        if modules.values().all(|module| module.calls.is_empty()) {
            return Ok(plan);
        }
        let mut complete = HashSet::new();
        let mut dependency_order = Vec::with_capacity(modules.len());
        for id in modules.keys() {
            visit_cycle(id, &modules, &mut complete, &mut dependency_order)?;
        }

        let mut export_types = HashMap::<String, String>::new();
        for id in dependency_order {
            let module = &modules[&id];
            plan.order.push(module.path.clone());
            let import_types = module
                .edges
                .iter()
                .map(|(call, target)| (call.specifier.clone(), export_types[target].clone()))
                .collect::<HashMap<_, _>>();
            let export_type = if let Some(parse) = parse {
                let parsed = parse(&module.path, &module.source)?;
                parsed.export_type(&import_types)
            } else {
                crate::compiler::infer_module_export_type(&module.source, &import_types)
            };
            plan.import_types.insert(module.path.clone(), import_types);
            plan.exports
                .insert(module.path.clone(), export_type.clone());
            export_types.insert(id.clone(), export_type);
        }

        let mut declared = HashMap::<String, u8>::new();
        for (side, original_paths) in originals.iter().enumerate() {
            for path in original_paths {
                if let Some(id) = path
                    .strip_prefix("src/")
                    .and_then(|path| path.strip_suffix(".luat"))
                {
                    *declared.entry(id.to_owned()).or_default() |= 1 << side;
                }
            }
        }
        let mut closures: [BTreeSet<String>; 3] = Default::default();
        for (side, original_paths) in originals.iter().enumerate() {
            for path in original_paths {
                let Some(id) = path
                    .strip_prefix("src/")
                    .and_then(|path| path.strip_suffix(".luat"))
                else {
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
            for id in &closures[side] {
                if plan.wrapped.contains(&modules[id].path)
                    && (side == 0 || !closures[0].contains(id))
                {
                    scripts[side].push(output_path_for(id, out_dir));
                }
            }
            for path in original_paths {
                if let Some(id) = path
                    .strip_prefix("src/")
                    .and_then(|path| path.strip_suffix(".luat"))
                {
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
                    scripts[side].push(path.clone());
                }
            }
        }
        plan.scripts = Some(scripts);
        Ok(plan)
    }
}
