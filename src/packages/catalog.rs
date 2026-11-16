use super::{MAX_FILE, MAX_FILES, MAX_INDEX, MAX_PACKAGE, validate_relative_file};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageManifest {
    pub name: String,
    pub version: String,
    pub author: String,
    #[serde(default)]
    pub description: String,
    pub license: String,
    #[serde(default)]
    pub declarations: Vec<String>,
    #[serde(default)]
    pub shared: Vec<String>,
    #[serde(default)]
    pub client: Vec<String>,
    #[serde(default)]
    pub server: Vec<String>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}
impl PackageManifest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name)
            || self.author.trim().is_empty()
            || self.license.trim().is_empty()
        {
            return Err("package name, author and license must be valid and non-empty".into());
        }
        Version::parse(&self.version).map_err(|e| e.to_string())?;
        validate_dependencies(&self.dependencies)?;
        for p in self
            .declarations
            .iter()
            .chain(&self.shared)
            .chain(&self.client)
            .chain(&self.server)
        {
            if p.is_empty()
                || p.contains(['\\', ':'])
                || p.starts_with('/')
                || p.split('/').any(|s| matches!(s, ".." | "." | ""))
            {
                return Err(format!("invalid package pattern '{p}'"));
            }
            glob::Pattern::new(p).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageIndex {
    pub schema: u32,
    pub name: String,
    pub versions: BTreeMap<String, PackageVersion>,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackageVersion {
    pub commit: String,
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
}
impl PackageVersion {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.commit.len() != 40
            || !self
                .commit
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("invalid package source commit".into());
        }
        validate_dependencies(&self.dependencies)?;
        if !self.files.contains_key("luat-package.toml") || self.files.len() > MAX_FILES {
            return Err("package must include luat-package.toml within file limit".into());
        }
        for (p, h) in &self.files {
            validate_relative_file(p)?;
            if h.len() != 64
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(format!("invalid SHA-256 for {p}"));
            }
        }
        Ok(())
    }
}
impl PackageIndex {
    pub(crate) fn validate(&self, expected: Option<&str>, _published: bool) -> Result<(), String> {
        if self.schema != 2
            || !valid_name(&self.name)
            || expected.is_some_and(|n| n != self.name)
            || self.versions.len() > 1024
        {
            return Err("invalid package catalog identity/schema".into());
        }
        for (v, r) in &self.versions {
            Version::parse(v).map_err(|e| e.to_string())?;
            r.validate()?;
        }
        Ok(())
    }
}
pub(crate) fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && super::safe_file_component(n)
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}
pub(crate) fn require_versions(raw: &str) -> Result<VersionReq, String> {
    if raw.trim() != raw || raw.is_empty() {
        return Err(format!("invalid version requirement '{raw}'"));
    }
    let text = if Version::parse(raw).is_ok() {
        format!("={raw}")
    } else {
        raw.to_owned()
    };
    VersionReq::parse(&text).map_err(|e| format!("invalid version requirement '{raw}': {e}"))
}
pub(crate) fn validate_dependencies(deps: &BTreeMap<String, String>) -> Result<(), String> {
    if deps.len() > MAX_FILES {
        return Err("too many package dependencies".into());
    }
    for (n, r) in deps {
        if !valid_name(n) {
            return Err(format!("invalid package name '{n}'"));
        }
        require_versions(r)?;
    }
    Ok(())
}

pub(crate) fn solve_versions(
    indexes: &BTreeMap<String, PackageIndex>,
    roots: &BTreeMap<String, String>,
    preferred: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    fn search(
        indexes: &BTreeMap<String, PackageIndex>,
        constraints: BTreeMap<String, Vec<String>>,
        selected: BTreeMap<String, String>,
        preferred: &BTreeMap<String, String>,
        cycle_error: &mut Option<String>,
    ) -> Option<BTreeMap<String, String>> {
        for (name, version) in &selected {
            let parsed = Version::parse(version).ok()?;
            if constraints.get(name).is_some_and(|requirements| {
                requirements
                    .iter()
                    .any(|raw| !require_versions(raw).is_ok_and(|req| req.matches(&parsed)))
            }) {
                return None;
            }
        }
        if let Some(name) = constraints
            .keys()
            .find(|name| !selected.contains_key(*name))
        {
            let index = indexes.get(name)?;
            let requirements = constraints[name]
                .iter()
                .map(|raw| require_versions(raw).ok())
                .collect::<Option<Vec<_>>>()?;
            let mut versions = index
                .versions
                .keys()
                .filter_map(|raw| {
                    Version::parse(raw)
                        .ok()
                        .map(|version| (version, raw.clone()))
                })
                .collect::<Vec<_>>();
            versions.sort_by(|a, b| {
                (preferred.get(name) == Some(&b.1))
                    .cmp(&(preferred.get(name) == Some(&a.1)))
                    .then_with(|| b.0.cmp(&a.0))
            });
            for (_, version) in versions
                .into_iter()
                .filter(|(v, _)| requirements.iter().all(|req| req.matches(v)))
            {
                let mut next = selected.clone();
                next.insert(name.clone(), version.clone());
                let mut pending = constraints.clone();
                for (dependency, requirement) in &index.versions[&version].dependencies {
                    pending
                        .entry(dependency.clone())
                        .or_default()
                        .push(requirement.clone());
                }
                if let Some(result) = search(indexes, pending, next, preferred, cycle_error) {
                    return Some(result);
                }
            }
            None
        } else {
            let mut complete = std::collections::HashSet::new();
            for name in selected.keys() {
                if let Err(error) =
                    check_cycles(name, indexes, &selected, &mut Vec::new(), &mut complete)
                {
                    *cycle_error = Some(error);
                    return None;
                }
            }
            Some(selected)
        }
    }
    let mut constraints = BTreeMap::new();
    for (name, requirement) in roots {
        constraints
            .entry(name.clone())
            .or_insert_with(Vec::new)
            .push(requirement.clone());
    }
    let mut cycle_error = None;
    let selected = search(
        indexes,
        constraints,
        BTreeMap::new(),
        preferred,
        &mut cycle_error,
    )
    .ok_or_else(|| {
        cycle_error.unwrap_or_else(|| {
            "no compatible package versions satisfy the dependency requirements".to_owned()
        })
    })?;
    fn check_cycles(
        name: &str,
        indexes: &BTreeMap<String, PackageIndex>,
        selected: &BTreeMap<String, String>,
        active: &mut Vec<String>,
        complete: &mut std::collections::HashSet<String>,
    ) -> Result<(), String> {
        if complete.contains(name) {
            return Ok(());
        }
        if let Some(start) = active.iter().position(|entry| entry == name) {
            let mut cycle = active[start..].to_vec();
            cycle.push(name.to_owned());
            return Err(format!("dependency cycle: {}", cycle.join(" -> ")));
        }
        active.push(name.to_owned());
        let version = &selected[name];
        for dependency in indexes[name].versions[version].dependencies.keys() {
            check_cycles(dependency, indexes, selected, active, complete)?;
        }
        active.pop();
        complete.insert(name.to_owned());
        Ok(())
    }
    let mut complete = std::collections::HashSet::new();
    for name in selected.keys() {
        check_cycles(name, indexes, &selected, &mut Vec::new(), &mut complete)?;
    }
    Ok(selected)
}

#[cfg(test)]
mod solver_tests {
    use super::*;

    fn release(_version: &str, dependencies: BTreeMap<String, String>) -> PackageVersion {
        PackageVersion {
            commit: "0".repeat(40),
            dependencies,
            files: BTreeMap::from([("luat-package.toml".into(), "0".repeat(64))]),
        }
    }

    #[test]
    fn solver_backtracks_to_find_shared_compatible_version() {
        let indexes = BTreeMap::from([
            (
                "a".into(),
                PackageIndex {
                    schema: 2,
                    name: "a".into(),
                    versions: BTreeMap::from([
                        (
                            "2.0.0".into(),
                            release("2.0.0", BTreeMap::from([("b".into(), "^2.0.0".into())])),
                        ),
                        (
                            "1.0.0".into(),
                            release("1.0.0", BTreeMap::from([("b".into(), "^1.0.0".into())])),
                        ),
                    ]),
                },
            ),
            (
                "b".into(),
                PackageIndex {
                    schema: 2,
                    name: "b".into(),
                    versions: BTreeMap::from([
                        ("2.0.0".into(), release("2.0.0", BTreeMap::new())),
                        ("1.0.0".into(), release("1.0.0", BTreeMap::new())),
                    ]),
                },
            ),
        ]);
        let roots = BTreeMap::from([("a".into(), "*".into()), ("b".into(), "^1.0.0".into())]);
        let versions = solve_versions(&indexes, &roots, &BTreeMap::new()).unwrap();
        assert_eq!(versions["a"], "1.0.0");
        assert_eq!(versions["b"], "1.0.0");
    }

    #[test]
    fn solver_rejects_dependency_cycles_with_the_chain() {
        let indexes = BTreeMap::from([
            (
                "a".into(),
                PackageIndex {
                    schema: 2,
                    name: "a".into(),
                    versions: BTreeMap::from([(
                        "1.0.0".into(),
                        release("1.0.0", BTreeMap::from([("b".into(), "*".into())])),
                    )]),
                },
            ),
            (
                "b".into(),
                PackageIndex {
                    schema: 2,
                    name: "b".into(),
                    versions: BTreeMap::from([(
                        "1.0.0".into(),
                        release("1.0.0", BTreeMap::from([("a".into(), "*".into())])),
                    )]),
                },
            ),
        ]);
        let error = solve_versions(
            &indexes,
            &BTreeMap::from([("a".into(), "*".into())]),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert_eq!(error, "dependency cycle: a -> b -> a");
    }

    #[test]
    fn solver_allows_explicit_prereleases_and_backtracks_out_of_cycles() {
        let mut indexes = BTreeMap::from([(
            "a".into(),
            PackageIndex {
                schema: 2,
                name: "a".into(),
                versions: BTreeMap::from([
                    (
                        "2.0.0".into(),
                        release("2.0.0", BTreeMap::from([("a".into(), "*".into())])),
                    ),
                    ("1.0.0".into(), release("1.0.0", BTreeMap::new())),
                    (
                        "3.0.0-beta.1".into(),
                        release("3.0.0-beta.1", BTreeMap::new()),
                    ),
                ]),
            },
        )]);
        let result = solve_versions(
            &indexes,
            &BTreeMap::from([("a".into(), "*".into())]),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(result["a"], "1.0.0");
        let result = solve_versions(
            &indexes,
            &BTreeMap::from([("a".into(), "3.0.0-beta.1".into())]),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(result["a"], "3.0.0-beta.1");
        indexes
            .get_mut("a")
            .unwrap()
            .versions
            .get_mut("2.0.0")
            .unwrap()
            .dependencies
            .clear();
        let result = solve_versions(
            &indexes,
            &BTreeMap::from([("a".into(), ">=2".into())]),
            &BTreeMap::from([("a".into(), "1.0.0".into())]),
        )
        .unwrap();
        assert_eq!(result["a"], "2.0.0");
    }

    #[test]
    fn solver_preserves_a_compatible_locked_release() {
        let index = PackageIndex {
            schema: 2,
            name: "example".into(),
            versions: BTreeMap::from([
                ("2.0.0".into(), release("2.0.0", BTreeMap::new())),
                ("1.0.0".into(), release("1.0.0", BTreeMap::new())),
            ]),
        };
        let versions = solve_versions(
            &BTreeMap::from([("example".into(), index)]),
            &BTreeMap::from([("example".into(), "*".into())]),
            &BTreeMap::from([("example".into(), "1.0.0".into())]),
        )
        .unwrap();
        assert_eq!(versions["example"], "1.0.0");
    }
}
pub(crate) fn regular_file(p: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(p)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_file()
    {
        return Err(format!(
            "expected regular file without symlinks: {}",
            p.display()
        ));
    }
    Ok(())
}
pub(crate) fn directory(p: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(p)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_dir()
    {
        return Err(format!(
            "expected directory without symlinks: {}",
            p.display()
        ));
    }
    Ok(())
}
pub(crate) fn read_manifest(root: &Path) -> Result<PackageManifest, String> {
    let p = root.join("luat-package.toml");
    regular_file(&p)?;
    if fs::metadata(&p).map_err(|e| e.to_string())?.len() > MAX_INDEX {
        return Err("package manifest too large".into());
    }
    let m: PackageManifest = toml::from_str(&fs::read_to_string(p).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    m.validate()?;
    Ok(m)
}
fn scan(
    root: &Path,
    dir: &Path,
    inherited: &[(String, String)],
    found: &mut BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    directory(dir)?;
    let base = dir
        .strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    let mut ignores = inherited.to_vec();
    if dir.join(".gitignore").is_file() {
        regular_file(&dir.join(".gitignore"))?;
        for l in fs::read_to_string(dir.join(".gitignore"))
            .map_err(|e| e.to_string())?
            .lines()
        {
            let l = l.trim();
            if !l.is_empty() && !l.starts_with('#') {
                ignores.push((base.clone(), l.to_owned()));
            }
        }
    }
    for e in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let p = e.path();
        let rel = p
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .ok_or("package paths must be UTF-8")?
            .replace('\\', "/");
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(
            name.as_str(),
            ".git" | ".luat" | "target" | "node_modules" | "dist" | "build"
        ) {
            continue;
        }
        let mut ignored = false;
        for (base, rule) in &ignores {
            let local = if base.is_empty() {
                Some(rel.as_str())
            } else {
                rel.strip_prefix(&format!("{base}/"))
            };
            let Some(local) = local else {
                continue;
            };
            let (neg, r) = rule
                .strip_prefix('!')
                .map_or((false, rule.as_str()), |r| (true, r));
            let r = r.trim_start_matches('/').trim_end_matches('/');
            if glob::Pattern::new(r)
                .is_ok_and(|g| g.matches(local) || (!r.contains('/') && g.matches(&name)))
            {
                ignored = !neg;
            }
        }
        if ignored {
            continue;
        }
        let t = e.file_type().map_err(|e| e.to_string())?;
        if t.is_symlink() {
            return Err(format!("symlink rejected: {}", p.display()));
        }
        if t.is_dir() {
            scan(root, &p, &ignores, found)?;
        } else if rel.ends_with(".luat") || rel.ends_with(".lua") {
            regular_file(&p)?;
            validate_relative_file(&rel)?;
            found.insert(rel, p);
            if found.len() >= MAX_FILES {
                return Err("too many package source files".into());
            }
        }
    }
    Ok(())
}
pub(crate) fn classified_files(
    root: &Path,
    m: &PackageManifest,
) -> Result<[Vec<PathBuf>; 4], String> {
    m.validate()?;
    let mut sources = BTreeMap::new();
    scan(root, root, &[], &mut sources)?;
    let mut out: [Vec<PathBuf>; 4] = Default::default();
    let mut assigned = BTreeSet::new();
    for (side, patterns) in [&m.declarations, &m.shared, &m.client, &m.server]
        .into_iter()
        .enumerate()
    {
        for p in patterns {
            let g = glob::Pattern::new(p).map_err(|e| e.to_string())?;
            let mut matched = false;
            for (rel, path) in &sources {
                if !g.matches(rel) {
                    continue;
                }
                matched = true;
                if rel.ends_with(".d.luat") != (side == 0) {
                    return Err(format!(
                        "'{rel}' has invalid declaration/runtime classification"
                    ));
                }
                if out[side].contains(path) {
                    continue;
                }
                if !assigned.insert(rel.clone()) {
                    return Err(format!("'{rel}' is classified in multiple contexts"));
                }
                out[side].push(path.clone());
            }
            if !matched {
                return Err(format!("package pattern '{p}' matched no files"));
            }
        }
    }
    if let Some(p) = sources.keys().find(|p| !assigned.contains(*p)) {
        return Err(format!("unclassified package source '{p}'"));
    }
    if assigned.is_empty() {
        return Err("package contains no modules or declarations".into());
    }
    Ok(out)
}
pub(crate) fn manifest_files(
    root: &Path,
    m: &PackageManifest,
) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut files = BTreeMap::new();
    regular_file(&root.join("luat-package.toml"))?;
    files.insert("luat-package.toml".into(), root.join("luat-package.toml"));
    for p in classified_files(root, m)?.into_iter().flatten() {
        let rel = p
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_str()
            .ok_or("package paths must be UTF-8")?
            .replace('\\', "/");
        files.insert(rel, p);
    }
    let (mut total, mut names) = (0, BTreeSet::new());
    for (rel, p) in &files {
        if !names.insert(rel.to_ascii_lowercase()) {
            return Err("package paths collide ignoring case".into());
        }
        let b = fs::read(p).map_err(|e| e.to_string())?;
        total += b.len();
        if b.len() as u64 > MAX_FILE || total > MAX_PACKAGE {
            return Err("package size limit exceeded".into());
        }
        std::str::from_utf8(&b).map_err(|e| e.to_string())?;
    }
    Ok(files)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unified_metadata_and_exact_requirements() {
        let m: PackageManifest =
            toml::from_str("name='example'\nversion='1.0.0'\nauthor='Test'\nlicense='MIT'\n")
                .unwrap();
        assert!(m.validate().is_ok());
        assert!(
            !require_versions("1.2.0")
                .unwrap()
                .matches(&Version::parse("1.3.0").unwrap())
        );
        assert!(
            require_versions("^1.2.0")
                .unwrap()
                .matches(&Version::parse("1.3.0").unwrap())
        );
        assert!(require_versions("1 || 2").is_err());
        assert!(!valid_name("@types/example"));
    }
}
