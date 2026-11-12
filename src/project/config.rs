use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub project: ProjectMetadata,
    #[serde(default)]
    pub compiler: CompilerConfig,
    #[serde(default)]
    pub fivem: FiveMConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProjectMetadata {
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CompilerConfig {
    pub out_dir: PathBuf,
    pub declarations: Vec<PathBuf>,
}

impl Default for CompilerConfig {
    fn default() -> Self {
        Self {
            out_dir: default_out_dir(),
            declarations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FiveMConfig {
    pub fx_version: String,
    pub game: Option<String>,
    pub games: Vec<String>,
    pub shared: Vec<String>,
    pub client: Vec<String>,
    pub server: Vec<String>,
    pub files: Vec<String>,
    pub dependencies: Vec<String>,
    pub exports: Vec<String>,
    pub server_exports: Vec<String>,
    pub provide: Vec<String>,
    pub escrow_ignore: Vec<String>,
    pub ui_page: Option<String>,
    pub loadscreen: Option<String>,
    pub loadscreen_cursor: bool,
    pub loadscreen_manual_shutdown: bool,
    pub server_only: bool,
    pub this_is_a_map: bool,
    pub node_version: Option<u32>,
    pub use_experimental_fxv2_oal: bool,
    pub clr_disable_task_scheduler: bool,
    pub before_level_meta: Option<String>,
    pub after_level_meta: Option<String>,
    pub replace_level_meta: Option<String>,
    pub resource_manifest_version: Option<String>,
    pub lua54: bool,
    pub data_file: Vec<DataFile>,
    pub entries: Vec<ManifestEntry>,
}

impl Default for FiveMConfig {
    fn default() -> Self {
        Self {
            fx_version: "cerulean".into(),
            game: None,
            games: Vec::new(),
            shared: Vec::new(),
            client: Vec::new(),
            server: Vec::new(),
            files: Vec::new(),
            dependencies: Vec::new(),
            exports: Vec::new(),
            server_exports: Vec::new(),
            provide: Vec::new(),
            escrow_ignore: Vec::new(),
            ui_page: None,
            loadscreen: None,
            loadscreen_cursor: false,
            loadscreen_manual_shutdown: false,
            server_only: false,
            this_is_a_map: false,
            node_version: None,
            use_experimental_fxv2_oal: false,
            clr_disable_task_scheduler: false,
            before_level_meta: None,
            after_level_meta: None,
            replace_level_meta: None,
            resource_manifest_version: None,
            lua54: false,
            data_file: Vec::new(),
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DataFile {
    pub r#type: String,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestEntry {
    pub key: String,
    pub value: toml::Value,
    pub extra: Option<toml::Value>,
}

pub(super) fn default_out_dir() -> PathBuf {
    PathBuf::from("dist")
}
