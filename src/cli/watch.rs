use std::{
    collections::BTreeMap,
    env, fs,
    io::IsTerminal,
    path::{Component, Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use luat::{OutputChange, Project, ProjectConfig, output};
use notify::{
    Event, EventKind, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};

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
                    format!("error LT5401: Could not resolve current directory: {error}"),
                )
            })?
            .join(folder)
    };
    let folder: PathBuf = folder.components().collect();
    if !folder.is_dir() {
        return Err((
            2,
            format!(
                "error LT5401: Project folder is not a directory: {}",
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
                "error LT5401: Project configuration not found: {}",
                config.display()
            ),
        ));
    }
    if !src.is_dir() {
        return Err((
            2,
            format!(
                "error LT5401: Source directory not found: {}",
                src.display()
            ),
        ));
    }

    let (sender, receiver) = mpsc::channel();
    let mut declarations = Project::load(&folder)
        .map(|project| project.declaration_files().to_vec())
        .unwrap_or_default();
    let mut watcher = notify::RecommendedWatcher::new(
        sender,
        notify::Config::default().with_follow_symlinks(false),
    )
    .map_err(|error| (2, format!("error LT5401: Could not start watcher: {error}")))?;
    let mut watched = BTreeMap::new();
    sync_watches(
        &mut watcher,
        &mut watched,
        watch_paths(&folder, &config),
        &[],
    )?;
    let path = super::render::display_path(&folder, None);
    let color = output::color_enabled(std::io::stdout().is_terminal());
    let path = super::render::paint(&path, super::render::Color::Muted, color);
    output::status("INFO", &format!("Watching {path}"));
    let mut session = luat::BuildSession::default();
    let mut warnings = Vec::new();
    build(
        Project::load(&folder).as_ref(),
        &mut session,
        &mut warnings,
        pretty,
    );

    loop {
        match receiver.recv() {
            Ok(Ok(event)) => {
                if !process_event(
                    &event,
                    &src,
                    &config,
                    &declarations,
                    &mut watcher,
                    &mut watched,
                )? {
                    continue;
                }
                let mut changed = true;
                while changed {
                    changed = false;
                    match receiver.recv_timeout(DEBOUNCE) {
                        Ok(Ok(next)) => {
                            changed = process_event(
                                &next,
                                &src,
                                &config,
                                &declarations,
                                &mut watcher,
                                &mut watched,
                            )?;
                        }
                        Ok(Err(error)) => {
                            return Err((2, format!("error LT5401: Filesystem watcher: {error}")));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return Err((2, "error LT5401: Filesystem watcher stopped".to_owned()));
                        }
                    }
                }
                if !src.is_dir() {
                    return Err((
                        2,
                        format!(
                            "error LT5401: Source directory was removed: {}",
                            src.display()
                        ),
                    ));
                }
                let project = Project::load(&folder);
                build(project.as_ref(), &mut session, &mut warnings, pretty);
                if let Ok(project) = project {
                    declarations = project.declaration_files().to_vec();
                }
            }
            Ok(Err(error)) => {
                return Err((2, format!("error LT5401: Filesystem watcher: {error}")));
            }
            Err(_) => return Err((2, "error LT5401: Filesystem watcher stopped".to_owned())),
        }
    }
}

fn build(
    project: Result<&Project, &luat::BuildError>,
    session: &mut luat::BuildSession,
    warnings: &mut Vec<String>,
    pretty: bool,
) {
    let changes = project
        .map_err(|error| super::project_error(error, pretty, "Build", None))
        .and_then(|project| {
            for warning in project.warnings() {
                if !warnings.contains(warning) {
                    output::warning(warning);
                }
            }
            warnings.clear();
            warnings.extend_from_slice(project.warnings());
            project.build_for_watch(session).map_err(|error| {
                super::project_error(&error, pretty, "Build", Some(project.root()))
            })
        });
    match changes {
        Ok(changes) => print_changes(changes),
        Err((_, message)) => {
            eprintln!("{}", output::stderr_message(&message));
        }
    }
}

fn print_changes(changes: Vec<OutputChange>) {
    if changes.is_empty() {
        return;
    }
    use super::render::{Color, display_path, paint};
    let color = output::color_enabled(std::io::stdout().is_terminal());
    let time = paint(&local_time(), Color::Muted, color);
    let tag = if color {
        "\x1b[1;38;2;157;130;241m[luat]\x1b[0m"
    } else {
        "[luat]"
    };
    let mut groups = [Vec::new(), Vec::new(), Vec::new()];
    for change in changes {
        let (index, path) = match change {
            OutputChange::Added(path) => (0, path),
            OutputChange::Updated(path) => (1, path),
            OutputChange::Removed(path) => (2, path),
        };
        groups[index].push(display_path(&path, None));
    }
    for (action, mut paths) in ["add", "update", "remove"].into_iter().zip(groups) {
        if paths.is_empty() {
            continue;
        }
        paths.sort_unstable();
        let paths = paths
            .iter()
            .map(|path| {
                if let Some((directory, file)) = path.rsplit_once('/') {
                    format!(
                        "{}{file}",
                        paint(&format!("{directory}/"), Color::Muted, color)
                    )
                } else {
                    path.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "{time} {tag} {} {paths}",
            paint(action, Color::Success, color)
        );
    }
}

#[cfg(windows)]
fn local_time() -> String {
    use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
    let mut time = std::mem::MaybeUninit::<SYSTEMTIME>::uninit();
    // GetLocalTime initializes the supplied SYSTEMTIME.
    let time = unsafe {
        GetLocalTime(time.as_mut_ptr());
        time.assume_init()
    };
    format!("{:02}:{:02}:{:02}", time.wHour, time.wMinute, time.wSecond)
}

#[cfg(unix)]
fn local_time() -> String {
    let mut local = std::mem::MaybeUninit::uninit();
    // localtime_r writes the calendar time into local on success.
    unsafe {
        let seconds = libc::time(std::ptr::null_mut());
        if libc::localtime_r(&seconds, local.as_mut_ptr()).is_null() {
            return "--:--:--".into();
        }
        let time = local.assume_init();
        format!("{:02}:{:02}:{:02}", time.tm_hour, time.tm_min, time.tm_sec)
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
            "--pretty" => {
                return Err((
                    2,
                    "error LT5001: '--pretty' supports only 'false'".to_owned(),
                ));
            }
            value if value.starts_with('-') => {
                return Err((2, format!("error LT5001: Unknown option '{value}'")));
            }
            value if folder.is_none() => {
                folder = Some(PathBuf::from(value));
                index += 1;
            }
            value => return Err((2, format!("error LT5001: Unexpected argument '{value}'"))),
        }
    }
    Ok((folder.unwrap_or_else(|| PathBuf::from(".")), pretty))
}

fn watch_paths(root: &Path, config: &Path) -> BTreeMap<PathBuf, RecursiveMode> {
    let mut paths = BTreeMap::from([(root.to_path_buf(), RecursiveMode::NonRecursive)]);
    for directory in [root.join("src"), root.join(".luat")] {
        if real_directory(&directory) {
            paths.insert(directory, RecursiveMode::Recursive);
        }
    }
    if let Some(config) = fs::read_to_string(config)
        .ok()
        .and_then(|text| toml::from_str::<ProjectConfig>(&text).ok())
    {
        for path in config
            .fivem
            .shared
            .iter()
            .chain(&config.fivem.client)
            .chain(&config.fivem.server)
        {
            if path.starts_with('@') || path.contains("://") {
                continue;
            }
            let directories = Path::new(path).parent().unwrap_or(Path::new(""));
            let components = directories
                .components()
                .filter(|part| *part != Component::CurDir)
                .map(|part| match part {
                    Component::Normal(name) => name.to_str(),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>();
            if let Some(components) = components {
                add_script_watches(root, &components, &mut paths);
            }
        }
    }
    let mut recursive = Vec::<PathBuf>::new();
    paths.retain(|path, mode| {
        if recursive.iter().any(|parent| path.starts_with(parent)) {
            return false;
        }
        if *mode == RecursiveMode::Recursive {
            recursive.push(path.clone());
        }
        true
    });
    paths
}

fn real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn add_script_watches(
    path: &Path,
    components: &[&str],
    paths: &mut BTreeMap<PathBuf, RecursiveMode>,
) {
    if paths.get(path) == Some(&RecursiveMode::Recursive) || !real_directory(path) {
        return;
    }
    paths.insert(path.to_path_buf(), RecursiveMode::NonRecursive);
    let Some((&component, remaining)) = components.split_first() else {
        return;
    };
    if component == "**" {
        paths.insert(path.to_path_buf(), RecursiveMode::Recursive);
    } else if component.contains(['*', '?', '[']) {
        if let Ok(pattern) = glob::Pattern::new(component)
            && let Ok(entries) = fs::read_dir(path)
        {
            for entry in entries.flatten() {
                if pattern.matches(&entry.file_name().to_string_lossy()) {
                    add_script_watches(&entry.path(), remaining, paths);
                }
            }
        }
    } else {
        add_script_watches(&path.join(component), remaining, paths);
    }
}

fn sync_watches(
    watcher: &mut impl Watcher,
    watched: &mut BTreeMap<PathBuf, RecursiveMode>,
    next: BTreeMap<PathBuf, RecursiveMode>,
    removed: &[PathBuf],
) -> Result<(), (u8, String)> {
    let needs_rewatch = |path: &Path| removed.iter().any(|old_path| path.starts_with(old_path));
    for (path, mode) in watched.iter() {
        if next.get(path) != Some(mode) || needs_rewatch(path) {
            watcher
                .unwatch(path)
                .or_else(|error| {
                    if matches!(error.kind, notify::ErrorKind::WatchNotFound) {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(|error| {
                    (
                        2,
                        format!(
                            "error LT5401: Could not stop watching {}: {error}",
                            path.display()
                        ),
                    )
                })?;
        }
    }
    for (path, mode) in &next {
        if watched.get(path) != Some(mode) || needs_rewatch(path) {
            watcher.watch(path, *mode).map_err(|error| {
                (
                    2,
                    format!("error LT5401: Could not watch {}: {error}", path.display()),
                )
            })?;
        }
    }
    *watched = next;
    Ok(())
}

fn process_event(
    event: &Event,
    src: &Path,
    config: &Path,
    declarations: &[PathBuf],
    watcher: &mut impl Watcher,
    watched: &mut BTreeMap<PathBuf, RecursiveMode>,
) -> Result<bool, (u8, String)> {
    let mut changed = false;
    if event.need_rescan()
        || event.paths.iter().any(|path| {
            path == config
                || is_structural(event) && (watched.contains_key(path) || real_directory(path))
        })
    {
        let next = watch_paths(config.parent().unwrap_or(Path::new(".")), config);
        let removed = match event.kind {
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                event.paths.as_slice()
            }
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
                event.paths.get(..1).unwrap_or_default()
            }
            _ => &[],
        };
        changed = *watched != next
            || watched
                .keys()
                .any(|path| removed.iter().any(|old_path| path.starts_with(old_path)));
        sync_watches(watcher, watched, next, removed)?;
    }
    Ok(changed || relevant(event, src, config, declarations, watched))
}

fn is_structural(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn relevant(
    event: &Event,
    src: &Path,
    config: &Path,
    declarations: &[PathBuf],
    watched: &BTreeMap<PathBuf, RecursiveMode>,
) -> bool {
    let structural = is_structural(event);
    event.need_rescan()
        || (structural && event.paths.is_empty())
        || event.paths.iter().any(|path| {
            path == config
                || path
                    == &config
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join("luat.lock")
                || path
                    == config
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(".luat/install.lock")
                        .as_path()
                || path.starts_with(
                    config
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(".luat"),
                )
                || declarations.iter().any(|declaration| {
                    path == declaration || structural && declaration.starts_with(path)
                })
                || path.starts_with(src)
                    && (path
                        .extension()
                        .is_some_and(|extension| extension == "luat" || extension == "lua")
                        || structural && path.extension().is_none())
                || structural
                    && (is_declaration(path)
                        || watched.iter().any(|(root, mode)| {
                            *mode == RecursiveMode::Recursive
                                && path.starts_with(root)
                                && declaration_tree(path)
                        }))
        })
}

fn is_declaration(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".d.luat"))
}

fn declaration_tree(path: &Path) -> bool {
    is_declaration(path)
        || real_directory(path)
            && fs::read_dir(path).is_ok_and(|entries| {
                entries
                    .flatten()
                    .any(|entry| declaration_tree(&entry.path()))
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

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
        let path = env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!(
                "absolute project {} {}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("luat.toml"), "[project]\nname = 'test'\n").unwrap();
        let result = Project::load(&path);
        assert!(result.is_ok(), "{result:?}");
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn filters_events_to_luat_changes_and_structural_directories_under_src() {
        let src = Path::new("project/src");
        let watched = BTreeMap::new();
        let mut event = Event::new(EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any,
        )));
        event.paths.push(src.join("nested/main.luat"));
        let config = Path::new("project/luat.toml");
        assert!(relevant(&event, src, config, &[], &watched));
        event.paths = vec![PathBuf::from("project/luat.lock")];
        assert!(relevant(&event, src, config, &[], &watched));
        event.paths = vec![src.join("module.lua")];
        assert!(relevant(&event, src, config, &[], &watched));
        event.paths = vec![src.join("nested/readme.txt")];
        assert!(!relevant(&event, src, config, &[], &watched));
        event.kind = EventKind::Create(notify::event::CreateKind::Folder);
        event.paths = vec![src.join("nested")];
        assert!(relevant(&event, src, config, &[], &watched));
        event.paths = vec![PathBuf::from("project/dist/main.lua")];
        assert!(!relevant(&event, src, config, &[], &watched));
        event.paths = vec![config.to_path_buf()];
        assert!(relevant(&event, src, config, &[], &watched));
        let declaration = PathBuf::from("project/types/api.d.luat");
        event.paths = vec![declaration.clone()];
        assert!(relevant(&event, src, config, &[declaration], &watched));
        event.paths = vec![PathBuf::from("project/types/new/nested/api.d.luat")];
        assert!(relevant(&event, src, config, &[], &watched));
        event.paths = vec![PathBuf::from(
            "project/.luat/packages/@types/example/1.0.0/client.d.luat",
        )];
        assert!(relevant(&event, src, config, &[], &watched));
    }

    #[test]
    fn notices_declarations_in_a_moved_directory() {
        let root = env::current_dir()
            .unwrap()
            .join("target/tests/watch-moved-declarations");
        let moved = root.join("types/new");
        fs::create_dir_all(&moved).unwrap();
        let mut event = Event::new(EventKind::Modify(notify::event::ModifyKind::Name(
            notify::event::RenameMode::To,
        )));
        event.paths.push(moved.clone());
        let src = root.join("src");
        let config = root.join("luat.toml");
        fs::create_dir_all(root.join("web/node_modules/dependency")).unwrap();
        fs::create_dir_all(&src).unwrap();
        fs::write(
            &config,
            "[project]\nname = 'test'\n[fivem]\nshared = ['types/**/*.d.luat']\n",
        )
        .unwrap();
        let watched = watch_paths(&root, &config);
        assert_eq!(watched[&root], RecursiveMode::NonRecursive);
        assert_eq!(watched[&root.join("types")], RecursiveMode::Recursive);
        assert!(
            !watched
                .keys()
                .any(|path| path.starts_with(root.join("web")))
        );
        assert!(!relevant(&event, &src, &config, &[], &watched));
        fs::write(moved.join("api.d.luat"), "typedef Api = string").unwrap();
        assert!(relevant(&event, &src, &config, &[], &watched));
        fs::remove_dir_all(root).unwrap();
    }
}
