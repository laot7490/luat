use std::fmt;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub location: SourceLocation,
    pub code: &'static str,
    pub length: usize,
    pub source_range: std::ops::Range<usize>,
    pub message: String,
}

impl Diagnostic {
    pub(crate) fn at(
        source: &str,
        start: usize,
        length: usize,
        code: &'static str,
        message: String,
    ) -> Self {
        let start = start.min(source.len());
        let start = (0..=start)
            .rev()
            .find(|offset| source.is_char_boundary(*offset))
            .unwrap_or(0);
        let end = source[start..]
            .char_indices()
            .nth(length)
            .map(|(offset, _)| start + offset)
            .unwrap_or(source.len());
        Self::range(source, start, end, code, message)
    }

    pub(crate) fn range(
        source: &str,
        start: usize,
        end: usize,
        code: &'static str,
        message: String,
    ) -> Self {
        let start = start.min(source.len());
        let start = (0..=start)
            .rev()
            .find(|offset| source.is_char_boundary(*offset))
            .unwrap_or(0);
        let end = end.min(source.len()).max(start);
        let end = (start..=end)
            .rev()
            .find(|offset| source.is_char_boundary(*offset))
            .unwrap_or(start);
        Self {
            location: location(source, start),
            code,
            length: source[start..end].chars().count().max(1),
            source_range: start..end,
            message,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.location.line, self.location.column, self.code, self.message
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompileError {
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            write!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for CompileError {}

pub(crate) fn location(source: &str, byte: usize) -> SourceLocation {
    let prefix = source.get(..byte.min(source.len())).unwrap_or(source);
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let column =
        prefix
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .fold(0, |column, character| {
                if character == '\t' {
                    (column / 4 + 1) * 4
                } else {
                    column + UnicodeWidthChar::width(character).unwrap_or(0)
                }
            })
            + 1;
    SourceLocation { line, column }
}

pub(crate) fn parser_diagnostics(
    errors: &[full_moon::Error],
    prefix: &str,
    source: &str,
    original_offset: impl Fn(usize) -> usize,
) -> Vec<Diagnostic> {
    errors
        .iter()
        .map(|error| {
            let (start, end) = error.range();
            let start = original_offset(start.bytes());
            let end = original_offset(end.bytes());
            Diagnostic::range(
                source,
                start,
                end.max(start),
                "LT1001",
                format!("{prefix}{}", error.error_message()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::Diagnostic;

    #[test]
    fn records_unicode_ranges_and_tab_aware_columns() {
        let source = "\tλx";
        let diagnostic = Diagnostic::at(source, 3, 1, "LT2001", "bad value".to_owned());
        assert_eq!(diagnostic.location.column, 6);
        assert_eq!(diagnostic.source_range, 3..4);
    }
}
