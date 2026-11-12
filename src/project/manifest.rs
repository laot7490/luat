use super::*;

pub(super) fn validate_manifest_paths(
    fivem: &FiveMConfig,
    root: &Path,
    config: &Path,
) -> Result<(), BuildError> {
    for value in fivem
        .shared
        .iter()
        .chain(&fivem.client)
        .chain(&fivem.server)
    {
        validate_manifest_path(root, config, value, true, false)?;
    }
    for value in [
        fivem.ui_page.as_deref(),
        fivem.loadscreen.as_deref(),
        fivem.before_level_meta.as_deref(),
        fivem.after_level_meta.as_deref(),
        fivem.replace_level_meta.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        validate_manifest_path(root, config, value, false, false)?;
    }
    for entry in &fivem.data_file {
        validate_manifest_path(root, config, &entry.path, false, true)?;
    }
    Ok(())
}

fn validate_manifest_path(
    root: &Path,
    config: &Path,
    value: &str,
    script: bool,
    allow_directory: bool,
) -> Result<(), BuildError> {
    if value.starts_with('@') || value.contains("://") {
        return Ok(());
    }
    let relative = Path::new(value);
    validate_project_path(relative, config)?;
    let luat = relative
        .extension()
        .is_some_and(|extension| extension == "luat")
        || value.contains(".luat");
    if luat && !relative.starts_with("src") {
        return Err(BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!("LuaT manifest paths must be under src/: {value}"),
        });
    }
    if !script && value.contains(".d.luat") {
        return Err(BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!("declaration files cannot be included as runtime assets: {value}"),
        });
    }
    let absolute_pattern = root.join(relative).to_string_lossy().replace('\\', "/");
    let mut paths = glob::glob(&absolute_pattern)
        .map_err(|error| BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!("invalid manifest path `{value}`: {error}"),
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| BuildError::Configuration {
            path: config.to_path_buf(),
            message: error.to_string(),
        })?;
    paths.sort();
    let canonical_root = fs::canonicalize(root).map_err(|source| BuildError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let mut matched = false;
    for path in paths {
        reject_symlink_path(root, &path, config)?;
        if path.is_dir() {
            if allow_directory {
                matched = true;
                let canonical_path = fs::canonicalize(&path).map_err(|source| BuildError::Io {
                    path: path.clone(),
                    source,
                })?;
                if !canonical_path.starts_with(&canonical_root) {
                    return Err(BuildError::Configuration {
                        path: config.to_path_buf(),
                        message: format!(
                            "manifest path resolves outside the project: {}",
                            path.display()
                        ),
                    });
                }
            }
            continue;
        }
        if !path.is_file() {
            continue;
        }
        matched = true;
        let canonical_path = fs::canonicalize(&path).map_err(|source| BuildError::Io {
            path: path.clone(),
            source,
        })?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(BuildError::Configuration {
                path: config.to_path_buf(),
                message: format!(
                    "manifest path resolves outside the project: {}",
                    path.display()
                ),
            });
        }
        if script
            && path
                .extension()
                .is_some_and(|extension| extension == "luat")
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".d.luat"))
        {
            return Err(BuildError::Configuration {
                path: config.to_path_buf(),
                message: format!("declaration files cannot be loaded as runtime scripts: {value}"),
            });
        }
        if !script
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".d.luat"))
        {
            return Err(BuildError::Configuration {
                path: config.to_path_buf(),
                message: format!("declaration files cannot be included as runtime assets: {value}"),
            });
        }
    }
    if !matched {
        return Err(BuildError::Configuration {
            path: config.to_path_buf(),
            message: format!("manifest path does not exist or match any files: {value}"),
        });
    }
    Ok(())
}

pub(super) fn is_builtin_manifest_key(key: &str) -> bool {
    [
        "fx_version",
        "game",
        "games",
        "name",
        "author",
        "description",
        "version",
        "shared_script",
        "shared_scripts",
        "client_script",
        "client_scripts",
        "server_script",
        "server_scripts",
        "file",
        "files",
        "dependency",
        "dependencies",
        "export",
        "exports",
        "server_export",
        "server_exports",
        "provide",
        "escrow_ignore",
        "ui_page",
        "loadscreen",
        "loadscreen_cursor",
        "loadscreen_manual_shutdown",
        "server_only",
        "this_is_a_map",
        "node_version",
        "use_experimental_fxv2_oal",
        "clr_disable_task_scheduler",
        "before_level_meta",
        "after_level_meta",
        "replace_level_meta",
        "resource_manifest_version",
        "lua54",
        "data_file",
    ]
    .contains(&key)
}

pub(super) fn is_manifest_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let valid = characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric());
    valid
        && ![
            "and", "break", "continue", "do", "else", "elseif", "end", "false", "for", "function",
            "goto", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true",
            "until", "while",
        ]
        .contains(&value)
}

fn manifest_path(value: &str, out_dir: &Path) -> String {
    let path = Path::new(value);
    if path
        .extension()
        .is_some_and(|extension| extension == "luat")
        && let Ok(relative) = path.strip_prefix("src")
    {
        let mut result = out_dir.join(relative);
        result.set_extension("lua");
        result.to_string_lossy().replace('\\', "/")
    } else {
        value.to_owned()
    }
}

fn lua_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\'' => escaped.push_str("\\'"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\0' => escaped.push_str("\\000"),
            ch if ch.is_control() => {
                let mut bytes = [0; 4];
                for byte in ch.encode_utf8(&mut bytes).bytes() {
                    escaped.push_str(&format!("\\{byte:03}"));
                }
            }
            ch => escaped.push(ch),
        }
    }
    format!("'{escaped}'")
}

fn lua_value(value: &toml::Value) -> Result<String, String> {
    Ok(match value {
        toml::Value::String(value) => lua_string(value),
        toml::Value::Integer(value) => value.to_string(),
        toml::Value::Float(value) if value.is_finite() => value.to_string(),
        toml::Value::Float(_) => {
            return Err("non-finite TOML floats cannot be represented in fxmanifest.lua".into());
        }
        toml::Value::Boolean(value) => value.to_string(),
        toml::Value::Array(values) => format!(
            "{{ {} }}",
            values
                .iter()
                .map(lua_value)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        toml::Value::Table(values) => format!(
            "{{ {} }}",
            values
                .iter()
                .map(|(key, value)| Ok(format!("[{}] = {}", lua_string(key), lua_value(value)?)))
                .collect::<Result<Vec<_>, String>>()?
                .join(", ")
        ),
        toml::Value::Datetime(_) => {
            return Err("TOML datetime values cannot be represented in fxmanifest.lua".into());
        }
    })
}

pub(super) fn render_manifest(config: &ProjectConfig, root: &Path) -> Result<String, String> {
    render_manifest_with_scripts(config, root, None)
}

pub(super) fn render_manifest_with_scripts(
    config: &ProjectConfig,
    root: &Path,
    scripts: Option<&[Vec<String>; 3]>,
) -> Result<String, String> {
    let f = &config.fivem;
    let mut lines = vec![
        format!("fx_version {}", lua_string(&f.fx_version)),
        format!("name {}", lua_string(&config.project.name)),
    ];
    let games = if let Some(game) = &f.game {
        vec![game.as_str()]
    } else if !f.games.is_empty() {
        f.games.iter().map(String::as_str).collect()
    } else {
        vec!["gta5"]
    };
    if games.len() == 1 {
        lines.push(format!("game {}", lua_string(games[0])));
    } else if !games.is_empty() {
        lines.push(format!(
            "games {{ {} }}",
            games
                .iter()
                .map(|value| lua_string(value))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (key, value) in [
        ("author", config.project.author.as_deref()),
        ("description", config.project.description.as_deref()),
        ("version", config.project.version.as_deref()),
    ] {
        if let Some(value) = value {
            lines.push(format!("{key} {}", lua_string(value)));
        }
    }
    for (index, (key, values)) in [
        ("shared_scripts", &f.shared),
        ("client_scripts", &f.client),
        ("server_scripts", &f.server),
    ]
    .into_iter()
    .enumerate()
    {
        let values = if let Some(scripts) = scripts {
            scripts[index].clone()
        } else {
            resolved_manifest_paths(values, root, &config.compiler.out_dir, false)?
        };
        if !values.is_empty() {
            lines.push(format!(
                "{key} {{ {} }}",
                values
                    .iter()
                    .map(|value| lua_string(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for (key, values) in [("files", &f.files), ("escrow_ignore", &f.escrow_ignore)] {
        let values = resolved_manifest_paths(values, root, &config.compiler.out_dir, false)?;
        if !values.is_empty() {
            lines.push(format!(
                "{key} {{ {} }}",
                values
                    .iter()
                    .map(|value| lua_string(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for (key, values) in [
        ("dependencies", &f.dependencies),
        ("exports", &f.exports),
        ("server_exports", &f.server_exports),
    ] {
        if !values.is_empty() {
            lines.push(format!(
                "{key} {{ {} }}",
                values
                    .iter()
                    .map(|value| lua_string(value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    for value in &f.provide {
        lines.push(format!("provide {}", lua_string(value)));
    }
    for (key, value) in [
        ("ui_page", f.ui_page.as_deref()),
        ("loadscreen", f.loadscreen.as_deref()),
        ("before_level_meta", f.before_level_meta.as_deref()),
        ("after_level_meta", f.after_level_meta.as_deref()),
        ("replace_level_meta", f.replace_level_meta.as_deref()),
        (
            "resource_manifest_version",
            f.resource_manifest_version.as_deref(),
        ),
    ] {
        if let Some(value) = value {
            lines.push(format!("{key} {}", lua_string(value)));
        }
    }
    for (key, enabled) in [
        ("loadscreen_cursor", f.loadscreen_cursor),
        ("loadscreen_manual_shutdown", f.loadscreen_manual_shutdown),
        ("server_only", f.server_only),
        ("this_is_a_map", f.this_is_a_map),
        ("use_experimental_fxv2_oal", f.use_experimental_fxv2_oal),
        ("clr_disable_task_scheduler", f.clr_disable_task_scheduler),
        ("lua54", f.lua54),
    ] {
        if enabled {
            lines.push(format!("{key} 'yes'"));
        }
    }
    if let Some(version) = f.node_version {
        lines.push(format!("node_version {}", lua_string(&version.to_string())));
    }
    for entry in &f.data_file {
        let paths = resolved_manifest_paths(
            std::slice::from_ref(&entry.path),
            root,
            &config.compiler.out_dir,
            true,
        )?;
        for path in paths {
            lines.push(format!(
                "data_file {} {}",
                lua_string(&entry.r#type),
                lua_string(&path)
            ));
        }
    }
    for entry in &f.entries {
        let value = lua_value(&entry.value)?;
        let mut line = format!("{} {}", entry.key, value);
        if let Some(extra) = &entry.extra {
            line.push(' ');
            line.push_str(&lua_value(extra)?);
        }
        lines.push(line);
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn resolved_manifest_paths(
    values: &[String],
    root: &Path,
    out_dir: &Path,
    allow_directory: bool,
) -> Result<Vec<String>, String> {
    Ok(expanded_paths(values, root, allow_directory)?
        .into_iter()
        .map(|value| manifest_path(&value, out_dir))
        .collect())
}

pub(super) fn expanded_paths(
    values: &[String],
    root: &Path,
    allow_directory: bool,
) -> Result<Vec<String>, String> {
    let mut resolved = Vec::new();
    for value in values {
        if value.starts_with('@') || value.contains("://") {
            resolved.push(value.clone());
            continue;
        }
        let pattern = root.join(value).to_string_lossy().replace('\\', "/");
        let mut matches = glob::glob(&pattern)
            .map_err(|error| format!("invalid manifest path `{value}`: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        matches.sort();
        for path in matches
            .into_iter()
            .filter(|path| path.is_file() || allow_directory && path.is_dir())
        {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| format!("manifest path is outside the project: {}", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            resolved.push(relative);
        }
    }
    Ok(resolved)
}
