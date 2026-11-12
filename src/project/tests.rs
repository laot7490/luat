use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{Project, ProjectConfig};

fn temp_project() -> PathBuf {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::current_dir()
        .unwrap()
        .join("target")
        .join(format!("luat-test-{}-{id}", std::process::id()))
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
        "type PlayerId = number\nprint('oops')",
    )
    .unwrap();
    fs::write(
        root.join("luat.toml"),
        "[project]\nname='test'\n[compiler]\ndeclarations=['types/api.d.luat']\n",
    )
    .unwrap();
    let project = Project::load(&root).unwrap();
    let error = project.check().unwrap_err();
    assert!(error.to_string().contains("type declarations only"));
    fs::write(root.join("types/api.d.luat"), "type PlayerId = number\n").unwrap();
    assert!(project.check().is_ok());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn check_accepts_runtime_sources() {
    let root = temp_project();
    fs::create_dir_all(root.join("src/client")).unwrap();
    fs::write(root.join("src/client/main.luat"), "print('ready')").unwrap();
    fs::write(root.join("luat.toml"), "[project]\nname='test'\n").unwrap();
    assert!(Project::load(&root).unwrap().check().is_ok());
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
