use super::{TypeAlias, check, string_key_options};
use crate::compiler::syntax::prepare;
use std::collections::HashMap;

#[test]
fn rejects_undeclared_type_names() {
    let source = "local marker: Marker = {}";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(error.diagnostics.len(), 1, "{error:?}");
    let diagnostic = &error.diagnostics[0];
    assert_eq!(diagnostic.code, "LT2006");
    assert!(diagnostic.message.contains("Marker"), "{diagnostic:?}");
    assert_eq!(&source[diagnostic.source_range.clone()], "Marker");
}

#[test]
fn rejects_undeclared_types_in_nested_annotations() {
    for source in [
        "typedef Box = { value: Missing? }",
        "typedef List<T> = { T }\nlocal values: List<Missing> = {}",
        "local function take(value: Missing) end",
        "local function get(): Missing return nil end",
        "local value = {} :: Missing",
        "local value: Missing<number> = {}",
        "typedef List<T> = { T }\nlocal value: T = 1",
        "local function identity<T>(value: T): T return value end\nlocal value: T = 1",
        "typedef Identity = <T>(T) -> T\nlocal value: T = 1",
        "do typedef Hidden = number end\nlocal value: Hidden = 1",
    ] {
        let error = crate::compile_source(source).unwrap_err();
        assert!(
            error.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "LT2006" && diagnostic.message.contains("Unknown type")
            }),
            "{source}: {error:?}"
        );
    }
}

#[test]
fn type_alias_names_are_not_taken_from_comments() {
    let source = "-- optional types = supported\ntypedef PlayerId = number\nlocal id: PlayerId = 1";
    assert!(crate::compile_source(source).is_ok());
    let invalid = source.replace("= 1", "= 'wrong'");
    let error = crate::compile_source(&invalid).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "LT2001");
}

#[test]
fn accepts_declared_types_and_scoped_generic_parameters() {
    for source in [
        "typedef Marker = { position: vector3, color: vector4? }\nlocal marker: Marker = { position = vector3(1, 2, 3) }",
        "typedef List<T> = { T }\nlocal values: List<number> = { 1 }",
        "local function identity<T>(value: T): T return value end",
        "typedef Identity = <T>(T) -> T",
        "typedef Node = { next: Node? }",
        "typedef First = Second\ntypedef Second = number",
        "do typedef Hidden = number\nlocal value: Hidden = 1 end",
        "typedef Builtins = any | unknown | never | nil | boolean | number | integer | string | table | thread | userdata | buffer | vector2 | vector3 | vector4 | quaternion",
    ] {
        assert!(crate::compile_source(source).is_ok(), "{source}");
    }
    let source = "local marker: Marker = {}";
    let declarations = ["typedef Marker = { color: vector4? }".to_owned()];
    assert!(crate::compiler::compile_source_with_declarations(source, &declarations).is_ok());
}

#[test]
fn record_error_highlights_invalid_job_value() {
    let source = "typedef Job = 'police' | 'medic'\ntypedef Player = { id: number, name: string, job: Job? }\nlocal player: Player = { id = 42, name = 'Ada', job = 'sex' }";
    let error = crate::compile_source(source).unwrap_err();
    let diagnostic = &error.diagnostics[0];
    assert_eq!(&source[diagnostic.source_range.clone()], "'sex'");
    assert!(diagnostic.message.contains("job"), "{diagnostic:?}");
    assert!(diagnostic.message.contains("Job"), "{diagnostic:?}");
    assert!(crate::compile_source(&source.replace("'sex'", "'police'")).is_ok());
    assert!(crate::compile_source(&source.replace("'sex'", "nil")).is_ok());
}

#[test]
fn reports_missing_required_record_fields() {
    let source = "typedef Job = 'police' | 'medic'\ntypedef Player = { id: number, name: string, job: Job }\nlocal player: Player = { id = 42, name = 'Ada' }";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(error.diagnostics.len(), 1, "{error:?}");
    assert!(
        error.diagnostics[0]
            .message
            .contains("required field 'job'")
    );
    assert!(error.diagnostics[0].message.contains("type 'Job'"));
    assert_eq!(&source[error.diagnostics[0].source_range.clone()], "{");
}

#[test]
fn checks_every_record_inside_a_generic_list() {
    let source = "typedef Player = { id: number, callsign: string }\ntypedef List<T> = { T }\nlocal players: List<Player> = {{ id = 1, callsign = 'A-01' }, { id = 2 }}";
    let error = crate::compile_source(source).unwrap_err();

    assert_eq!(error.diagnostics.len(), 1, "{error:?}");
    assert!(error.diagnostics[0].message.contains("callsign"));
    assert!(error.diagnostics[0].message.contains("[2].callsign"));
    assert_eq!(&source[error.diagnostics[0].source_range.clone()], "{");
    assert!(
        crate::compile_source(&source.replace("{ id = 2 }", "{ id = 2, callsign = 'L-02' }"))
            .is_ok()
    );

    let nested = "typedef Player = { callsign: string }\ntypedef List<T> = { T }\ntypedef Box<T> = { value: T }\nlocal box: Box<List<Player> > = { value = { { callsign = 'A' }, { } } }";
    let error = crate::compile_source(nested).unwrap_err();
    assert!(error.diagnostics[0].message.contains("value[2].callsign"));
}

#[test]
fn checks_nested_recursive_records_inside_lists() {
    let valid = "typedef Node = { value: string, next: Node? }\ntypedef List<T> = { T }\nlocal nodes: List<Node> = { { value = 'root', next = { value = 'leaf' } } }";
    assert!(crate::compile_source(valid).is_ok());

    let invalid = valid.replace("value = 'leaf'", "value = 2");
    let error = crate::compile_source(&invalid).unwrap_err();
    assert!(error.diagnostics[0].message.contains("[1].next.value"));
}

#[test]
fn checks_generic_arrays_at_calls_returns_and_indexed_assignments() {
    let source = "typedef Player = { name: string, callsign: string }\ntypedef List<T> = { T }\nlocal function take(players: List<Player>) end\ntake({ { name = 'Ada', callsign = 'A-01' }, { name = 'Linus' } })\nlocal function getPlayers(): List<Player>\n    return { { name = 'Grace' } }\nend\nlocal players: List<Player> = { { name = 'Ada', callsign = 'A-01' } }\nplayers[2] = { name = 'Linus' }";
    let error = crate::compile_source(source).unwrap_err();

    assert_eq!(error.diagnostics.len(), 3, "{error:?}");
    assert!(error.diagnostics[0].message.contains("[2].callsign"));
    assert!(error.diagnostics[1].message.contains("[1].callsign"));
    assert!(
        error.diagnostics[2].message.contains("players[2]"),
        "{error:?}"
    );
    assert!(
        error.diagnostics[2].message.contains("callsign"),
        "{error:?}"
    );
}

#[test]
fn checks_every_primitive_inside_a_generic_list() {
    assert!(
        crate::compile_source(
            "typedef List<T> = { T }\nlocal values: List<number> = { 1, 'wrong' }"
        )
        .is_err()
    );
    let nested = "typedef List<T> = { T }\ntypedef Box<T> = { value: T }\nlocal value: Box<List<number> > = { value = { 1, 'wrong' } }";
    let error = crate::compile_source(nested).unwrap_err();
    assert!(error.diagnostics[0].message.contains("value[2]"));
}

#[test]
fn rejects_generic_aliases_with_the_wrong_number_of_arguments() {
    for (source, expected_range) in [
        (
            "typedef List<T> = { T }\nlocal values: List<number, string> = {}",
            "List<number, string>",
        ),
        ("typedef List<T> = { T }\nlocal values: List = {}", "List"),
    ] {
        let error = crate::compile_source(source).unwrap_err();
        assert_eq!(error.diagnostics[0].code, "LT2006");
        assert_eq!(
            &source[error.diagnostics[0].source_range.clone()],
            expected_range
        );
    }
}

#[test]
fn rejects_unguarded_alias_cycles_but_accepts_recursive_records() {
    let source = "typedef First = Second\ntypedef Second = First";
    let error = crate::compile_source(source).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2006")
    );

    let recursive =
        "typedef Node = { value: string, next: Node? }\nlocal root: Node = { value = 'root' }";
    assert!(crate::compile_source(recursive).is_ok());
}

#[test]
fn rejects_alias_expansion_chains_over_128_levels() {
    let mut source = "typedef A0 = number\n".to_owned();
    for index in 1..=129 {
        source.push_str(&format!("typedef A{index} = A{}\n", index - 1));
    }

    let error = crate::compile_source(&source).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2006")
    );
}

#[test]
fn rejects_conflicting_generic_function_bindings() {
    let compatible = "local function same<T>(first: T, second: T): T\n    return first\nend\nlocal result: number = same(1, 1.5)";
    assert!(crate::compile_source(compatible).is_ok());

    let source = "local function same<T>(first: T, second: T): T\n    return first\nend\nlocal result: number = same(1, 'wrong')";
    let error = crate::compile_source(source).unwrap_err();

    assert_eq!(error.diagnostics[0].code, "LT2002");
    assert_eq!(
        &source[error.diagnostics[0].source_range.clone()],
        "'wrong'"
    );
}

#[test]
fn binds_generic_list_and_box_parameters_from_known_values() {
    let valid = "typedef Player = { name: string }\ntypedef List<T> = { T }\ntypedef Box<T> = { value: T }\nlocal function first<T>(items: List<T>): T\n    return items[1]\nend\nlocal players: List<Player> = { { name = 'Ada' } }\nlocal player: Player = first(players)\nlocal box: Box<Player> = { value = player }";
    assert!(crate::compile_source(valid).is_ok());

    let invalid = valid.replace(
        "local player: Player = first(players)",
        "local player: string = first(players)",
    );
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn reports_errors_from_each_if_expression_branch() {
    let source = "typedef List<T> = { T }\nlocal values: List<number> = if true then { 'bad' } else { false }";
    let error = crate::compile_source(source).unwrap_err();

    assert_eq!(error.diagnostics.len(), 2, "{error:?}");
}

#[test]
fn checks_table_values_in_parenthesized_and_nested_if_expressions() {
    for source in [
        "typedef Player = { callsign: string }\nlocal player: Player = (if true then {} else { callsign = 'ok' })",
        "typedef Player = { callsign: string }\ntypedef List<T> = { T }\nlocal players: List<Player> = { if true then {} else { callsign = 'ok' } }",
        "typedef Player = { callsign: string }\ntypedef Box<T> = { value: T }\nlocal box: Box<Player> = { value = if true then {} else { callsign = 'ok' } }",
    ] {
        let error = crate::compile_source(source).unwrap_err();
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("callsign")),
            "{source}: {error:?}"
        );
    }
}

#[test]
fn maps_if_branch_mismatches_after_templates_to_the_original_source() {
    let source = "local text = `hello {name}`\nlocal value: number = if true then 1 else 'wrong'\n-- trailing comment";
    let error = crate::compile_source(source).unwrap_err();
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.message.contains("'wrong'"))
        .expect("expected the invalid if branch diagnostic");

    assert_eq!(diagnostic.location.line, 2);
    assert_eq!(&source[diagnostic.source_range.clone()], "'wrong'");
}

#[test]
fn validates_dictionary_values_without_treating_keys_as_record_fields() {
    let valid = "typedef Dictionary<V> = { [string]: V }\nlocal values: Dictionary<number> = { good = 1, alsoGood = 2 }";
    assert!(crate::compile_source(valid).is_ok());

    let invalid = valid.replace("alsoGood = 2", "alsoGood = 'wrong'");
    let error = crate::compile_source(&invalid).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("alsoGood"))
    );
}

#[test]
fn validates_dictionary_keys_and_mixed_record_indexers() {
    let valid = "typedef Dictionary<K, V> = { [K]: V }\ntypedef PlayerMap = { [string]: number, count: number }\nlocal key: string = 'score'\nlocal values: Dictionary<string, number> = { score = 1, ['level'] = 2, [key] = 3 }\nlocal players: PlayerMap = { count = 2, extra = 3 }";
    assert!(crate::compile_source(valid).is_ok());

    let invalid_key = valid.replace("score = 1", "[1] = 1");
    assert!(crate::compile_source(&invalid_key).is_err());

    let invalid_indexer_value = valid.replace("extra = 3", "extra = 'bad'");
    assert!(crate::compile_source(&invalid_indexer_value).is_err());

    let mixed = "typedef Mixed = { name: string, [string]: number }\nlocal value: Mixed = { name = 'Ada', score = 2 }\nlocal name: string = value['name']\nlocal score: number = value['score']";
    assert!(crate::compile_source(mixed).is_ok());
}

#[test]
fn resolves_union_typed_keys_for_record_reads_and_writes() {
    let valid = "typedef Key = 'a' | 'b'\ntypedef Record = { a: string, b: string }\nlocal key: Key = 'a'\nlocal record: Record = { a = 'first', b = 'second' }\nlocal value: string = record[key]\nrecord[key] = 'updated'";
    let result = crate::compile_source(valid);
    assert!(result.is_ok(), "{result:?}");

    let mixed = "typedef Key = 'a' | 'b'\ntypedef Record = { a: string, b: number }\nlocal key: Key = 'a'\nlocal record: Record = { a = 'first', b = 2 }\nlocal value: string = record[key]";
    assert!(crate::compile_source(mixed).is_err());

    let invalid_write = mixed.replace(
        "local value: string = record[key]",
        "record[key] = 'updated'",
    );
    assert!(crate::compile_source(&invalid_write).is_err());

    let unknown = valid.replace("'a' | 'b'", "'a' | 'missing'");
    let error = crate::compile_source(&unknown).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2005")
    );
}

#[test]
fn resolves_optional_aliases_around_records() {
    let valid = "typedef Player = { name: string }\ntypedef MaybePlayer = Player?\ntypedef MaybeUnionPlayer = Player | nil\ntypedef List<T> = { T }\ntypedef MaybeList = List<string>?\ntypedef MaybeUnionList = List<string> | nil\ntypedef Box = { item: MaybePlayer }\nlocal direct: MaybePlayer = { name = 'Ada' }\nlocal directName: string? = direct.name\nlocal unionPlayer: MaybeUnionPlayer = { name = 'Linus' }\nlocal unionName: string? = unionPlayer.name\nlocal list: MaybeList = { 'Ada' }\nlocal first: string? = list[1]\nlocal unionList: MaybeUnionList = { 'Linus' }\nlocal unionFirst: string? = unionList[1]\nlocal empty: Box = {}\nlocal filled: Box = { item = { name = 'Linus' } }";
    let result = crate::compile_source(valid);
    assert!(result.is_ok(), "{result:?}");

    let invalid = valid.replace("name = 'Linus'", "name = 42");
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn allows_dynamic_record_keys_only_when_all_literal_keys_are_valid() {
    let aliases = HashMap::from([(
        "Key".to_owned(),
        TypeAlias {
            parameters: Vec::new(),
            definition: "'a' | 'b'".to_owned(),
        },
    )]);
    assert_eq!(
        string_key_options("Key", &aliases),
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    let valid = "typedef Key = 'a' | 'b'\ntypedef Record = { a: string, b: string }\nlocal key: Key = 'a'\nlocal value: Record = { a = 'first', b = 'second', [key] = 'third' }";
    let result = crate::compile_source(valid);
    assert!(result.is_ok(), "{result:?}");

    let unknown = valid.replace("local key: Key = 'a'", "local key: string = 'a'");
    assert!(crate::compile_source(&unknown).is_err());

    let partial = valid.replace("'a' | 'b'", "'a' | 'missing'");
    assert!(crate::compile_source(&partial).is_err());

    let wrong_value = valid
        .replace("b: string", "b: number")
        .replace("b = 'second'", "b = 2");
    assert!(crate::compile_source(&wrong_value).is_err());
}

#[test]
fn checks_every_array_entry_for_positional_and_explicit_indices() {
    let source =
        "typedef List<T> = { T }\nlocal values: List<number> = { 'first', 2, [5] = 'last' }";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(error.diagnostics.len(), 2, "{error:?}");
    assert!(error.diagnostics[0].message.contains("[1]"));
    assert!(error.diagnostics[1].message.contains("[5]"));

    let dictionary = "typedef Dictionary = { [number]: string }\nlocal values: Dictionary = { 'first', [4] = 'fourth' }";
    assert!(crate::compile_source(dictionary).is_ok());
}

#[test]
fn optional_field_forms_accept_absence_and_check_present_values() {
    for source in [
        "typedef Value = string?\ntypedef Record = { value: Value }\nlocal item: Record = {}",
        "typedef Record = { value: string? }\nlocal item: Record = {}",
        "typedef Record = { value: string | nil }\nlocal item: Record = {}",
        "typedef Record = { value?: string }\nlocal item: Record = {}",
    ] {
        assert!(crate::compile_source(source).is_ok(), "{source}");
    }
    for source in [
        "typedef Record = { value?: string }\nlocal item: Record = { value = 1 }",
        "typedef Record = { value: string? }\nlocal item: Record = { value = 1 }",
    ] {
        assert!(crate::compile_source(source).is_err(), "{source}");
    }
}

#[test]
fn checks_semicolon_separated_records_with_comments_and_trailing_separators() {
    let valid = "typedef Player = { name: string, -- field\n level: number; }\nlocal player: Player = { name = 'Ada'; -- name\n level = 2; }";
    let result = crate::compile_source(valid);
    assert!(result.is_ok(), "{result:?}");

    let invalid = valid.replace("level = 2", "level = 'bad'");
    let error = crate::compile_source(&invalid).unwrap_err();
    assert_eq!(&invalid[error.diagnostics[0].source_range.clone()], "'bad'");
}

#[test]
fn keeps_escaped_quotes_and_commas_inside_table_string_values() {
    let source = r#"typedef Message = { value: string, count: number }
local message: Message = { value = "comma, escaped \"quote\"", count = 1, }"#;
    let result = crate::compile_source(source);
    assert!(result.is_ok(), "{result:?}");
    let invalid = source.replace("count = 1", "count = 'bad'");
    let error = crate::compile_source(&invalid).unwrap_err();
    assert_eq!(&invalid[error.diagnostics[0].source_range.clone()], "'bad'");
}

#[test]
fn uses_generic_array_element_types_for_index_reads_and_writes() {
    let read = "typedef List<T> = { T }\nlocal values: List<number> = { 1 }\nlocal label: string = values[1]";
    assert!(crate::compile_source(read).is_err());

    let write = "typedef List<T> = { T }\nlocal values: List<number> = { 1 }\nvalues[1] = 'wrong'";
    assert!(crate::compile_source(write).is_err());
}

#[test]
fn checks_table_values_against_a_complete_union_member() {
    let invalid = "typedef A = { a: string, c: number }\ntypedef B = { b: number, d: string }\ntypedef Either = A | B | nil\nlocal value: Either = { a = 'ok', d = 'wrong' }";
    assert!(crate::compile_source(invalid).is_err());

    let valid = invalid.replace("{ a = 'ok', d = 'wrong' }", "{ a = 'ok', c = 1 }");
    assert!(crate::compile_source(&valid).is_ok());
}

#[test]
fn accepts_quoted_bracket_keys_for_record_fields() {
    for (key, access) in [
        ("name", ".name"),
        ("['name']", "['name']"),
        ("[\"name\"]", "[\"name\"]"),
    ] {
        let source = format!(
            "typedef Player = {{ name: string }}\nlocal player: Player = {{ {key} = 'Ada' }}\nlocal name: string = player{access}"
        );
        assert!(crate::compile_source(&source).is_ok(), "{source}");
    }
}

#[test]
fn highlights_multibyte_values_with_byte_safe_source_ranges() {
    let source =
        "typedef Player = { callsign: number }\nlocal player: Player = { callsign = 'Çağrı' }";
    let error = crate::compile_source(source).unwrap_err();
    let diagnostic = &error.diagnostics[0];

    assert_eq!(&source[diagnostic.source_range.clone()], "'Çağrı'");
    assert_eq!(diagnostic.location.line, 2);
}

#[test]
fn merges_intersection_fields_before_checking_extra_fields() {
    let source = "typedef A = { a: string }\ntypedef B = { b: number }\ntypedef Both = A & B\nlocal value: Both = { a = 'ok' }";
    let error = crate::compile_source(source).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("required field 'b'"))
    );

    let valid = source.replace("{ a = 'ok' }", "{ a = 'ok', b = 1 }");
    assert!(crate::compile_source(&valid).is_ok());
}

#[test]
fn resolves_optional_aliases_when_checking_missing_fields() {
    let valid = "typedef MaybeName = string?\ntypedef Player = { name: MaybeName }\nlocal player: Player = {}";
    assert!(crate::compile_source(valid).is_ok());

    let invalid = valid.replace(
        "local player: Player = {}",
        "local player: Player = { name = 42 }",
    );
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn checks_structural_compatibility_between_typed_records() {
    let valid = "typedef First = { name: string }\ntypedef Second = { name: string }\nlocal first: First = { name = 'Ada' }\nlocal second: Second = first";
    assert!(crate::compile_source(valid).is_ok());

    let invalid = valid.replace(
        "typedef Second = { name: string }",
        "typedef Second = { name: number }",
    );
    assert!(crate::compile_source(&invalid).is_err());

    let parenthesized = "typedef First = { name: string }\nlocal first: First = ({ name = 'Ada' })";
    assert!(crate::compile_source(parenthesized).is_ok());
}

#[test]
fn resolves_alias_chains_for_record_access_and_intersections() {
    let source = "typedef Player = { name: string }\ntypedef PlayerAlias = Player\ntypedef PlayerAlias2 = PlayerAlias\nlocal player: PlayerAlias2 = { name = 'Ada' }\nlocal name: string = player.name";
    let result = crate::compile_source(source);
    assert!(result.is_ok(), "{result:?}");

    let invalid_access = source.replace(
        "local name: string = player.name",
        "local missing = player.missing",
    );
    let error = crate::compile_source(&invalid_access).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2005")
    );

    let intersection = "typedef A = { a: string }\ntypedef AAlias = A\ntypedef B = { b: number }\ntypedef Both = AAlias & B\nlocal both: Both = { a = 'ok', b = 2 }";
    let result = crate::compile_source(intersection);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn preserves_record_shapes_through_unannotated_locals() {
    let invalid = "local source = { name = 1 }\nlocal target: { name: string } = source";
    assert!(crate::compile_source(invalid).is_err());

    let valid = invalid.replace("name = 1", "name = 'Ada'");
    assert!(crate::compile_source(&valid).is_ok());
}

#[test]
fn reports_all_record_problems_in_type_declaration_order() {
    let source = "typedef Player = { id: number, name: string, job: string }\nlocal player: Player = { id = 'bad', name = 3, foo = true }";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(error.diagnostics.len(), 4, "{error:?}");
    assert!(error.diagnostics[0].message.contains("Field 'id'"));
    assert!(error.diagnostics[1].message.contains("Field 'name'"));
    assert!(
        error.diagnostics[2]
            .message
            .contains("required field 'job'")
    );
    assert!(error.diagnostics[3].message.contains("field 'foo'"));
    assert_eq!(&source[error.diagnostics[0].source_range.clone()], "'bad'");
    assert_eq!(&source[error.diagnostics[3].source_range.clone()], "foo");
}

#[test]
fn reports_nested_missing_record_paths() {
    let source = "typedef Job = 'police' | 'medic'\ntypedef Player = { profile: { job: Job } }\nlocal player: Player = { profile = {} }";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(error.diagnostics.len(), 1, "{error:?}");
    assert!(error.diagnostics[0].message.contains("profile.job"));
    assert_eq!(&source[error.diagnostics[0].source_range.clone()], "{");
}

#[test]
fn reports_function_contexts_and_repeated_argument_locations() {
    let source = "local function take(first: number, second: number) end\ntake('bad', 'bad')";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(error.diagnostics.len(), 2, "{error:?}");
    assert!(
        error.diagnostics[0]
            .message
            .contains("Argument 1 ('first') of function 'take'")
    );
    assert!(
        error.diagnostics[1]
            .message
            .contains("Argument 2 ('second') of function 'take'")
    );
    assert_eq!(&source[error.diagnostics[0].source_range.clone()], "'bad'");
    assert_eq!(&source[error.diagnostics[1].source_range.clone()], "'bad'");
    assert_ne!(
        error.diagnostics[0].source_range,
        error.diagnostics[1].source_range
    );
}

#[test]
fn accepts_fivem_vector2_and_vector4_types() {
    let source = "local point: vector2 = { x = 1, y = 2 }\nlocal color: vector4 = { x = 1, y = 2, z = 3, w = 4 }";
    assert!(crate::compile_source(source).is_ok());
    assert!(crate::compile_source("local point: vector2 = { x = 1 }").is_err());
    assert!(crate::compile_source("local color: vector4 = { x = 1, y = 2, z = 3 }").is_err());
}

#[test]
fn checks_vector_constructor_dimensions_arguments_and_record_shapes() {
    for (good, bad) in [
        (
            "local p: vector2 = vector2(1, 2)",
            "local p: vector2 = vector2(1, 2, 3)",
        ),
        (
            "local p: vector3 = vector3(1, 2, 3)",
            "local p: vector3 = vector3(1, 2)",
        ),
        (
            "local p: vector4 = vector4(1, 2, 3, 4)",
            "local p: vector4 = vector4(1, 2, 3)",
        ),
    ] {
        assert!(crate::compile_source(good).is_ok(), "{good}");
        assert!(crate::compile_source(bad).is_err(), "{bad}");
    }
    assert!(crate::compile_source("local p: vector3 = vector4(1, 2, 3, 4)").is_err());
    assert!(
        crate::compile_source("typedef Point = vector3\nlocal p: Point = vector4(1, 2, 3, 4)")
            .is_err()
    );
    assert!(crate::compile_source("local p: vector3 = vector3(1, 'x', 3)").is_err());
    assert!(crate::compile_source("local p: vector3 = {x=1, y='x', z=3}").is_err());
    assert!(crate::compile_source("local p: vector4 = {x=1, y=2, z=3, w=4}").is_ok());
    assert!(
        crate::compile_source("local n: number = 2\nlocal p: vector3 = {x=n, y=n, z=n}").is_ok()
    );
    assert!(
        crate::compile_source("local s: string = 'bad'\nlocal p: vector3 = {x=s, y=2, z=3}")
            .is_err()
    );
    assert!(
        crate::compile_source(
            "typedef P = { coords?: vector3 }\nlocal p: P = {}\np = {coords = nil}"
        )
        .is_ok()
    );
    assert!(
        crate::compile_source(
            "typedef P = { coords?: vector3 }\nlocal p: P = {coords = {x=1, y=2, z=3}}"
        )
        .is_ok()
    );
    assert!(
        crate::compile_source(
            "typedef P = { coords?: vector3 }\nlocal p: P = {coords = {x=1, y=2}}"
        )
        .is_err()
    );
}

#[test]
fn rejects_decimal_vector_dimension_mismatches() {
    for source in [
        "local spawnOffset: vector3 = vector4(0.0, 0.0, 1.0, 0.0)",
        "local spawnOffset: vector3 = vector3(0.0, 0.0, 1.0)\nspawnOffset = vector4(0.0, 0.0, 1.0, 0.0)",
        "local function take(value: vector3) end\ntake(vector4(0.0, 0.0, 1.0, 0.0))",
        "local function make(): vector3 return vector4(0.0, 0.0, 1.0, 0.0) end",
    ] {
        let error = crate::compile_source(source).unwrap_err();
        assert!(error.to_string().contains("vector4"), "{error}");
        assert!(error.to_string().contains("vector3"), "{error}");
    }
    assert!(crate::compile_source("local p: vector4 = vector4(0.0, 0.0, 1.0, 0.0)").is_ok());
    assert!(crate::compile_source("local x = vector4('0.0', 0.0, 1.0, 0.0)").is_err());
}

#[test]
fn reports_missing_record_fields_in_expression_contexts() {
    let errors = crate::compile_source(
            "typedef Player = { name: string }\ntypedef List<T> = { T }\nlocal player: Player = { name = 'Ada' }\nlocal players: List<Player> = { player }\nlocal first: Player = players[1]\nlocal firstName: string = first.ax ?? 'Unknown'",
        )
        .unwrap_err();
    assert_eq!(errors.diagnostics.len(), 1, "{errors}");
    assert_eq!(errors.diagnostics[0].code, "LT2005");
    assert_eq!(
        &"typedef Player = { name: string }\ntypedef List<T> = { T }\nlocal player: Player = { name = 'Ada' }\nlocal players: List<Player> = { player }\nlocal first: Player = players[1]\nlocal firstName: string = first.ax ?? 'Unknown'"
            [errors.diagnostics[0].source_range.clone()],
        "ax"
    );
    for source in [
        "typedef P = { name: string }\nlocal p: P = { name = 'Ada' }\nlocal x = p.ax",
        "typedef P = { name: string }\nlocal p: P = { name = 'Ada' }\np.ax = 'x'",
        "typedef P = { name: string }\nlocal function f(p: P): string return p.ax end",
        "typedef P = { name: string }\nlocal function f(value: string) end\nlocal p: P = { name = 'Ada' }\nf(p.ax)",
        "typedef P = { name: string }\nlocal p: P = { name = 'Ada' }\nlocal x = { value = p.ax }",
        "typedef P = { nested: P }\nlocal p: P = { nested = { nested = { nested = nil } } }\nlocal x = p.nested.ax",
    ] {
        assert!(crate::compile_source(source).is_err(), "{source}");
    }
    for source in [
        "typedef P = { name: string }\nlocal p: P = { name = 'Ada' }\nlocal x = p.name",
        "typedef P = { name?: string }\nlocal p: P? = nil\nlocal x = p?.name",
        "typedef P = { name: string }\nlocal p: P? = nil\nlocal x = p?.name",
        "typedef Box<T> = { value: T }\nlocal box: Box<string> = { value = 'ok' }\nlocal x = box.value",
        "local x = external.value",
        "local x = false ?? true",
    ] {
        assert!(crate::compile_source(source).is_ok(), "{source}");
    }
    assert!(
        crate::compile_source(
            "typedef P = { name: string }\nlocal p: P? = nil\nlocal x = p?.missing"
        )
        .unwrap_err()
        .to_string()
        .contains("LT2005")
    );
}

#[test]
fn record_field_errors_keep_boundary_codes_and_value_ranges() {
    let declarations = "typedef Job = 'police' | 'medic'\ntypedef Player = { job: Job? }\n";
    for (body, code) in [
        ("local p: Player = { job = 'sex' }", "LT2001"),
        ("local p: Player = {}\np = { job = 'sex' }", "LT2001"),
        (
            "local function take(p: Player) end\ntake({ job = 'sex' })",
            "LT2002",
        ),
        (
            "local function make(): Player\nreturn { job = 'sex' }\nend",
            "LT2004",
        ),
    ] {
        let source = format!("{declarations}{body}");
        let error = crate::compile_source(&source).unwrap_err();
        assert_eq!(error.diagnostics.len(), 1, "{error:?}");
        let diagnostic = &error.diagnostics[0];
        assert_eq!(diagnostic.code, code);
        assert_eq!(&source[diagnostic.source_range.clone()], "'sex'");
        assert!(diagnostic.message.contains("Field 'job'"));
        assert!(crate::compile_source(&source.replace("'sex'", "\"police\"")).is_ok());
    }
}

#[test]
fn reports_only_provable_primitive_mismatches() {
    let source = "typedef Count = number\nlocal count: Count = 'many'";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    assert_eq!(check(source, &prepared, &ast, &[]).len(), 1);

    let source = "typedef Job = 'police' | 'medic'\nlocal job: Job = 'police'";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    assert!(check(source, &prepared, &ast, &[]).is_empty());
}

#[test]
fn rejects_invalid_record_literals_and_unknown_fields() {
    let source = "typedef Job = 'police' | 'medic'\ntypedef Player = { id: number, job: Job? }\nlocal player: Player = { id = 42, job = 'sex', foo = 1 }\nprint(player.foo)";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert!(diagnostics.len() >= 2, "{diagnostics:?}");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2001")
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2005")
    );
}

#[test]
fn checks_known_function_calls_and_ignores_unknown_external_calls() {
    let source = "local function take(value: number): number return value end\ntake('wrong')\nExternalFunction('anything')";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "LT2002");
}

#[test]
fn checks_anonymous_function_values_and_table_fields() {
    for (source, expected) in [
        (
            "local emit = function(status: 'info' | 'warning', message: string) end\nemit('random', 'message')",
            "LT2002",
        ),
        (
            "local Config = {}\nConfig.Print = function(status: 'info' | 'warning', message: string) end\nConfig.Print('random', 'message')",
            "LT2002",
        ),
        (
            "local Config = {}\nConfig.Print = function(status: 'info' | 'warning', message: string) end\nConfig.Print('info')",
            "LT2002",
        ),
        (
            "local Config = {}\nConfig.Print = function(status: 'info' | 'warning', message: string) end\nConfig.print('info', 'message')",
            "LT2005",
        ),
    ] {
        let prepared = prepare(source);
        let ast =
            full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
                .into_ast();
        let diagnostics = check(source, &prepared, &ast, &[]);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == expected),
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn preserves_anonymous_function_signatures_through_local_references() {
    for source in [
        "local emit = function(status: 'info' | 'warning', message: string) end\nlocal copy = emit\ncopy('random', 'message')",
        "local emit\n emit = function(status: 'info' | 'warning', message: string) end\nemit('random', 'message')",
        "local Config = { Print = function(status: 'info' | 'warning', message: string) end }\nConfig.Print('random', 'message')",
        "local Config = { nested = { Print = function(status: 'info' | 'warning', message: string) end } }\nConfig.nested.Print('random', 'message')",
        "local Config = { Print = function(status: 'info' | 'warning', message: string) end }\nConfig['Print']('random', 'message')",
        "local Config = {}\nfunction Config:Print(status: 'info' | 'warning', message: string) end\nConfig:Print('random', 'message')",
    ] {
        let error = crate::compile_source(source).expect_err(source);
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "LT2002"),
            "{source}: {error:?}"
        );
    }
}

#[test]
fn updates_reassigned_function_signatures_and_respects_local_shadowing() {
    let reassigned =
        "local emit = function(value: number) end\nemit = function(value: string) end\nemit(1)";
    assert!(crate::compile_source(reassigned).is_err());
    let shadowed =
        "local emit = function(value: number) end\ndo\n local emit = 1\n emit()\nend\nemit(1)";
    let error = crate::compile_source(shadowed).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2008"),
        "{error:?}"
    );
    assert!(
        error
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "LT2002")
    );
}

#[test]
fn resolves_missing_fields_in_calls_and_allows_external_dynamic_calls() {
    for source in [
        "local cfg = {}\nlocal value = cfg.print",
        "local cfg = {}\ncfg.print('x')",
        "local cfg = {}\ncfg['print']('x')",
    ] {
        let error = crate::compile_source(source).expect_err(source);
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "LT2005"),
            "{source}: {error:?}"
        );
        assert_eq!(
            error
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "LT2005")
                .count(),
            1,
            "{source}: {error:?}"
        );
    }
    assert!(crate::compile_source("ExternalFunction('anything')").is_ok());
    assert!(crate::compile_source("local value: any = {}\nvalue('anything')").is_ok());
}

#[test]
fn checks_lua_parenthesis_free_calls_and_highlights_the_argument() {
    let source = "local take = function(value: number) end\ntake 'wrong'";
    let error = crate::compile_source(source).unwrap_err();
    let diagnostic = error
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "LT2002")
        .unwrap();
    assert_eq!(&source[diagnostic.source_range.clone()], "'wrong'");
    let table_call =
        "local take = function(value: { count: number }) end\ntake { count = 'wrong' }";
    let error = crate::compile_source(table_call).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2002"),
        "{error:?}"
    );
}

#[test]
fn preserves_generic_vararg_and_tuple_signatures_on_function_values() {
    let source = "local unpack = function<T>(first: T, ...: number): (T, number) return first, ... end\nlocal copy = unpack\ncopy('ok', 'wrong')";
    let error = crate::compile_source(source).unwrap_err();
    assert!(
        error
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "LT2002"),
        "{error:?}"
    );
}

#[test]
fn module_exports_union_only_top_level_return_types() {
    let imports = HashMap::new();
    assert_eq!(
        super::infer_module_export_type("if true then return 1 else return 'text' end", &imports,),
        "'text' | number"
    );
    assert_eq!(
        super::infer_module_export_type(
            "local function hidden(): string return 'text' end",
            &imports,
        ),
        "nil"
    );
}

#[test]
fn rejects_calls_to_known_non_callable_values() {
    for source in [
        "local value = 1\nvalue()",
        "local value = 'text'\nvalue()",
        "local value = {}\nvalue()",
    ] {
        let error = crate::compile_source(source).expect_err(source);
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "LT2008"),
            "{source}: {error:?}"
        );
    }
}

#[test]
fn rejects_missing_function_fields_through_record_aliases() {
    let source = "typedef Config = { Print: (message: string) -> nil }\ntypedef Wrapper = { config: Config }\nlocal cfg: Config = { Print = function(message: string) end }\nlocal wrapper: Wrapper = { config = cfg }\ncfg.Missing('x')\ncfg['Missing']('x')\nwrapper.config.Missing('x')";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(
        error
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "LT2005")
            .count(),
        3,
        "{error:?}"
    );
}

#[test]
fn keeps_table_field_callback_signatures_after_block_scope_ends() {
    let source = "local config = {}\ndo config.Print = function(value: number) end end\nconfig.Print('wrong')";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(error.diagnostics[0].code, "LT2002", "{error:?}");
}

#[test]
fn checks_literal_returns_against_known_function_signatures() {
    let source = "local function take(): number return 'wrong' end";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "LT2004");
    assert!(
        diagnostics[0]
            .message
            .contains("Return value of function 'take'")
    );
}

#[test]
fn checks_record_literals_at_known_call_and_return_boundaries() {
    let source = "local function take(player: { name: number }) end\ntake({ name = 'Ada' })\nlocal function getPlayer(): { name: number } return { name = 'Ada' } end";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert_eq!(diagnostics.len(), 2);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "LT2002" || diagnostic.code == "LT2004")
    );
}

#[test]
fn checks_record_field_types_inside_known_function_returns() {
    let source = "typedef Player = { name: number }\nlocal function playerName(player: Player): string return player.name end";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "LT2004");
}

#[test]
fn optional_property_access_adds_nil_to_a_known_field_type() {
    let source = "typedef Player = { id: number }\nlocal player: Player? = nil\nlocal id: number = player?.id";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "LT2001");
}

#[test]
fn block_locals_do_not_leak_or_overwrite_outer_types() {
    let source = "local value: number = 1\ndo\n    local value: string = 'inside'\nend\nlocal after: number = value";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    assert!(check(source, &prepared, &ast, &[]).is_empty());
}

#[test]
fn substitutes_generic_alias_parameters_in_record_fields() {
    let source = "typedef Box<T> = { value: T }\nlocal box: Box<number> = { value = 'wrong' }";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "LT2001");
}

#[test]
fn checks_later_assignments_to_annotated_variables_and_record_fields() {
    let source = "typedef Player = { name: number }\nlocal count: number = 1\ncount = 'wrong'\nlocal player: Player = { name = 1 }\nplayer.name = 'Ada'";
    let prepared = prepare(source);
    let ast =
        full_moon::parse_fallible(&prepared.text, full_moon::LuaVersion::cfxlua().with_luau())
            .into_ast();
    let diagnostics = check(source, &prepared, &ast, &[]);
    assert_eq!(diagnostics.len(), 2);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code == "LT2001")
    );
}

#[test]
fn checks_each_if_expression_branch_against_the_expected_type() {
    assert!(crate::compile_source("local value: number = if true then 1 else 2").is_ok());
    let error =
        crate::compile_source("local value: number = if true then 1 else 'wrong'").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not assignable to type 'number'")
    );

    assert!(
        crate::compile_source(
            "local function take(value: number) end\ntake(if true then 1 else 'wrong')"
        )
        .is_err()
    );
    assert!(
        crate::compile_source(
            "local function take(): number return if true then 1 else 'wrong' end"
        )
        .is_err()
    );
    assert!(
        crate::compile_source(
            "local value: {name: number} = if true then {name = 1} else {name = 'wrong'}"
        )
        .is_err()
    );
    assert!(crate::compile_source(
            "local function get(): {name: number} return if true then {name = 1} else {name = 'wrong'} end"
        )
        .is_err());

    assert!(
        crate::compile_source("local value: number = if\n    true then 1 else 'wrong'").is_err()
    );
}

#[test]
fn rejects_const_reassignment_but_allows_field_mutation() {
    assert!(
        crate::compile_source("const selam: number = 15\nselam = 16")
            .unwrap_err()
            .to_string()
            .contains("LT2007")
    );
    assert!(crate::compile_source("const config = { value = 1 }\nconfig.value = 2").is_ok());

    for (source, location) in [
        (
            "const value = 1\n-- comment before assignment\nvalue = 2",
            "3:1",
        ),
        (
            "const value = 1\nvalue -- comment before operator\n= 2",
            "2:1",
        ),
        (
            "const value = 1\n-- comment before compound assignment\nvalue += 2",
            "3:1",
        ),
    ] {
        let error = crate::compile_source(source).unwrap_err().to_string();
        assert_eq!(error.matches("LT2007").count(), 1, "{source}: {error}");
        assert!(error.contains(location), "{error}");
        assert!(
            error.contains("Cannot assign to const variable 'value'."),
            "{error}"
        );
    }
}

#[test]
fn function_parameters_shadow_outer_consts_for_reassignment() {
    let source = "const value = 1\nlocal function update(value: number) value = 2 end\nlocal callback = function(value: number) value = 3 end";
    assert!(crate::compile_source(source).is_ok());
}

#[test]
fn loop_variables_shadow_outer_consts_for_reassignment() {
    let source = "const value = 1\nfor value = 1, 2 do value = 3 end\nfor value in pairs({ value = 1 }) do value = 'text' end";
    assert!(crate::compile_source(source).is_ok());
}

#[test]
fn mutable_local_shadows_outer_const_for_reassignment() {
    let result = crate::compile_source(
        "const value = 1\ndo\n local value = 'text'\n value = 'updated'\nend",
    );
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn checks_const_annotations_against_scoped_expression_types() {
    let source = "local count: number = 1\nconst label: string = count";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn shadowing_names_are_not_visible_inside_their_initializers() {
    let outer_const = "const value = 1\nlocal value = function() value = 2 end";
    assert!(crate::compile_source(outer_const).is_err());
    let outer_mutable = "local value = 1\nconst value = function() value = 2 end";
    assert!(crate::compile_source(outer_mutable).is_ok());
}

#[test]
fn checks_const_annotations() {
    assert!(crate::compile_source("const count: number = 'wrong'").is_err());
    assert!(crate::compile_source("const count: number = 1").is_ok());
}

#[test]
fn numeric_for_variable_is_typed_and_does_not_escape() {
    let invalid = "for index = 1, 3 do\n local value: string = index\nend";
    assert!(crate::compile_source(invalid).is_err());
    let escaped = "for index = 1, 3 do end\nlocal value: string = index";
    assert!(crate::compile_source(escaped).is_ok());
}

#[test]
fn iterator_function_return_types_bind_generic_for_variables() {
    let source = "local function iterator(): (number, string) return 1, 'value' end\nfor key, value in iterator() do\n local invalid: number = value\nend";
    let error = crate::compile_source(source).unwrap_err().to_string();
    assert!(error.contains("LT2001"), "{error}");
}

#[test]
fn ipairs_binds_the_array_element_type() {
    let source = "typedef List = { number }\nlocal values: List = { 1 }\nfor _, value in ipairs(values) do\n local label: string = value\nend";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn pairs_binds_dictionary_key_and_value_types() {
    let source = "typedef Dictionary = { [string]: number }\nlocal values: Dictionary = { score = 1 }\nfor key, value in pairs(values) do\n local label: string = value\nend";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn checks_each_value_returned_into_local_bindings() {
    let source = "local function pair(): (number, string)\n return 1, 'ok'\nend\nlocal count: number, label: string = pair()";
    let result = crate::compile_source(source);
    assert!(result.is_ok(), "{result:?}");
    let invalid = source.replace("label: string", "label: number");
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn checks_final_tuple_values_in_assignments() {
    let source = "local function pair(): (number, number) return 1, 2 end\nlocal count: number, label: string\ncount, label = pair()";
    let error = crate::compile_source(source).unwrap_err().to_string();
    assert!(error.contains("Variable 'label'"), "{error}");
}

#[test]
fn checks_missing_local_values_as_nil() {
    let source = "local count: number, label: string = 1";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn parenthesized_tuple_call_is_one_function_argument() {
    let source = "local function pair(): (number, string) return 1, 'ok' end\nlocal function consume(count: number, label: string) end\nconsume((pair()))";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn expands_final_tuple_call_in_return_values() {
    let source = "local function pair(): (number, string) return 1, 'ok' end\nlocal function relay(): (number, string) return pair() end";
    assert!(crate::compile_source(source).is_ok());
    let invalid = source.replace("return pair()", "return (pair())");
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn expands_tuple_return_in_final_call_argument() {
    let valid = "local function pair(): (number, string)\n return 1, 'ok'\nend\nlocal function consume(count: number, label: string) end\nconsume(pair())";
    assert!(crate::compile_source(valid).is_ok());
    let invalid = valid.replace("label: string", "label: number");
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn expands_only_the_final_call_in_local_expression_lists() {
    let source = "local function pair(): (number, string) return 1, 'ok' end\nlocal enabled: boolean, count: number, label: string = true, pair()";
    assert!(crate::compile_source(source).is_ok());
    let invalid = source.replace("label: string", "label: number");
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn parenthesized_call_is_one_value_in_local_expression_lists() {
    let source = "local function pair(): (number, string) return 1, 'ok' end\nlocal count: number, label: string = (pair())";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn forwards_typed_varargs_with_their_element_type() {
    let valid = "local function consume(...: number) end\nlocal function forward(...: number) consume(...) end";
    assert!(crate::compile_source(valid).is_ok());
    let invalid = valid.replace("forward(...: number)", "forward(...: string)");
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn empty_return_values_are_checked_as_nil() {
    let source = "local function pair(): (number, string) return end";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn checks_typed_vararg_values() {
    let source = "local function numbers(...: number) end\nnumbers(1, 'bad')";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn checks_missing_required_arguments_as_nil() {
    let source = "local function need(value: number) end\nneed()";
    let error = crate::compile_source(source).unwrap_err().to_string();
    assert!(error.contains("LT2002"), "{error}");
    assert!(!error.contains("LT2003"), "{error}");
}

#[test]
fn reports_all_missing_literal_and_string_arguments() {
    let source = "function sss(status: 'info', message: string) end\nsss()";
    let error = crate::compile_source(source).unwrap_err();
    let arguments = error
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "LT2002")
        .collect::<Vec<_>>();
    assert_eq!(arguments.len(), 2, "{error:?}");
    assert!(arguments[0].message.contains("Argument 1 ('status')"));
    assert!(arguments[1].message.contains("Argument 2 ('message')"));
}

#[test]
fn rejects_non_literal_values_for_literal_parameters() {
    for source in [
        "local function take(value: 'info') end\ntake(nil)",
        "local function take(value: 'info' | 'warning') end\ntake(1)",
        "local text: string = 'info'\nlocal function take(value: 'info') end\ntake(text)",
    ] {
        assert!(crate::compile_source(source).is_err(), "{source}");
    }
    for source in [
        "local function take(value: 'info') end\ntake('info')",
        "typedef Status = 'info' | 'warning'\nlocal function take(value: Status?) end\ntake('info')",
    ] {
        assert!(crate::compile_source(source).is_ok(), "{source}");
    }
}

#[test]
fn checks_record_function_call_return_types() {
    for source in [
        "local function use(cfg: { print: (status: string) -> boolean })\nlocal retval: number = cfg.print('hello')\nend",
        "local function use(cfg: { print: (status: string) -> boolean })\nlocal retval: number = cfg['print']('hello')\nend",
        "local function use(cfg: { nested: { print: (status: string) -> boolean } })\nlocal retval: number = cfg.nested.print('hello')\nend",
    ] {
        let error = crate::compile_source(source).expect_err(source);
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "LT2001"),
            "{error:?}"
        );
    }
    assert!(crate::compile_source("local function use(cfg: { print: (status: string) -> boolean })\nlocal retval: boolean = cfg.print('hello')\nend").is_ok());
}

#[test]
fn accepts_omitted_optional_parameters() {
    let source = "local function describe(id: number, label: string?) end\ndescribe(1)";
    assert!(crate::compile_source(source).is_ok());
}

#[test]
fn coalescing_removes_nil_from_optional_values() {
    let valid = "local input: number? = nil\nlocal value: number = input ?? 1";
    assert!(crate::compile_source(valid).is_ok());
    let invalid = "local input: number? = nil\nlocal value: string = input ?? 1";
    assert!(crate::compile_source(invalid).is_err());
}

#[test]
fn rejects_known_incompatible_arithmetic_operands() {
    let error = crate::compile_source("local value = 'text' + 1")
        .unwrap_err()
        .to_string();
    assert!(error.contains("LT2001"), "{error}");
}

#[test]
fn infers_supported_ast_operator_results_and_assertions() {
    let source = "local text: string = 1 .. 'x'\nlocal negative: number = -1\nlocal length: number = #'x'\nlocal asserted: number = 'x' :: number\nlocal flag: boolean = not false";
    let result = crate::compile_source(source);
    assert!(result.is_ok(), "{result:?}");
    for invalid in ["true .. 'x'", "1 < 'x'", "-true"] {
        let error = crate::compile_source(&format!("local value = {invalid}")).unwrap_err();
        assert!(error.to_string().contains("LT2001"), "{error}");
    }
}

#[test]
fn comparison_result_is_boolean_for_type_assignability() {
    assert!(crate::compile_source("local result: number = 1 < 2").is_err());
    assert!(crate::compile_source("local result: boolean = 1 < 2").is_ok());
}

#[test]
fn checks_compound_assignment_against_inferred_variable_type() {
    let invalid = crate::compile_source("local value: number = 1\nvalue += 'bad'");
    assert!(invalid.is_err(), "{invalid:?}");
    assert!(crate::compile_source("local value: number = 1\nvalue += 2").is_ok());
}

#[test]
fn local_function_value_shadows_collected_signature() {
    let source = "local function use(value: number) end\ndo\n local use = function(value) end\n use('text')\nend";
    assert!(crate::compile_source(source).is_ok());
}

#[test]
fn local_function_signatures_are_restored_after_nested_blocks() {
    let source = "local function choose(value: number) end\ndo\n local function choose(value: string) end\n choose('inner')\nend\nchoose(1)";
    assert!(crate::compile_source(source).is_ok());
}

#[test]
fn anonymous_function_return_uses_its_own_annotation() {
    let source = "local function outer(): number\n local callback = function(): string return 'ok' end\n return 1\nend";
    assert!(crate::compile_source(source).is_ok());
    let invalid = source.replace("return 'ok'", "return 1");
    assert!(crate::compile_source(&invalid).is_err());
}

#[test]
fn parallel_local_initializers_use_the_outer_scope() {
    let source =
        "local shadow = 'outer'\nlocal shadow, copy = 1, shadow\nlocal value: string = copy";
    assert!(crate::compile_source(source).is_ok());
}

#[test]
fn type_alias_shadowing_is_restored_after_nested_blocks() {
    let source = "typedef Value = string\ndo\n typedef Value = number\n local inner: Value = 1\nend\nlocal outer: Value = 'text'";
    assert!(crate::compile_source(source).is_ok());
}

#[test]
fn number_unification_and_computed_array_indices() {
    for source in [
        "local count: integer = 1.5\nlocal value: number = count",
        "typedef Player = { name: string }\nlocal players: { Player } = {}\nplayers[#players + 1] = { name = 'Ada' }",
        "typedef Handler<T> = (value: T) -> boolean\nlocal handlers: { Handler<number> } = {}\nfor _, handler in ipairs(handlers) do handler(1) end",
    ] {
        let result = crate::compile_source(source);
        assert!(
            result.is_ok(),
            "{source}\n{result:?}\nprepared: {}",
            prepare(source).text
        );
    }
}

#[test]
fn dynamic_record_reads_are_optional_and_writes_stay_checked() {
    let source = "local jobs = { police = true, medic = false }\nlocal job: string = 'other'\nlocal allowed: boolean? = jobs[job]";
    let result = crate::compile_source(source);
    assert!(result.is_ok(), "{result:?}");
    assert!(crate::compile_source(&source.replace("boolean?", "boolean")).is_err());
    assert!(crate::compile_source(&format!("{source}\njobs[job] = true")).is_err());
    assert!(
        crate::compile_source("local jobs = { police = true }\nlocal value = jobs['typo']")
            .is_err()
    );
}

#[test]
fn nil_guards_narrow_only_the_reachable_branch() {
    let source = "typedef Player = { name: string }\nlocal function take(name: string) end\nlocal function use(player: Player?)\n if player == nil or player.name == '' then return end\n take(player.name)\nend";
    let result = crate::compile_source(source);
    assert!(result.is_ok(), "{result:?}");
    let guarded = "typedef Player = { name: string }\nlocal function take(name: string) end\nlocal function use(player: Player?)\n if player ~= nil then take(player.name) end\nend";
    assert!(crate::compile_source(guarded).is_ok());
    assert!(
        crate::compile_source(&guarded.replace(
            "take(player.name) end",
            "player = nil\n take(player.name) end"
        ))
        .is_err()
    );
}

#[test]
fn postfix_arrays_share_existing_type_checks() {
    for source in [
        "typedef Player = { name: string }\nlocal players: Player[] = {}\nplayers[#players + 1] = { name = 'Ada' }",
        "typedef List<T> = T[]\nlocal values: List<number[]> = { { 1, 2 } }",
        "local values: (number | nil)[] = { 1 }\nlocal matrix: number[][] = { { 1 } }\nlocal maybe: number[]? = nil",
        "local callbacks: (() -> number)[] = {}\nlocal function values(): number[] return { 1 } end",
    ] {
        let result = crate::compile_source(source);
        assert!(
            result.is_ok(),
            "{source}\n{result:?}\nprepared: {}",
            prepare(source).text
        );
    }
    assert!(crate::compile_source("local values: number[] = { 'bad' }").is_err());
    assert!(crate::compile_source("local values: number[ = {}").is_err());
}

#[test]
fn optional_callbacks_generics_and_shadowed_guards() {
    assert!(crate::compile_source("local callback: () -> number? = nil").is_err());
    assert!(
        crate::compile_source("typedef Box<T> = { value: T }\nlocal box: Box<number>? = nil")
            .is_ok()
    );
    let source = "typedef Player = { name: string }\nlocal function take(value: string) end\nlocal function use(p: Player?)\n if p then\n local p: Player? = nil\n take(p.name)\n end\nend";
    assert!(crate::compile_source(source).is_err());
}

#[test]
fn postfix_ranges_and_numeric_compatibility_preserve_source_text() {
    let source =
        "-- Türkçe integer[]\nlocal text = 'integer[]'\nlocal values: number[] = { 'bad' }";
    let error = crate::compile_source(source).unwrap_err();
    assert_eq!(&source[error.diagnostics[0].source_range.clone()], "'bad'");
    let source = "local fields: { integer: number, kind: 'integer' } = { integer = 1, kind = 'integer' }\nlocal value: integer = 1.5";
    let result = crate::compile_source(source);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(super::display_type("{ { number } }"), "number[][]");
    assert_eq!(
        super::display_type("{ name: string, values: { number } }"),
        "{ name: string, values: number[] }"
    );
}
