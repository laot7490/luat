use std::{
    env,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use luat::Project;
use notify::{Event, EventKind, RecursiveMode, Watcher};

const DEBOUNCE: Duration = Duration::from_millis(200);

pub(crate) fn run(args: &[String]) -> Result<(), (u8, String)> {
    let (folder, pretty) = parse_args(args)?;
    let folder = if folder.is_absolute() {
        folder
    } else {
        env::current_dir()
            .map_err(|error| {
                (
                    2,
                    format!("error: could not resolve current directory: {error}"),
                )
            })?
            .join(folder)
    };
    if !folder.is_dir() {
        return Err((
            2,
            format!(
                "error: project folder is not a directory: {}",
                folder.display()
            ),
        ));
    }
    let config = folder.join("luat.toml");
    let src = folder.join("src");
    if !config.is_file() {
        return Err((
            2,
            format!(
                "error: project configuration not found: {}",
                config.display()
            ),
        ));
    }
    if !src.is_dir() {
        return Err((
            2,
            format!("error: source directory not found: {}", src.display()),
        ));
    }

    let (sender, receiver) = mpsc::channel();
    let project = Project::load(&folder)
        .map_err(|error| (2, super::render::render_build_error(&error, pretty)))?;
    let declarations = project.declaration_files().to_vec();
    let mut watcher = notify::recommended_watcher(sender)
        .map_err(|error| (2, format!("error: could not start watcher: {error}")))?;
    watcher
        .watch(&src, RecursiveMode::Recursive)
        .map_err(|error| {
            (
                2,
                format!("error: could not watch {}: {error}", src.display()),
            )
        })?;
    watcher
        .watch(&config, RecursiveMode::NonRecursive)
        .map_err(|error| {
            (
                2,
                format!("error: could not watch {}: {error}", config.display()),
            )
        })?;
    for directory in declarations
        .iter()
        .filter_map(|path| path.parent())
        .filter(|directory| !directory.starts_with(&src))
        .collect::<std::collections::HashSet<_>>()
    {
        watcher
            .watch(directory, RecursiveMode::NonRecursive)
            .map_err(|error| {
                (
                    2,
                    format!("error: could not watch {}: {error}", directory.display()),
                )
            })?;
    }
    println!(
        "\nWatching {} for source and configuration changes",
        folder.display()
    );
    build(&folder, pretty);

    loop {
        match receiver.recv() {
            Ok(Ok(event)) if relevant(&event, &src, &config, &declarations) => {
                let mut changed = true;
                while changed {
                    changed = false;
                    match receiver.recv_timeout(DEBOUNCE) {
                        Ok(Ok(next)) if relevant(&next, &src, &config, &declarations) => {
                            changed = true
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => {
                            return Err((2, format!("error: filesystem watcher: {error}")));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return Err((2, "error: filesystem watcher stopped".to_owned()));
                        }
                    }
                }
                if !src.is_dir() {
                    return Err((
                        2,
                        format!("error: source directory was removed: {}", src.display()),
                    ));
                }
                build(&folder, pretty);
            }
            Ok(Ok(_)) => {}
            Ok(Err(error)) => return Err((2, format!("error: filesystem watcher: {error}"))),
            Err(_) => return Err((2, "error: filesystem watcher stopped".to_owned())),
        }
    }
}

fn build(folder: &Path, pretty: bool) {
    let result = Project::load(folder)
        .map_err(|error| (2, super::render::render_build_error(&error, pretty)))
        .and_then(|project| super::run_project("build", project, pretty));
    if let Err((_, message)) = result {
        eprintln!("\n{message}");
    }
}

fn parse_args(args: &[String]) -> Result<(PathBuf, bool), (u8, String)> {
    let mut folder = None;
    let mut pretty = true;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pretty" if args.get(index + 1).is_some_and(|value| value == "false") => {
                pretty = false;
                index += 2;
            }
            "--pretty" => return Err((2, "error: '--pretty' supports only 'false'".to_owned())),
            value if value.starts_with('-') => {
                return Err((2, format!("error: unknown option '{value}'")));
            }
            value if folder.is_none() => {
                folder = Some(PathBuf::from(value));
                index += 1;
            }
            value => return Err((2, format!("error: unexpected argument '{value}'"))),
        }
    }
    Ok((folder.unwrap_or_else(|| PathBuf::from(".")), pretty))
}

fn relevant(event: &Event, src: &Path, config: &Path, declarations: &[PathBuf]) -> bool {
    let structural = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(notify::event::ModifyKind::Name(_))
    );
    event.paths.iter().any(|path| {
        path == config
            || declarations.iter().any(|declaration| path == declaration)
            || path.starts_with(src)
                && (path
                    .extension()
                    .is_some_and(|extension| extension == "luat")
                    || structural && path.extension().is_none())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_and_explicit_folder_with_spaces() {
        assert_eq!(parse_args(&[]).unwrap(), (PathBuf::from("."), true));
        assert_eq!(
            parse_args(&["A proje".into(), "--pretty".into(), "false".into()]).unwrap(),
            (PathBuf::from("A proje"), false)
        );
    }

    #[test]
    fn rejects_unknown_options_and_extra_folders() {
        assert!(parse_args(&["--bad".into()]).is_err());
        assert!(parse_args(&["one".into(), "two".into()]).is_err());
    }

    #[test]
    fn absolute_project_path_loads_without_canonicalization() {
        let path = env::current_dir().unwrap().join("example");
        assert!(Project::load(&path).is_ok());
    }

    #[test]
    fn filters_events_to_luat_changes_and_structural_directories_under_src() {
        let src = Path::new("project/src");
        let mut event = Event::new(EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any,
        )));
        event.paths.push(src.join("nested/main.luat"));
        let config = Path::new("project/luat.toml");
        assert!(relevant(&event, src, config, &[]));
        event.paths = vec![src.join("nested/readme.txt")];
        assert!(!relevant(&event, src, config, &[]));
        event.kind = EventKind::Create(notify::event::CreateKind::Folder);
        event.paths = vec![src.join("nested")];
        assert!(relevant(&event, src, config, &[]));
        event.paths = vec![PathBuf::from("project/dist/main.lua")];
        assert!(!relevant(&event, src, config, &[]));
        event.paths = vec![config.to_path_buf()];
        assert!(relevant(&event, src, config, &[]));
        let declaration = PathBuf::from("project/types/api.d.luat");
        event.paths = vec![declaration.clone()];
        assert!(relevant(&event, src, config, &[declaration]));
    }
}
