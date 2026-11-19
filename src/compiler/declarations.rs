use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::PathBuf,
};

use full_moon::{
    LuaVersion,
    ast::{self, luau::TypeDeclaration},
    node::Node,
    tokenizer::{Lexer, LexerResult, TokenType},
    visitors::Visitor,
};

use crate::{CompileError, Diagnostic, ExecutionContext};

use super::{syntax::PreparedSource, typecheck::FunctionSignature};

#[derive(Clone)]
pub(crate) struct TypeAlias {
    pub(crate) parameters: Vec<String>,
    pub(crate) definition: String,
}

#[derive(Default)]
pub(crate) struct AliasCollector {
    pub(crate) aliases: HashMap<String, TypeAlias>,
}

impl Visitor for AliasCollector {
    fn visit_type_declaration(&mut self, node: &TypeDeclaration) {
        let name = node.type_name().token().to_string();
        let parameters = node
            .generics()
            .into_iter()
            .flat_map(|generics| generics.generics().iter())
            .map(|parameter| parameter.parameter().to_string().trim().to_owned())
            .collect();
        self.aliases.insert(
            name,
            TypeAlias {
                parameters,
                definition: super::typecheck::strip_lua_comments(
                    node.type_definition().to_string().trim(),
                ),
            },
        );
    }
}

#[derive(Clone)]
pub(crate) struct DeclarationLocation {
    pub(crate) path: Option<PathBuf>,
    pub(crate) range: Range<usize>,
}

pub(crate) struct DeclaredFunction {
    pub(super) signature: FunctionSignature,
    pub(super) project: bool,
    pub(crate) context: Option<ExecutionContext>,
    pub(crate) documentation: String,
    pub(crate) location: DeclarationLocation,
}

pub(crate) struct DeclaredGlobal {
    pub(crate) name: String,
    pub(crate) type_name: String,
    pub(crate) documentation: String,
    pub(crate) location: DeclarationLocation,
    pub(crate) context: Option<ExecutionContext>,
    pub(super) project: bool,
}

pub(crate) struct DeclarationSource {
    pub(crate) path: Option<PathBuf>,
    pub(crate) context: Option<ExecutionContext>,
    pub(crate) text: String,
}

pub(crate) struct DeclarationFile {
    pub(crate) path: Option<PathBuf>,
    pub(crate) source: String,
    pub(crate) prepared: PreparedSource,
    pub(crate) ast: full_moon::ast::Ast,
    pub(super) project: bool,
    pub(crate) context: Option<ExecutionContext>,
}

#[derive(Default)]
pub(crate) struct DeclarationSet {
    pub(crate) aliases: HashMap<String, TypeAlias>,
    pub(crate) scoped_aliases: HashMap<ExecutionContext, HashMap<String, TypeAlias>>,
    pub(crate) functions: HashMap<String, Vec<DeclaredFunction>>,
    pub(crate) globals: HashMap<String, DeclaredGlobal>,
    pub(crate) scoped_globals: HashMap<ExecutionContext, HashMap<String, DeclaredGlobal>>,
    pub(crate) files: Vec<DeclarationFile>,
    pub(super) package_aliases: HashMap<Option<ExecutionContext>, HashMap<String, String>>,
}

impl DeclarationSet {
    pub(crate) fn from_texts(sources: &[String]) -> Result<Self, CompileError> {
        Self::from_ranked_sources(
            sources
                .iter()
                .map(|text| DeclarationSource {
                    path: None,
                    context: None,
                    text: text.clone(),
                })
                .map(|source| (source, false)),
            false,
        )
        .map_err(|(_, error)| error)
    }

    pub(crate) fn from_sources(
        sources: impl IntoIterator<Item = DeclarationSource>,
    ) -> Result<Self, (Option<PathBuf>, CompileError)> {
        Self::from_ranked_sources(sources.into_iter().map(|source| (source, false)), true)
    }

    pub(crate) fn from_ranked_sources(
        sources: impl IntoIterator<Item = (DeclarationSource, bool)>,
        validate_declarations: bool,
    ) -> Result<Self, (Option<PathBuf>, CompileError)> {
        let mut set = Self::default();
        let mut project_aliases = HashMap::new();
        let mut project_scoped_aliases: HashMap<ExecutionContext, HashMap<String, TypeAlias>> =
            HashMap::new();
        for (source, project) in sources {
            let context = source.context;
            let (prepared, ast, globals) = parse_declaration_file(&source.text)
                .map_err(|error| (source.path.clone(), error))?;
            let mut aliases = AliasCollector::default();
            aliases.visit_ast(&ast);

            for statement in ast.nodes().stmts() {
                if let ast::Stmt::FunctionDeclaration(function) = statement {
                    let name = function.name().to_string().trim().to_owned();
                    let mut collector = super::typecheck::FunctionCollector::default();
                    collector.add_ast(name.clone(), function.body());
                    if let Some(signature) = collector.functions.remove(&name) {
                        let start = function
                            .name()
                            .start_position()
                            .map(|position| prepared.original_offset(position.bytes()))
                            .unwrap_or_default();
                        let end = function
                            .name()
                            .end_position()
                            .map(|position| prepared.original_offset(position.bytes()))
                            .unwrap_or(start);
                        if set.globals.contains_key(&name)
                            || set.scoped_globals.iter().any(|(side, globals)| {
                                (context.is_none() || context == Some(*side))
                                    && globals.contains_key(&name)
                            })
                        {
                            return Err((
                                source.path,
                                CompileError {
                                    diagnostics: vec![Diagnostic::at(
                                        &source.text,
                                        start,
                                        name.chars().count(),
                                        "LT2009",
                                        format!(
                                            "Global '{}' is declared as both a function and a value.",
                                            name
                                        ),
                                    )],
                                },
                            ));
                        }
                        set.functions
                            .entry(name.clone())
                            .or_default()
                            .push(DeclaredFunction {
                                signature,
                                project,
                                context,
                                documentation: documentation_before(&source.text, start),
                                location: DeclarationLocation {
                                    path: source.path.clone(),
                                    range: start..end,
                                },
                            });
                    }
                }
            }

            let (shared_aliases, scoped_aliases) = if project {
                (&mut project_aliases, &mut project_scoped_aliases)
            } else {
                (&mut set.aliases, &mut set.scoped_aliases)
            };
            for (name, alias) in aliases.aliases {
                if let Some(context) = context {
                    let shared = shared_aliases.get(&name);
                    let scoped = scoped_aliases
                        .get(&context)
                        .and_then(|aliases| aliases.get(&name));
                    if [shared, scoped].into_iter().flatten().any(|previous| {
                        previous.parameters != alias.parameters
                            || previous.definition != alias.definition
                    }) {
                        return Err((
                            source.path.clone(),
                            CompileError {
                                diagnostics: vec![Diagnostic::at(
                                    &source.text,
                                    0,
                                    name.chars().count(),
                                    "LT2009",
                                    format!(
                                        "Type alias '{name}' has conflicting declarations in the same execution context."
                                    ),
                                )],
                            },
                        ));
                    }
                    scoped_aliases
                        .entry(context)
                        .or_default()
                        .insert(name, alias);
                } else {
                    if shared_aliases.get(&name).is_some_and(|previous| {
                        previous.parameters != alias.parameters
                            || previous.definition != alias.definition
                    }) {
                        return Err((
                            source.path.clone(),
                            CompileError {
                                diagnostics: vec![Diagnostic::at(
                                    &source.text,
                                    0,
                                    name.chars().count(),
                                    "LT2009",
                                    format!(
                                        "Type alias '{name}' has conflicting declarations in the same execution context."
                                    ),
                                )],
                            },
                        ));
                    }
                    if scoped_aliases
                        .values()
                        .filter_map(|aliases| aliases.get(&name))
                        .any(|previous| {
                            previous.parameters != alias.parameters
                                || previous.definition != alias.definition
                        })
                    {
                        return Err((
                            source.path.clone(),
                            CompileError {
                                diagnostics: vec![Diagnostic::at(
                                    &source.text,
                                    0,
                                    name.chars().count(),
                                    "LT2009",
                                    format!(
                                        "Shared type alias '{name}' conflicts with an environment-specific declaration."
                                    ),
                                )],
                            },
                        ));
                    }
                    shared_aliases.insert(name, alias);
                }
            }
            for mut global in globals {
                global.location.path = source.path.clone();
                global.context = context;
                global.project = project;
                if set.functions.get(&global.name).is_some_and(|functions| {
                    functions.iter().any(|function| {
                        context.is_none()
                            || function.context.is_none()
                            || function.context == context
                    })
                }) {
                    return Err((
                        source.path,
                        CompileError {
                            diagnostics: vec![Diagnostic::at(
                                &source.text,
                                global.location.range.start,
                                global.name.chars().count(),
                                "LT2009",
                                format!(
                                    "Global '{}' is declared as both a function and a value.",
                                    global.name
                                ),
                            )],
                        },
                    ));
                }
                let previous = context
                    .and_then(|context| set.scoped_globals.get(&context))
                    .and_then(|globals| globals.get(&global.name))
                    .or_else(|| set.globals.get(&global.name))
                    .or_else(|| {
                        context
                            .is_none()
                            .then(|| {
                                set.scoped_globals
                                    .values()
                                    .filter_map(|globals| globals.get(&global.name))
                                    .find(|previous| previous.type_name != global.type_name)
                            })
                            .flatten()
                    });
                if let Some(previous) = previous
                    && previous.type_name != global.type_name
                {
                    return Err((
                        source.path,
                        CompileError {
                            diagnostics: vec![Diagnostic::at(
                                &source.text,
                                global.location.range.start,
                                global.name.chars().count(),
                                "LT2009",
                                format!(
                                    "Global '{}' has conflicting declarations: '{}' and '{}'.",
                                    global.name, previous.type_name, global.type_name
                                ),
                            )],
                        },
                    ));
                }
                if let Some(context) = context {
                    set.scoped_globals
                        .entry(context)
                        .or_default()
                        .entry(global.name.clone())
                        .or_insert(global);
                } else {
                    set.globals.entry(global.name.clone()).or_insert(global);
                }
            }

            set.files.push(DeclarationFile {
                path: source.path,
                source: source.text,
                prepared,
                ast,
                context,
                project,
            });
        }
        if validate_declarations {
            super::typecheck::validate_declaration_types(&set, false)?;
        }
        let mut bound_aliases = Vec::new();
        for (scope, context) in [
            None,
            Some(ExecutionContext::Client),
            Some(ExecutionContext::Server),
        ]
        .into_iter()
        .enumerate()
        {
            let scoped = context.and_then(|side| set.scoped_aliases.get(&side));
            let project_scoped = context.and_then(|side| project_scoped_aliases.get(&side));
            let mut package_names = set
                .aliases
                .keys()
                .chain(scoped.into_iter().flat_map(|aliases| aliases.keys()));
            if package_names.any(|name| {
                project_aliases.contains_key(name)
                    || project_scoped.is_some_and(|aliases| aliases.contains_key(name))
            }) {
                let mut aliases = set.aliases.clone();
                if let Some(scoped) = scoped {
                    aliases.extend(scoped.clone());
                }
                let bindings: HashMap<_, _> = aliases
                    .keys()
                    .map(|name| (name.clone(), format!("$package{scope}_{name}")))
                    .collect();
                let mut bound = HashMap::new();
                for (name, mut alias) in aliases {
                    alias.definition = super::typecheck::bind_package_aliases(
                        &alias.definition,
                        &bindings,
                        &alias.parameters,
                    );
                    bound.insert(bindings[&name].clone(), alias.clone());
                    if !project_aliases.contains_key(&name)
                        && project_scoped.is_none_or(|aliases| !aliases.contains_key(&name))
                    {
                        bound.insert(name, alias);
                    }
                }
                set.package_aliases.insert(context, bindings);
                bound_aliases.push((context, bound));
            }
        }
        for aliases in set.scoped_aliases.values_mut() {
            aliases.retain(|name, _| !project_aliases.contains_key(name));
        }
        set.aliases.extend(project_aliases);
        for (context, aliases) in project_scoped_aliases {
            set.scoped_aliases
                .entry(context)
                .or_default()
                .extend(aliases);
        }
        for (context, aliases) in bound_aliases {
            if let Some(context) = context {
                set.scoped_aliases
                    .entry(context)
                    .or_default()
                    .extend(aliases);
            } else {
                set.aliases.extend(aliases);
            }
        }
        if validate_declarations && set.files.iter().any(|file| file.project) {
            super::typecheck::validate_declaration_types(&set, true)?;
        }
        Ok(set)
    }
}

pub(crate) fn parse_declaration_file(
    source: &str,
) -> Result<(PreparedSource, full_moon::ast::Ast, Vec<DeclaredGlobal>), CompileError> {
    if let Some(error) = super::syntax::invalid_type_alias_error(source) {
        return Err(error);
    }
    let prepared = super::syntax::prepare(source);
    if !prepared.template_diagnostics.is_empty() {
        return Err(CompileError {
            diagnostics: prepared.template_diagnostics,
        });
    }
    let version = LuaVersion::cfxlua().with_luau();
    let tokens = match Lexer::new(&prepared.text, version).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => Vec::new(),
    };
    let mut text = prepared.text.as_bytes().to_vec();
    let mut declarations = HashSet::new();
    for token in tokens {
        if !matches!(
            token.token_type(),
            TokenType::Identifier { identifier } if identifier.to_string() == "declare"
        ) {
            continue;
        }
        let start = token.start_position().bytes();
        let line_start = prepared.text[..start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        if !prepared.text[line_start..start].trim().is_empty() {
            continue;
        }
        text[start..start + "declare".len()].copy_from_slice(b"local  ");
        declarations.insert(start);
    }
    let text = String::from_utf8(text).expect("keyword replacement preserves UTF-8");
    let parsed = full_moon::parse_fallible(&text, version);
    if !parsed.errors().is_empty() {
        return Err(CompileError {
            diagnostics: crate::diagnostic::parser_diagnostics(
                parsed.errors(),
                "",
                source,
                |offset| prepared.original_offset(offset),
            ),
        });
    }
    let ast = parsed.into_ast();
    let mut globals = Vec::new();
    for statement in ast.nodes().stmts() {
        match statement {
            ast::Stmt::TypeDeclaration(_) | ast::Stmt::ExportedTypeDeclaration(_) => {}
            ast::Stmt::FunctionDeclaration(function)
                if function.body().block().stmts().next().is_none() => {}
            ast::Stmt::LocalAssignment(local)
                if declarations.contains(
                    &local
                        .local_token()
                        .start_position()
                        .map(|position| position.bytes())
                        .unwrap_or_default(),
                ) =>
            {
                let start = local
                    .local_token()
                    .start_position()
                    .map(|position| position.bytes())
                    .unwrap_or_default();
                let names = local.names().iter().collect::<Vec<_>>();
                let type_specifier = local.type_specifiers().next().flatten();
                if names.len() != 1
                    || !local.expressions().is_empty()
                    || local.equal_token().is_some()
                    || type_specifier.is_none()
                {
                    return Err(executable_declaration_error(source, &prepared, start));
                }
                let name = names[0].token().to_string();
                let name_start = names[0]
                    .start_position()
                    .map(|position| prepared.original_offset(position.bytes()))
                    .unwrap_or_default();
                let name_end = names[0]
                    .end_position()
                    .map(|position| prepared.original_offset(position.bytes()))
                    .unwrap_or(name_start);
                let start = prepared.original_offset(start);
                globals.push(DeclaredGlobal {
                    project: false,
                    name,
                    type_name: type_specifier
                        .unwrap()
                        .type_info()
                        .to_string()
                        .trim()
                        .to_owned(),
                    documentation: documentation_before(source, start),
                    location: DeclarationLocation {
                        path: None,
                        range: name_start..name_end,
                    },
                    context: None,
                });
            }
            _ => {
                let start = statement
                    .start_position()
                    .map(|position| position.bytes())
                    .unwrap_or_default();
                return Err(executable_declaration_error(source, &prepared, start));
            }
        }
    }
    Ok((prepared, ast, globals))
}

fn executable_declaration_error(
    source: &str,
    prepared: &PreparedSource,
    start: usize,
) -> CompileError {
    CompileError {
        diagnostics: vec![Diagnostic::at(
            source,
            prepared.original_offset(start),
            1,
            "LT1002",
            "Declaration files may contain type declarations and empty function signatures only."
                .to_owned(),
        )],
    }
}

fn documentation_before(source: &str, offset: usize) -> String {
    let Some(prefix) = source.get(..offset) else {
        return String::new();
    };
    let lines = prefix
        .lines()
        .rev()
        .take_while(|line| line.trim_start().starts_with("---"))
        .collect::<Vec<_>>();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::{DeclarationSet, DeclarationSource};
    use crate::{
        Diagnostic,
        project::{BuildError, Project},
    };
    use std::{fs, time::SystemTime};

    #[test]
    fn declaration_context_comes_from_the_manifest() {
        let declarations = DeclarationSet::from_sources([DeclarationSource {
            path: None,
            context: Some(crate::ExecutionContext::Server),
            text: "---@client\ntypedef Api = number\nfunction query(): Api end".to_owned(),
        }])
        .unwrap();
        assert_eq!(
            declarations.files[0].context,
            Some(crate::ExecutionContext::Server)
        );
        assert!(declarations.scoped_aliases[&crate::ExecutionContext::Server].contains_key("Api"));
    }

    fn check_project(declaration: &str, source: &str) -> Result<(), Vec<Diagnostic>> {
        let id = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target/tests")
            .join(format!("declarations-{}-{id}", std::process::id()));
        fs::create_dir_all(root.join("types")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("types/api.d.luat"), declaration).unwrap();
        fs::write(root.join("src/main.luat"), source).unwrap();
        fs::write(
            root.join("luat.toml"),
            "[project]\nname='test'\n[fivem]\nshared=['types/api.d.luat', 'src/main.luat']\n",
        )
        .unwrap();

        let result = Project::load(&root).unwrap().check();
        fs::remove_dir_all(&root).unwrap();
        match result {
            Ok(result) => {
                assert_eq!(result.compiled_files, 1);
                Ok(())
            }
            Err(BuildError::Compile { error, .. }) => Err(error.diagnostics),
            Err(BuildError::CompileMany(errors)) => Err(errors
                .into_iter()
                .flat_map(|(_, error)| error.diagnostics)
                .collect()),
            Err(error) => panic!("expected source diagnostics, got {error}"),
        }
    }

    fn diagnostic_codes(error: &[Diagnostic]) -> Vec<&str> {
        error.iter().map(|diagnostic| diagnostic.code).collect()
    }

    #[test]
    fn accepts_ambient_function_signatures() {
        let declaration = "function add(left: integer, right: integer): integer end";
        assert!(check_project(declaration, "local result: integer = add(1, 2)").is_ok());

        let error = check_project(declaration, "add('wrong', 2)").unwrap_err();
        assert!(diagnostic_codes(&error).contains(&"LT2002"), "{error:?}");
    }

    #[test]
    fn accepts_declared_globals() {
        let result = check_project(
            "typedef CustomStateBag = string\ndeclare CustomGlobalState: CustomStateBag",
            "local state: CustomStateBag = CustomGlobalState",
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn checks_declared_overloads() {
        let declarations =
            "function parse(value: string): number end\nfunction parse(value: number): string end";
        let model = DeclarationSet::from_texts(&[declarations.to_owned()]).unwrap();
        assert_eq!(model.functions["parse"].len(), 2);
        assert!(check_project(declarations, "local text: string = parse(42)").is_ok());

        let error = check_project(declarations, "parse(true)").unwrap_err();
        assert!(diagnostic_codes(&error).contains(&"LT2002"), "{error:?}");
        let error = check_project(declarations, "local wrong: boolean = parse(42)").unwrap_err();
        assert!(
            error.iter().any(|diagnostic| diagnostic.code == "LT2001"
                && diagnostic
                    .message
                    .contains("not assignable to type 'boolean'")),
            "{error:?}"
        );
    }

    #[test]
    fn treats_function_intersections_as_overloads() {
        let declarations = "typedef Parse = ((value: string) -> number) & ((value: number) -> string)\ndeclare parse: Parse";
        assert!(
            check_project(declarations, "local text: string = parse(42)").is_ok(),
            "{:?}",
            check_project(declarations, "local text: string = parse(42)")
        );
        assert!(check_project(declarations, "local count: number = parse('hello')").is_ok());

        let error = check_project(declarations, "parse(true)").unwrap_err();
        assert!(diagnostic_codes(&error).contains(&"LT2002"), "{error:?}");
    }

    #[test]
    fn local_values_shadow_declared_globals() {
        let declaration =
            "typedef CustomStateBag = string\ndeclare CustomGlobalState: CustomStateBag";
        let result = check_project(
            declaration,
            "local CustomGlobalState: number = 1\nlocal value: number = CustomGlobalState",
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn preserves_multiple_return_signatures() {
        let declaration = "function get_pair(): (integer, string) end";
        assert!(
            check_project(declaration, "local id: integer, name: string = get_pair()",).is_ok()
        );
    }

    #[test]
    fn allows_omitted_optional_signature_parameters() {
        assert!(
            check_project(
                "function greet(name: string?): string end",
                "local greeting: string = greet()",
            )
            .is_ok()
        );
    }

    #[test]
    fn infers_generic_ambient_return_types() {
        let declaration = "function identity<T>(value: T): T end";
        assert!(check_project(declaration, "local text: string = identity('hello')").is_ok());

        let error =
            check_project(declaration, "local count: number = identity('hello')").unwrap_err();
        assert!(diagnostic_codes(&error).contains(&"LT2001"), "{error:?}");
    }

    #[test]
    fn checks_ambient_vararg_parameters() {
        let declaration = "function sum(...: number): number end";
        assert!(check_project(declaration, "local total: number = sum(1, 2, 3)").is_ok());

        let error = check_project(declaration, "sum(1, 'wrong')").unwrap_err();
        assert!(diagnostic_codes(&error).contains(&"LT2002"), "{error:?}");
    }

    #[test]
    fn validates_generated_fivem_declarations() {
        let root = std::env::current_dir().unwrap().join("src/definitions");
        let sources = [
            ("fivem-client.d.luat", Some(crate::ExecutionContext::Client)),
            ("fivem-server.d.luat", Some(crate::ExecutionContext::Server)),
            ("fivem-shared.d.luat", None),
            (
                "runtime-client.d.luat",
                Some(crate::ExecutionContext::Client),
            ),
            (
                "runtime-server.d.luat",
                Some(crate::ExecutionContext::Server),
            ),
            ("runtime-shared.d.luat", None),
        ]
        .into_iter()
        .map(|(name, context)| DeclarationSource {
            path: Some(root.join(name)),
            context,
            text: std::fs::read_to_string(root.join(name)).unwrap(),
        })
        .collect::<Vec<_>>();
        let declarations = DeclarationSet::from_sources(sources).unwrap();
        assert!(declarations.functions.contains_key("AddAudioSubmixOutput"));
        assert!(
            declarations
                .functions
                .contains_key("GetCurrentResourceName")
        );
    }

    #[test]
    fn rejects_executable_declarations() {
        for declaration in [
            "typedef Count = number\nlocal initialized: Count = 1",
            "function run(): () print('runtime') end",
            "declare Count: number = 1",
        ] {
            let error = check_project(declaration, "local value = 1").unwrap_err();
            assert!(diagnostic_codes(&error).contains(&"LT1002"), "{error:?}");
        }
    }

    #[test]
    fn reports_conflicting_global_declarations() {
        let error = check_project(
            "declare Shared: number\ndeclare Shared: string",
            "local value = 1",
        )
        .unwrap_err();
        assert!(diagnostic_codes(&error).contains(&"LT2009"), "{error:?}");
    }
}
