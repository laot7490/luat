use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{BuildError, OutputChange, Project, ProjectConfig};
use crate::Diagnostic;

fn temp_project() -> PathBuf {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::current_dir().unwrap().join("target/tests");
    fs::create_dir_all(&root).unwrap();
    root.join(format!("luat-test-{}-{id}", std::process::id()))
}

fn main_diagnostic(error: &BuildError) -> (&PathBuf, &Diagnostic) {
    let errors = match error {
        BuildError::Compile { path, error } => return (path, &error.diagnostics[0]),
        BuildError::CompileMany(errors) => errors,
        _ => panic!("expected a compile error: {error}"),
    };
    let (path, error) = errors
        .iter()
        .find(|(path, _)| path.ends_with("main.luat"))
        .unwrap();
    (path, &error.diagnostics[0])
}

#[test]
fn build_writes_dist_paths_and_keeps_old_output_on_compile_error() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::write(root.join("src/client/main.luat"), "local count: number = 1").unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
    )
    .unwrap();
    let project = Project::load(&root).unwrap();
    project.build().unwrap();
    assert!(root.join("dist/client/main.lua").exists());
    assert!(
        fs::read_to_string(root.join("fxmanifest.lua"))
            .unwrap()
            .contains("dist/client/main.lua")
    );
    fs::write(
        root.join("src/client/main.luat"),
        "local count: number = 'wrong'",
    )
    .unwrap();
    assert!(project.build().is_err());
    assert!(root.join("dist/client/main.lua").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn watch_session_reuses_unaffected_modules_and_recovers_from_failed_builds() {
    let root = temp_project();
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='watch-test'\n[fivem]\nclient=['src/*.luat']\n",
    )
    .unwrap();
    let api = src.join("api.luat");
    let main = src.join("main.luat");
    fs::write(&api, "return { value = 1 }").unwrap();
    fs::write(
        &main,
        "local api = import('./api')\nlocal value: number = api.value",
    )
    .unwrap();
    let mut session = super::BuildSession::default();
    let rebuild =
        |session: &mut super::BuildSession| Project::load(&root).unwrap().build_for_watch(session);
    rebuild(&mut session).unwrap();
    let before = fs::read(root.join("dist/main.lua")).unwrap();
    let modified = fs::metadata(root.join("dist/main.lua"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(
        Project::load(&root)
            .unwrap()
            .build_for_watch(&mut super::BuildSession::default())
            .unwrap()
            .is_empty()
    );
    assert!(rebuild(&mut session).unwrap().is_empty());
    fs::write(root.join("dist/local.txt"), "keep").unwrap();
    let parsed = session.previous.as_ref().unwrap().parsed[&main].clone();
    fs::write(&api, "return { value = 'bad' }").unwrap();
    assert!(rebuild(&mut session).is_err());
    assert_eq!(before, fs::read(root.join("dist/main.lua")).unwrap());
    let extra = src.join("extra.luat");
    fs::write(&extra, "return true").unwrap();
    assert!(rebuild(&mut session).is_err());
    assert!(!root.join("dist/extra.lua").exists());
    fs::write(&api, "return { value = 2 }").unwrap();
    let changes = rebuild(&mut session).unwrap();
    assert!(changes.contains(&OutputChange::Updated(PathBuf::from("dist/api.lua"))));
    assert!(changes.contains(&OutputChange::Added(PathBuf::from("dist/extra.lua"))));
    assert!(std::sync::Arc::ptr_eq(
        &parsed,
        &session.previous.as_ref().unwrap().parsed[&main]
    ));
    assert_eq!(before, fs::read(root.join("dist/main.lua")).unwrap());
    assert_eq!(
        modified,
        fs::metadata(root.join("dist/main.lua"))
            .unwrap()
            .modified()
            .unwrap()
    );
    assert!(root.join("dist/local.txt").exists());
    fs::rename(extra, src.join("renamed.luat")).unwrap();
    let changes = rebuild(&mut session).unwrap();
    assert!(changes.contains(&OutputChange::Removed(PathBuf::from("dist/extra.lua"))));
    assert!(changes.contains(&OutputChange::Added(PathBuf::from("dist/renamed.lua"))));
    assert!(!root.join("dist/extra.lua").exists());
    assert!(root.join("dist/renamed.lua").exists());
    let output = fs::read(root.join("dist/renamed.lua")).unwrap();
    Project::load(&root).unwrap().build().unwrap();
    assert_eq!(output, fs::read(root.join("dist/renamed.lua")).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn config_defaults_and_rejects_output_path_traversal() {
    let config = toml::from_str::<ProjectConfig>("[project]\nname='test'").unwrap();
    assert_eq!(config.compiler.out_dir, PathBuf::from("dist"));
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::write(root.join("src/client/main.luat"), "return true").unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[compiler]\nout_dir='../escape'\n",
    )
    .unwrap();
    assert!(Project::load(&root).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn build_manages_the_entire_dist_directory_and_writes_manifest() {
    let root = temp_project();
    fs::create_dir_all(root.join("dist")).unwrap();
    fs::write(root.join("dist/keep.txt"), "keep").unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='empty'\n").unwrap();
    Project::load(&root).unwrap().build().unwrap();
    assert!(!root.join("dist/keep.txt").exists());
    assert!(root.join("fxmanifest.lua").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_output_directories_that_contain_sources() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::write(root.join("src/client/main.luat"), "return true").unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[compiler]\nout_dir='src'\n",
    )
    .unwrap();
    assert!(Project::load(&root).is_err());
    assert_eq!(
        fs::read_to_string(root.join("src/client/main.luat")).unwrap(),
        "return true"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn declaration_files_cannot_silently_drop_runtime_code() {
    let root = temp_project();
    fs::create_dir_all(root.join("types")).unwrap();
    fs::write(
        root.join("types/api.d.luat"),
        "typedef PlayerId = number\nprint('oops')",
    )
    .unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nshared=['types/*.d.luat']\n",
    )
    .unwrap();
    let project = Project::load(&root).unwrap();
    let error = project.check().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("type declarations and empty function signatures only"),
        "{error}"
    );
    fs::write(root.join("types/api.d.luat"), "typedef PlayerId = number\n").unwrap();
    assert!(project.check().is_ok());
    assert_eq!(project.build().unwrap().compiled_files, 0);
    let manifest = fs::read_to_string(root.join("fxmanifest.lua")).unwrap();
    assert!(!manifest.contains("api.d.luat") && !manifest.contains("api.d.lua"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn check_accepts_runtime_sources() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::write(root.join("src/client/main.luat"), "print('ready')").unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='test'\n").unwrap();
    let result = Project::load(&root).unwrap().check();
    assert!(result.is_ok(), "{result:?}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn client_context_rejects_server_only_natives() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::write(root.join("src/client/main.luat"), "MumbleCreateChannel(1)").unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
    )
    .unwrap();

    let error = Project::load(&root).unwrap().check().unwrap_err();
    assert_eq!(main_diagnostic(&error).1.code, "LT2010");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_scopes_declaration_aliases_and_globals() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("types")).unwrap();
    fs::write(
        root.join("types/client.d.luat"),
        "typedef PlatformValueType = string\ndeclare PlatformValue: PlatformValueType",
    )
    .unwrap();
    fs::write(
        root.join("types/server.d.luat"),
        "typedef PlatformValueType = number\ndeclare PlatformValue: PlatformValueType",
    )
    .unwrap();
    fs::write(
        root.join("src/client/main.luat"),
        "local value: string = PlatformValue",
    )
    .unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='test'\n[fivem]\nclient=['types/client.d.luat', 'src/client/main.luat']\nserver=['types/server.d.luat']\n").unwrap();

    let result = Project::load(&root).unwrap().check();
    assert!(result.is_ok(), "{result:?}");
    fs::write(
        root.join("src/client/main.luat"),
        "local value = PlatformServerValue",
    )
    .unwrap();
    fs::write(
        root.join("types/server.d.luat"),
        "declare PlatformServerValue: number",
    )
    .unwrap();
    let error = Project::load(&root).unwrap().check().unwrap_err();
    assert_eq!(main_diagnostic(&error).1.code, "LT2010");
    fs::write(
        root.join("types/server.d.luat"),
        "declare ServerApi: { value: number, call: () -> () }\nfunction ServerCallback() end",
    )
    .unwrap();
    for source in [
        "local value = ServerApi.value",
        "ServerApi.call()",
        "local callback = ServerCallback",
    ] {
        fs::write(root.join("src/client/main.luat"), source).unwrap();
        let error = Project::load(&root).unwrap().check().unwrap_err();
        assert_eq!(main_diagnostic(&error).1.code, "LT2010");
    }
    fs::write(
        root.join("src/client/main.luat"),
        "local function ServerCallback() end\nlocal callback = ServerCallback\ncallback()",
    )
    .unwrap();
    assert!(Project::load(&root).unwrap().check().is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_natives_are_filtered_by_execution_context() {
    for (side, function) in [
        (
            "client",
            "PerformHttpRequest('https://example.com', function() end)",
        ),
        ("server", "TriggerServerEvent('event')"),
    ] {
        let root = temp_project();
        fs::create_dir_all(root.join(format!("src/{side}"))).unwrap();
        fs::write(root.join(format!("src/{side}/main.luat")), function).unwrap();
        fs::write(
            root.join("luat.toml"),
            format!("[project]\nname='test'\n[fivem]\n{side}=['src/{side}/main.luat']\n"),
        )
        .unwrap();

        let error = Project::load(&root).unwrap().check().unwrap_err();
        assert_eq!(main_diagnostic(&error).1.code, "LT2010", "{side}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn same_native_name_keeps_its_context_specific_signature() {
    for (side, source, succeeds) in [
        ("client", "GetPlayerPed('12')", false),
        ("server", "GetPlayerPed('12')", true),
    ] {
        let root = temp_project();
        fs::create_dir_all(root.join(format!("src/{side}"))).unwrap();
        fs::write(root.join(format!("src/{side}/main.luat")), source).unwrap();
        fs::write(
            root.join("luat.toml"),
            format!("[project]\nname='test'\n[fivem]\n{side}=['src/{side}/main.luat']\n"),
        )
        .unwrap();

        let result = Project::load(&root).unwrap().check();
        assert_eq!(result.is_ok(), succeeds, "{side}: {result:?}");
        if let Err(error) = result {
            assert_eq!(main_diagnostic(&error).1.code, "LT2002");
        }
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn shared_natives_and_runtime_declarations_work_in_both_contexts() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::write(
		root.join("src/shared/main.luat"),
		"GetCurrentResourceName()\nCitizen.Wait(0)\njson.encode({ ready = true })\nexports('ready', function() end)",
	)
	.unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nshared=['src/shared/main.luat']\n",
    )
    .unwrap();

    assert!(Project::load(&root).unwrap().check().is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn imports_propagate_both_execution_contexts_into_shared_modules() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("src/server")).unwrap();
    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::write(
        root.join("src/shared/resource.luat"),
        "return GetCurrentResourceName()",
    )
    .unwrap();
    fs::write(
        root.join("src/client/main.luat"),
        "local resource = import('@/shared/resource')\nprint(resource)",
    )
    .unwrap();
    fs::write(
        root.join("src/server/main.luat"),
        "local resource = import('@/shared/resource')\nprint(resource)",
    )
    .unwrap();
    fs::write(
		root.join("luat.toml"),
		"[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\nserver=['src/server/main.luat']\n",
	)
	.unwrap();

    assert!(Project::load(&root).unwrap().check().is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_check_uses_unsaved_imported_module_text_without_writing_it() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::write(
        root.join("src/shared/api.luat"),
        "return { get = function(): string return 'ready' end }",
    )
    .unwrap();
    fs::write(
        root.join("src/client/main.luat"),
        "local api = import('@/shared/api')\nlocal value: string = api.get()",
    )
    .unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
    )
    .unwrap();
    let project = Project::load(&root).unwrap();
    assert!(project.check().is_ok());

    let overlay_path = root.join("src/shared/api.luat");
    let overlays = std::collections::HashMap::from([(
        overlay_path.clone(),
        "return { get = function(): number return 1 end }".to_owned(),
    )]);
    let error = project.check_with_overlays(&overlays).unwrap_err();
    assert_eq!(main_diagnostic(&error).1.code, "LT2001");
    assert!(
        fs::read_to_string(overlay_path)
            .unwrap()
            .contains("return 'ready'")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn project_check_uses_unsaved_declaration_text() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("types")).unwrap();
    fs::write(
        root.join("types/api.d.luat"),
        "function getValue(): string end",
    )
    .unwrap();
    fs::write(
        root.join("src/client/main.luat"),
        "local value: string = getValue()",
    )
    .unwrap();
    fs::write(
		root.join("luat.toml"),
		"[project]\nname='test'\n[fivem]\nshared=['types/*.d.luat']\nclient=['src/client/main.luat']\n",
	)
	.unwrap();
    let project = Project::load(&root).unwrap();
    assert!(project.check().is_ok());

    let path = root.join("types/api.d.luat");
    let overlays = std::collections::HashMap::from([(
        path.clone(),
        "function getValue(): number end".to_owned(),
    )]);
    let error = project.check_with_overlays(&overlays).unwrap_err();
    assert_eq!(main_diagnostic(&error).1.code, "LT2001");
    assert!(fs::read_to_string(path).unwrap().contains(": string"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ignores_unreferenced_sources_and_unconfigured_declarations() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("src/unused")).unwrap();
    fs::write(root.join("src/client/main.luat"), "print('ready')").unwrap();
    fs::write(
        root.join("src/unused/broken.luat"),
        "local x: number = 'bad'",
    )
    .unwrap();
    fs::write(root.join("src/unused/types.d.luat"), "not valid lua !!!").unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
    )
    .unwrap();
    let project = Project::load(&root).unwrap();
    let checked = project.check().unwrap();
    assert_eq!(checked.compiled_files, 1);
    assert_eq!(checked.declaration_files, 0);
    let built = project.build().unwrap();
    assert_eq!(built.compiled_files, 1);
    assert!(!root.join("dist/unused/broken.lua").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn includes_transitive_imports_from_manifest_entries() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::write(
        root.join("src/client/main.luat"),
        "local value = import('@/shared/one')\nprint(value)",
    )
    .unwrap();
    fs::write(root.join("src/shared/one.luat"), "return import('./two')").unwrap();
    fs::write(root.join("src/shared/two.luat"), "return 42").unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
    )
    .unwrap();
    let project = Project::load(&root).unwrap();
    assert_eq!(project.check().unwrap().compiled_files, 3);
    project.build().unwrap();
    for path in [
        "dist/client/main.lua",
        "dist/shared/one.lua",
        "dist/shared/two.lua",
    ] {
        assert!(root.join(path).exists(), "missing {path}");
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn orders_explicit_glob_entries_through_an_implicit_import() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/shared/helpers")).unwrap();
    fs::write(
        root.join("src/shared/a.luat"),
        "print(import('./helpers/b'))",
    )
    .unwrap();
    fs::write(
        root.join("src/shared/helpers/b.luat"),
        "return import('../z')",
    )
    .unwrap();
    fs::write(root.join("src/shared/z.luat"), "return 42").unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nshared=['src/shared/*.luat']\n",
    )
    .unwrap();

    let project = Project::load(&root).unwrap();
    project.check().unwrap();
    project.build().unwrap();
    let manifest = fs::read_to_string(root.join("fxmanifest.lua")).unwrap();
    let dependency = manifest.find("dist/_luat/entries/shared/z.lua").unwrap();
    let consumer = manifest.find("dist/_luat/entries/shared/a.lua").unwrap();
    assert!(dependency < consumer, "{manifest}");
    assert!(manifest.contains("dist/shared/helpers/b.lua"), "{manifest}");
    assert!(
        !manifest.contains("dist/_luat/entries/shared/helpers/b.lua"),
        "{manifest}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_installed_package_files_in_script_lists() {
    let root = temp_project();
    let module = ".luat/packages/example/1.0.0/init.lua";
    fs::create_dir_all(root.join(module).parent().unwrap()).unwrap();
    fs::write(root.join(module), "return {}").unwrap();
    for pattern in [module, "**/init.lua"] {
        fs::write(
            root.join("luat.toml"),
            format!("[project]\nname='test'\n[fivem]\nshared=['{pattern}']\n"),
        )
        .unwrap();
        let Err(error) = Project::load(&root) else {
            panic!("accepted {pattern}")
        };
        assert!(
            error.to_string().contains("cannot be listed as a script"),
            "{error}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_imports_keep_the_last_successful_build() {
    let root = temp_project();
    let shared = root.join("src/shared");
    let server = root.join("src/server");
    fs::create_dir_all(&shared).unwrap();
    fs::create_dir_all(&server).unwrap();
    let importer = shared.join("main.luat");
    fs::write(&importer, "print('valid')").unwrap();
    fs::write(shared.join("cycle.luat"), "return import('./main')").unwrap();
    fs::write(server.join("private.luat"), "return 42").unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='test'\n[fivem]\nshared=['src/shared/main.luat']\nserver=['src/server/private.luat']\n").unwrap();
    let project = Project::load(&root).unwrap();
    project.build().unwrap();
    let output = root.join("dist/shared/main.lua");
    let original = fs::read(&output).unwrap();
    for (specifier, code) in [
        ("./missing", "LT4003"),
        ("./cycle", "LT4005"),
        ("../server/private", "LT4006"),
    ] {
        fs::write(&importer, format!("return import('{specifier}')")).unwrap();
        for error in [project.check().unwrap_err(), project.build().unwrap_err()] {
            let (_, diagnostic) = main_diagnostic(&error);
            assert_eq!(diagnostic.code, code, "{error}");
        }
        assert_eq!(fs::read(&output).unwrap(), original);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn checks_imported_table_function_and_reexport_signatures() {
    for (module, call) in [
        (
            "src/shared/api.luat",
            "return { Print = function(status: 'info' | 'warning', message: string) end }",
        ),
        (
            "src/shared/api.luat",
            "local api = {}\napi.Print = function(status: 'info' | 'warning', message: string) end\nreturn api",
        ),
        (
            "src/shared/api.luat",
            "typedef Level = 'info' | 'warning'\nreturn { Print = function(status: Level, message: string) end }",
        ),
        (
            "src/shared/api.luat",
            "return function(status: 'info' | 'warning', message: string) end",
        ),
        ("src/shared/api.luat", "return import('./nested')"),
    ] {
        let root = temp_project();
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        fs::create_dir_all(root.join("src/shared/nested")).unwrap();
        let target = if call.contains("nested") {
            fs::write(
                root.join("src/shared/nested.luat"),
                "return { Print = function(status: 'info' | 'warning', message: string) end }",
            )
            .unwrap();
            "cfg.Print('random', 'message')"
        } else if call.contains("Print") {
            "cfg.Print('random', 'message')"
        } else {
            "cfg('random', 'message')"
        };
        fs::write(root.join(module), call).unwrap();
        let main_source =
            format!("local text = `Merhaba {{1}}`\nlocal cfg = import('@/shared/api')\n{target}");
        fs::write(root.join("src/client/main.luat"), &main_source).unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("dist/client")).unwrap();
        fs::write(root.join("dist/client/main.lua"), "preserve me").unwrap();
        let project = Project::load(&root).unwrap();
        let check_error = project.check().expect_err(call);
        let build_error = project.build().expect_err(call);
        let (check_path, check_diagnostic) = main_diagnostic(&check_error);
        let (build_path, build_diagnostic) = main_diagnostic(&build_error);
        assert_eq!(check_path, build_path);
        assert_eq!(check_diagnostic, build_diagnostic);
        assert_eq!(check_diagnostic.code, "LT2002");
        assert_eq!(
            &main_source[check_diagnostic.source_range.clone()],
            "'random'"
        );
        assert_eq!(
            fs::read_to_string(root.join("dist/client/main.lua")).unwrap(),
            "preserve me"
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn checks_return_types_of_imported_function_fields() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/server")).unwrap();
    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::write(
        root.join("src/shared/config.luat"),
        "typedef Status = 'info'\nreturn { print = function(status: Status, message: string): boolean return true end }",
    )
    .unwrap();
    let source =
        "const cfg = import('@/shared/config')\nlocal retval: number = cfg.print('info', 'hello')";
    fs::write(root.join("src/server/main.luat"), source).unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nserver=['src/server/main.luat']\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("dist/server")).unwrap();
    fs::write(root.join("dist/server/main.lua"), "preserve me").unwrap();

    let project = Project::load(&root).unwrap();
    let check_error = project.check().unwrap_err();
    let build_error = project.build().unwrap_err();
    let (_, check_diagnostic) = main_diagnostic(&check_error);
    let (_, build_diagnostic) = main_diagnostic(&build_error);
    assert_eq!(check_diagnostic.code, "LT2001");
    assert_eq!(check_diagnostic, build_diagnostic);
    assert_eq!(
        fs::read_to_string(root.join("dist/server/main.lua")).unwrap(),
        "preserve me"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn resolves_exported_aliases_in_their_defining_module_and_keeps_import_runtime() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::write(
        root.join("src/shared/api.luat"),
        "typedef Print = number\ntypedef Status = 'info' | 'warning'\ntypedef Message = { text: string }\ntypedef Api<T> = { Print: (status: Status, message: T) -> nil }\nlocal api: Api<Message> = { Print = function(status: Status, message: Message) end }\nreturn api",
    )
    .unwrap();
    fs::write(
        root.join("src/client/main.luat"),
        "typedef Print = string\ntypedef Status = number\ntypedef Message = { other: number }\ntypedef Api<T> = { Print: (status: Status, message: T) -> nil }\nlocal cfg = import('@/shared/api')\ncfg.Print('info', { text = 'ready' })",
    )
    .unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[fivem]\nclient=['src/client/main.luat']\n",
    )
    .unwrap();
    let project = Project::load(&root).unwrap();
    assert!(project.check().is_ok());
    project.build().unwrap();
    assert!(
        fs::read_to_string(root.join("dist/client/main.lua"))
            .unwrap()
            .contains("__luat.import")
    );
    assert!(
        fs::read_to_string(root.join("dist/shared/api.lua"))
            .unwrap()
            .contains("__luat.register")
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn manifest_remaps_sources_and_serializes_extra_values() {
    let config = toml::from_str::<ProjectConfig>("[project]\nname='test'\n[fivem]\nclient=['src/client/*.luat']\ndependencies=['some.luat']\n[[fivem.entries]]\nkey='custom_info'\nvalue='one'\nextra={enabled=true,count=42}\n").unwrap();
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::write(root.join("src/client/zeta.luat"), "return true").unwrap();
    fs::write(root.join("src/client/main.luat"), "return true").unwrap();
    let manifest = super::manifest::render_manifest(&config, &root).unwrap();
    assert!(manifest.contains("client_scripts { 'dist/client/main.lua', 'dist/client/zeta.lua' }"));
    assert!(manifest.contains("name 'test'"));
    assert!(manifest.contains("game 'gta5'"));
    assert!(manifest.contains("dependencies { 'some.luat' }"));
    assert!(manifest.contains("custom_info 'one' { ['count'] = 42, ['enabled'] = true }"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_modules_require_chunk_return_and_remain_lazy() {
    let root = temp_project();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("luat-package.toml"),
        "name='module-test'\nversion='1.0.0'\nauthor='test'\nlicense='MIT'\nshared=['init.luat']\n",
    )
    .unwrap();
    fs::write(
        root.join("init.luat"),
        "local function nested() return 1 end",
    )
    .unwrap();
    assert!(
        Project::load(root.join("luat-package.toml"))
            .and_then(|p| p.check())
            .is_err()
    );
    fs::write(root.join("init.luat"), "return false").unwrap();
    Project::load(root.join("luat-package.toml"))
        .unwrap()
        .check()
        .unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_check_resolves_relative_lua_modules_and_contexts() {
    let root = temp_project();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("luat-package.toml"), "name='module-test'\nversion='1.0.0'\nauthor='test'\nlicense='MIT'\nshared=['init.luat','value.lua']\nserver=['secret.luat']\n").unwrap();
    fs::write(
        root.join("init.luat"),
        "const value: string = import('./value.lua')\nreturn value",
    )
    .unwrap();
    fs::write(root.join("value.lua"), "return 42").unwrap();
    fs::write(root.join("secret.luat"), "return 'secret'").unwrap();
    let error = Project::load(root.join("luat-package.toml"))
        .unwrap()
        .check()
        .unwrap_err();
    assert!(error.to_string().contains("number"), "{error}");
    fs::write(root.join("value.lua"), "return 'ok'").unwrap();
    Project::load(root.join("luat-package.toml"))
        .unwrap()
        .check()
        .unwrap();
    fs::write(root.join("init.luat"), "return import('./secret.luat')").unwrap();
    let error = Project::load(root.join("luat-package.toml"))
        .unwrap()
        .check()
        .unwrap_err();
    assert!(error.to_string().contains("cannot import"), "{error}");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_return_validation_uses_chunk_ast() {
    for source in [
        "return {}",
        "return false",
        "local x = 1\nreturn x",
        "-- return nil\nreturn 3",
    ] {
        crate::compiler::validate_package_return(source).unwrap();
    }
    for source in [
        "",
        "return",
        "return nil",
        "return ((nil))",
        "local function f() return 1 end",
        "local x = 'return 1'",
    ] {
        assert!(
            crate::compiler::validate_package_return(source).is_err(),
            "{source}"
        );
    }
}

#[test]
fn imported_exports_use_declarations_and_each_execution_context() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/shared")).unwrap();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::create_dir_all(root.join("src/server")).unwrap();
    fs::create_dir_all(root.join("types")).unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='test'\n[fivem]\nclient=['types/client.d.luat', 'src/client/main.luat']\nserver=['types/server.d.luat', 'src/server/main.luat']\n").unwrap();
    fs::write(
        root.join("types/client.d.luat"),
        "typedef Platform = string\ndeclare PlatformValue: Platform\ndeclare OtherValue: number",
    )
    .unwrap();
    fs::write(
        root.join("types/server.d.luat"),
        "typedef Platform = number\ndeclare PlatformValue: Platform\ndeclare OtherValue: string",
    )
    .unwrap();
    fs::write(root.join("src/shared/value.luat"), "return PlatformValue").unwrap();
    fs::write(
        root.join("src/shared/api.luat"),
        "return function(value: Platform): Platform return value end",
    )
    .unwrap();
    let client = "local value: string = import('@/shared/value')\nlocal api = import('@/shared/api')\nlocal result: string = api('hello')";
    let server = "local value: number = import('@/shared/value')\nlocal api = import('@/shared/api')\nlocal result: number = api(42)";
    fs::write(root.join("src/client/main.luat"), client).unwrap();
    fs::write(root.join("src/server/main.luat"), server).unwrap();
    let project = Project::load(&root).unwrap();
    project.check().unwrap();
    project.build().unwrap();
    for (side, source) in [("client", server), ("server", client)] {
        let path = root.join(format!("src/{side}/main.luat"));
        let error = project
            .check_with_overlays(&std::collections::HashMap::from([(
                path,
                source.to_owned(),
            )]))
            .unwrap_err();
        assert!(error.to_string().contains("not assignable"), "{error}");
    }
    let old = project.analyze_imports().unwrap();
    let path = root.join("src/shared/value.luat");
    let overlays =
        std::collections::HashMap::from([(path.clone(), "return OtherValue".to_owned())]);
    let next = project.analyze_imports_with_overlays(&overlays).unwrap();
    assert_eq!(old.exports[&path], next.exports[&path]);
    let declarations = project.read_declarations().unwrap();
    let parse = |_: &std::path::Path, source: &str| {
        Ok(std::sync::Arc::new(
            crate::compiler::ParsedSource::new_prepared(
                source,
                crate::compiler::syntax::prepare(source),
            ),
        ))
    };
    let (checked, result, _) = project.check_editor_with_plan(
        &next,
        Some(&old),
        &std::collections::BTreeSet::from([path]),
        &declarations,
        &parse,
    );
    assert!(result.is_err());
    assert!(checked.contains(&root.join("src/client/main.luat")));
    assert!(checked.contains(&root.join("src/server/main.luat")));
}

#[test]
fn watch_reconciles_stale_output_and_reports_manifest_only_changes() {
    let root = temp_project();
    fs::create_dir_all(root.join("dist/old")).unwrap();
    fs::write(root.join("dist/old/dead.lua"), "old").unwrap();
    fs::write(root.join("dist/same.lua"), "same").unwrap();
    fs::write(root.join("fxmanifest.lua"), "manifest").unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='watch'\n").unwrap();
    let project = Project::load(&root).unwrap();
    let outputs = [(PathBuf::from("same.lua"), "same".into())];
    assert_eq!(
        project.publish_watch(&outputs, "manifest", None).unwrap(),
        [OutputChange::Removed(PathBuf::from("dist/old/dead.lua"))],
    );
    assert!(!root.join("dist/old").exists());
    assert_eq!(
        project
            .publish_watch(&outputs, "updated", Some(&outputs))
            .unwrap(),
        [OutputChange::Updated(PathBuf::from("fxmanifest.lua"))],
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn watch_rolls_back_when_windows_prevents_renaming_an_output() {
    use std::os::windows::fs::OpenOptionsExt;

    let root = temp_project();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='watch'\n").unwrap();
    let project = Project::load(&root).unwrap();
    let previous = [
        (PathBuf::from("a.lua"), "old a".into()),
        (PathBuf::from("b.lua"), "old b".into()),
    ];
    project
        .publish_watch(&previous, "old manifest", None)
        .unwrap();
    let locked = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(root.join("dist/b.lua"))
        .unwrap();
    let next = [
        (PathBuf::from("a.lua"), "new a".into()),
        (PathBuf::from("b.lua"), "new b".into()),
    ];
    let error = project
        .publish_watch(&next, "new manifest", Some(&previous))
        .unwrap_err();
    assert!(matches!(error, BuildError::Io { path, .. } if path == root.join("dist/b.lua")));
    assert_eq!(
        fs::read_to_string(root.join("dist/a.lua")).unwrap(),
        "old a"
    );
    assert_eq!(
        fs::read_to_string(root.join("dist/b.lua")).unwrap(),
        "old b"
    );
    assert_eq!(
        fs::read_to_string(root.join("fxmanifest.lua")).unwrap(),
        "old manifest"
    );
    drop(locked);
    assert_eq!(
        project
            .publish_watch(&next, "new manifest", Some(&previous))
            .unwrap()
            .len(),
        3
    );
    fs::remove_dir_all(root).unwrap();
}
