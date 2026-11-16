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
    pub(crate) context: Option<ExecutionContext>,
    #[allow(dead_code)]
    pub(crate) documentation: String,
    #[allow(dead_code)]
    pub(crate) location: DeclarationLocation,
}

pub(crate) struct DeclaredGlobal {
    pub(crate) name: String,
    pub(crate) type_name: String,
    #[allow(dead_code)]
    pub(crate) documentation: String,
    pub(crate) location: DeclarationLocation,
    pub(crate) context: Option<ExecutionContext>,
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
}

impl DeclarationSet {
    pub(crate) fn extend_ambient(&mut self, mut package: Self) -> Result<(), String> {
        super::typecheck::expand_ambient_exports(&mut package);
        for (name, global) in &package.globals {
            if self
                .globals
                .get(name)
                .is_some_and(|old| old.type_name != global.type_name)
            {
                return Err(format!("conflicting package global '{name}'"));
            }
        }
        self.globals.extend(package.globals);
        for (side, globals) in package.scoped_globals {
            let current = self.scoped_globals.entry(side).or_default();
            for (name, global) in globals {
                if current
                    .get(&name)
                    .is_some_and(|old| old.type_name != global.type_name)
                {
                    return Err(format!("conflicting package global '{name}'"));
                }
                current.insert(name, global);
            }
        }
        for (name, functions) in package.functions {
            self.functions.entry(name).or_default().extend(functions);
        }
        self.files.extend(package.files);
        Ok(())
    }

    pub(crate) fn from_texts(sources: &[String]) -> Result<Self, CompileError> {
        Self::from_sources_inner(
            sources.iter().map(|text| DeclarationSource {
                path: None,
                context: None,
                text: text.clone(),
            }),
            false,
        )
        .map_err(|(_, error)| error)
    }

    pub(crate) fn from_sources(
        sources: impl IntoIterator<Item = DeclarationSource>,
    ) -> Result<Self, (Option<PathBuf>, CompileError)> {
        Self::from_sources_inner(sources, true)
    }

    fn from_sources_inner(
        sources: impl IntoIterator<Item = DeclarationSource>,
        validate_declarations: bool,
    ) -> Result<Self, (Option<PathBuf>, CompileError)> {
        let mut set = Self::default();
        for source in sources {
            let annotation =
                declaration_context(&source.text).map_err(|error| (source.path.clone(), error))?;
            if annotation.is_some() && source.context.is_some() && annotation != source.context {
                return Err((
                    source.path,
                    CompileError {
                        diagnostics: vec![Diagnostic::at(
                            &source.text,
                            0,
                            1,
                            "LT2011",
                            "Declaration context conflicts with its built-in execution context."
                                .to_owned(),
                        )],
                    },
                ));
            }
            let context = annotation.or(source.context);
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

            for (name, alias) in aliases.aliases {
                if let Some(context) = context {
                    let shared = set.aliases.get(&name);
                    let scoped = set
                        .scoped_aliases
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
                    set.scoped_aliases
                        .entry(context)
                        .or_default()
                        .insert(name, alias);
                } else {
                    if set
                        .scoped_aliases
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
                    set.aliases.insert(name, alias);
                }
            }
            for mut global in globals {
                global.location.path = source.path.clone();
                global.context = context;
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
            });
        }
        if validate_declarations {
            super::typecheck::validate_declaration_types(&set)?;
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

fn declaration_context(source: &str) -> Result<Option<ExecutionContext>, CompileError> {
    let version = LuaVersion::cfxlua().with_luau();
    let tokens = match Lexer::new(source, version).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => Vec::new(),
    };
    let mut first_code = None;
    let mut directives = Vec::new();
    for token in tokens {
        let start = token.start_position().bytes();
        let end = token.end_position().bytes();
        let line = source.get(start..end).unwrap_or_default();
        let comment = matches!(
            token.token_type(),
            TokenType::SingleLineComment { .. }
                | TokenType::MultiLineComment { .. }
                | TokenType::CStyleComment { .. }
        );
        if matches!(token.token_type(), TokenType::SingleLineComment { .. })
            && ["---@client", "---@server", "---@shared"]
                .iter()
                .any(|tag| line.trim() == *tag)
        {
            let context = match line.trim() {
                "---@client" => Some(ExecutionContext::Client),
                "---@server" => Some(ExecutionContext::Server),
                _ => None,
            };
            directives.push((start, line.trim().to_owned(), context));
        } else if !comment && !line.trim().is_empty() && first_code.is_none() {
            first_code = Some(start);
        }
    }
    if directives.len() > 1 {
        let (offset, tag, _) = &directives[1];
        return Err(CompileError {
            diagnostics: vec![Diagnostic::at(
                source,
                *offset,
                tag.len(),
                "LT2011",
                "A declaration file may have only one execution context tag.".to_owned(),
            )],
        });
    }
    let Some((offset, tag, context)) = directives.into_iter().next() else {
        return Ok(None);
    };
    if first_code.is_some_and(|first| offset > first) {
        return Err(CompileError {
            diagnostics: vec![Diagnostic::at(
                source,
                offset,
                tag.len(),
                "LT2011",
                "Execution context tags must appear before declarations.".to_owned(),
            )],
        });
    }
    Ok(context)
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
    fn ambient_packages_expand_private_aliases_before_merging() {
        let mut declarations = DeclarationSet::default();
        for text in [
            "typedef Private = number\ndeclare First: { value: Private }",
            "typedef Private = string\ndeclare Second: { value: Private }",
        ] {
            let package = DeclarationSet::from_texts(&[text.into()]).unwrap();
            declarations.extend_ambient(package).unwrap();
        }
        assert!(!declarations.aliases.contains_key("Private"));
        assert!(declarations.globals["First"].type_name.contains("number"));
        assert!(declarations.globals["Second"].type_name.contains("string"));
        assert!(!declarations.globals["First"].type_name.contains("Private"));
    }

    #[test]
    fn reads_file_level_context_tags_only_from_comments() {
        let declarations = DeclarationSet::from_sources([DeclarationSource {
            path: None,
            context: None,
            text: "---@server\ntypedef Api = number\nfunction query(): Api end".to_owned(),
        }])
        .unwrap();
        assert_eq!(
            declarations.files[0].context,
            Some(crate::ExecutionContext::Server)
        );
        assert!(
            declarations
                .scoped_aliases
                .contains_key(&crate::ExecutionContext::Server)
        );

        let error = DeclarationSet::from_sources([DeclarationSource {
            path: None,
            context: None,
            text: "typedef Api = number\n---@client\nfunction query(): Api end".to_owned(),
        }])
        .err()
        .expect("tag after declarations should fail");
        assert_eq!(error.1.diagnostics[0].code, "LT2011");
    }

    #[test]
    fn conflicting_context_tags_are_rejected() {
        let error = DeclarationSet::from_sources([DeclarationSource {
            path: None,
            context: None,
            text: "---@client\n---@server\nfunction query(): number end".to_owned(),
        }])
        .err()
        .expect("duplicate context tags should fail");
        assert_eq!(error.1.diagnostics[0].code, "LT2011");
    }

    #[test]
    fn context_tags_allow_leading_whitespace_and_conflicts_ignore_file_order() {
        assert!(
            DeclarationSet::from_sources([DeclarationSource {
                path: None,
                context: None,
                text: "\n--- Description\n\n---@client\ndeclare Value: number".into()
            }])
            .is_ok()
        );
        for texts in [
            ["---@client\ndeclare Value: string", "declare Value: number"],
            [
                "---@client\ndeclare Value: string",
                "---@client\nfunction Value() end",
            ],
            ["declare Value: number", "---@client\ndeclare Value: string"],
        ] {
            assert!(DeclarationSet::from_texts(&texts.map(str::to_owned)).is_err());
        }
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
            "[project]\nname='test'\n[compiler]\ndeclarations=['types/api.d.luat']\n[fivem]\nshared=['src/main.luat']\n",
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
            "typedef StateBag = string\ndeclare GlobalState: StateBag",
            "local state: StateBag = GlobalState",
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
        let declaration = "typedef StateBag = string\ndeclare GlobalState: StateBag";
        let result = check_project(
            declaration,
            "local GlobalState: number = 1\nlocal value: number = GlobalState",
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
        let root = std::env::current_dir().unwrap().join("lsp/definitions");
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
