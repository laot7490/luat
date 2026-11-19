use super::*;

impl Project {
    pub(super) fn publish(
        &self,
        outputs: &[(PathBuf, String)],
        manifest: &str,
    ) -> Result<(), BuildError> {
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

impl Project {
    fn validate_watch_path(&self, path: &Path) -> Result<(), BuildError> {
        for component in path.ancestors().take_while(|part| *part != self.root) {
            if let Some(meta) = metadata(component)?
                && is_link(&meta)
            {
                return Err(BuildError::Configuration {
                    path: component.to_path_buf(),
                    message: "symlink or junction is not allowed in watch output".into(),
                });
            }
        }
        Ok(())
    }

    pub(super) fn publish_watch(
        &self,
        outputs: &[(PathBuf, String)],
        manifest: &str,
        previous: Option<&[(PathBuf, String)]>,
    ) -> Result<Vec<OutputChange>, BuildError> {
        use std::collections::BTreeMap;

        let mut expected: BTreeMap<_, _> = outputs
            .iter()
            .map(|(path, contents)| (self.output_dir.join(path), contents.as_bytes()))
            .collect();
        expected.insert(self.root.join("fxmanifest.lua"), manifest.as_bytes());
        let mut old_files = BTreeSet::new();
        let mut old_dirs = BTreeSet::new();
        if let Some(previous) = previous {
            for (path, _) in previous {
                let mut path = self.output_dir.join(path);
                old_files.insert(path.clone());
                while path.pop() && path != self.output_dir {
                    old_dirs.insert(path.clone());
                }
            }
        } else if self.output_dir.is_dir() {
            self.validate_watch_path(&self.output_dir)?;
            inventory(&self.output_dir, &mut old_files, &mut old_dirs)?;
        }

        old_dirs.retain(|dir| {
            !expected
                .range::<Path, _>((
                    std::ops::Bound::Excluded(dir.as_path()),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .is_some_and(|(path, _)| path.starts_with(dir))
        });
        let mut changes = BTreeMap::new();
        for path in old_files
            .iter()
            .filter(|path| !expected.contains_key(*path))
        {
            self.validate_watch_path(path)?;
            if metadata(path)?.is_some_and(|metadata| !metadata.is_dir()) {
                changes.insert(
                    path.clone(),
                    (None, OutputChange::Removed(relative(&self.root, path))),
                );
            }
        }
        #[cfg(windows)]
        let removed_names: HashSet<_> = changes
            .keys()
            .map(|path| path.to_string_lossy().to_lowercase())
            .collect();
        for (path, contents) in expected {
            self.validate_watch_path(&path)?;
            let existing = metadata(&path)?.is_some_and(|metadata| metadata.is_file());
            #[cfg(windows)]
            let renamed = removed_names.contains(&path.to_string_lossy().to_lowercase());
            #[cfg(not(windows))]
            let renamed = false;
            let existing = existing && !renamed;
            if existing && io_at(&path, fs::read(&path))? == contents {
                continue;
            }
            let relative = relative(&self.root, &path);
            let change = if existing {
                OutputChange::Updated(relative)
            } else {
                OutputChange::Added(relative)
            };
            changes.insert(path, (Some(contents), change));
        }

        let mut created = Vec::new();
        if changes.is_empty() && old_dirs.is_empty() && self.output_dir.is_dir() {
            return Ok(Vec::new());
        }
        let staging = unique_sibling(&self.root.join("fxmanifest.lua"), "watch")?;
        io_at(&staging, fs::create_dir(&staging))?;
        let mut saved = Vec::new();
        let mut installed = Vec::new();
        let mut removed_dirs = Vec::new();
        let result = (|| {
            for (index, (_, (contents, _))) in changes.iter().enumerate() {
                if let Some(contents) = contents {
                    let staged = staging.join(format!("new-{index}"));
                    io_at(&staged, fs::write(&staged, contents))?;
                }
            }
            for (index, (path, (_, change))) in changes.iter().enumerate() {
                if !matches!(change, OutputChange::Added(_)) {
                    let backup = staging.join(format!("old-{index}"));
                    io_at(path, fs::rename(path, &backup))?;
                    saved.push((path, backup));
                }
            }
            for path in old_dirs.iter().rev() {
                self.validate_watch_path(path)?;
                match fs::remove_dir(path) {
                    Ok(()) => removed_dirs.push(path),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::DirectoryNotEmpty
                                | io::ErrorKind::NotFound
                                | io::ErrorKind::NotADirectory
                        ) => {}
                    Err(error) => io_at(path, Err(error))?,
                }
            }
            create_dirs(&self.output_dir, &mut created)?;
            for (index, (path, (contents, _))) in changes.iter().enumerate() {
                if contents.is_some() {
                    create_dirs(path.parent().expect("output has a parent"), &mut created)?;
                    io_at(path, fs::rename(staging.join(format!("new-{index}")), path))?;
                    installed.push(path);
                }
            }
            Ok::<_, BuildError>(())
        })();
        if let Err(error) = result {
            let mut failure = None;
            for path in installed.into_iter().rev() {
                if let Err(err) = io_at(path, fs::remove_file(path)) {
                    failure = Some(err);
                }
            }
            for path in created.iter().rev() {
                if let Err(err) = io_at(path, fs::remove_dir(path)) {
                    failure = Some(err);
                }
            }
            for path in removed_dirs.into_iter().rev() {
                if let Err(err) = io_at(path, fs::create_dir(path)) {
                    failure = Some(err);
                }
            }
            for (path, backup) in saved.into_iter().rev() {
                if let Err(err) = io_at(path, fs::rename(backup, path)) {
                    failure = Some(err);
                }
            }
            if let Some(failure) = failure {
                return Err(BuildError::Configuration {
                    path: staging,
                    message: format!(
                        "could not publish watch output ({error}); rollback failed ({failure}); inspect this directory for backups"
                    ),
                });
            }
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        if let Err(error) = fs::remove_dir_all(&staging) {
            crate::output::warning(&format!(
                "Build succeeded, but the output backup remains at {} ({error}).",
                staging.display()
            ));
        }
        Ok(changes.into_values().map(|(_, change)| change).collect())
    }
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .expect("output is inside the project")
        .to_path_buf()
}

fn io_at<T>(path: &Path, result: io::Result<T>) -> Result<T, BuildError> {
    result.map_err(|source| BuildError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn metadata(path: &Path) -> Result<Option<fs::Metadata>, BuildError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => io_at(path, Err(error)),
    }
}

fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.is_symlink()
}

fn inventory(
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
    dirs: &mut BTreeSet<PathBuf>,
) -> Result<(), BuildError> {
    for entry in io_at(directory, fs::read_dir(directory))? {
        let entry = io_at(directory, entry)?;
        let path = entry.path();
        let meta = io_at(&path, fs::symlink_metadata(&path))?;
        if meta.is_dir() && !is_link(&meta) {
            dirs.insert(path.clone());
            inventory(&path, files, dirs)?;
        } else {
            files.insert(path);
        }
    }
    Ok(())
}

fn create_dirs(path: &Path, created: &mut Vec<PathBuf>) -> Result<(), BuildError> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_dirs(parent, created)?;
    }
    io_at(path, fs::create_dir(path))?;
    created.push(path.to_path_buf());
    Ok(())
}
