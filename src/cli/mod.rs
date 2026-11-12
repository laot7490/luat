use crate::output;
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
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("luat {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", usage());
        return Ok(());
    }
    let Some(command) = args.first().map(String::as_str) else {
        println!("{}", usage());
        return Ok(());
    };
    if command == "create" {
        return create::run(&args[1..]);
    }
    if command == "watch" {
        return watch::run(&args[1..]);
    }
    if !matches!(command, "check" | "build") {
        return Err((
            2,
            format!("error: unknown command '{command}'\n\n{}", usage()),
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
            "--project" => return Err((2, "error: '--project' requires a path".to_owned())),
            "--pretty" if args.get(index + 1).is_some_and(|value| value == "false") => {
                pretty = false;
                index += 2;
            }
            option => return Err((2, format!("error: unknown option '{option}'"))),
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
                "error: project configuration not found: {}\nRun `luat create` to create a project here, or pass a project folder with `--project`.\n",
                config_path.display()
            ),
        ));
    }
    let project = Project::load(project_path)
        .map_err(|error| (2, render::render_build_error(&error, pretty)))?;
    run_project(command, project, pretty)
}

fn run_project(command: &str, project: Project, pretty: bool) -> Result<(), (u8, String)> {
    for warning in project.warnings() {
        output::warning(warning);
    }
    let started = Instant::now();
    let result = if command == "check" {
        project.check()
    } else {
        project.build()
    };
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
            "{}",
            paint(
                &format!("Build complete in {}ms", started.elapsed().as_millis()),
                Color::Success,
                std::io::stdout().is_terminal()
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
            "{}",
            paint(
                &format!(
                    "Checked {} source {} and {} {}",
                    result.compiled_files,
                    plural(result.compiled_files, "file", "files"),
                    result.declaration_files,
                    plural(result.declaration_files, "declaration", "declarations")
                ),
                Color::Success,
                std::io::stdout().is_terminal()
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
    "Usage: luat <command> [options]\n\nCommands:\n  check                   Check project scripts\n  build                   Compile project scripts\n  watch [directory]       Build and rebuild when source files change\n  create [directory]      Create a project in the given directory\n\nOptions for check/build:\n  --project <directory>   Use a project directory (default: current directory)\n  --pretty false          Print plain diagnostics without styling\n\nOptions:\n  -h, --help              Display this help\n  -V, --version           Display the version"
}
