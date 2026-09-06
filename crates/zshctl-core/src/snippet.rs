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

pub fn matching_snippet(snippets: &[Snippet], key: &str) -> Option<usize> {
    let key = key.trim();
    snippets
        .iter()
        .position(|snippet| {
            snippet
                .name
                .as_deref()
                .is_some_and(|candidate| candidate.trim() == key)
        })
        .or_else(|| {
            snippets.iter().position(|snippet| {
                snippet
                    .keyword
                    .as_deref()
                    .is_some_and(|candidate| candidate.trim() == key)
            })
        })
}

pub fn matches_snippet_context(snippet: &Snippet, left: &str, right: &str) -> bool {
    let Some(context) = &snippet.context else {
        return true;
    };
    if context.global {
        return true;
    }
    let left = normalize(left, false, true);
    let right = normalize(right, true, false);
    let full = format!("{left}{right}");
    matches_optional(&context.buffer, &full)
        && matches_optional(&context.lbuffer, &left)
        && matches_optional(&context.rbuffer, &right)
}

pub fn insert_snippet(snippets: &[Snippet], name: &str, left: &str, right: &str) -> EditResult {
    let Some(index) = matching_snippet(snippets, name) else {
        return EditResult::Failure;
    };
    insert_snippet_at_with_context(snippets, index, left, right, left, right)
}

pub fn insert_snippet_at(
    snippets: &[Snippet],
    index: usize,
    left: &str,
    right: &str,
) -> EditResult {
    insert_snippet_at_with_context(snippets, index, left, right, left, right)
}

pub fn insert_snippet_at_with_context(
    snippets: &[Snippet],
    index: usize,
    left: &str,
    right: &str,
    context_left: &str,
    context_right: &str,
) -> EditResult {
    let Some(snippet) = snippets.get(index) else {
        return EditResult::Failure;
    };
    if !matches_snippet_context(snippet, context_left, context_right) {
        return EditResult::Failure;
    }
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
    for snippet in snippets {
        if snippet.keyword.as_deref() != Some(last.as_str()) {
            continue;
        }
        if !matches_snippet_context(snippet, &left, &right)
            || (snippet.context.is_none() && last != first)
        {
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
    snippets.iter().position(|snippet| {
        snippet.keyword.as_deref() == Some(last.as_str())
            && matches_snippet_context(snippet, &left, &right)
            && (snippet.context.is_some() || last == first)
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
    fn unnamed_snippet_can_be_inserted_by_keyword() {
        let snippets = vec![Snippet {
            name: None,
            keyword: Some("gs".into()),
            snippet: "git status".into(),
            context: None,
            evaluate: false,
        }];
        assert_eq!(
            insert_snippet(&snippets, "gs", "", ""),
            EditResult::Success {
                buffer: "git status ".into(),
                cursor: 11,
            }
        );
    }

    #[test]
    fn matching_index_uses_the_same_context_rules() {
        assert_eq!(matching_auto_snippet(&fixture(), "find . S", ""), Some(1));
        assert_eq!(matching_auto_snippet(&fixture(), "S", ""), None);
    }

    #[test]
    fn direct_insertion_rejects_a_context_mismatch() {
        let snippets = vec![Snippet {
            name: Some("contextual".into()),
            keyword: Some("ctx".into()),
            snippet: "echo context".into(),
            context: Some(SnippetContext {
                lbuffer: Some("^allowed$".into()),
                ..Default::default()
            }),
            evaluate: false,
        }];
        assert_eq!(
            insert_snippet(&snippets, "contextual", "blocked", ""),
            EditResult::Failure
        );
        assert!(matches!(
            insert_snippet(&snippets, "contextual", "allowed", ""),
            EditResult::Success { .. }
        ));
    }
}
