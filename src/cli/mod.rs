use luat::{BuildError, BuildProgress, BuildStage, Project, output};
use std::{
    env,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

mod create;
mod render;
mod watch;

fn plural<'a>(count: usize, singular: &'a str, plural: &'a str) -> &'a str {
    if count == 1 { singular } else { plural }
}

pub(crate) fn run(args: Vec<String>) -> Result<(), (u8, String)> {
    if args
        .first()
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!("luat {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args
        .first()
        .is_some_and(|arg| arg == "--help" || arg == "-h")
    {
        println!("{}", usage());
        return Ok(());
    }
    let Some(command) = args.first().map(String::as_str) else {
        println!("{}", usage());
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
        let diagnostic_root = std::path::absolute(&root).ok();
        luat::packages::author::check_package(&root).map_err(|error| {
            let code = match &error {
                BuildError::Compile { .. } | BuildError::CompileMany(_) => 1,
                _ => 2,
            };
            (
                code,
                render::render_build_error(&error, pretty, "Check", diagnostic_root.as_deref()),
            )
        })?;
        output::status("SUCCESS", &format!("Checked package in {}", root.display()));
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
        return luat::packages::run(&args);
    }
    if !matches!(command, "check" | "build") {
        return Err((
            2,
            format!("error LT5001: Unknown command '{command}'\n{}", usage()),
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
    let started = Instant::now();
    let project = Project::load(project_path).map_err(|error| {
        (
            2,
            render::render_build_error(
                &error,
                pretty,
                if command == "check" { "Check" } else { "Build" },
                config_path.parent(),
            ),
        )
    })?;
    run_project(command, project, pretty, started)
}

fn run_project(
    command: &str,
    project: Project,
    pretty: bool,
    started: Instant,
) -> Result<(), (u8, String)> {
    for warning in project.warnings() {
        output::warning(warning);
    }
    let result = if command == "check" {
        project.check()
    } else {
        production_build(&project)
    }
    .map_err(|error| {
        project_error(
            &error,
            pretty,
            if command == "check" { "Check" } else { "Build" },
            Some(project.root()),
        )
    })?;
    let count = result.compiled_files;
    let files = plural(count, "file", "files");
    if result.output_dir.is_some() {
        let modules = plural(count, "module", "modules");
        let color = output::color_enabled(std::io::stdout().is_terminal());
        println!(
            "{}",
            render::paint(
                &format!("✓ {count} {modules} transformed."),
                render::Color::Success,
                color
            )
        );
        println!(
            "{}",
            render::paint(
                &format!("✓ built in {}", build_duration(started.elapsed())),
                render::Color::Success,
                color
            )
        );
    } else {
        let mut message = format!("Checked {count} source {files}");
        let declarations = result.declaration_files;
        if declarations > 0 {
            let label = plural(declarations, "declaration", "declarations");
            message.push_str(&format!(" and {declarations} {label}"));
        }
        output::status("SUCCESS", &format!("{message}."));
    }
    Ok(())
}

fn project_error(
    error: &BuildError,
    pretty: bool,
    action: &str,
    root: Option<&Path>,
) -> (u8, String) {
    let code = match error {
        BuildError::Compile { error, .. } if !is_internal_error(error) => 1,
        BuildError::CompileMany(errors)
            if !errors.iter().any(|(_, error)| is_internal_error(error)) =>
        {
            1
        }
        _ => 2,
    };
    (
        code,
        render::render_build_error(error, pretty, action, root),
    )
}

fn production_build(project: &Project) -> Result<luat::BuildResult, BuildError> {
    output::status("INFO", "Building for production...");
    let interactive = std::io::stdout().is_terminal() && std::io::stderr().is_terminal();
    let mut last = None;
    let result = project.build_with_file_progress(&mut luat::BuildSession::default(), |event| {
        if !interactive {
            return;
        }
        let (phase, path) = match event {
            BuildProgress::Analyzing(path) => ("analyzing", path),
            BuildProgress::Transforming(path) => ("transforming", path),
            BuildProgress::Stage(BuildStage::Finalize) => {
                if last.take().is_some() {
                    let _ = clear_progress_line();
                }
                return;
            }
            BuildProgress::Stage(_) => return,
        };
        if last.is_some_and(|(previous, time): (&str, Instant)| {
            previous == phase && time.elapsed() < Duration::from_millis(50)
        }) {
            return;
        }
        let _ = clear_progress_line();
        let row = format!(
            "{phase} {}",
            render::display_path(path, Some(project.root()))
        );
        let row = truncate_progress(&row, 119);
        let _ = write!(
            std::io::stderr(),
            "\r\x1b[2K{}",
            render::paint(&row, render::Color::Muted, true)
        );
        last = Some((phase, Instant::now()));
    });
    if last.is_some() {
        let _ = clear_progress_line();
    }
    result
}

fn clear_progress_line() -> std::io::Result<()> {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    stderr.write_all(b"\r\x1b[2K")?;
    stderr.flush()
}

fn truncate_progress(row: &str, width: usize) -> String {
    if row.chars().count() <= width {
        row.to_owned()
    } else {
        let mut truncated = row
            .chars()
            .take(width.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }
}

fn build_duration(duration: Duration) -> String {
    if duration < Duration::from_secs(1) {
        format!("{}ms", duration.as_millis())
    } else {
        format!("{:.2}s", duration.as_secs_f64())
    }
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
    fn formats_build_duration_in_milliseconds_or_seconds() {
        use std::time::Duration;
        assert_eq!(super::build_duration(Duration::from_millis(999)), "999ms");
        assert_eq!(super::build_duration(Duration::from_millis(8_400)), "8.40s");
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
