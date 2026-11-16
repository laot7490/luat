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
