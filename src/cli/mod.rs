use luat::{BuildError, Project};
use std::{env, io::IsTerminal, path::PathBuf, time::Instant};

mod create;
mod render;
mod watch;
use render::{Color, paint};

fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

pub(crate) fn run(args: Vec<String>) -> Result<(), (u8, String)> {
    if args
        .first()
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!("\nluat {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        println!("\n{}", usage());
        return Ok(());
    }
    let Some(command) = args.first().map(String::as_str) else {
        println!("\n{}", usage());
        return Ok(());
    };
    if command == "package"
        && args
            .get(1)
            .is_some_and(|s| !matches!(s.as_str(), "init" | "check" | "--help" | "-h"))
    {
        return Err((
            2,
            format!(
                "error LT5001: Unknown package command '{}'\n{}",
                args[1],
                command_usage("package")
            ),
        ));
    }
    if args[1..].iter().any(|a| a == "--help" || a == "-h")
        && matches!(
            command,
            "check"
                | "build"
                | "watch"
                | "create"
                | "package"
                | "add"
                | "install"
                | "update"
                | "remove"
        )
    {
        println!("{}", command_usage(command));
        return Ok(());
    }
    if command == "package" && args.get(1).is_some_and(|a| a == "check") {
        let (path, pretty, positional) = luat::packages::parse_options(&args[2..])?;
        if !positional.is_empty() {
            return Err((
                2,
                format!("error LT5001: Unexpected argument '{}'", positional[0]),
            ));
        }
        let root = if path.is_dir() {
            path
        } else {
            if path.file_name().is_none_or(|n| n != "luat-package.toml") {
                return Err((
                    2,
                    "error LT5001: Package check requires luat-package.toml".into(),
                ));
            }
            path.parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_path_buf()
        };
        luat::packages::author::check_package(&root).map_err(|error| {
            let code = match &error {
                BuildError::Compile { .. } | BuildError::CompileMany(_) => 1,
                _ => 2,
            };
            (code, render::render_build_error(&error, pretty))
        })?;
        println!("Success: Checked package in {}", root.display());
        return Ok(());
    }
    if command == "create" {
        return create::run(&args[1..]);
    }
    if command == "package" {
        return luat::packages::author::run(&args[1..]);
    }
    if command == "watch" {
        return watch::run(&args[1..]);
    }
    if matches!(command, "add" | "install" | "update" | "remove") {
        let mut package_args = vec![command.to_owned()];
        package_args.extend_from_slice(&args[1..]);
        return luat::packages::run(&package_args);
    }
    if !matches!(command, "check" | "build") {
        return Err((
            2,
            format!("error LT5001: Unknown command '{command}'\n\n{}", usage()),
        ));
    }
    let mut project_path = PathBuf::from(".");
    let mut pretty = true;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--project" if index + 1 < args.len() && !args[index + 1].starts_with('-') => {
                project_path = PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--project" => return Err((2, "error LT5001: '--project' requires a path".to_owned())),
            "--pretty" if args.get(index + 1).is_some_and(|value| value == "false") => {
                pretty = false;
                index += 2;
            }
            "--pretty" => return Err((2, "error LT5001: '--pretty' supports only 'false'".into())),
            option => return Err((2, format!("error LT5001: Unknown option '{option}'"))),
        }
    }
    let config_path = if project_path.is_dir() {
        project_path.join("luat.toml")
    } else {
        project_path.clone()
    };
    if !config_path.is_file() {
        return Err((
            2,
            format!(
                "error LT5101: Project configuration not found: {}\nRun `luat create` to create a project here, or pass a project folder with `--project`.\n",
                config_path.display()
            ),
        ));
    }
    let project = Project::load(project_path)
        .map_err(|error| (2, render::render_build_error(&error, pretty)))?;
    run_project(command, project, pretty)
}

fn run_project(command: &str, project: Project, pretty: bool) -> Result<(), (u8, String)> {
    run_project_with(&project, pretty, || {
        if command == "check" {
            project.check()
        } else {
            project.build()
        }
    })
}

fn run_project_with(
    project: &Project,
    pretty: bool,
    run: impl FnOnce() -> Result<luat::BuildResult, BuildError>,
) -> Result<(), (u8, String)> {
    for warning in project.warnings() {
        eprintln!(
            "\n{} {warning}",
            paint(
                "warning:",
                Color::Yellow,
                pretty && std::io::stderr().is_terminal()
            )
        );
    }
    let started = Instant::now();
    let result = run();
    let result = result.map_err(|error| match error {
        BuildError::Compile { path, error } => {
            let code = if is_internal_error(&error) { 2 } else { 1 };
            (code, render::render_errors(&[(path, error)], pretty))
        }
        BuildError::CompileMany(errors) => {
            let code = if errors.iter().any(|(_, error)| is_internal_error(error)) {
                2
            } else {
                1
            };
            (code, render::render_errors(&errors, pretty))
        }
        error => (2, render::render_build_error(&error, pretty)),
    })?;
    if let Some(output_dir) = result.output_dir {
        println!(
            "\n{}",
            paint(
                &format!(
                    "Success: Build completed in {}ms",
                    started.elapsed().as_millis()
                ),
                Color::Success,
                pretty && std::io::stdout().is_terminal()
            )
        );
        println!(
            "  {} {} compiled",
            result.compiled_files,
            plural(result.compiled_files, "file", "files")
        );
        println!("  Output: {}", output_dir.display());
    } else {
        println!(
            "\n{}",
            paint(
                &format!(
                    "Success: Checked {} source {} and {} {}",
                    result.compiled_files,
                    plural(result.compiled_files, "file", "files"),
                    result.declaration_files,
                    plural(result.declaration_files, "declaration", "declarations")
                ),
                Color::Success,
                pretty && std::io::stdout().is_terminal()
            )
        );
    }
    Ok(())
}

fn is_internal_error(error: &luat::CompileError) -> bool {
    error
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.code, "LT3002" | "LT9001" | "LT9002" | "LT9003"))
}

fn usage() -> &'static str {
    "Usage: luat <command> [options]\n\nCommands:\n  check                   Check project scripts\n  build                   Compile project scripts\n  watch [directory]       Build and rebuild when source files change\n  create [directory]      Create a project in the given directory\n  add <package[@version]> Add or update packages\n  install                 Install packages from the manifest and lock\n  update [package ...]    Update dependencies within their version ranges\n  remove <package>        Remove a direct dependency\n  package <init|check>     Develop packages\n\nOptions for install:\n  --locked               Require an unchanged luat.lock\n\nOptions for check/build/package/add/install/update/remove:\n  --project <path>        Select a configuration or directory (default: current directory)\n  --pretty false          Print plain output without styling\n\nOptions:\n  -h, --help              Display this help\n  -V, --version           Display this version"
}

fn command_usage(command: &str) -> String {
    let syntax = match command {
        "package" => "package <init|check> [--project <path>] [--pretty false]".to_owned(),
        "create" => "create [directory] [--pretty false]".to_owned(),
        "watch" => "watch [directory] [--pretty false]".to_owned(),
        "add" => "add <package[@version] ...> [--project <path>] [--pretty false]".to_owned(),
        "remove" => "remove <package ...> [--project <path>] [--pretty false]".to_owned(),
        "update" => "update [package ...] [--project <path>] [--pretty false]".to_owned(),
        "install" => "install [--locked] [--project <path>] [--pretty false]".to_owned(),
        _ => format!("{command} [--project <path>] [--pretty false]"),
    };
    format!("Usage: luat {syntax}")
}

#[cfg(test)]
mod cleanup_tests {
    #[test]
    fn operational_errors_keep_exit_codes_and_report_explicit_diagnostics() {
        for (args, expected) in [
            (vec!["unknown"], "LT5001"),
            (vec!["check", "--project"], "LT5001"),
            (
                vec![
                    "check",
                    "--project",
                    "target/tests/absent-cli-config/luat.toml",
                ],
                "LT5101",
            ),
            (vec!["create", "--invalid"], "LT5001"),
            (vec!["watch", "target/tests/absent-cli-watch"], "LT5401"),
            (vec!["package", "init", "INVALID"], "LT5001"),
            (
                vec![
                    "package",
                    "check",
                    "--project",
                    "target/tests/absent-cli-package/luat-package.toml",
                ],
                "LT5101",
            ),
        ] {
            let (exit, message) =
                super::run(args.into_iter().map(str::to_owned).collect()).unwrap_err();
            assert_eq!(exit, 2);
            assert!(message.contains(&format!("error {expected}:")), "{message}");
            assert!(!message.contains('\u{1b}'), "{message}");
        }
    }

    #[test]
    fn help_lists_package_last_and_removed_commands_fail() {
        let help = super::usage();
        assert!(help.find("remove <").unwrap() < help.find("package <").unwrap());
        assert!(help.contains("Develop packages"));
        for command in ["build", "version", "registry-check", "registry-build"] {
            assert!(super::run(vec!["package".into(), command.into()]).is_err());
        }
        for command in [
            "check", "build", "create", "watch", "package", "add", "install", "update", "remove",
        ] {
            assert!(super::run(vec![command.into(), "--help".into()]).is_ok());
        }
    }
}
