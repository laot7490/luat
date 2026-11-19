use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
fn valid_component(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    if name.ends_with(['.', ' ']) || name.contains(|ch: char| ch < ' ' || ":<>\"|?*".contains(ch)) {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
}

#[cfg(not(windows))]
fn valid_component(_name: &std::ffi::OsStr) -> bool {
    true
}

fn create_project(root: &Path, name: &str, version: &str, out_dir: &str) -> Result<(), String> {
    let config = format!(
        "[project]\nname = {}\nversion = {}\n\n[compiler]\nout_dir = {}\n\n[fivem]\nfx_version = \"cerulean\"\ngame = \"gta5\"\nshared = [\"src/types/index.d.luat\", \"src/shared/config.luat\"]\nclient = [\"src/client/main.luat\"]\nserver = [\"src/server/main.luat\"]\n",
        toml::Value::String(name.to_owned()),
        toml::Value::String(version.to_owned()),
        toml::Value::String(out_dir.to_owned()),
    );
    let files = [
        ("luat.toml", config.as_bytes()),
        ("src/client/main.luat", b"".as_slice()),
        ("src/shared/config.luat", b"".as_slice()),
        ("src/server/main.luat", b"".as_slice()),
        ("src/types/index.d.luat", b"".as_slice()),
    ];
    let directories = [
        "",
        "src",
        "src/client",
        "src/shared",
        "src/server",
        "src/types",
    ];
    for (path, is_file) in directories
        .iter()
        .map(|path| (*path, false))
        .chain(files.iter().map(|(path, _)| (*path, true)))
    {
        let path = if path.is_empty() {
            root.to_path_buf()
        } else {
            root.join(path)
        };
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "error LT5301: Refusing to follow symlink {}",
                    path.display()
                ));
            }
            Ok(_) if is_file => {
                return Err(format!("error LT5301: {} already exists", path.display()));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!(
                    "error LT5301: {} is not a directory",
                    path.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("error LT5301: {}: {error}", path.display())),
        }
    }

    for directory in directories {
        let path = root.join(directory);
        fs::create_dir_all(&path)
            .map_err(|error| format!("error LT5301: {}: {error}", path.display()))?;
    }

    for (relative, content) in files {
        let path = root.join(relative);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("error LT5301: {}: {error}", path.display()))?;
        io::Write::write_all(&mut file, content)
            .map_err(|error| format!("error LT5301: {}: {error}", path.display()))?;
    }
    Ok(())
}

fn ask(
    prompt: &mut impl FnMut(&str, Option<&str>) -> io::Result<Option<String>>,
    label: &str,
    default: &str,
    invalid: &str,
    valid: impl Fn(&str) -> bool,
) -> Result<String, (u8, String)> {
    loop {
        let value = prompt(label, Some(default))
            .map_err(|error| (2, format!("error LT5301: {error}")))?
            .ok_or_else(|| {
                (
                    2,
                    "error LT5301: Input ended before project setup was complete".into(),
                )
            })?;
        if valid(&value) {
            return Ok(value);
        }
        luat::output::stderr("ERROR", invalid);
    }
}

fn valid_output(root: &Path, value: &str) -> bool {
    let mut components = Path::new(value).components().peekable();
    let Some(Component::Normal(first)) = components.peek() else {
        return false;
    };
    if [
        "src",
        ".luat",
        ".git",
        "luat.toml",
        "luat.lock",
        "fxmanifest.lua",
    ]
    .iter()
    .any(|reserved| first.eq_ignore_ascii_case(std::ffi::OsStr::new(reserved)))
    {
        return false;
    }
    let mut path = root.to_path_buf();
    for component in components {
        let Component::Normal(name) = component else {
            return false;
        };
        if !valid_component(name) {
            return false;
        }
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => return false,
        }
    }
    match fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(error) => error.kind() == io::ErrorKind::NotFound,
    }
}

pub(crate) fn run(args: &[String]) -> Result<(), (u8, String)> {
    let root = setup(Path::new("."), args, luat::output::read_prompt)?;
    luat::output::status(
        "SUCCESS",
        &format!("Created LuaT project in {}", root.display()),
    );
    luat::output::status(
        "INFO",
        "Run `luat check` to check your scripts or `luat build` to build the resource.",
    );
    Ok(())
}

fn setup(
    base: &Path,
    args: &[String],
    mut prompt: impl FnMut(&str, Option<&str>) -> io::Result<Option<String>>,
) -> Result<PathBuf, (u8, String)> {
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--pretty" && args.get(index + 1).is_some_and(|value| value == "false") {
            index += 2;
        } else {
            positional.push(args[index].clone());
            index += 1;
        }
    }
    let args = positional.as_slice();
    if args.len() > 1
        || args.first().is_some_and(|arg| {
            arg.starts_with('-')
                || Path::new(arg)
                    .file_name()
                    .is_none_or(|name| name != arg.as_str() || !valid_component(name))
        })
    {
        return Err((
            2,
            "error LT5001: Invalid arguments.\n  help: Usage: luat create [name] [--pretty false]"
                .to_owned(),
        ));
    }
    let root = args
        .first()
        .map_or_else(|| base.to_path_buf(), |name| base.join(name));
    let name = ask(
        &mut prompt,
        "name",
        args.first().map_or("luat-resource", String::as_str),
        "Enter a project name without path separators.",
        |value| !value.is_empty() && !value.contains(['/', '\\']),
    )?;
    let version = ask(
        &mut prompt,
        "version",
        "1.0.0",
        "Enter a valid semantic version.",
        |value| semver::Version::parse(value).is_ok(),
    )?;
    let out_dir = ask(
        &mut prompt,
        "out_dir",
        "dist",
        "Enter a new or empty output directory inside the project that does not overlap project files.",
        |value| valid_output(&root, value),
    )?;
    create_project(&root, &name, &version, &out_dir).map_err(|error| (2, error))?;
    Ok(root)
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
        super::create_project(&root, "luat-resource", "1.0.0", "dist").unwrap();

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
        assert!(super::create_project(&root, "example", "1.0.0", "dist").is_err());
        assert_eq!(
            fs::read_to_string(root.join("luat.toml")).unwrap(),
            "keep me"
        );
        fs::remove_file(root.join("luat.toml")).unwrap();
        fs::write(root.join("src/shared"), "keep me").unwrap();
        assert!(super::create_project(&root, "example", "1.0.0", "dist").is_err());
        assert_eq!(
            fs::read_to_string(root.join("src/shared")).unwrap(),
            "keep me"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn escapes_project_name_in_toml() {
        let root = temp_dir("Türkçe proje");
        fs::create_dir_all(&root).unwrap();
        super::create_project(&root, "Türkçe \"proje\"", "2.0.0", "build/dist").unwrap();
        let config: toml::Table =
            toml::from_str(&fs::read_to_string(root.join("luat.toml")).unwrap()).unwrap();
        assert_eq!(config["project"]["name"].as_str(), Some("Türkçe \"proje\""));
        assert_eq!(config["project"]["version"].as_str(), Some("2.0.0"));
        assert_eq!(config["compiler"]["out_dir"].as_str(), Some("build/dist"));
        assert!(luat::Project::load(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsafe_output_paths_and_existing_files() {
        let root = temp_dir("invalid-output");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("existing"), "keep me").unwrap();
        fs::create_dir(root.join("populated")).unwrap();
        fs::write(root.join("populated/data"), "keep me").unwrap();
        for output in [
            ".",
            "../dist",
            "dist/../other",
            "src",
            ".luat",
            "existing/child",
            "populated",
        ] {
            assert!(!super::valid_output(&root, output), "{output}");
        }
        assert!(super::valid_output(&root, "build/dist"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_devices_and_normalized_aliases() {
        let root = temp_dir("device-output");
        for output in [
            "NUL",
            "build/AUX.txt",
            "build/COM¹",
            "build/LpT9.log",
            "dist.",
        ] {
            assert!(!super::valid_output(&root, output), "{output}");
        }
        assert!(super::valid_output(&root, "build/COM10"));
        assert!(
            super::setup(&root, &["NUL.txt".into()], |_, _| {
                panic!("invalid directory names must be rejected before prompts");
            })
            .is_err()
        );
    }

    #[test]
    fn prompts_for_defaults_in_the_current_directory() {
        let root = temp_dir("create-current");
        let mut prompts = Vec::new();
        assert_eq!(
            super::setup(&root, &[], |label, default| {
                prompts.push((label.to_owned(), default.unwrap().to_owned()));
                Ok(default.map(str::to_owned))
            })
            .unwrap(),
            root
        );
        assert_eq!(
            prompts
                .iter()
                .map(|(label, default)| (label.as_str(), default.as_str()))
                .collect::<Vec<_>>(),
            [
                ("name", "luat-resource"),
                ("version", "1.0.0"),
                ("out_dir", "dist")
            ]
        );
        assert!(root.join("luat.toml").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retries_invalid_answers_and_keeps_the_named_target() {
        let root = temp_dir("create-named");
        let mut answers = [
            "a/b",
            "custom name",
            "latest",
            "2.3.4",
            "../dist",
            "build/dist",
        ]
        .into_iter();
        let target = super::setup(&root, &["my-app".to_owned()], |label, default| {
            if label == "name" {
                assert_eq!(default, Some("my-app"));
            }
            Ok(answers.next().map(str::to_owned))
        })
        .unwrap();
        assert_eq!(target, root.join("my-app"));
        assert!(answers.next().is_none());
        let config: toml::Table =
            toml::from_str(&fs::read_to_string(target.join("luat.toml")).unwrap()).unwrap();
        assert_eq!(config["project"]["name"].as_str(), Some("custom name"));
        assert_eq!(config["project"]["version"].as_str(), Some("2.3.4"));
        assert_eq!(config["compiler"]["out_dir"].as_str(), Some("build/dist"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_eof_before_creating_files() {
        let root = temp_dir("create-eof");
        let error = super::setup(&root, &[], |label, default| {
            Ok(if label == "name" {
                default.map(str::to_owned)
            } else {
                None
            })
        })
        .unwrap_err();
        assert!(error.1.contains("Input ended before project setup"));
        assert!(!root.exists());
    }
}
