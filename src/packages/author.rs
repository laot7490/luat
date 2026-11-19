use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn run(args: &[String]) -> Result<(), (u8, String)> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err((2, usage()));
    };
    let mut root = PathBuf::from(".");
    let mut positional = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--project" if i + 1 < args.len() && !args[i + 1].starts_with('-') => {
                root = args[i + 1].clone().into();
                i += 2;
            }
            "--pretty" if args.get(i + 1).is_some_and(|v| v == "false") => i += 2,
            v if !v.starts_with('-') => {
                positional.push(v);
                i += 1;
            }
            _ => return Err((2, usage())),
        }
    }
    match (command, positional.as_slice()) {
        ("init", []) => init(&root),
        ("check", []) => check_package(&root).map_err(err),
        _ => Err((2, usage())),
    }
}
fn usage() -> String {
    "error LT5001: Invalid package command arguments.\n  help: Usage: luat package <init|check> [--project <directory>] [--pretty false]".into()
}
pub fn check_package(root: &Path) -> Result<(), crate::BuildError> {
    let config_error = |message| crate::BuildError::Configuration {
        path: root.join("luat-package.toml"),
        message,
    };
    let manifest = super::read_manifest(root).map_err(config_error)?;
    super::manifest_files(root, &manifest).map_err(config_error)?;
    crate::Project::load(root.join("luat-package.toml"))
        .and_then(|project| project.check())
        .map(|_| ())
}
fn ask(label: &str, default: Option<&str>) -> Result<String, (u8, String)> {
    crate::output::read_prompt(label, default)
        .map_err(err)?
        .ok_or_else(|| err("input ended before package setup was complete"))
}
fn init(base: &Path) -> Result<(), (u8, String)> {
    let name = loop {
        let value = ask("Package name", None)?;
        if super::catalog::valid_name(&value) {
            break value;
        }
        crate::output::stderr(
            "ERROR",
            "Use lowercase letters, numbers, _ or - in package name.",
        );
    };
    let author = loop {
        let value = ask("Author", None)?;
        if !value.is_empty() {
            break value;
        }
        crate::output::stderr("ERROR", "Author cannot be empty.");
    };
    let description = ask("Description", None)?;
    let version = loop {
        let value = ask("Version", Some("1.0.0"))?;
        if semver::Version::parse(&value).is_ok() {
            break value;
        }
        crate::output::stderr("ERROR", "Enter a valid semantic version.");
    };
    let license = loop {
        let value = ask("License", Some("MIT"))?;
        if !value.is_empty() {
            break value;
        }
        crate::output::stderr("ERROR", "License cannot be empty.");
    };
    create(base, &name, &author, &description, &version, &license)
}
fn create(
    base: &Path,
    name: &str,
    author: &str,
    description: &str,
    version: &str,
    license: &str,
) -> Result<(), (u8, String)> {
    if !super::catalog::valid_name(name) {
        return Err(err("invalid package name"));
    }
    let root = base.join(name);
    if fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(err("target directory must not be a symlink"));
    }
    if root.exists() && (root.is_file() || root.read_dir().map_err(err)?.next().is_some()) {
        return Err(err("target directory must be empty"));
    }
    fs::create_dir_all(&root).map_err(err)?;
    let manifest = format!(
        "name = {name:?}\nversion = {version:?}\nauthor = {author:?}\ndescription = {description:?}\nlicense = {license:?}\n\nshared = [\"init.luat\"]\nclient = []\nserver = []\n\n[dependencies]\n"
    );
    fs::write(root.join("luat-package.toml"), manifest).map_err(err)?;
    fs::write(root.join("init.luat"), "return {}\n").map_err(err)?;
    crate::output::status("SUCCESS", &format!("Created {name}@{version}"));
    crate::output::status("INFO", "Edit the modules, then run `luat package check`.");
    Ok(())
}
fn err(error: impl std::fmt::Display) -> (u8, String) {
    (2, format!("error LT5501: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!(
                "author-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&root).unwrap();
        root
    }
    fn package() -> PathBuf {
        let base = temp();
        create(&base, "example", "Example", "", "1.0.0", "MIT").unwrap();
        let root = base.join("example");
        let text = fs::read_to_string(root.join("luat-package.toml"))
            .unwrap()
            .replace("author = \"\"", "author = \"Example\"");
        fs::write(root.join("luat-package.toml"), text).unwrap();
        root
    }
    #[test]
    fn init_creates_flat_package_from_prompt_answers() {
        let base = temp();
        create(&base, "example", "Author", "Description", "1.2.3", "MIT").unwrap();
        let root = base.join("example");
        let text = fs::read_to_string(root.join("luat-package.toml")).unwrap();
        assert!(text.contains("author = \"Author\""));
        assert!(text.contains("description = \"Description\""));
        assert!(text.contains("version = \"1.2.3\""));
        assert!(!text.contains("versions"));
        assert_eq!(
            fs::read_to_string(root.join("init.luat")).unwrap(),
            "return {}\n"
        );
        assert!(!root.join("1.0.0").exists());
        assert!(create(&base, "example", "Author", "", "1.0.0", "MIT").is_err());
        assert!(create(&base, "../escape", "Author", "", "1.0.0", "MIT").is_err());
    }
    #[test]
    fn check_is_read_only() {
        let root = package();
        let before = fs::read(root.join("luat-package.toml")).unwrap();
        check_package(&root).unwrap();
        assert_eq!(fs::read(root.join("luat-package.toml")).unwrap(), before);
        assert!(!root.join("target").exists());
        fs::write(root.join("init.luat"), "local function f() return {} end\n").unwrap();
        assert!(check_package(&root).is_err());
    }
}
