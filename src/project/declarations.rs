use super::*;

impl Project {
    #[cfg(test)]
    pub(super) fn read_declarations(&self) -> Result<std::sync::Arc<DeclarationSet>, BuildError> {
        self.read_editor_declarations(&HashMap::new(), &mut HashMap::new())
    }

    pub(crate) fn read_editor_declarations(
        &self,
        overlays: &HashMap<PathBuf, String>,
        cache: &mut ScopedDeclarations,
    ) -> Result<std::sync::Arc<DeclarationSet>, BuildError> {
        let mut checked = HashSet::new();
        for (path, root) in &self.package_declaration_roots {
            if !(self.package_root && root == &self.root) && checked.insert(root) {
                self.cached_declarations(Some(path), overlays, cache)?;
            }
        }
        self.cached_declarations(None, overlays, cache)
    }

    pub(super) fn read_declarations_for(
        &self,
        path: Option<&Path>,
        overlays: &HashMap<PathBuf, String>,
    ) -> Result<DeclarationSet, BuildError> {
        self.parse_declarations(&self.declaration_inputs(path, overlays)?)
    }

    pub(super) fn populate_declarations(
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

    pub(super) fn declaration_owner(&self, path: Option<&Path>) -> Option<PathBuf> {
        path.and_then(|path| {
            self.package_sources
                .get(path)
                .map(|source| &source.root)
                .or_else(|| self.package_declaration_roots.get(path))
        })
        .or_else(|| self.package_root.then_some(&self.root))
        .cloned()
    }

    pub(super) fn cached_declarations(
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
                text: crate::definitions::FIVEM_CLIENT.to_owned(),
            },
            DeclarationSource {
                path: None,
                context: Some(crate::ExecutionContext::Server),
                text: crate::definitions::FIVEM_SERVER.to_owned(),
            },
            DeclarationSource {
                path: None,
                context: None,
                text: crate::definitions::FIVEM_SHARED.to_owned(),
            },
            DeclarationSource {
                path: None,
                context: None,
                text: crate::definitions::RUNTIME_SHARED.to_owned(),
            },
            DeclarationSource {
                path: None,
                context: Some(crate::ExecutionContext::Client),
                text: crate::definitions::RUNTIME_CLIENT.to_owned(),
            },
            DeclarationSource {
                path: None,
                context: Some(crate::ExecutionContext::Server),
                text: crate::definitions::RUNTIME_SERVER.to_owned(),
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
}
