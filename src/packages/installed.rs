use super::*;

#[derive(Clone, Debug)]
pub(crate) struct InstalledPackage {
    pub root: PathBuf,
    pub manifest: PackageManifest,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LockFile {
    pub(super) schema: u32,
    pub(super) packages: BTreeMap<String, LockedPackage>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LockedPackage {
    pub(super) version: String,
    pub(super) commit: String,
    pub(super) files: BTreeMap<String, String>,
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
