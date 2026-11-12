use std::{fs, io, path::Path};

pub(crate) fn create_project(root: &Path) -> Result<(), String> {
    let files = [
        "luat.toml",
        "src/client/main.luat",
        "src/shared/config.luat",
        "src/server/main.luat",
        "types/index.d.luat",
    ];
    let directories = ["src", "src/client", "src/shared", "src/server", "types"];
    for path in files.iter().chain(directories.iter()) {
        let path = root.join(path);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "error: refusing to follow symlink {}",
                    path.display()
                ));
            }
            Ok(metadata) if files.iter().any(|file| root.join(file) == path) => {
                let _ = metadata;
                return Err(format!("error: {} already exists", path.display()));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("error: {} is not a directory", path.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("error: {}: {error}", path.display())),
        }
    }

    fs::create_dir_all(root).map_err(|error| format!("error: {}: {error}", root.display()))?;
    for directory in directories {
        let path = root.join(directory);
        fs::create_dir_all(&path).map_err(|error| format!("error: {}: {error}", path.display()))?;
    }

    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("my-resource");
    let config = format!(
        "[project]\nname = {}\nversion = \"1.0.0\"\n\n[compiler]\nout_dir = \"dist\"\ndeclarations = [\"types/index.d.luat\"]\n\n[fivem]\nfx_version = \"cerulean\"\ngame = \"gta5\"\nshared = [\"src/shared/config.luat\"]\nclient = [\"src/client/main.luat\"]\nserver = [\"src/server/main.luat\"]\n",
        toml::Value::String(name.to_owned())
    );
    for (relative, content) in [
        ("luat.toml", config.as_bytes()),
        ("src/client/main.luat", b"".as_slice()),
        ("src/shared/config.luat", b"".as_slice()),
        ("src/server/main.luat", b"".as_slice()),
        ("types/index.d.luat", b"".as_slice()),
    ] {
        let path = root.join(relative);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("error: {}: {error}", path.display()))?;
        io::Write::write_all(&mut file, content)
            .map_err(|error| format!("error: {}: {error}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn run(args: &[String]) -> Result<(), (u8, String)> {
    if args.len() > 1 || args.first().is_some_and(|arg| arg.starts_with('-')) {
        return Err((2, "error: usage: luat create [directory]".to_owned()));
    }
    let root = args.first().map_or_else(|| ".", String::as_str);
    let root = std::path::PathBuf::from(root);
    create_project(&root).map_err(|error| (2, error))?;
    println!(
        "Created LuaT project in {}\nRun `luat check` to check your scripts or `luat build` to build the resource.",
        root.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::SystemTime};

    fn temp_dir(name: &str) -> PathBuf {
        std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "luat-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
    }

    #[test]
    fn creates_empty_sources_and_buildable_project() {
        let root = temp_dir("create");
        fs::create_dir_all(&root).unwrap();
        super::create_project(&root).unwrap();

        for path in [
            "src/client/main.luat",
            "src/shared/config.luat",
            "src/server/main.luat",
        ] {
            assert_eq!(fs::metadata(root.join(path)).unwrap().len(), 0);
        }
        let project = luat::Project::load(&root).unwrap();
        project.check().unwrap();
        project.build().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preserves_existing_files_and_rejects_non_directory_parent() {
        let root = temp_dir("collision");
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::write(root.join("luat.toml"), "keep me").unwrap();
        assert!(super::create_project(&root).is_err());
        assert_eq!(
            fs::read_to_string(root.join("luat.toml")).unwrap(),
            "keep me"
        );
        fs::remove_file(root.join("luat.toml")).unwrap();
        fs::write(root.join("src/shared"), "keep me").unwrap();
        assert!(super::create_project(&root).is_err());
        assert_eq!(
            fs::read_to_string(root.join("src/shared")).unwrap(),
            "keep me"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn escapes_directory_name_in_toml() {
        let root = temp_dir("Türkçe proje");
        fs::create_dir_all(&root).unwrap();
        super::create_project(&root).unwrap();
        assert!(luat::Project::load(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
