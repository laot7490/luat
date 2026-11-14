use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Input {
    source: String,
    file: OnceLock<Arc<AnalyzedFile>>,
}

#[derive(Clone, Default)]
pub(super) struct Inputs(Arc<Mutex<HashMap<PathBuf, Vec<Arc<Input>>>>>);

impl Inputs {
    pub(super) fn file(&self, path: &Path, source: &str) -> Arc<AnalyzedFile> {
        let input = {
            let mut inputs = self.0.lock().unwrap();
            let versions = inputs.entry(path.to_path_buf()).or_default();
            versions.retain(|input| {
                input.source == source
                    || Arc::strong_count(input) > 1
                    || input.file.get().is_some_and(|file| {
                        Arc::strong_count(file) > 1 || Arc::strong_count(&file.parsed) > 1
                    })
            });
            if let Some(input) = versions.iter().find(|input| input.source == source) {
                Arc::clone(input)
            } else {
                let input = Arc::new(Input {
                    source: source.to_owned(),
                    file: OnceLock::new(),
                });
                versions.push(Arc::clone(&input));
                input
            }
        };
        Arc::clone(input.file.get_or_init(|| {
            if path.to_string_lossy().ends_with(".d.luat") {
                if let Ok((prepared, ast, _)) =
                    crate::compiler::declarations::parse_declaration_file(source)
                {
                    return Arc::new(AnalyzedFile::from_ast(
                        source,
                        prepared,
                        &ast,
                        Vec::new(),
                        true,
                    ));
                }
            }
            Arc::new(AnalyzedFile::new(source, Vec::new()))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("{name}-{id}"));
        fs::create_dir_all(root.join("src/client")).unwrap();
        fs::create_dir_all(root.join("src/shared")).unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='cache-test'\n[fivem]\nclient=['src/client/*.luat']\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn unchanged_disk_diagnostics_survive_an_unrelated_edit() {
        let root = project("cached-diagnostics");
        let main = root.join("src/client/main.luat");
        let bad = root.join("src/client/bad.luat");
        fs::write(&main, "print(1)").unwrap();
        fs::write(&bad, "local count: number = 'wrong'").unwrap();
        let mut host = AnalysisHost::new();
        host.set_file(main.clone(), "print(1)".into());
        let first = host.snapshot();
        assert!(!first.diagnostics(&bad).unwrap().is_empty());
        host.set_file(main, "print(2)".into());
        let next = host.snapshot();
        assert_eq!(first.diagnostics(&bad), next.diagnostics(&bad));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn adding_an_import_checks_the_newly_reachable_module() {
        let root = project("new-import-diagnostics");
        let main = root.join("src/client/main.luat");
        let api = root.join("src/shared/api.luat");
        fs::write(&main, "print(1)").unwrap();
        fs::write(
            &api,
            "local count: number = 'wrong'\nreturn { count = count }",
        )
        .unwrap();
        let mut host = AnalysisHost::new();
        host.set_file(main.clone(), "print(1)".into());
        host.snapshot();
        host.set_file(
            main,
            "local api = import('@/shared/api')\nprint(api.count)".into(),
        );
        assert!(!host.snapshot().diagnostics(&api).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sibling_declaration_changes_revalidate_unchanged_documents() {
        let root = project("sibling-declarations");
        fs::write(root.join("src/client/main.luat"), "print(1)").unwrap();
        let first = root.join("a.d.luat");
        let sibling = root.join("b.d.luat");
        let mut host = AnalysisHost::new();
        host.set_file(first.clone(), "typedef Derived = Base".into());
        host.set_file(sibling.clone(), "typedef Base = number".into());
        let diagnostics = host.snapshot().diagnostics(&first).unwrap().to_vec();
        assert!(
            diagnostics.is_empty(),
            "unexpected diagnostics: {diagnostics:?}"
        );
        host.set_file(sibling, "typedef Other = number".into());
        assert!(!host.snapshot().diagnostics(&first).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn editor_queries_keep_import_types_and_recover_changed_member_prefixes() {
        let root = project("cached-imports");
        let main = root.join("src/client/main.luat");
        let api = root.join("src/shared/api.luat");
        let source = "local api = import('@/shared/api')\napi.read('hello')";
        fs::write(&main, source).unwrap();
        fs::write(
            &api,
            "return { read = function(key: string): string return key end }",
        )
        .unwrap();
        let mut host = AnalysisHost::new();
        host.set_file(main.clone(), source.into());
        let checked = host.snapshot();
        let editor = host.editor_snapshot();
        assert_eq!(checked.hover(&main, 7), editor.hover(&main, 7));
        let changed = format!("{source}\napi.");
        host.set_file(main.clone(), changed.clone());
        let editor = host.editor_snapshot();
        assert!(
            editor
                .completions(&main, changed.len())
                .iter()
                .any(|item| item.label == "read")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_new_config_reassigns_a_previously_standalone_buffer() {
        let root = project("cached-project-root");
        let config = root.join("luat.toml");
        let text = fs::read_to_string(&config).unwrap();
        fs::remove_file(&config).unwrap();
        let main = root.join("src/client/main.luat");
        fs::write(&main, "print(1)").unwrap();
        let mut host = AnalysisHost::new();
        host.set_file(main.clone(), "print(1)".into());
        assert!(host.snapshot().project_root(&main).is_none());
        fs::write(&config, text).unwrap();
        host.invalidate_path(&config);
        assert_eq!(host.snapshot().project_root(&main), Some(root.clone()));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_identifier_lookup_matches_scanning_at_utf8_boundaries() {
        let source = "local value = 'Türkçe 🚀'\n-- hidden\nprint(value)";
        let file = AnalyzedFile::new(source, Vec::new());
        for offset in 0..=source.len() {
            if source.is_char_boundary(offset) {
                assert_eq!(
                    file.identifier_range_at(offset),
                    identifier_range_at(source, offset)
                );
            }
        }
    }

    #[test]
    fn cancelled_analysis_keeps_the_latest_change_pending() {
        let path = PathBuf::from("cancelled.luat");
        let mut host = AnalysisHost::new();
        host.set_file(path.clone(), "local value = 1".into());
        host.snapshot();
        host.set_file(path.clone(), "local value = 2".into());
        assert!(host.snapshot_cancellable(&|| true).is_none());
        assert_eq!(host.snapshot().source(&path), Some("local value = 2"));
    }
}
