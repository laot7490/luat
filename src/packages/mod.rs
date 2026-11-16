use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{IsTerminal, Read, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use toml_edit::{DocumentMut, value};

pub mod author;
use catalog::PackageIndex;
mod catalog;

pub(crate) use catalog::{
    PackageManifest, classified_files, manifest_files, read_manifest, require_versions,
};

#[derive(Clone, Debug)]
pub(crate) struct InstalledPackage {
    pub root: PathBuf,
    pub manifest: PackageManifest,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockFile {
    schema: u32,
    packages: BTreeMap<String, LockedPackage>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LockedPackage {
    version: String,
    commit: String,
    files: BTreeMap<String, String>,
}

pub(crate) fn installed_packages(
    root: &Path,
    dependencies: &BTreeMap<String, String>,
) -> Result<Vec<InstalledPackage>, String> {
    if dependencies.is_empty() {
        return Ok(Vec::new());
    }
    let path = root.join("luat.lock");
    let lock: LockFile = toml::from_str(
        &fs::read_to_string(&path)
            .map_err(|_| "packages are not locked; run `luat install`".to_owned())?,
    )
    .map_err(|e| format!("error LT5301: Invalid luat.lock: {e}"))?;
    if lock.schema != 1 {
        return Err("unsupported luat.lock schema".into());
    }
    let mut result = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut active = std::collections::HashSet::new();
    fn visit(
        name: &str,
        lock: &LockFile,
        root: &Path,
        visited: &mut std::collections::HashSet<String>,
        active: &mut std::collections::HashSet<String>,
        result: &mut Vec<InstalledPackage>,
    ) -> Result<(), String> {
        if active.contains(name) {
            return Err(format!("package dependency cycle includes {name}"));
        }
        if !visited.insert(name.to_owned()) {
            return Ok(());
        }
        active.insert(name.to_owned());
        let entry = lock
            .packages
            .get(name)
            .ok_or_else(|| format!("{name} is missing from luat.lock; run `luat install`"))?;
        let directory = root.join(".luat/packages").join(name).join(&entry.version);
        let manifest = read_manifest(&directory).map_err(|e| {
            format!(
                "{name}@{} is not installed ({e}); run `luat install`",
                entry.version
            )
        })?;
        if manifest.name != name || manifest.version != entry.version {
            return Err(format!("{name} installed identity mismatch"));
        }
        if manifest.dependencies.len() > MAX_FILES {
            return Err(format!("{name} has too many dependencies"));
        }
        for (file, expected) in &entry.files {
            validate_relative_file(file)?;
            let path = directory.join(file);
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.file_type().is_file()
                || sha256(&fs::read(path).map_err(|e| e.to_string())?) != *expected
            {
                return Err(format!(
                    "{name}@{} integrity check failed for {file}",
                    entry.version
                ));
            }
        }
        let actual = manifest_files(&directory, &manifest)?;
        if actual.keys().ne(entry.files.keys()) {
            return Err(format!(
                "{name}@{} files differ from luat.lock",
                entry.version
            ));
        }
        for dependency in dependencies_for(&manifest) {
            let locked = lock
                .packages
                .get(dependency)
                .ok_or_else(|| format!("dependency {dependency} is missing from luat.lock"))?;
            let requirement = require_versions(&manifest.dependencies[dependency])?;
            if !requirement.matches(&Version::parse(&locked.version).map_err(|e| e.to_string())?) {
                return Err(format!(
                    "locked {dependency}@{} does not satisfy {name}'s requirement",
                    locked.version
                ));
            }
        }
        for dependency in manifest.dependencies.keys() {
            visit(dependency, lock, root, visited, active, result)?;
        }
        active.remove(name);
        result.push(InstalledPackage {
            root: directory,
            manifest,
        });
        Ok(())
    }
    for (name, requirement) in dependencies {
        let entry = lock
            .packages
            .get(name)
            .ok_or_else(|| format!("{name} is missing from luat.lock; run `luat install`"))?;
        let requirement = require_versions(requirement)?;
        let version = Version::parse(&entry.version).map_err(|e| e.to_string())?;
        if !requirement.matches(&version) {
            return Err(format!(
                "locked {name}@{} does not satisfy the project's requirement",
                entry.version
            ));
        }
        visit(name, &lock, root, &mut visited, &mut active, &mut result)?;
    }
    Ok(result)
}

fn dependencies_for(manifest: &PackageManifest) -> impl Iterator<Item = &String> {
    manifest.dependencies.keys()
}

const REGISTRY: &str = "https://raw.githubusercontent.com/laot7490/luat-packages";
const MAX_FILE: u64 = 4 * 1024 * 1024;
const MAX_INDEX: u64 = 1024 * 1024;
const MAX_PACKAGE: usize = 32 * 1024 * 1024;
const MAX_FILES: usize = 256;

#[derive(Clone, Debug)]
struct Spec {
    name: String,
    version: Option<String>,
}

pub fn run(args: &[String]) -> Result<(), (u8, String)> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err((2, usage().to_owned()));
    };
    let (project, pretty, positional) = parse_options(&args[1..])?;
    let config_path = project_config_path(&project)?;
    let root = config_path.parent().unwrap_or_else(|| Path::new("."));
    match command {
        "add" => add(root, &config_path, &positional, pretty),
        "install" => install(root, &config_path, &positional, pretty),
        "remove" => remove(root, &config_path, &positional, pretty),
        "update" => update(root, &config_path, &positional, pretty),
        _ => Err((2, usage().to_owned())),
    }
}

fn usage() -> &'static str {
    "error LT5001: Invalid package command arguments.\n  help: Usage: luat <add|install|update|remove> [package[@version-requirement] ...] [--locked] [--project <directory>] [--pretty false]"
}

pub fn parse_options(args: &[String]) -> Result<(PathBuf, bool, Vec<String>), (u8, String)> {
    let mut project = PathBuf::from(".");
    let mut pretty = true;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--project" if index + 1 < args.len() && !args[index + 1].starts_with('-') => {
                project = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--project" => return Err((2, "error LT5001: '--project' requires a path".to_owned())),
            "--pretty" if args.get(index + 1).is_some_and(|value| value == "false") => {
                pretty = false;
                index += 2;
            }
            "--pretty" => {
                return Err((
                    2,
                    "error LT5001: '--pretty' supports only 'false'".to_owned(),
                ));
            }
            "--locked" => {
                positional.push("--locked".into());
                index += 1;
            }
            value if value.starts_with('-') => {
                return Err((2, format!("error LT5001: Unknown option '{value}'")));
            }
            value => {
                positional.push(value.to_owned());
                index += 1;
            }
        }
    }
    Ok((project, pretty, positional))
}

fn project_config_path(project: &Path) -> Result<PathBuf, (u8, String)> {
    let path = if project.is_dir() {
        if project.join("luat.toml").is_file() && project.join("luat-package.toml").is_file() {
            return Err((
                2,
                "error LT5101: Both luat.toml and luat-package.toml exist; select a file with --project"
                    .into(),
            ));
        }
        if project.join("luat.toml").is_file() {
            project.join("luat.toml")
        } else if project.join("luat-package.toml").is_file() {
            project.join("luat-package.toml")
        } else {
            project.join("luat.toml")
        }
    } else {
        project.to_path_buf()
    };
    if path
        .file_name()
        .is_none_or(|n| n != "luat.toml" && n != "luat-package.toml")
    {
        return Err((2, "error LT5101: Use luat.toml or luat-package.toml".into()));
    }
    if !path.is_file() {
        return Err((
            2,
            format!(
                "error LT5101: Project configuration not found: {}",
                path.display()
            ),
        ));
    }
    path.canonicalize()
        .map_err(|error| (2, format!("error LT5301: {}: {error}", path.display())))
}

fn parse_spec(raw: &str, allow_version: bool) -> Result<Spec, String> {
    let (name, version) = match raw.rsplit_once('@') {
        Some((name, version)) => (name, Some(version)),
        _ => (raw, None),
    };
    if !crate::packages::catalog::valid_name(name) {
        return Err(format!("invalid package name '{name}'"));
    }
    if version.is_some() && !allow_version {
        return Err(format!(
            "remove takes a package name without a version: '{raw}'"
        ));
    }
    if let Some(version) = version {
        crate::packages::require_versions(version)?;
    }
    Ok(Spec {
        name: name.to_owned(),
        version: version.map(str::to_owned),
    })
}

fn update(root: &Path, config: &Path, args: &[String], pretty: bool) -> Result<(), (u8, String)> {
    let (doc, _) = read_config(config)?;
    let table = doc
        .get("dependencies")
        .and_then(toml_edit::Item::as_table_like);
    let names: Vec<String> = if args.is_empty() {
        table
            .into_iter()
            .flat_map(|t| t.iter().map(|(n, _)| n.to_owned()))
            .collect()
    } else {
        args.to_vec()
    };
    let specs = names
        .iter()
        .map(|name| {
            parse_spec(name, false).map_err(|e| (2, format!("error LT5001: {e}")))?;
            let requirement = table
                .and_then(|t| t.get(name))
                .and_then(toml_edit::Item::as_str)
                .ok_or_else(|| {
                    (
                        2,
                        format!("error LT5001: '{name}' is not a direct dependency"),
                    )
                })?;
            Ok(format!("{name}@{requirement}"))
        })
        .collect::<Result<Vec<_>, (u8, String)>>()?;
    resolve_and_install(root, config, &specs, pretty, REGISTRY, false, Some(args))
}

fn add(root: &Path, config: &Path, args: &[String], pretty: bool) -> Result<(), (u8, String)> {
    if args.is_empty() {
        return Err((
            2,
            "error LT5001: Add requires at least one package; use luat install to restore dependencies"
                .into(),
        ));
    }
    install_packages(root, config, args, pretty, REGISTRY, false)
}

fn install_packages(
    root: &Path,
    config: &Path,
    args: &[String],
    pretty: bool,
    registry: &str,
    install_only: bool,
) -> Result<(), (u8, String)> {
    resolve_and_install(root, config, args, pretty, registry, install_only, None)
}

fn resolve_and_install(
    root: &Path,
    config: &Path,
    args: &[String],
    pretty: bool,
    registry: &str,
    install_only: bool,
    refresh: Option<&[String]>,
) -> Result<(), (u8, String)> {
    let (mut doc, original) = read_config(config)?;
    if !doc.contains_key("dependencies") {
        doc["dependencies"] = toml_edit::table();
    }
    let table = doc["dependencies"]
        .as_table_like_mut()
        .ok_or((2, "error LT5101: Dependencies must be a table".into()))?;
    let mut unversioned = Vec::new();
    for arg in args {
        let spec = parse_spec(arg, true).map_err(|e| (2, format!("error LT5001: {e}")))?;
        if args
            .iter()
            .filter(|other| {
                parse_spec(other, true).is_ok_and(|candidate| candidate.name == spec.name)
            })
            .count()
            > 1
        {
            return Err((
                2,
                format!(
                    "error LT5001: Package {} was specified more than once",
                    spec.name
                ),
            ));
        }
        if spec.version.is_none() {
            unversioned.push(spec.name.clone());
        }
        if refresh.is_none() {
            table.insert(
                &spec.name,
                value(spec.version.unwrap_or_else(|| "*".into())),
            );
        }
    }
    let roots = table
        .iter()
        .map(|(name, req)| {
            req.as_str()
                .map(|req| (name.to_owned(), req.to_owned()))
                .ok_or((
                    2,
                    format!("error LT5101: Dependency {name} must be a version requirement"),
                ))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    catalog::validate_dependencies(&roots).map_err(|e| (2, format!("error LT5101: {e}")))?;
    if roots.is_empty() {
        println!("Success: No packages are declared.");
        return Ok(());
    }
    let lock_before_resolution = fs::read(root.join("luat.lock")).ok();
    let existing_lock = root.join("luat.lock");
    if existing_lock.is_file() {
        let text = fs::read_to_string(&existing_lock)
            .map_err(|e| (2, format!("error LT5301: Could not read luat.lock: {e}")))?;
        let lock: LockFile = toml::from_str(&text)
            .map_err(|e| (2, format!("error LT5301: Invalid luat.lock: {e}")))?;
        if lock.schema != 1 {
            return Err((2, "error LT5301: Unsupported luat.lock schema".into()));
        }
    }
    let lock = fs::read_to_string(&existing_lock)
        .ok()
        .and_then(|text| toml::from_str::<LockFile>(&text).ok());
    let mut preferred: BTreeMap<String, String> = lock
        .as_ref()
        .map(|lock| {
            lock.packages
                .iter()
                .map(|(name, package)| (name.clone(), package.version.clone()))
                .collect()
        })
        .unwrap_or_default();
    for arg in args.iter().filter(|_| !install_only && refresh.is_none()) {
        preferred.remove(
            &parse_spec(arg, true)
                .map_err(|e| (2, format!("error LT5001: {e}")))?
                .name,
        );
    }
    let mut indexes = BTreeMap::new();
    let mut pending = roots.keys().cloned().collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        if indexes.contains_key(&name) {
            continue;
        }
        let index = fetch_index(registry, &name).map_err(|e| (2, e))?;
        for release in index.versions.values() {
            for dependency in release.dependencies.keys() {
                if !indexes.contains_key(dependency) {
                    pending.push(dependency.clone());
                }
            }
        }
        indexes.insert(name, index);
        if indexes.len() > 256 {
            return Err((
                2,
                "error LT5203: Dependency graph exceeds 256 packages".into(),
            ));
        }
    }
    if let Some(names) = refresh {
        if names.is_empty() {
            preferred.clear();
        } else {
            let mut pending = names.to_vec();
            let mut visited = std::collections::BTreeSet::new();
            while let Some(name) = pending.pop() {
                if !visited.insert(name.clone()) {
                    continue;
                }
                if let Some(version) = preferred.remove(&name)
                    && let Some(release) = indexes
                        .get(&name)
                        .and_then(|index| index.versions.get(&version))
                {
                    pending.extend(release.dependencies.keys().cloned());
                }
            }
        }
    }
    let versions = catalog::solve_versions(&indexes, &roots, &preferred).map_err(|e| {
        (
            2,
            format!("error LT5203: {e}\n  help: Review the dependency version requirements."),
        )
    })?;
    let _guard = acquire_lock(root)?;
    if fs::read(config).ok().as_deref() != Some(original.as_slice())
        || fs::read(root.join("luat.lock")).ok() != lock_before_resolution
    {
        return Err((2, "error LT5301: Project files changed during package resolution; retry the operation".into()));
    }
    if !install_only {
        for name in unversioned {
            table.insert(&name, value(format!("^{}", versions[&name])));
        }
    }
    let stage = root.join(".luat/.staging").join(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    ensure_managed_path(root, &stage).map_err(|e| (2, e))?;
    fs::create_dir_all(&stage).map_err(|e| (2, format!("error LT5301: {e}")))?;
    let _cleanup = PackageStage(stage.clone());
    let mut packages = BTreeMap::new();
    let mut installs: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (name, version) in &versions {
        let release = &indexes[name].versions[version];
        let dest = root.join(".luat/packages").join(name).join(version);
        ensure_managed_path(root, &dest).map_err(|e| (2, e))?;
        let existing_valid = fs::read_to_string(root.join("luat.lock"))
            .ok()
            .and_then(|s| toml::from_str::<LockFile>(&s).ok())
            .and_then(|l| l.packages.get(name).cloned())
            .is_some_and(|locked| {
                locked.version == *version
                    && locked.commit == release.commit
                    && validate_locked(&dest, &locked).is_ok()
            });
        if existing_valid {
            packages.insert(
                name.clone(),
                LockedPackage {
                    version: version.clone(),
                    commit: release.commit.clone(),
                    files: release.files.clone(),
                },
            );
            continue;
        }
        let package_stage = stage.join(name).join(version);
        fs::create_dir_all(&package_stage).map_err(|e| (2, format!("error LT5301: {e}")))?;
        let mut package_size = 0usize;
        for (relative, hash) in &release.files {
            let bytes =
                fetch_file(registry, name, &release.commit, relative).map_err(|e| (2, e))?;
            package_size = package_size.saturating_add(bytes.len());
            if package_size > MAX_PACKAGE {
                return Err((
                    2,
                    format!("error LT5204: {name}@{version} exceeds the package size limit"),
                ));
            }
            if sha256(&bytes) != *hash {
                return Err((
                    2,
                    format!("error LT5204: {name}@{version} {relative}: SHA-256 mismatch"),
                ));
            }
            let path = package_stage.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| (2, format!("error LT5301: {e}")))?;
            }
            fs::write(path, bytes).map_err(|e| (2, format!("error LT5301: {e}")))?;
        }
        let manifest = crate::packages::read_manifest(&package_stage).map_err(|e| {
            (
                2,
                format!("error LT5204: Invalid downloaded manifest for {name}@{version}: {e}"),
            )
        })?;
        if manifest.name != *name
            || manifest.version != *version
            || manifest.dependencies != release.dependencies
        {
            return Err((
                2,
                format!("error LT5204: {name}@{version} catalog metadata mismatch"),
            ));
        }
        installs.push((package_stage, dest));
        packages.insert(
            name.clone(),
            LockedPackage {
                version: version.clone(),
                commit: release.commit.clone(),
                files: release.files.clone(),
            },
        );
    }
    let lock_text = toml::to_string(&LockFile {
        schema: 1,
        packages,
    })
    .map_err(|e| (2, format!("error LT5301: {e}")))?;
    let lock_path = root.join("luat.lock");
    let lock_tmp = stage.join("luat.lock");
    fs::write(&lock_tmp, &lock_text).map_err(|e| (2, format!("error LT5301: {e}")))?;
    let mut published: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let mut config_written = false;
    let publish = (|| {
        for (idx, (src, dest)) in installs.iter().enumerate() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| (2, format!("error LT5301: {e}")))?;
            }
            let backup = if dest.exists() {
                let backup = stage.join(format!("backup-{idx}"));
                fs::rename(dest, &backup).map_err(|e| (2, format!("error LT5301: {e}")))?;
                Some(backup)
            } else {
                None
            };
            if let Err(error) = fs::rename(src, dest) {
                if let Some(backup) = &backup {
                    let _ = fs::rename(backup, dest);
                }
                return Err((2, format!("error LT5301: {error}")));
            }
            published.push((dest.clone(), backup));
        }
        if !install_only && refresh.is_none() {
            write_config(config, &doc, &original).map_err(|e| (2, e))?;
            config_written = true;
        }
        replace_file(&lock_tmp, &lock_path)
            .map_err(|e| (2, format!("error LT5301: Could not write luat.lock: {e}")))?;
        Ok::<(), (u8, String)>(())
    })();
    if let Err(error) = publish {
        let error = if config_written {
            restore_config(config, doc.to_string().as_bytes(), &original)
                .err()
                .map_or(error.clone(), |rollback| {
                    (
                        2,
                        format!("{}; config rollback failed: {rollback}", error.1),
                    )
                })
        } else {
            error
        };
        for (dest, backup) in published.into_iter().rev() {
            if dest.exists() {
                let _ = fs::remove_dir_all(&dest);
            }
            if let Some(backup) = backup {
                let _ = fs::rename(backup, dest);
            }
        }
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    let _ = fs::remove_dir_all(stage);
    status(pretty, "Installed", &format!("{} packages", versions.len()));
    Ok(())
}

struct PackageStage(PathBuf);

impl Drop for PackageStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn validate_locked(directory: &Path, locked: &LockedPackage) -> Result<(), String> {
    let manifest = crate::packages::read_manifest(directory)?;
    if manifest.version != locked.version {
        return Err("locked version mismatch".into());
    }
    let actual = crate::packages::manifest_files(directory, &manifest)?;
    if actual.keys().ne(locked.files.keys()) {
        return Err("locked files mismatch".into());
    }
    for (name, path) in actual {
        if sha256(&fs::read(path).map_err(|e| e.to_string())?) != locked.files[&name] {
            return Err(format!("hash mismatch: {name}"));
        }
    }
    Ok(())
}

fn install(root: &Path, config: &Path, args: &[String], pretty: bool) -> Result<(), (u8, String)> {
    if args == ["--locked"] {
        install_locked(root, config, REGISTRY)?;
        status(pretty, "Installed", "packages from luat.lock");
        return Ok(());
    }
    if !args.is_empty() {
        return Err((2, usage().into()));
    }
    install_packages(root, config, args, pretty, REGISTRY, true)
}

fn install_locked(root: &Path, config: &Path, registry: &str) -> Result<(), (u8, String)> {
    let _guard = acquire_lock(root)?;
    let (doc, original) = read_config(config)?;
    let dependencies = doc
        .get("dependencies")
        .and_then(toml_edit::Item::as_table_like)
        .into_iter()
        .flat_map(|table| table.iter())
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.to_owned(), value.to_owned()))
                .ok_or((
                    2,
                    "error LT5101: Invalid dependency requirement.".to_owned(),
                ))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    catalog::validate_dependencies(&dependencies).map_err(|e| (2, format!("error LT5101: {e}")))?;
    let lock_bytes =
        fs::read(root.join("luat.lock")).map_err(|e| (2, format!("error LT5301: {e}")))?;
    let lock: LockFile = toml::from_str(
        std::str::from_utf8(&lock_bytes).map_err(|e| (2, format!("error LT5301: {e}")))?,
    )
    .map_err(|e| (2, format!("error LT5301: {e}")))?;
    if lock.schema != 1 {
        return Err((2, "error LT5301: Unsupported lock schema.".into()));
    }
    for (name, requirement) in &dependencies {
        let entry = lock.packages.get(name).ok_or((
            2,
            format!("error LT5301: Package {name} is missing from luat.lock."),
        ))?;
        if !require_versions(requirement)
            .map_err(|e| (2, format!("error LT5203: {e}")))?
            .matches(
                &Version::parse(&entry.version).map_err(|e| (2, format!("error LT5301: {e}")))?,
            )
        {
            return Err((
                2,
                format!("error LT5203: Locked package {name} does not satisfy the manifest."),
            ));
        }
    }
    let stage = root
        .join(".luat/.staging")
        .join(format!("locked-{}", std::process::id()));
    ensure_managed_path(root, &stage).map_err(|e| (2, e))?;
    fs::create_dir_all(&stage).map_err(|e| (2, format!("error LT5301: {e}")))?;
    let _cleanup = PackageStage(stage.clone());
    fs::write(stage.join("luat.lock"), &lock_bytes)
        .map_err(|e| (2, format!("error LT5301: {e}")))?;
    let mut installs = Vec::new();
    for (name, entry) in &lock.packages {
        if !catalog::valid_name(name) {
            return Err((2, "error LT5301: Invalid locked package name.".into()));
        }
        Version::parse(&entry.version).map_err(|e| (2, format!("error LT5301: {e}")))?;
        catalog::PackageVersion {
            commit: entry.commit.clone(),
            dependencies: BTreeMap::new(),
            files: entry.files.clone(),
        }
        .validate()
        .map_err(|e| (2, format!("error LT5204: {e}")))?;
        let dest = root.join(".luat/packages").join(name).join(&entry.version);
        ensure_managed_path(root, &dest).map_err(|e| (2, e))?;
        let cached = validate_locked(&dest, entry).is_ok();
        let staged = stage.join(".luat/packages").join(name).join(&entry.version);
        let mut total = 0usize;
        for (relative, hash) in &entry.files {
            let bytes = if cached {
                fs::read(dest.join(relative)).map_err(|e| (2, format!("error LT5301: {e}")))?
            } else {
                fetch_file(registry, name, &entry.commit, relative).map_err(|e| (2, e))?
            };
            total += bytes.len();
            if total > MAX_PACKAGE {
                return Err((
                    2,
                    "error LT5204: Locked package size limit exceeded.".into(),
                ));
            }
            if sha256(&bytes) != *hash {
                return Err((
                    2,
                    format!("error LT5204: Hash mismatch for {name}/{relative}."),
                ));
            }
            let path = staged.join(relative);
            fs::create_dir_all(path.parent().unwrap())
                .map_err(|e| (2, format!("error LT5301: {e}")))?;
            fs::write(path, bytes).map_err(|e| (2, format!("error LT5301: {e}")))?;
        }
        if !cached {
            installs.push((staged, dest));
        }
    }
    let packages =
        installed_packages(&stage, &dependencies).map_err(|e| (2, format!("error LT5204: {e}")))?;
    if packages.len() != lock.packages.len() {
        return Err((
            2,
            "error LT5301: Luat.lock contains unused packages.\n  help: Run `luat install`.".into(),
        ));
    }
    if fs::read(config).map_err(|e| (2, format!("error LT5301: {e}")))? != original
        || fs::read(root.join("luat.lock")).map_err(|e| (2, format!("error LT5301: {e}")))?
            != lock_bytes
    {
        return Err((
            2,
            "error LT5301: The manifest or lock file changed during installation.".into(),
        ));
    }
    let mut published: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let result = (|| -> std::io::Result<()> {
        for (index, (source, dest)) in installs.iter().enumerate() {
            fs::create_dir_all(dest.parent().unwrap())?;
            let backup = if dest.exists() {
                let backup = stage.join(format!("backup-{index}"));
                fs::rename(dest, &backup)?;
                Some(backup)
            } else {
                None
            };
            published.push((dest.clone(), backup));
            fs::rename(source, dest)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for (dest, backup) in published.into_iter().rev() {
            if dest.exists() {
                fs::remove_dir_all(&dest).map_err(|e| (2, format!("{error}; rollback: {e}")))?;
            }
            if let Some(backup) = backup {
                fs::rename(backup, dest).map_err(|e| (2, format!("{error}; rollback: {e}")))?;
            }
        }
        return Err((2, format!("error LT5301: {error}")));
    }
    Ok(())
}

fn remove(root: &Path, config: &Path, args: &[String], pretty: bool) -> Result<(), (u8, String)> {
    if args.is_empty() {
        return Err((2, usage().to_owned()));
    }
    let specs = args
        .iter()
        .map(|raw| {
            parse_spec(raw, false).map_err(|message| (2, format!("error LT5001: {message}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let _lock = acquire_lock(root)?;
    let (mut doc, original) = read_config(config)?;
    for spec in &specs {
        if doc
            .get("dependencies")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|t| t.get(&spec.name))
            .is_none()
        {
            return Err((
                2,
                format!("error LT5001: '{}' is not a direct dependency", spec.name),
            ));
        }
    }
    for spec in specs {
        doc.get_mut("dependencies")
            .and_then(toml_edit::Item::as_table_like_mut)
            .and_then(|table| table.remove(&spec.name));
    }
    let lock_path = root.join("luat.lock");
    let mut lock_update = None;
    if lock_path.is_file() {
        let text = fs::read_to_string(&lock_path).map_err(|e| (2, format!("error LT5301: {e}")))?;
        let mut lock: LockFile = toml::from_str(&text)
            .map_err(|e| (2, format!("error LT5301: Invalid luat.lock: {e}")))?;
        if lock.schema != 1 {
            return Err((2, "error LT5301: Unsupported luat.lock schema".into()));
        }
        for (name, package) in &lock.packages {
            if !catalog::valid_name(name) || Version::parse(&package.version).is_err() {
                return Err((
                    2,
                    "error LT5301: Invalid package identity in luat.lock".into(),
                ));
            }
            ensure_managed_path(
                root,
                &root
                    .join(".luat/packages")
                    .join(name)
                    .join(&package.version),
            )
            .map_err(|e| (2, e))?;
        }
        let mut keep = std::collections::HashSet::new();
        let mut pending = doc
            .get("dependencies")
            .and_then(toml_edit::Item::as_table_like)
            .into_iter()
            .flat_map(|t| t.iter().map(|(n, _)| n.to_owned()))
            .collect::<Vec<_>>();
        while let Some(name) = pending.pop() {
            if !keep.insert(name.clone()) {
                continue;
            }
            if let Some(package) = lock.packages.get(&name) {
                let directory = root
                    .join(".luat/packages")
                    .join(&name)
                    .join(&package.version);
                ensure_managed_path(root, &directory).map_err(|e| (2, e))?;
                let manifest = crate::packages::read_manifest(&directory).map_err(|e| (2, format!("error LT5301: Cannot determine dependencies of {name}: {e}; run luat install first")))?;
                pending.extend(manifest.dependencies.keys().cloned());
            }
        }
        let stale = lock
            .packages
            .keys()
            .filter(|name| !keep.contains(*name))
            .cloned()
            .filter_map(|name| {
                lock.packages.get(&name).map(|package| {
                    let directory = root
                        .join(".luat/packages")
                        .join(&name)
                        .join(&package.version);
                    (name, directory, package.clone())
                })
            })
            .collect::<Vec<_>>();
        lock.packages.retain(|name, _| keep.contains(name));
        let serialized = toml::to_string(&lock).map_err(|e| (2, format!("error LT5301: {e}")))?;
        let temp = lock_path.with_extension("lock.tmp");
        fs::write(&temp, serialized).map_err(|e| (2, format!("error LT5301: {e}")))?;
        lock_update = Some((temp, stale));
    }
    if let Err(error) = write_config(config, &doc, &original) {
        if let Some((temp, _)) = &lock_update {
            let _ = fs::remove_file(temp);
        }
        return Err((2, error));
    }
    if let Some((temp, stale)) = lock_update {
        if let Err(error) = replace_file(&temp, &lock_path) {
            let _ = fs::remove_file(&temp);
            let rollback = restore_config(config, doc.to_string().as_bytes(), &original);
            return Err((
                2,
                match rollback {
                    Ok(()) => format!("error LT5301: Could not update luat.lock: {error}"),
                    Err(rollback) => format!(
                        "error LT5301: Could not update luat.lock: {error}; config rollback failed: {rollback}"
                    ),
                },
            ));
        }
        for (name, directory, package) in stale {
            if validate_locked(&directory, &package).is_ok()
                && let Err(error) = fs::remove_dir_all(&directory)
            {
                eprintln!(
                    "{}",
                    crate::output::stderr_message(&format!(
                        "warning LT5301: Could not remove package {name}: {error}"
                    ))
                );
            }
        }
    }
    status(pretty, "Saved", &config.display().to_string());
    Ok(())
}

fn fetch_index(registry: &str, name: &str) -> Result<PackageIndex, String> {
    let url = format!("{registry}/main/index/{name}.toml");
    let text = request(&url, MAX_INDEX).map_err(|error| {
        if matches!(error, ureq::Error::StatusCode(404)) {
            format!("error LT5201: Package '{name}' was not found in the LuaT registry.\n  help: Check the package name and confirm that it has been published.")
        } else {
            format!("error LT5202: Could not load registry data for package '{name}': {error}\n  help: Check your network connection and registry availability, then retry.")
        }
    })?;
    let index: PackageIndex = toml::from_str(&text).map_err(|error| {
        format!("error LT5202: The registry returned invalid catalog data for '{name}': {error}\n  help: Verify the registry catalog and retry.")
    })?;
    index.validate(Some(name), true).map_err(|error| {
        format!("error LT5202: The registry returned invalid catalog data for '{name}': {error}\n  help: Verify the registry catalog and retry.")
    })?;
    Ok(index)
}

fn fetch_file(registry: &str, name: &str, commit: &str, relative: &str) -> Result<Vec<u8>, String> {
    let path = relative
        .split('/')
        .map(urlencoding)
        .collect::<Vec<_>>()
        .join("/");
    let url = format!("{registry}/{commit}/packages/{name}/{path}");
    let text = request(&url, MAX_FILE).map_err(|e| {
        format!("error LT5204: Could not download {name}/{relative} at {commit}: {e}\n  help: Check registry availability and retry the package operation.")
    })?;
    Ok(text.into_bytes())
}

fn request(url: &str, limit: u64) -> Result<String, ureq::Error> {
    request_with_timeout(url, limit, Duration::from_secs(30), Duration::from_secs(10))
}

fn request_with_timeout(
    url: &str,
    limit: u64,
    global_timeout: Duration,
    connect_timeout: Duration,
) -> Result<String, ureq::Error> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(global_timeout))
        .timeout_connect(Some(connect_timeout))
        .build()
        .into();
    let mut response = agent.get(url).call()?;
    let mut body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut body)
        .map_err(ureq::Error::Io)?;
    if body.len() as u64 > limit {
        return Err(ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Registry response exceeds the size limit.",
        )));
    }
    String::from_utf8(body).map_err(|error| {
        ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
}

fn validate_relative_file(raw: &str) -> Result<(), String> {
    let path = Path::new(raw);
    if raw.contains('\\')
        || raw
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.is_absolute()
        || !(raw == "luat-package.toml"
            || raw.ends_with(".d.luat")
            || raw.ends_with(".luat")
            || raw.ends_with(".lua"))
        || path.components().any(|component| match component {
            Component::Normal(part) => !safe_file_component(part.to_string_lossy().as_ref()),
            _ => true,
        })
    {
        return Err(format!("invalid declaration path '{raw}'"));
    }
    Ok(())
}

fn safe_file_component(part: &str) -> bool {
    let stem = part
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !part.is_empty()
        && !part.ends_with(['.', ' '])
        && !part.contains([':', '<', '>', '"', '|', '?', '*'])
        && !matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        && !matches!(
            stem.as_str(),
            "COM¹" | "COM²" | "COM³" | "LPT¹" | "LPT²" | "LPT³"
        )
}

fn ensure_managed_path(root: &Path, path: &Path) -> Result<(), String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "error LT5301: Managed path escapes project: {}",
            path.display()
        )
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!(
                "error LT5301: Unsafe managed path: {}",
                path.display()
            ));
        };
        current.push(component);
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(format!(
                "error LT5301: Refusing symlinked managed path {}",
                current.display()
            ));
        }
    }
    Ok(())
}

fn read_config(path: &Path) -> Result<(DocumentMut, Vec<u8>), (u8, String)> {
    let text = fs::read_to_string(path)
        .map_err(|error| (2, format!("error LT5301: {}: {error}", path.display())))?;
    let doc = text.parse().map_err(|error| {
        (
            2,
            format!("error LT5301: Invalid {}: {error}", path.display()),
        )
    })?;
    Ok((doc, text.into_bytes()))
}

fn write_config(path: &Path, doc: &DocumentMut, original: &[u8]) -> Result<(), String> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let temp = path.with_extension(format!("toml.{}.tmp", std::process::id()));
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("error LT5301: {}: {error}", temp.display()))?;
    file.write_all(doc.to_string().as_bytes())
        .map_err(|error| format!("error LT5301: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("error LT5301: {error}"))?;
    if fs::read(path).map_err(|error| format!("error LT5301: {error}"))? != original {
        let _ = fs::remove_file(&temp);
        return Err(
            "error LT5301: luat.toml changed during package operation; your changes were preserved"
                .into(),
        );
    }
    replace_file(&temp, path)
        .map_err(|error| format!("error LT5301: Could not update {}: {error}", path.display()))
}

fn restore_config(path: &Path, expected: &[u8], original: &[u8]) -> Result<(), String> {
    let temp = path.with_extension(format!(
        "toml.{}.{}.rollback.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| format!("error LT5301: {}: {error}", temp.display()))?;
        file.write_all(original)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("error LT5301: {error}"))?;
        if fs::read(path).map_err(|error| format!("error LT5301: {error}"))? != expected {
            return Err(
                "error LT5301: luat.toml changed during rollback; your changes were preserved"
                    .into(),
            );
        }
        replace_file(&temp, path).map_err(|error| {
            format!(
                "error LT5301: Could not restore {}: {error}",
                path.display()
            )
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    if destination.exists() {
        let backup = destination.with_extension(format!(
            "toml.{}.{}.bak",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::rename(destination, &backup)?;
        if let Err(error) = fs::rename(source, destination) {
            let _ = fs::rename(&backup, destination);
            return Err(error);
        }
        let _ = fs::remove_file(backup);
    } else {
        fs::rename(source, destination)?;
    }
    Ok(())
}
#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

fn acquire_lock(root: &Path) -> Result<fs::File, (u8, String)> {
    let directory = root.join(".luat");
    if fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err((
            2,
            format!(
                "error LT5301: Refusing symlinked managed directory {}",
                directory.display()
            ),
        ));
    }
    fs::create_dir_all(&directory)
        .map_err(|error| (2, format!("error LT5301: {}: {error}", directory.display())))?;
    let ignore = directory.join(".gitignore");
    if fs::symlink_metadata(&ignore).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err((
            2,
            format!("error LT5301: Refusing symlinked {}", ignore.display()),
        ));
    }
    if !ignore.exists() {
        fs::write(&ignore, "*\n")
            .map_err(|error| (2, format!("error LT5301: {}: {error}", ignore.display())))?;
    }
    let path = directory.join("install.lock");
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err((
            2,
            format!("error LT5301: Refusing symlinked {}", path.display()),
        ));
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|error| (2, format!("error LT5301: {}: {error}", path.display())))?;
    file.try_lock().map_err(|error| {
        (
            2,
            format!("error LT5301: Another LuaT package operation is running ({error})"),
        )
    })?;
    Ok(file)
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn urlencoding(value: &str) -> String {
    value.bytes().fold(String::new(), |mut encoded, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
        encoded
    })
}
fn status(pretty: bool, label: &str, value: &str) {
    if pretty && std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
        println!("\x1b[32mSuccess:\x1b[0m {label} {value}");
    } else {
        println!("Success: {label} {value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use std::{
        net::{TcpListener, TcpStream},
        thread,
    };

    #[test]
    fn preserves_inline_dependencies_and_external_config_edits() {
        let root = temp_project();
        let config = root.join("luat.toml");
        fs::write(
            &config,
            "dependencies = { example = '1.0.0' }\n[project]\nname='test'\n",
        )
        .unwrap();
        let (mut doc, original) = read_config(&config).unwrap();
        doc["dependencies"]["other"] = value("2.0.0");
        write_config(&config, &doc, &original).unwrap();
        let (doc, original) = read_config(&config).unwrap();
        assert_eq!(doc["dependencies"]["example"].as_str(), Some("1.0.0"));
        fs::write(&config, "[project]\nname='edited'\n").unwrap();
        assert!(write_config(&config, &doc, &original).is_err());
        assert!(fs::read_to_string(&config).unwrap().contains("edited"));
        assert!(remove(&root, &config, &["absent".into()], false).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_registry_integrity_check_keeps_config_lock_and_install_unchanged() {
        let root = temp_project();
        let manifest =
            "name='example'\nversion='1.0.0'\nauthor='Test'\nlicense='MIT'\nshared=['init.luat']\n";
        let index = format!(
            "schema=2\nname='example'\n[versions.'1.0.0']\ncommit='{}'\n[versions.'1.0.0'.files]\n'luat-package.toml'='{}'\n'init.luat'='{}'\n",
            "0".repeat(40),
            sha256(manifest.as_bytes()),
            "0".repeat(64)
        );
        let (registry, server) = serve(vec![
            ("200 OK".into(), index),
            ("200 OK".into(), manifest.into()),
        ]);
        let original = fs::read(root.join("luat.toml")).unwrap();
        assert!(
            install_packages(
                &root,
                &root.join("luat.toml"),
                &["example@1.0.0".into()],
                false,
                &registry,
                false
            )
            .is_err()
        );
        server.join().unwrap();
        assert_eq!(fs::read(root.join("luat.toml")).unwrap(), original);
        assert!(!root.join("luat.lock").exists());
        assert!(!root.join(".luat/packages/example").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_registry_package_has_a_distinct_operational_error() {
        let (registry, server) = serve(vec![("404 Not Found".into(), String::new())]);
        let error = fetch_index(&registry, "missing").unwrap_err();
        server.join().unwrap();

        assert_eq!(
            error,
            "error LT5201: Package 'missing' was not found in the LuaT registry.\n  help: Check the package name and confirm that it has been published."
        );
    }

    #[test]
    fn adding_missing_registry_package_does_not_create_managed_directory() {
        let root = temp_project();
        let config = root.join("luat.toml");
        let original = fs::read(&config).unwrap();
        let (registry, server) = serve(vec![("404 Not Found".into(), String::new())]);

        assert!(install_packages(
            &root,
            &config,
            &["example".into()],
            false,
            &registry,
            false,
        ).is_err());
        server.join().unwrap();

        assert!(!root.join(".luat").exists());
        assert!(!root.join("luat.lock").exists());
        assert_eq!(fs::read(config).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registry_server_failure_is_not_reported_as_a_missing_package() {
        let (registry, server) = serve(vec![("503 Service Unavailable".into(), String::new())]);
        let error = fetch_index(&registry, "example").unwrap_err();
        server.join().unwrap();
        assert!(error.contains("error LT5202:"), "{error}");
        assert!(error.contains("503"), "{error}");
        assert!(!error.contains("LT5201"), "{error}");
    }

    #[test]
    fn remove_prunes_unused_transitive_packages_and_keeps_shared_dependencies() {
        let root = temp_project();
        let install = root.join(".luat/packages");
        let mut locked = BTreeMap::new();
        for (name, deps) in [
            ("app", vec!["common", "orphan"]),
            ("orphan", vec![]),
            ("other", vec!["common"]),
            ("common", vec![]),
        ] {
            let dir = install.join(name).join("1.0.0");
            fs::create_dir_all(dir.join("types")).unwrap();
            fs::write(
                dir.join("types/api.d.luat"),
                "---@shared\ntypedef Api = number\n",
            )
            .unwrap();
            let manifest = format!(
                "name='{name}'\nversion='1.0.0'\nauthor='Test'\nlicense='MIT'\ndeclarations=['types/api.d.luat']\n[dependencies]\n{}",
                deps.iter()
                    .map(|d| format!("{d}='1.0.0'\n"))
                    .collect::<String>()
            );
            fs::write(dir.join("luat-package.toml"), &manifest).unwrap();
            locked.insert(
                name.into(),
                LockedPackage {
                    version: "1.0.0".into(),
                    commit: "0".repeat(40),
                    files: BTreeMap::from([
                        ("luat-package.toml".into(), sha256(manifest.as_bytes())),
                        (
                            "types/api.d.luat".into(),
                            sha256(b"---@shared\ntypedef Api = number\n"),
                        ),
                    ]),
                },
            );
        }
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[dependencies]\napp='1.0.0'\nother='1.0.0'\n",
        )
        .unwrap();
        fs::write(
            root.join("luat.lock"),
            toml::to_string(&LockFile {
                schema: 1,
                packages: locked,
            })
            .unwrap(),
        )
        .unwrap();
        remove(&root, &root.join("luat.toml"), &["app".into()], false).unwrap();
        let lock: LockFile =
            toml::from_str(&fs::read_to_string(root.join("luat.lock")).unwrap()).unwrap();
        assert!(!lock.packages.contains_key("app"));
        assert!(!install.join("orphan/1.0.0").exists());
        assert!(lock.packages.contains_key("other"));
        assert!(lock.packages.contains_key("common"));
        assert!(!install.join("app/1.0.0").exists());
        assert!(install.join("common/1.0.0").exists());
        fs::remove_dir_all(root).unwrap();
    }

    fn serve(responses: Vec<(String, String)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            for (status, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(30);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "test HTTP request did not arrive"
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("test HTTP listener failed: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let bytes = stream.read(&mut buffer).unwrap();
                    if bytes == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..bytes]);
                }
                respond(&mut stream, &status, &body);
            }
        });
        (format!("http://{address}"), server)
    }

    fn respond(stream: &mut TcpStream, status: &str, body: &str) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    fn temp_project() -> PathBuf {
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!(
                "luat-packages-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::write(
            root.join("src/client/main.luat"),
            "local value = PackageValue",
        )
        .unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn local_registry_add_keeps_manifest_and_writes_verified_lock() {
        let root = temp_project();
        let manifest = "name = \"example\"\nversion = \"1.0.0\"\nauthor = \"Test\"\nlicense = \"MIT\"\nshared = [\"init.luat\"]\n";
        let source = "return {}\n";
        let files = BTreeMap::from([
            ("luat-package.toml", sha256(manifest.as_bytes())),
            ("init.luat", sha256(source.as_bytes())),
        ]);
        let index = format!(
            "schema = 2\nname = \"example\"\n[versions.\"1.0.0\"]\ncommit = \"{}\"\n[versions.\"1.0.0\".dependencies]\n[versions.\"1.0.0\".files]\n\"luat-package.toml\" = \"{}\"\n\"init.luat\" = \"{}\"\n",
            "0".repeat(40),
            files["luat-package.toml"],
            files["init.luat"]
        );
        let (registry, server) = serve(vec![
            ("200 OK".into(), index),
            ("200 OK".into(), source.into()),
            ("200 OK".into(), manifest.into()),
        ]);
        install_packages(
            &root,
            &root.join("luat.toml"),
            &["example@^1.0".into()],
            false,
            &registry,
            false,
        )
        .unwrap();
        server.join().unwrap();
        let installed = root.join(".luat/packages/example/1.0.0/luat-package.toml");
        assert_eq!(fs::read_to_string(installed).unwrap(), manifest);
        assert!(
            installed_packages(&root, &BTreeMap::from([("example".into(), "^1.0".into())])).is_ok()
        );
        assert!(
            fs::read_to_string(root.join("luat.lock"))
                .unwrap()
                .contains("commit")
        );
        install(&root, &root.join("luat.toml"), &["--locked".into()], false).unwrap();
        let before = fs::read(root.join("luat.lock")).unwrap();
        fs::remove_dir_all(root.join(".luat/packages")).unwrap();
        let (registry, server) = serve(vec![
            ("200 OK".into(), source.into()),
            ("200 OK".into(), manifest.into()),
        ]);
        install_locked(&root, &root.join("luat.toml"), &registry).unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(root.join("luat.lock")).unwrap(), before);
        assert!(
            root.join(".luat/packages/example/1.0.0/init.luat")
                .is_file()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_packages_rejects_root_lock_versions_outside_the_requirement() {
        let root = temp_project();
        let package = root.join(".luat/packages/example/1.0.0");
        fs::create_dir_all(&package).unwrap();
        let manifest =
            "name='example'\nversion='1.0.0'\nauthor='Test'\nlicense='MIT'\nshared=['init.luat']\n";
        let source = "return {}\n";
        fs::write(package.join("luat-package.toml"), manifest).unwrap();
        fs::write(package.join("init.luat"), source).unwrap();
        fs::write(
            root.join("luat.lock"),
            toml::to_string(&LockFile {
                schema: 1,
                packages: BTreeMap::from([(
                    "example".into(),
                    LockedPackage {
                        version: "1.0.0".into(),
                        commit: "0".repeat(40),
                        files: BTreeMap::from([
                            ("luat-package.toml".into(), sha256(manifest.as_bytes())),
                            ("init.luat".into(), sha256(source.as_bytes())),
                        ]),
                    },
                )]),
            })
            .unwrap(),
        )
        .unwrap();

        let error = installed_packages(&root, &BTreeMap::from([("example".into(), "^2.0".into())]))
            .unwrap_err();
        assert!(error.contains("does not satisfy the project's requirement"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_packages_rejects_dependency_cycles_in_the_lock_graph() {
        let root = temp_project();
        let mut packages = BTreeMap::new();
        for (name, dependency) in [("a", "b"), ("b", "a")] {
            let directory = root.join(format!(".luat/packages/{name}/1.0.0"));
            fs::create_dir_all(&directory).unwrap();
            let manifest = format!(
                "name='{name}'\nversion='1.0.0'\nauthor='Test'\nlicense='MIT'\nshared=['init.luat']\n[dependencies]\n{dependency}='1.0.0'\n"
            );
            let source = "return {}\n";
            fs::write(directory.join("luat-package.toml"), &manifest).unwrap();
            fs::write(directory.join("init.luat"), source).unwrap();
            packages.insert(
                name.into(),
                LockedPackage {
                    version: "1.0.0".into(),
                    commit: "0".repeat(40),
                    files: BTreeMap::from([
                        ("luat-package.toml".into(), sha256(manifest.as_bytes())),
                        ("init.luat".into(), sha256(source.as_bytes())),
                    ]),
                },
            );
        }
        fs::write(
            root.join("luat.lock"),
            toml::to_string(&LockFile {
                schema: 1,
                packages,
            })
            .unwrap(),
        )
        .unwrap();

        let error =
            installed_packages(&root, &BTreeMap::from([("a".into(), "1.0.0".into())])).unwrap_err();
        assert!(error.contains("package dependency cycle"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn add_without_a_range_saves_a_compatible_range() {
        let root = temp_project();
        let manifest = "name = \"example\"\nversion = \"1.0.0\"\nauthor = \"Test\"\nlicense = \"MIT\"\nshared = [\"init.luat\"]\n";
        let source = "return {}\n";
        let index = format!(
            "schema = 2\nname = \"example\"\n[versions.\"1.0.0\"]\ncommit = \"{}\"\n[versions.\"1.0.0\".dependencies]\n[versions.\"1.0.0\".files]\n\"luat-package.toml\" = \"{}\"\n\"init.luat\" = \"{}\"\n",
            "0".repeat(40),
            sha256(manifest.as_bytes()),
            sha256(source.as_bytes())
        );
        let (registry, server) = serve(vec![
            ("200 OK".into(), index),
            ("200 OK".into(), source.into()),
            ("200 OK".into(), manifest.into()),
        ]);

        install_packages(
            &root,
            &root.join("luat.toml"),
            &["example".into()],
            false,
            &registry,
            false,
        )
        .unwrap();
        server.join().unwrap();
        let config = fs::read_to_string(root.join("luat.toml")).unwrap();
        let doc: DocumentMut = config.parse().unwrap();
        assert_eq!(doc["dependencies"]["example"].as_str(), Some("^1.0.0"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn remove_rejects_a_malformed_lock_without_changing_the_manifest() {
        let root = temp_project();
        let config = root.join("luat.toml");
        fs::write(
            &config,
            "[project]\nname='test'\n[dependencies]\nexample='1.0.0'\n",
        )
        .unwrap();
        fs::write(root.join("luat.lock"), "not valid toml = [\n").unwrap();
        let original = fs::read(&config).unwrap();

        assert!(remove(&root, &config, &["example".into()], false).is_err());
        assert_eq!(fs::read(&config).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_rollback_restores_only_the_version_written_by_the_operation() {
        let root = temp_project();
        let config = root.join("luat.toml");
        let original = fs::read(&config).unwrap();
        let expected = b"[project]\nname='updated'\n";
        fs::write(&config, expected).unwrap();
        restore_config(&config, expected, &original).unwrap();
        assert_eq!(fs::read(&config).unwrap(), original);

        fs::write(&config, b"user edit\n").unwrap();
        assert!(restore_config(&config, expected, &original).is_err());
        assert_eq!(fs::read(&config).unwrap(), b"user edit\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn package_config_uses_the_standalone_manifest() {
        let root = temp_project();
        fs::remove_file(root.join("luat.toml")).unwrap();
        fs::write(root.join("luat-package.toml"), "name='example'").unwrap();
        assert_eq!(
            project_config_path(&root).unwrap(),
            root.join("luat-package.toml").canonicalize().unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validates_only_exact_scoped_package_specs() {
        assert_eq!(
            parse_spec("ox_lib@1.2.3", true).unwrap().version.as_deref(),
            Some("1.2.3")
        );
        assert!(parse_spec("../escape", true).is_err());
        assert!(parse_spec("@types/example", true).is_err());
        assert!(parse_spec("example@^1.0", true).is_ok());
        assert!(parse_spec("example@1.0.0", false).is_err());
    }

    #[test]
    fn rejects_paths_that_escape_package_directories() {
        for path in [
            "../outside.d.luat",
            "C:/outside.d.luat",
            "nested\\outside.d.luat",
            "readme.md",
            "a//b.d.luat",
            "a/./b.d.luat",
        ] {
            assert!(validate_relative_file(path).is_err(), "{path}");
        }
        assert!(validate_relative_file("shared/client.d.luat").is_ok());
        for path in [
            "CON.d.luat",
            "CONIN$.d.luat",
            "COM¹.d.luat",
            "nested/AUX.d.luat",
            "trailing./file.d.luat",
        ] {
            assert!(validate_relative_file(path).is_err(), "{path}");
        }
    }

    #[test]
    fn loads_project_packages_from_the_config_directory() {
        let root = temp_project();
        let package = root.join(".luat/packages/example/1.0.0");
        fs::create_dir_all(package.join("nested")).unwrap();
        let declaration = "---@shared\ndeclare PackageValue: { value: string }\n";
        fs::write(package.join("nested/shared.d.luat"), declaration).unwrap();
        let manifest = "name='example'\nversion='1.0.0'\nauthor='Test'\nlicense='MIT'\ndeclarations=['nested/shared.d.luat']\n";
        fs::write(package.join("luat-package.toml"), manifest).unwrap();
        fs::write(
            root.join("src/client/main.luat"),
            "local value: string = PackageValue.value",
        )
        .unwrap();
        fs::write(root.join("luat.toml"), "[project]\nname='test'\n[dependencies]\nexample='1.0.0'\n[fivem]\nclient=['src/client/main.luat']\n").unwrap();
        fs::write(root.join(".gitignore"), "dist/\n").unwrap();
        fs::create_dir_all(root.join(".luat")).unwrap();
        fs::write(root.join(".luat/.gitignore"), "*\n").unwrap();
        fs::write(
            root.join("luat.lock"),
            toml::to_string(&LockFile {
                schema: 1,
                packages: BTreeMap::from([(
                    "example".into(),
                    LockedPackage {
                        version: "1.0.0".into(),
                        commit: "0".repeat(40),
                        files: BTreeMap::from([
                            ("luat-package.toml".into(), sha256(manifest.as_bytes())),
                            (
                                "nested/shared.d.luat".into(),
                                sha256(declaration.as_bytes()),
                            ),
                        ]),
                    },
                )]),
            })
            .unwrap(),
        )
        .unwrap();

        let project = crate::Project::load(&root).unwrap();
        assert!(
            project
                .declaration_files()
                .contains(&package.join("nested/shared.d.luat"))
        );
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).unwrap(),
            "dist/\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn http_requests_reject_status_truncated_and_oversized_bodies() {
        let (url, server) = serve(vec![
            ("404 Not Found".into(), "missing".into()),
            ("200 OK".into(), "payload".into()),
        ]);
        assert!(request(&url, 100).is_err());
        assert!(request(&url, 2).is_err());
        server.join().unwrap();
    }

    #[test]
    fn http_requests_reject_truncated_bodies() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc")
                .unwrap();
        });
        assert!(request(&format!("http://{address}"), 100).is_err());
        server.join().unwrap();
    }

    #[test]
    fn http_requests_stop_on_a_response_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            thread::sleep(Duration::from_millis(150));
        });
        assert!(
            request_with_timeout(
                &format!("http://{address}"),
                100,
                Duration::from_millis(30),
                Duration::from_millis(30),
            )
            .is_err()
        );
        server.join().unwrap();
    }

    #[test]
    fn configuration_selection_rejects_ambiguity_and_legacy_names() {
        let root = temp_project();
        fs::write(root.join("luat-package.toml"), "name='example'").unwrap();
        assert!(
            project_config_path(&root)
                .unwrap_err()
                .1
                .contains("error LT5101: Both")
        );
        assert!(crate::Project::load(&root).is_err());
        assert!(crate::Project::load(root.join("luat.toml")).is_ok());
        assert!(project_config_path(&root.join("luat.toml")).is_ok());
        assert!(project_config_path(&root.join("luat-package.toml")).is_ok());
        fs::write(root.join("package.toml"), "name='legacy'").unwrap();
        assert!(project_config_path(&root.join("package.toml")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_preserves_requirements_and_refreshes_locked_versions() {
        let root = temp_project();
        let config = root.join("luat.toml");
        fs::write(
            &config,
            "[project]\nname='test'\n[dependencies]\nexample='^1.0.0'\n",
        )
        .unwrap();
        let before = fs::read(&config).unwrap();
        fs::write(
            root.join("luat.lock"),
            toml::to_string(&LockFile {
                schema: 1,
                packages: BTreeMap::from([(
                    "example".into(),
                    LockedPackage {
                        version: "1.0.0".into(),
                        commit: "a".repeat(40),
                        files: BTreeMap::new(),
                    },
                )]),
            })
            .unwrap(),
        )
        .unwrap();
        let commit = "a".repeat(40);
        let manifest =
            "name='example'\nversion='1.1.0'\nauthor='Test'\nlicense='MIT'\nshared=['init.luat']\n";
        let source = "return {}\n";
        let index = format!(
            "schema=2\nname='example'\n[versions.'1.1.0']\ncommit='{commit}'\n[versions.'1.1.0'.files]\n'luat-package.toml'='{}'\n'init.luat'='{}'\n",
            sha256(manifest.as_bytes()),
            sha256(source.as_bytes())
        );
        let (registry, server) = serve(vec![
            ("200 OK".into(), index),
            ("200 OK".into(), source.into()),
            ("200 OK".into(), manifest.into()),
        ]);
        resolve_and_install(
            &root,
            &config,
            &["example@^1.0.0".into()],
            false,
            &registry,
            false,
            Some(&[]),
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&config).unwrap(), before);
        let lock: LockFile =
            toml::from_str(&fs::read_to_string(root.join("luat.lock")).unwrap()).unwrap();
        assert_eq!(lock.packages["example"].version, "1.1.0");
        install_locked(&root, &config, "http://127.0.0.1:1").unwrap();
        assert!(update(&root, &config, &["missing".into()], false).is_err());
        assert!(remove(&root, &config, &["missing".into()], false).is_err());
        remove(&root, &config, &["example".into()], false).unwrap();
        assert!(!root.join(".luat/packages/example/1.1.0").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
