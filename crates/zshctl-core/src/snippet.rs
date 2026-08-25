use crate::placeholder::apply_first_placeholder;
use fancy_regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnippetContext {
    #[serde(default)]
    pub global: bool,
    #[serde(default)]
    pub buffer: Option<String>,
    #[serde(default)]
    pub lbuffer: Option<String>,
    #[serde(default)]
    pub rbuffer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snippet {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub keyword: Option<String>,
    pub snippet: String,
    #[serde(default)]
    pub context: Option<SnippetContext>,
    #[serde(default)]
    pub evaluate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EditResult {
    Success { buffer: String, cursor: usize },
    Failure,
}

pub fn insert_snippet(snippets: &[Snippet], name: &str, left: &str, right: &str) -> EditResult {
    let Some(snippet) = snippets.iter().find(|snippet| {
        snippet
            .name
            .as_deref()
            .is_some_and(|candidate| candidate.trim() == name.trim())
    }) else {
        return EditResult::Failure;
    };
    let left = normalize(left, false, true);
    let right = normalize(right, true, false);
    let prepared = apply_first_placeholder(&snippet.snippet, snippet.snippet.chars().count() + 1);
    EditResult::Success {
        cursor: left.chars().count() + prepared.cursor,
        buffer: format!("{left}{}{right} ", prepared.buffer),
    }
}

pub fn auto_snippet(snippets: &[Snippet], left: &str, right: &str) -> EditResult {
    let has_leading = left.chars().next().is_some_and(char::is_whitespace);
    let left = normalize(left, false, true);
    let right = normalize(right, true, false);
    if !right.is_empty() && !right.starts_with(' ') {
        return EditResult::Failure;
    }
    let tokens = shell_words::split(left.trim()).unwrap_or_default();
    let Some(first) = tokens.first() else {
        return EditResult::Failure;
    };
    let last = tokens.last().expect("non-empty tokens");
    let prefix = match tokens.len() {
        0 | 1 => has_leading.then_some(" ").unwrap_or_default().to_owned(),
        _ => format!(
            "{}{} ",
            if has_leading { " " } else { "" },
            tokens[..tokens.len() - 1].join(" ")
        ),
    };
    let full = format!("{left}{right}");
    for snippet in snippets {
        if snippet.keyword.as_deref() != Some(last.as_str()) {
            continue;
        }
        if let Some(context) = &snippet.context {
            if !context.global
                && (!matches_optional(&context.buffer, &full)
                    || !matches_optional(&context.lbuffer, &left)
                    || !matches_optional(&context.rbuffer, &right))
            {
                continue;
            }
        } else if last != first {
            continue;
        }
        let prepared =
            apply_first_placeholder(&snippet.snippet, snippet.snippet.chars().count() + 1);
        let cursor = prefix.chars().count() + prepared.cursor;
        let mut buffer = format!("{prefix}{}{right}", prepared.buffer);
        if buffer.chars().count() < cursor {
            buffer.push(' ');
        }
        return EditResult::Success { buffer, cursor };
    }
    EditResult::Failure
}

/// Returns the index of the snippet that auto expansion would select.
/// This lets callers defer potentially side-effecting `evaluate` commands until
/// after all keyword and context checks have succeeded.
pub fn matching_auto_snippet(snippets: &[Snippet], left: &str, right: &str) -> Option<usize> {
    let left = normalize(left, false, true);
    let right = normalize(right, true, false);
    if !right.is_empty() && !right.starts_with(' ') {
        return None;
    }
    let tokens = shell_words::split(left.trim()).unwrap_or_default();
    let first = tokens.first()?;
    let last = tokens.last()?;
    let full = format!("{left}{right}");
    snippets.iter().position(|snippet| {
        if snippet.keyword.as_deref() != Some(last.as_str()) {
            return false;
        }
        if let Some(context) = &snippet.context {
            context.global
                || (matches_optional(&context.buffer, &full)
                    && matches_optional(&context.lbuffer, &left)
                    && matches_optional(&context.rbuffer, &right))
        } else {
            last == first
        }
    })
}

pub fn prepare_preprompt(template: &str) -> EditResult {
    if template.trim().is_empty() {
        return EditResult::Failure;
    }
    let template = if template.ends_with(' ') {
        template.into()
    } else {
        format!("{template} ")
    };
    let edit = apply_first_placeholder(&template, template.chars().count());
    EditResult::Success {
        buffer: edit.buffer,
        cursor: edit.cursor,
    }
}

fn matches_optional(pattern: &Option<String>, value: &str) -> bool {
    pattern.as_deref().is_none_or(|pattern| {
        Regex::new(pattern).is_ok_and(|regex| regex.is_match(value).unwrap_or(false))
    })
}

fn normalize(value: &str, keep_leading: bool, keep_trailing: bool) -> String {
    let leading = keep_leading && value.chars().next().is_some_and(char::is_whitespace);
    let trailing = keep_trailing && value.chars().last().is_some_and(char::is_whitespace);
    let normalized = shell_words::split(value)
        .map(|tokens| tokens.join(" "))
        .unwrap_or_else(|_| value.split_whitespace().collect::<Vec<_>>().join(" "));
    format!(
        "{}{}{}",
        if leading { " " } else { "" },
        normalized,
        if trailing { " " } else { "" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<Snippet> {
        vec![
            Snippet {
                name: Some("git status".into()),
                keyword: Some("gs".into()),
                snippet: "git status --short --branch".into(),
                context: None,
                evaluate: false,
            },
            Snippet {
                name: None,
                keyword: Some("S".into()),
                snippet: "| sed 's/{{MATCH}}/{{REPLACE}}/g'".into(),
                context: Some(SnippetContext {
                    lbuffer: Some(r".+\s".into()),
                    ..Default::default()
                }),
                evaluate: false,
            },
        ]
    }

    #[test]
    fn expands_global_first_word() {
        assert_eq!(
            auto_snippet(&fixture(), "  gs", ""),
            EditResult::Success {
                buffer: " git status --short --branch ".into(),
                cursor: 29,
            }
        );
    }

    #[test]
    fn applies_context_and_first_placeholder() {
        assert_eq!(
            auto_snippet(&fixture(), "find . S", ""),
            EditResult::Success {
                buffer: "find . | sed 's//{{REPLACE}}/g'".into(),
                cursor: 16,
            }
        );
    }

    #[test]
    fn no_match_does_not_edit_buffer() {
        assert_eq!(auto_snippet(&fixture(), "missing", ""), EditResult::Failure);
    }

    #[test]
    fn matching_index_uses_the_same_context_rules() {
        assert_eq!(matching_auto_snippet(&fixture(), "find . S", ""), Some(1));
        assert_eq!(matching_auto_snippet(&fixture(), "S", ""), None);
    }
}
