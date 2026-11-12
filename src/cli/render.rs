use luat::{BuildError, Diagnostic};
use std::{env, io::IsTerminal, path::PathBuf};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy)]
pub(super) enum Color {
    Error,
    Success,
    Cyan,
    Yellow,
    Muted,
}

pub(super) fn paint(text: &str, color: Color, enabled: bool) -> String {
    if !enabled || env::var_os("NO_COLOR").is_some() {
        return text.to_owned();
    }
    let code = match color {
        Color::Error => "31",
        Color::Success => "32",
        Color::Cyan => "36",
        Color::Yellow => "33",
        Color::Muted => "90",
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

pub(super) fn render_build_error(error: &BuildError, pretty: bool) -> String {
    match error {
        BuildError::Io { path, source } => format!("error: {}: {source}", path.display()),
        BuildError::Configuration { path, message } => {
            format!("error: {}: {message}", path.display())
        }
        BuildError::Compile { path, error } => {
            render_errors(&[(path.clone(), error.clone())], pretty).to_string()
        }
        BuildError::CompileMany(errors) => render_errors(errors, pretty).to_string(),
    }
}

pub(super) fn render_errors(errors: &[(PathBuf, luat::CompileError)], pretty: bool) -> String {
    let color = pretty && std::io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none();
    let mut rendered = Vec::new();
    let count = errors
        .iter()
        .map(|(_, error)| error.diagnostics.len())
        .sum::<usize>();
    for (path, error) in errors {
        let source = std::fs::read_to_string(path);
        for diagnostic in &error.diagnostics {
            let file_level = diagnostic.source_range.is_empty()
                && matches!(diagnostic.code, "LT9001" | "LT9003");
            rendered.push(match (&source, pretty, file_level) {
                (Ok(_), true, true) => render_file_diagnostic(path, diagnostic, color),
                (Ok(source), true, false) => render_diagnostic(path, source, diagnostic, color),
                (Ok(_), false, true) => format!(
                    "{}: error {}: {}",
                    path.display(),
                    diagnostic.code,
                    diagnostic.message
                ),
                (Ok(source), false, false) => {
                    let (line, column) = source_position(source, diagnostic.source_range.start);
                    format!(
                        "{}:{}:{}: error {}: {}",
                        path.display(),
                        line,
                        column,
                        diagnostic.code,
                        diagnostic.message
                    )
                }
                (Err(error), true, _) => {
                    render_unavailable_diagnostic(path, diagnostic, &error.to_string(), color)
                }
                (Err(error), false, _) => format!(
                    "{}:{}:{}: error {}: {} (source unavailable: {error})",
                    path.display(),
                    diagnostic.location.line,
                    diagnostic.location.column,
                    diagnostic.code,
                    diagnostic.message
                ),
            });
        }
    }
    if pretty {
        let error_word = if count == 1 { "error" } else { "errors" };
        let files = errors
            .iter()
            .map(|(path, _)| path)
            .collect::<std::collections::HashSet<_>>()
            .len();
        let file_word = if files == 1 { "file" } else { "files" };
        rendered.push(format!(
            "Found {count} {error_word} in {} {file_word}.",
            files
        ));
    }
    rendered.join(if pretty { "\n\n" } else { "\n" })
}

fn render_diagnostic(
    path: &std::path::Path,
    source: &str,
    diagnostic: &Diagnostic,
    color: bool,
) -> String {
    let start = source_offset(source, diagnostic.source_range.start);
    let (line_number, column) = source_position(source, start);
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |index| start + index);
    let line_end = if source.as_bytes().get(line_end.wrapping_sub(1)) == Some(&b'\r') {
        line_end - 1
    } else {
        line_end
    };
    let line = &source[line_start..line_end];
    let start_column = display_width(&source[line_start..start.min(line_end)]);
    let end = source_offset(source, diagnostic.source_range.end)
        .min(line_end)
        .max(start.min(line_end));
    let highlight_width = display_width_from(start_column, &source[start.min(line_end)..end])
        .saturating_sub(start_column)
        .max(1);
    let expanded = expand_tabs(line);
    let underline = "~".repeat(highlight_width);
    let number_width = line_number.to_string().len().max(4);
    let header = format!(
        "{}:{}:{} - {} {}: {}",
        paint(&path.display().to_string(), Color::Cyan, color),
        paint(&line_number.to_string(), Color::Yellow, color),
        paint(&column.to_string(), Color::Yellow, color),
        paint("error", Color::Error, color),
        paint(diagnostic.code, Color::Muted, color),
        diagnostic.message
    );
    let code_line = format!("{line_number:>number_width$} {expanded}");
    let marker = format!(
        "{}{}{}",
        " ".repeat(number_width + 1),
        " ".repeat(start_column),
        underline
    );
    if color {
        format!("{header}\n\n{code_line}\n\x1b[31m{marker}\x1b[0m")
    } else {
        format!("{header}\n\n{code_line}\n{marker}")
    }
}

fn render_file_diagnostic(path: &std::path::Path, diagnostic: &Diagnostic, color: bool) -> String {
    format!(
        "{} - {} {}: {}",
        paint(&path.display().to_string(), Color::Cyan, color),
        paint("error", Color::Error, color),
        paint(diagnostic.code, Color::Muted, color),
        diagnostic.message
    )
}

fn render_unavailable_diagnostic(
    path: &std::path::Path,
    diagnostic: &Diagnostic,
    error: &str,
    color: bool,
) -> String {
    format!(
        "{}:{}:{} - {} {}: {}\nSource unavailable: {error}",
        paint(&path.display().to_string(), Color::Cyan, color),
        paint(&diagnostic.location.line.to_string(), Color::Yellow, color),
        paint(
            &diagnostic.location.column.to_string(),
            Color::Yellow,
            color
        ),
        paint("error", Color::Error, color),
        paint(diagnostic.code, Color::Muted, color),
        diagnostic.message
    )
}

fn source_offset(source: &str, offset: usize) -> usize {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

fn source_position(source: &str, offset: usize) -> (usize, usize) {
    let offset = source_offset(source, offset);
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    (line, display_width(&prefix[line_start..]) + 1)
}

fn display_width(value: &str) -> usize {
    display_width_from(0, value)
}

fn display_width_from(mut column: usize, value: &str) -> usize {
    for character in value.chars() {
        column = if character == '\t' {
            (column / 4 + 1) * 4
        } else {
            column + UnicodeWidthChar::width(character).unwrap_or(0)
        };
    }
    column
}

fn expand_tabs(value: &str) -> String {
    let mut expanded = String::new();
    let mut column = 0;
    for character in value.chars() {
        if character == '\t' {
            let next = (column / 4 + 1) * 4;
            expanded.push_str(&" ".repeat(next - column));
            column = next;
        } else {
            expanded.push(character);
            column += UnicodeWidthChar::width(character).unwrap_or(0);
        }
    }
    expanded
}

#[cfg(test)]
mod tests {
    use super::{render_diagnostic, render_errors};
    use luat::{CompileError, Diagnostic, SourceLocation};
    use std::path::{Path, PathBuf};

    #[test]
    fn renders_the_source_line_and_marker_from_the_diagnostic_range() {
        let source = "-- comment\nlocal bad = {}\n-- another comment";
        let start = source.find("{}").unwrap();
        let diagnostic = Diagnostic {
            location: SourceLocation {
                line: 99,
                column: 99,
            },
            code: "LT2001",
            length: 1,
            source_range: start..start + 1,
            message: "missing field".to_owned(),
        };
        let rendered = render_diagnostic(Path::new("main.luat"), source, &diagnostic, false);

        assert!(rendered.contains("main.luat:2:13"), "{rendered}");
        assert!(rendered.contains("   2 local bad = {}"), "{rendered}");
        assert!(
            rendered.contains(&format!("{}~", " ".repeat(17))),
            "{rendered}"
        );
    }

    #[test]
    fn does_not_show_a_source_line_for_an_unlocated_formatter_error() {
        let error = CompileError {
            diagnostics: vec![Diagnostic {
                location: SourceLocation { line: 1, column: 1 },
                code: "LT9001",
                length: 1,
                source_range: 0..0,
                message: "Could not format generated CfxLua".to_owned(),
            }],
        };
        let rendered = render_errors(&[(PathBuf::from("Cargo.toml"), error)], true);

        assert!(!rendered.contains("Cargo.toml:1:1"), "{rendered}");
        assert!(!rendered.contains("[package]"), "{rendered}");
        assert!(rendered.contains("error LT9001"), "{rendered}");
    }

    #[test]
    fn computes_display_coordinates_from_the_source_range() {
        assert_eq!(super::source_position("first\r\n\tλ value", 8), (2, 5));
    }

    #[test]
    fn clips_multiline_unicode_ranges_at_the_first_crlf_line() {
        let source = "\tlocal value = 'Ça'\r\nnext line";
        let start = source.find("'Ça'").unwrap();
        let end = source.find("next").unwrap() + 4;
        let diagnostic = Diagnostic {
            location: SourceLocation {
                line: 80,
                column: 80,
            },
            code: "LT2001",
            length: 1,
            source_range: start..end,
            message: "bad value".to_owned(),
        };
        let rendered = render_diagnostic(Path::new("main.luat"), source, &diagnostic, false);

        assert!(rendered.contains("main.luat:1:19"), "{rendered}");
        assert!(
            rendered.contains("   1     local value = 'Ça'"),
            "{rendered}"
        );
        assert!(!rendered.contains('\r'), "{rendered}");
        assert_eq!(rendered.lines().last().unwrap().matches('~').count(), 4);
    }

    #[test]
    fn renders_eof_ranges_on_the_final_empty_line() {
        let source = "first\r\n";
        let diagnostic = Diagnostic {
            location: SourceLocation {
                line: 90,
                column: 90,
            },
            code: "LT1001",
            length: 1,
            source_range: source.len()..source.len(),
            message: "unexpected end".to_owned(),
        };
        let rendered = render_diagnostic(Path::new("main.luat"), source, &diagnostic, false);

        assert!(rendered.contains("main.luat:2:1"), "{rendered}");
        assert!(rendered.contains("   2 \n     ~"), "{rendered}");
    }

    #[test]
    fn plain_diagnostics_derive_coordinates_from_the_source_range() {
        let path =
            std::env::temp_dir().join(format!("luat-render-plain-{}.luat", std::process::id()));
        let source = "-- comment\r\n\tlocal bad = {}\r\n";
        std::fs::write(&path, source).unwrap();
        let start = source.find("{}").unwrap();
        let error = CompileError {
            diagnostics: vec![Diagnostic {
                location: SourceLocation { line: 1, column: 1 },
                code: "LT2001",
                length: 1,
                source_range: start..start + 1,
                message: "missing field".to_owned(),
            }],
        };
        let rendered = render_errors(&[(path.clone(), error)], false);
        let _ = std::fs::remove_file(&path);

        assert!(rendered.contains(":2:17: error LT2001"), "{rendered}");
        assert!(!rendered.contains("-- comment"), "{rendered}");
    }

    #[test]
    fn reports_when_source_is_unavailable_without_rendering_a_blank_excerpt() {
        let error = CompileError {
            diagnostics: vec![Diagnostic {
                location: SourceLocation { line: 3, column: 4 },
                code: "LT2001",
                length: 1,
                source_range: 10..11,
                message: "type mismatch".to_owned(),
            }],
        };
        let rendered = render_errors(
            &[(
                PathBuf::from("missing-source-for-renderer-test.luat"),
                error,
            )],
            true,
        );

        assert!(!rendered.contains("\n\n    3"), "{rendered}");
        assert!(rendered.contains("Source unavailable"), "{rendered}");
    }
}
