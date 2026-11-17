use super::*;
use crate::packages::{InstalledPackage, PackageManifest};

#[derive(Clone, Debug)]
pub(super) struct PackageSource {
    pub root: PathBuf,
    pub manifest: PackageManifest,
    pub sides: u8,
}

impl Project {
    pub(super) fn load_package(path: &Path, text: &str) -> Result<Self, BuildError> {
        let manifest: PackageManifest =
            toml::from_str(text).map_err(|error| BuildError::Configuration {
                path: path.into(),
                message: error.to_string(),
            })?;
        let root = path.parent().unwrap().to_path_buf();
        manifest
            .validate()
            .map_err(|message| BuildError::Configuration {
                path: path.into(),
                message,
            })?;
        let installed = crate::packages::installed_packages(&root, &manifest.dependencies)
            .map_err(|message| BuildError::Configuration {
                path: path.into(),
                message,
            })?;
        let config = ProjectConfig {
            project: config::ProjectMetadata {
                name: manifest.name.clone(),
                version: Some(manifest.version.clone()),
                author: Some(manifest.author.clone()),
                description: Some(manifest.description.clone()),
            },
            compiler: config::CompilerConfig::default(),
            fivem: FiveMConfig::default(),
            dependencies: manifest.dependencies.clone(),
        };
        let mut project = Self {
            config_path: path.into(),
            config,
            output_dir: root.join("target/package"),
            root: root.clone(),
            manifest: String::new(),
            warnings: vec![],
            declarations: vec![],
            sources: vec![],
            package_sources: HashMap::new(),
            package_declaration_roots: HashMap::new(),
            package_manifests: HashMap::new(),
            package_root: true,
        };
        project.attach_packages(installed)?;
        project.attach_packages(vec![InstalledPackage { root, manifest }])?;
        Ok(project)
    }

    pub(super) fn attach_packages(
        &mut self,
        packages: Vec<InstalledPackage>,
    ) -> Result<(), BuildError> {
        for package in packages {
            let classified = crate::packages::classified_files(&package.root, &package.manifest)
                .map_err(|message| BuildError::Configuration {
                    path: package.root.join("luat-package.toml"),
                    message,
                })?;
            self.package_manifests
                .insert(package.root.clone(), package.manifest.clone());
            for path in &classified[0] {
                self.package_declaration_roots
                    .insert(path.clone(), package.root.clone());
                if !self.declarations.contains(path) {
                    self.declarations.push(path.clone());
                    self.sources.push(SourceFile {
                        input: path.clone(),
                        output: None,
                    });
                }
            }
            for (side, paths) in classified[1..].iter().enumerate() {
                for path in paths {
                    if let Some(source) = self.package_sources.get_mut(path) {
                        source.sides |= 1 << side;
                        continue;
                    }
                    let relative = path.strip_prefix(&package.root).unwrap();
                    let output = PathBuf::from(format!(
                        "_luat/packages/{}/{}.lua",
                        package.manifest.name,
                        relative.to_string_lossy().replace('\\', "/")
                    ));
                    self.sources.push(SourceFile {
                        input: path.clone(),
                        output: Some(output),
                    });
                    self.package_sources.insert(
                        path.clone(),
                        PackageSource {
                            root: package.root.clone(),
                            manifest: package.manifest.clone(),
                            sides: 1 << side,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    pub(super) fn package_declaration_scope(
        &self,
        path: Option<&Path>,
    ) -> Option<HashSet<PathBuf>> {
        let root = path
            .and_then(|path| {
                self.package_sources
                    .get(path)
                    .map(|source| &source.root)
                    .or_else(|| self.package_declaration_roots.get(path))
            })
            .or_else(|| self.package_root.then_some(&self.root))?;
        let mut roots = HashSet::new();
        let mut pending = vec![root.clone()];
        while let Some(root) = pending.pop() {
            if !roots.insert(root.clone()) {
                continue;
            }
            if let Some(manifest) = self.package_manifests.get(&root) {
                for name in manifest.dependencies.keys() {
                    if let Some((root, _)) = self
                        .package_manifests
                        .iter()
                        .find(|(_, manifest)| &manifest.name == name)
                    {
                        pending.push(root.clone());
                    }
                }
            }
        }
        Some(roots)
    }

    pub(crate) fn editor_visible_paths(&self, requester: &Path) -> Option<HashSet<PathBuf>> {
        self.package_declaration_scope(Some(requester))
            .map(|roots| {
                self.sources
                    .iter()
                    .filter(|file| {
                        self.package_declaration_roots
                            .get(&file.input)
                            .or_else(|| {
                                self.package_sources
                                    .get(&file.input)
                                    .map(|source| &source.root)
                            })
                            .is_some_and(|root| roots.contains(root))
                    })
                    .map(|file| file.input.clone())
                    .collect()
            })
    }

    pub(super) fn module_id(&self, path: &Path) -> Option<String> {
        self.sources
            .iter()
            .find(|file| file.input == path)?
            .output
            .as_ref()
            .map(|output| {
                output
                    .with_extension("")
                    .to_string_lossy()
                    .replace('\\', "/")
            })
    }

    pub(super) fn validate_package_return(
        &self,
        path: &Path,
        source: &str,
    ) -> Result<(), BuildError> {
        if !self.package_sources.contains_key(path) {
            return Ok(());
        }
        if path.extension().is_some_and(|extension| extension == "lua") {
            let parsed = full_moon::parse_fallible(source, full_moon::LuaVersion::cfxlua());
            if !parsed.errors().is_empty() {
                return Err(BuildError::Configuration {
                    path: path.into(),
                    message: format!("Invalid Lua module syntax: {}", parsed.errors()[0]),
                });
            }
        }
        crate::compiler::validate_package_return(source).map_err(|error| BuildError::Compile {
            path: path.into(),
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!(
                "package-declarations-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("types")).unwrap();
        fs::write(
            root.join("types/local.d.luat"),
            "typedef ConsumerOnly = boolean",
        )
        .unwrap();
        fs::write(root.join("src/client/main.luat"), "").unwrap();
        fs::write(root.join("luat.toml"), "[project]\nname='test'\n[compiler]\ndeclarations=['types/*.d.luat']\n[fivem]\nclient=['src/client/main.luat']\n").unwrap();
        Project::load(root).unwrap()
    }

    fn attach(
        project: &mut Project,
        name: &str,
        declarations: &str,
        runtime: bool,
        dependencies: &[&str],
    ) -> PathBuf {
        let root = project.root.join(".luat/packages").join(name).join("1.0.0");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("api.d.luat"), declarations).unwrap();
        let manifest: PackageManifest = toml::from_str(&format!(
            "name='{name}'\nversion='1.0.0'\nauthor='test'\nlicense='MIT'\ndeclarations=['api.d.luat']\n{}\n[dependencies]\n{}",
            if runtime { "shared=['init.luat']" } else { "" },
            dependencies.iter().map(|name| format!("{name}='1.0.0'\n")).collect::<String>()
        )).unwrap();
        if runtime {
            fs::write(root.join("init.luat"), "return {}").unwrap();
        }
        project
            .attach_packages(vec![InstalledPackage {
                root: root.clone(),
                manifest,
            }])
            .unwrap();
        root
    }

    #[test]
    fn package_declarations_expose_aliases_including_runtime_packages_and_keep_contexts() {
        let mut project = project();
        attach(
            &mut project,
            "ambient",
            "typedef SharedId = number",
            false,
            &[],
        );
        attach(
            &mut project,
            "runtime",
            "---@client\ntypedef ClientRecord = { id: SharedId }",
            true,
            &["ambient"],
        );
        let path = project.root.join("src/client/main.luat");
        fs::write(&path, "const value: ClientRecord = { id = 1 }").unwrap();
        project.check_current().unwrap();
        fs::write(&path, "const value: ClientRecord = { id = 'wrong' }").unwrap();
        let error = project.check_current().unwrap_err();
        assert!(error.to_string().contains("LT2001"), "{error}");
        let declarations = project.read_declarations().unwrap();
        assert!(declarations.aliases.contains_key("SharedId"));
        assert!(
            declarations.scoped_aliases[&crate::ExecutionContext::Client]
                .contains_key("ClientRecord")
        );
        assert!(
            !declarations
                .scoped_aliases
                .get(&crate::ExecutionContext::Server)
                .is_some_and(|aliases| aliases.contains_key("ClientRecord"))
        );
        fs::remove_dir_all(project.root).unwrap();
    }

    #[test]
    fn package_declarations_follow_dependency_chain_without_consumer_or_sibling_types() {
        let mut project = project();
        attach(&mut project, "base", "typedef BaseId = number", false, &[]);
        attach(
            &mut project,
            "api",
            "typedef ApiId = BaseId",
            false,
            &["base"],
        );
        attach(
            &mut project,
            "sibling",
            "typedef SiblingOnly = string",
            false,
            &[],
        );
        let root = attach(
            &mut project,
            "runtime",
            "typedef OwnId = ApiId",
            true,
            &["api"],
        );
        let declarations = project
            .read_declarations_for(Some(&root.join("init.luat")), &HashMap::new())
            .unwrap();
        for name in ["BaseId", "ApiId", "OwnId"] {
            assert!(declarations.aliases.contains_key(name), "{name}");
        }
        for name in ["ConsumerOnly", "SiblingOnly"] {
            assert!(!declarations.aliases.contains_key(name), "{name}");
        }
        let declarations = project
            .read_declarations_for(Some(&root.join("api.d.luat")), &HashMap::new())
            .unwrap();
        assert!(declarations.aliases.contains_key("BaseId"));
        assert!(!declarations.aliases.contains_key("ConsumerOnly"));
        fs::remove_dir_all(project.root).unwrap();
    }

    #[test]
    fn package_declarations_reject_conflicting_public_aliases() {
        let mut project = project();
        attach(
            &mut project,
            "first",
            "typedef Duplicate = number",
            false,
            &[],
        );
        attach(
            &mut project,
            "second",
            "typedef Duplicate = string",
            true,
            &[],
        );
        assert!(
            project
                .read_declarations()
                .err()
                .unwrap()
                .to_string()
                .contains("conflicting declarations")
        );
        fs::remove_dir_all(project.root).unwrap();
    }

    #[test]
    fn package_declarations_cannot_resolve_undeclared_sibling_aliases() {
        let mut project = project();
        attach(
            &mut project,
            "sibling",
            "typedef SiblingOnly = number",
            false,
            &[],
        );
        attach(
            &mut project,
            "api",
            "typedef OwnId = SiblingOnly",
            false,
            &[],
        );
        assert!(
            project
                .read_declarations()
                .err()
                .unwrap()
                .to_string()
                .contains("Unknown type 'SiblingOnly'")
        );
        fs::remove_dir_all(project.root).unwrap();
    }

    #[test]
    fn emitted_package_modules_are_escrow_ignored_with_custom_output_directory() {
        let mut project = project();
        let base = attach(&mut project, "base", "typedef BaseId = number", true, &[]);
        let api = attach(
            &mut project,
            "api",
            "typedef ApiId = number",
            true,
            &["base"],
        );
        attach(
            &mut project,
            "unused",
            "typedef UnusedId = number",
            true,
            &[],
        );
        fs::write(base.join("init.luat"), "return { value = 1 }").unwrap();
        fs::write(api.join("init.luat"), "return import('base')").unwrap();
        fs::write(
            project.root.join("src/client/main.luat"),
            "const api = import('api')\nconst value: number = api.value",
        )
        .unwrap();
        project
            .config
            .dependencies
            .insert("api".into(), "1.0.0".into());
        project.config.compiler.out_dir = "custom-output".into();
        project.output_dir = project.root.join("custom-output");
        project.config.fivem.escrow_ignore = vec!["user-owned.lua".into()];
        project.build_current().unwrap();
        let manifest = fs::read_to_string(project.root.join("fxmanifest.lua")).unwrap();
        let ignores = manifest
            .split("escrow_ignore {")
            .nth(1)
            .unwrap()
            .split('}')
            .next()
            .unwrap();
        assert!(ignores.contains("user-owned.lua"));
        for name in ["api", "base"] {
            let emitted = format!("custom-output/_luat/packages/{name}/init.luat.lua");
            assert!(project.root.join(&emitted).is_file(), "{emitted}");
            assert!(ignores.contains(&emitted), "{ignores}");
        }
        assert!(!ignores.contains("client/main.lua"));
        assert!(!ignores.contains("unused"));
        assert!(!project.output_dir.join("_luat/packages/unused").exists());
        fs::remove_dir_all(project.root).unwrap();
    }
}
