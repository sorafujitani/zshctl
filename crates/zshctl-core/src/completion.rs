use fancy_regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionRule {
    pub name: String,
    pub patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub value: String,
    pub display: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinGitSource {
    Status,
    Files,
    Refs,
    Branches,
    Tags,
    Remotes,
    Worktrees,
    Commits,
    Stashes,
}

pub fn builtin_git_source(buffer: &str) -> Option<BuiltinGitSource> {
    let normalized = buffer.split_whitespace().collect::<Vec<_>>();
    if normalized.first().copied() != Some("git")
        || !buffer.chars().last().is_some_and(char::is_whitespace)
    {
        return None;
    }
    match normalized.get(1).copied() {
        Some("add" | "restore") => Some(BuiltinGitSource::Status),
        Some("status") => Some(BuiltinGitSource::Status),
        Some("switch") => Some(BuiltinGitSource::Branches),
        Some("tag") => Some(BuiltinGitSource::Tags),
        Some("remote") => Some(BuiltinGitSource::Remotes),
        Some("worktree") => Some(BuiltinGitSource::Worktrees),
        Some("stash")
            if matches!(
                normalized.get(2),
                Some(&("apply" | "drop" | "pop" | "show"))
            ) =>
        {
            Some(BuiltinGitSource::Stashes)
        }
        Some("commit")
            if normalized
                .iter()
                .any(|argument| argument.starts_with("--fixup")) =>
        {
            Some(BuiltinGitSource::Commits)
        }
        Some("checkout" | "reset" | "rebase" | "merge" | "diff") => {
            if normalized.contains(&"--") {
                Some(BuiltinGitSource::Files)
            } else {
                Some(BuiltinGitSource::Refs)
            }
        }
        Some("log") if normalized.contains(&"--") => Some(BuiltinGitSource::Files),
        Some("log") => Some(BuiltinGitSource::Refs),
        _ => None,
    }
}

pub fn matching_rule<'a>(rules: &'a [CompletionRule], buffer: &str) -> Option<&'a CompletionRule> {
    rules.iter().find(|rule| {
        let included = rule
            .patterns
            .iter()
            .any(|pattern| regex_matches(pattern, buffer));
        let excluded = rule
            .exclude_patterns
            .iter()
            .any(|pattern| regex_matches(pattern, buffer));
        included && !excluded
    })
}

pub fn normalize_candidates(
    lines: impl IntoIterator<Item = String>,
    maximum: usize,
) -> Vec<Candidate> {
    let mut seen = HashSet::new();
    lines
        .into_iter()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r').to_owned();
            if line.is_empty() || !seen.insert(line.clone()) {
                return None;
            }
            let (value, display) = line
                .split_once('\t')
                .map(|(value, display)| (value.to_owned(), Some(display.to_owned())))
                .unwrap_or_else(|| (line, None));
            Some(Candidate { value, display })
        })
        .take(maximum)
        .collect()
}

pub fn shell_insert(
    buffer: &str,
    cursor: usize,
    replacement: &str,
) -> crate::placeholder::CursorEdit {
    let mut chars: Vec<char> = buffer.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor]
        .iter()
        .rposition(|character| character.is_whitespace())
        .map_or(0, |position| position + 1);
    chars.splice(start..cursor, replacement.chars());
    crate::placeholder::CursorEdit {
        buffer: chars.into_iter().collect(),
        cursor: start + replacement.chars().count(),
    }
}

fn regex_matches(pattern: &str, value: &str) -> bool {
    Regex::new(pattern).is_ok_and(|regex| regex.is_match(value).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusions_win_over_patterns() {
        let rules = [CompletionRule {
            name: "git".into(),
            patterns: vec![r"^git ".into()],
            exclude_patterns: vec![r"^git commit ".into()],
        }];
        assert_eq!(matching_rule(&rules, "git branch").unwrap().name, "git");
        assert!(matching_rule(&rules, "git commit -m").is_none());
    }

    #[test]
    fn supports_lookaround_patterns() {
        let rules = [CompletionRule {
            name: "lookahead".into(),
            patterns: vec![r"^git (?=branch)".into()],
            exclude_patterns: vec![],
        }];
        assert!(matching_rule(&rules, "git branch").is_some());
        assert!(matching_rule(&rules, "git tag").is_none());
    }

    #[test]
    fn candidates_are_deduplicated_and_bounded() {
        let candidates =
            normalize_candidates(["a".into(), "a".into(), "b\tlabel".into(), "c".into()], 2);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1].display.as_deref(), Some("label"));
    }

    #[test]
    fn insertion_replaces_current_word_at_unicode_cursor() {
        assert_eq!(
            shell_insert("echo あbc tail", 8, "日本"),
            crate::placeholder::CursorEdit {
                buffer: "echo 日本 tail".into(),
                cursor: 7,
            }
        );
    }

    #[test]
    fn maps_public_git_completion_families() {
        assert_eq!(
            builtin_git_source("git add "),
            Some(BuiltinGitSource::Status)
        );
        assert_eq!(
            builtin_git_source("git switch "),
            Some(BuiltinGitSource::Branches)
        );
        assert_eq!(builtin_git_source("git tag "), Some(BuiltinGitSource::Tags));
        assert_eq!(
            builtin_git_source("git remote "),
            Some(BuiltinGitSource::Remotes)
        );
        assert_eq!(
            builtin_git_source("git worktree add "),
            Some(BuiltinGitSource::Worktrees)
        );
        assert_eq!(
            builtin_git_source("git checkout main -- "),
            Some(BuiltinGitSource::Files)
        );
        assert_eq!(
            builtin_git_source("git stash pop "),
            Some(BuiltinGitSource::Stashes)
        );
    }
}
