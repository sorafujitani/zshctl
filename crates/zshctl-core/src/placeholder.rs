use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorEdit {
    pub buffer: String,
    /// Unicode scalar offset suitable for ZLE's character-based CURSOR.
    pub cursor: usize,
}

/// Removes the first `{{placeholder}}`. Whitespace and braces are not valid
/// inside a placeholder, following zshctl' placeholder contract.
pub fn apply_first_placeholder(text: &str, fallback_cursor: usize) -> CursorEdit {
    let chars: Vec<char> = text.chars().collect();
    let Some((start, end)) = first_placeholder_range(&chars) else {
        return CursorEdit {
            buffer: text.into(),
            cursor: fallback_cursor,
        };
    };
    let mut buffer = String::new();
    buffer.extend(chars[..start].iter());
    buffer.extend(chars[end..].iter());
    CursorEdit {
        buffer,
        cursor: start,
    }
}

pub fn next_placeholder(buffer: &str) -> Option<CursorEdit> {
    let chars: Vec<char> = buffer.chars().collect();
    let (start, end) = first_placeholder_range(&chars)?;
    let mut output = String::new();
    output.extend(chars[..start].iter());
    output.extend(chars[end..].iter());
    Some(CursorEdit {
        buffer: output,
        cursor: start,
    })
}

fn first_placeholder_range(chars: &[char]) -> Option<(usize, usize)> {
    for start in 0..chars.len().saturating_sub(3) {
        if chars[start] != '{' || chars[start + 1] != '{' {
            continue;
        }
        let mut index = start + 2;
        while index + 1 < chars.len() {
            if chars[index] == '}' && chars[index + 1] == '}' {
                return (index > start + 2).then_some((start, index + 2));
            }
            if chars[index].is_whitespace() || matches!(chars[index], '{' | '}') {
                break;
            }
            index += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn removes_first_placeholder_and_places_cursor_at_content() {
        assert_eq!(
            apply_first_placeholder("cmd {{TARGET}} {{TAIL}}", 99),
            CursorEdit {
                buffer: "cmd  {{TAIL}}".into(),
                cursor: 4,
            }
        );
    }

    #[test]
    fn uses_fallback_for_missing_or_malformed_placeholder() {
        assert_eq!(
            apply_first_placeholder("echo {{hello world}}", 12).cursor,
            12
        );
        assert_eq!(next_placeholder("echo {{hello"), None);
    }

    #[test]
    fn uses_character_offsets_for_unicode() {
        assert_eq!(next_placeholder("あ{{い}}").unwrap().cursor, 1);
    }

    proptest! {
        #[test]
        fn arbitrary_buffers_never_produce_invalid_cursor(buffer in ".{0,2048}") {
            if let Some(edit) = next_placeholder(&buffer) {
                prop_assert!(edit.cursor <= edit.buffer.chars().count());
            }
        }
    }
}
