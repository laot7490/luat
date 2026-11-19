use luat::analysis::TextEdit;
use tower_lsp_server::ls_types::{self as lsp, Position, TextEdit as LspTextEdit};

#[derive(Clone)]
pub(super) struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    pub(super) fn range(&self, source: &str, range: std::ops::Range<usize>) -> lsp::Range {
        lsp::Range::new(
            self.offset_to_position(source, range.start),
            self.offset_to_position(source, range.end),
        )
    }

    pub(super) fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        Self { starts }
    }

    pub(super) fn position_to_offset(&self, source: &str, position: Position) -> usize {
        let line = (position.line as usize).min(self.starts.len().saturating_sub(1));
        let start = self.starts[line];
        let end = source[start..]
            .find('\n')
            .map_or(source.len(), |offset| start + offset);
        let end = if end > start && source.as_bytes()[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        let wanted = position.character as usize;
        let mut units = 0;
        for (offset, character) in source[start..end].char_indices() {
            if units + character.len_utf16() > wanted {
                return start + offset;
            }
            units += character.len_utf16();
            if units == wanted {
                return start + offset + character.len_utf8();
            }
        }
        end
    }

    pub(super) fn offset_to_position(&self, source: &str, offset: usize) -> Position {
        let offset = offset.min(source.len());
        let line = self
            .starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = self.starts[line];
        Position::new(
            line as u32,
            source[start..offset].encode_utf16().count() as u32,
        )
    }
}

pub(super) fn byte_range_to_lsp(
    source: &str,
    range: std::ops::Range<usize>,
) -> tower_lsp_server::ls_types::Range {
    LineIndex::new(source).range(source, range)
}

pub(super) fn to_lsp_text_edit(source: &str, edit: TextEdit) -> LspTextEdit {
    LspTextEdit {
        range: byte_range_to_lsp(source, edit.range),
        new_text: edit.new_text,
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;
    use tower_lsp_server::ls_types::Position;

    #[test]
    fn utf16_positions_round_trip_with_emoji_turkish_tabs_and_crlf() {
        let source = "🚀\tçağrı\r\nnext";
        let index = LineIndex::new(source);
        let offset = source.find("çağrı").unwrap();
        let position = index.offset_to_position(source, offset);
        assert_eq!(position, Position::new(0, 3));
        assert_eq!(index.position_to_offset(source, position), offset);
        let next = source.find("next").unwrap();
        assert_eq!(
            index.position_to_offset(source, Position::new(1, 4)),
            next + 4
        );
    }
}
