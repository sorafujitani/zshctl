//! YAML configuration discovery, precedence, parsing, merging, and invalidation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use thiserror::Error;
use zshctl_core::snippet::Snippet;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub snippets: Vec<Snippet>,
    #[serde(default)]
    pub completions: Vec<Completion>,
    #[serde(default)]
    pub history: HistorySettings,
    #[serde(skip)]
    history_presence: HistoryPresence,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HistoryPresence {
    default_scope: bool,
    redact: bool,
    fzf_command: bool,
    fzf_options: bool,
    keymap: KeymapPresence,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct KeymapPresence {
    delete_soft: bool,
    delete_hard: bool,
    toggle_scope: bool,
    toggle_preview: bool,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct SettingsDocument {
    snippets: Vec<Snippet>,
    completions: Vec<Completion>,
    history: Option<HistoryDocument>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct HistoryDocument {
    default_scope: Option<String>,
    redact: Option<Vec<String>>,
    keymap: Option<KeymapDocument>,
    fzf_command: Option<Option<String>>,
    fzf_options: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct KeymapDocument {
    delete_soft: Option<String>,
    delete_hard: Option<String>,
    toggle_scope: Option<String>,
    toggle_preview: Option<String>,
}

impl SettingsDocument {
    fn into_settings(self) -> Result<Settings, String> {
        let mut settings = Settings {
            snippets: self.snippets,
            completions: self.completions,
            ..Settings::default()
        };
        let Some(history) = self.history else {
            return Ok(settings);
        };
        if let Some(scope) = history.default_scope {
            if !matches!(
                scope.as_str(),
                "global" | "repository" | "directory" | "session"
            ) {
                return Err(format!("unsupported history defaultScope: {scope}"));
            }
            settings.history.default_scope = scope;
            settings.history_presence.default_scope = true;
        }
        if let Some(redact) = history.redact {
            settings.history.redact = redact;
            settings.history_presence.redact = true;
        }
        if let Some(command) = history.fzf_command {
            settings.history.fzf_command = command;
            settings.history_presence.fzf_command = true;
        }
        if let Some(options) = history.fzf_options {
            settings.history.fzf_options = options;
            settings.history_presence.fzf_options = true;
        }
        if let Some(keymap) = history.keymap {
            if let Some(value) = keymap.delete_soft {
                settings.history.keymap.delete_soft = value;
                settings.history_presence.keymap.delete_soft = true;
            }
            if let Some(value) = keymap.delete_hard {
                settings.history.keymap.delete_hard = value;
                settings.history_presence.keymap.delete_hard = true;
            }
            if let Some(value) = keymap.toggle_scope {
                settings.history.keymap.toggle_scope = value;
                settings.history_presence.keymap.toggle_scope = true;
            }
            if let Some(value) = keymap.toggle_preview {
                settings.history.keymap.toggle_preview = value;
                settings.history_presence.keymap.toggle_preview = true;
            }
        }
        Ok(settings)
    }
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        SettingsDocument::deserialize(deserializer)?
            .into_settings()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Completion {
    pub name: String,
    pub patterns: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    #[serde(default)]
    pub source_command: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub callback: Option<String>,
    #[serde(default)]
    pub callback_zero: bool,
    #[serde(default, deserialize_with = "deserialize_options")]
    pub options: Vec<String>,
}

fn deserialize_options<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(Vec::new()),
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(|value| {
                value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                    serde::de::Error::custom("fzf option arrays must contain strings")
                })
            })
            .collect(),
        serde_json::Value::Object(values) => Ok(values
            .into_iter()
            .filter_map(|(key, value)| match value {
                serde_json::Value::Bool(false) => None,
                serde_json::Value::Bool(true) | serde_json::Value::Null => Some(key),
                serde_json::Value::String(value) if key == "--preview" => {
                    Some(format!("{key}=\"{}\"", value.replace('"', "\\\"")))
                }
                serde_json::Value::String(value) => Some(format!("{key}={value}")),
                serde_json::Value::Number(value) => Some(format!("{key}={value}")),
                serde_json::Value::Array(values) if key == "--expect" => Some(format!(
                    "--expect=\"{}\"",
                    values
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                )),
                serde_json::Value::Array(values) if key == "--bind" => Some(format!(
                    "--bind=\"{}\"",
                    values
                        .iter()
                        .filter_map(|value| Some(format!(
                            "{}:{}",
                            value.get("key")?.as_str()?,
                            value.get("action")?.as_str()?
                        )))
                        .collect::<Vec<_>>()
                        .join(",")
                )),
                _ => None,
            })
            .collect()),
        _ => Err(serde::de::Error::custom(
            "fzf options must be an object or string array",
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HistorySettings {
    pub default_scope: String,
    pub redact: Vec<String>,
    pub keymap: HistoryKeymapSettings,
    pub fzf_command: Option<String>,
    pub fzf_options: Vec<String>,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            default_scope: "global".into(),
            redact: Vec::new(),
            keymap: HistoryKeymapSettings::default(),
            fzf_command: None,
            fzf_options: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HistoryKeymapSettings {
    pub delete_soft: String,
    pub delete_hard: String,
    pub toggle_scope: String,
    pub toggle_preview: String,
}

impl Default for HistoryKeymapSettings {
    fn default() -> Self {
        Self {
            delete_soft: "ctrl-d".into(),
            delete_hard: "alt-d".into(),
            toggle_scope: "ctrl-r".into(),
            toggle_preview: "?".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Yaml(PathBuf),
}

pub fn discover(home: &Path, cwd: &Path, env: &impl Env) -> Vec<ConfigSource> {
    if let Some(explicit) = env
        .get("ZSHCTL_CONFIG")
        .filter(|value| !value.trim().is_empty())
    {
        return vec![source_for(PathBuf::from(explicit))];
    }
    let project_root = find_project_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut sources = Vec::new();
    if let Some(local) = env
        .get("ZSHCTL_LOCAL_CONFIG_PATH")
        .filter(|value| !value.trim().is_empty())
    {
        append_path(&mut sources, project_root.join(local.trim()));
    } else if !truthy(env.get("ZSHCTL_DISABLE_AUTOMATIC_WORKSPACE_LOOKUP")) {
        append_directory(&mut sources, &project_root.join(".zshctl"));
    }

    if let Some(directory) = env
        .get("ZSHCTL_HOME")
        .filter(|value| !value.trim().is_empty())
    {
        append_path(&mut sources, PathBuf::from(directory));
    }

    if env.get("ZSHCTL_HOME").is_none() {
        let mut bases = vec![
            env.get("XDG_CONFIG_HOME")
                .filter(|value| !value.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config")),
        ];
        if let Some(raw) = env.get("XDG_CONFIG_DIRS") {
            bases.extend(
                raw.split(':')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from),
            );
        }
        for base in bases {
            append_directory(&mut sources, &base.join("zshctl"));
        }
    }
    sources.dedup();
    sources
}

fn append_path(sources: &mut Vec<ConfigSource>, path: PathBuf) {
    if path.is_file() {
        sources.push(source_for(path));
    } else {
        append_directory(sources, &path);
    }
}

fn append_directory(sources: &mut Vec<ConfigSource>, directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("yml" | "yaml")
                )
        })
        .collect::<Vec<_>>();
    paths.sort();
    sources.extend(paths.into_iter().map(source_for));
}

fn find_project_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|directory| directory.join(".git").exists())
        .map(Path::to_path_buf)
}

fn truthy(value: Option<String>) -> bool {
    value.is_some_and(|value| !matches!(value.trim(), "" | "0" | "false"))
}

fn source_for(path: PathBuf) -> ConfigSource {
    ConfigSource::Yaml(path)
}

pub trait Env {
    fn get(&self, key: &str) -> Option<String>;
}

impl Env for HashMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        HashMap::get(self, key).cloned()
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid YAML configuration {path}: {source}")]
    InvalidYaml {
        path: PathBuf,
        source: serde_yaml::Error,
    },
}

pub fn load_yaml(path: &Path) -> Result<Settings, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.into(),
        source,
    })?;
    serde_yaml::from_str(&text).map_err(|source| ConfigError::InvalidYaml {
        path: path.into(),
        source,
    })
}

pub fn merge(mut base: Settings, overlay: Settings) -> Settings {
    base.snippets.extend(overlay.snippets);
    base.completions.extend(overlay.completions);
    if overlay.history_presence.default_scope {
        base.history.default_scope = overlay.history.default_scope;
        base.history_presence.default_scope = true;
    }
    if overlay.history_presence.redact {
        base.history.redact = overlay.history.redact;
        base.history_presence.redact = true;
    }
    if overlay.history_presence.fzf_command {
        base.history.fzf_command = overlay.history.fzf_command;
        base.history_presence.fzf_command = true;
    }
    if overlay.history_presence.fzf_options {
        base.history.fzf_options = overlay.history.fzf_options;
        base.history_presence.fzf_options = true;
    }
    if overlay.history_presence.keymap.delete_soft {
        base.history.keymap.delete_soft = overlay.history.keymap.delete_soft;
        base.history_presence.keymap.delete_soft = true;
    }
    if overlay.history_presence.keymap.delete_hard {
        base.history.keymap.delete_hard = overlay.history.keymap.delete_hard;
        base.history_presence.keymap.delete_hard = true;
    }
    if overlay.history_presence.keymap.toggle_scope {
        base.history.keymap.toggle_scope = overlay.history.keymap.toggle_scope;
        base.history_presence.keymap.toggle_scope = true;
    }
    if overlay.history_presence.keymap.toggle_preview {
        base.history.keymap.toggle_preview = overlay.history.keymap.toggle_preview;
        base.history_presence.keymap.toggle_preview = true;
    }
    base
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Fingerprint {
    path: PathBuf,
    modified: Option<SystemTime>,
    length: u64,
    content_hash: u64,
}

#[derive(Default)]
pub struct ConfigCache {
    entries: HashMap<Fingerprint, Settings>,
    effective: HashMap<Vec<Fingerprint>, Settings>,
}

impl ConfigCache {
    pub fn load(&mut self, sources: &[ConfigSource]) -> Result<Settings, ConfigError> {
        let fingerprints = sources
            .iter()
            .map(|source| match source {
                ConfigSource::Yaml(path) => fingerprint(path),
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(settings) = self.effective.get(&fingerprints) {
            return Ok(settings.clone());
        }
        let mut effective = Settings::default();
        let mut live = Vec::new();
        let mut fingerprint_index = 0;
        for source in sources {
            match source {
                ConfigSource::Yaml(path) => {
                    let fingerprint = fingerprints[fingerprint_index].clone();
                    fingerprint_index += 1;
                    live.push(fingerprint.clone());
                    let settings = if let Some(settings) = self.entries.get(&fingerprint) {
                        settings.clone()
                    } else {
                        let settings = load_yaml(path)?;
                        self.entries.insert(fingerprint, settings.clone());
                        settings
                    };
                    effective = merge(effective, settings);
                }
            }
        }
        self.entries
            .retain(|fingerprint, _| live.contains(fingerprint));
        self.effective
            .retain(|key, _| key.iter().all(|fingerprint| live.contains(fingerprint)));
        self.effective.insert(fingerprints, effective.clone());
        Ok(effective)
    }

    pub fn invalidate(&mut self, path: &Path) {
        self.entries
            .retain(|fingerprint, _| fingerprint.path != path);
        self.effective
            .retain(|key, _| key.iter().all(|fingerprint| fingerprint.path != path));
    }
}

fn fingerprint(path: &Path) -> Result<Fingerprint, ConfigError> {
    let metadata = fs::metadata(path).map_err(|source| ConfigError::Read {
        path: path.into(),
        source,
    })?;
    let content = fs::read(path).map_err(|source| ConfigError::Read {
        path: path.into(),
        source,
    })?;
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    Ok(Fingerprint {
        path: path.into(),
        modified: metadata.modified().ok(),
        length: metadata.len(),
        content_hash: hasher.finish(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_zshctl_config_is_used() {
        let env = HashMap::from([("ZSHCTL_CONFIG".into(), "/native.yml".into())]);
        assert_eq!(
            discover(Path::new("/home/me"), Path::new("/work"), &env),
            vec![ConfigSource::Yaml("/native.yml".into())]
        );
    }

    #[test]
    fn malformed_yaml_reports_source_path() {
        let temporary = tempfile::NamedTempFile::new().unwrap();
        fs::write(temporary.path(), "snippets: [").unwrap();
        let error = load_yaml(temporary.path()).unwrap_err().to_string();
        assert!(error.contains(temporary.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn zshctl_home_is_scanned_directly_and_files_are_sorted() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("20-second.yml"), "snippets: []").unwrap();
        fs::write(directory.path().join("10-first.yaml"), "snippets: []").unwrap();
        let env = HashMap::from([(
            "ZSHCTL_HOME".into(),
            directory.path().to_string_lossy().into_owned(),
        )]);
        let sources = discover(Path::new("/home/me"), Path::new("/work"), &env);
        assert_eq!(
            sources,
            vec![
                ConfigSource::Yaml(directory.path().join("10-first.yaml")),
                ConfigSource::Yaml(directory.path().join("20-second.yml")),
            ]
        );
    }

    #[test]
    fn non_yaml_files_and_dynamic_hooks_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("ignored.ts"), "export default {};").unwrap();
        fs::write(directory.path().join("config.yml"), "snippets: []").unwrap();
        let env = HashMap::from([(
            "ZSHCTL_HOME".into(),
            directory.path().to_string_lossy().into_owned(),
        )]);
        assert_eq!(
            discover(Path::new("/home/me"), Path::new("/work"), &env),
            vec![ConfigSource::Yaml(directory.path().join("config.yml"))]
        );

        let invalid = "completions:\n  - name: dynamic\n    patterns: ['^x $']\n    sourceFunction: forbidden\n";
        assert!(serde_yaml::from_str::<Settings>(invalid).is_err());
    }

    #[test]
    fn zshctl_home_replaces_default_directory() {
        let native = tempfile::tempdir().unwrap();
        fs::write(native.path().join("native.yml"), "snippets: []").unwrap();
        let env = HashMap::from([("ZSHCTL_HOME".into(), native.path().to_string_lossy().into())]);
        assert_eq!(
            discover(Path::new("/home/me"), Path::new("/work"), &env),
            vec![ConfigSource::Yaml(native.path().join("native.yml"))]
        );
    }

    #[test]
    fn yaml_fzf_option_map_is_normalized() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yml");
        fs::write(
            &path,
            "completions:\n  - name: test\n    patterns: ['^x $']\n    sourceCommand: echo x\n    options:\n      --multi: true\n      --prompt: 'Pick> '\n",
        )
        .unwrap();
        let settings = load_yaml(&path).unwrap();
        assert!(settings.completions[0].options.contains(&"--multi".into()));
        assert!(
            settings.completions[0]
                .options
                .contains(&"--prompt=Pick> ".into())
        );
    }

    #[test]
    fn history_merge_only_overwrites_explicit_fields() {
        let base: Settings = serde_yaml::from_str(
            "history:\n  defaultScope: directory\n  keymap:\n    deleteSoft: ctrl-x\n    toggleScope: ctrl-t\n",
        )
        .unwrap();
        let overlay: Settings = serde_yaml::from_str(
            "history:\n  redact: ['token=.*']\n  keymap:\n    deleteHard: alt-x\n",
        )
        .unwrap();
        let settings = merge(base, overlay);
        assert_eq!(settings.history.default_scope, "directory");
        assert_eq!(settings.history.redact, vec!["token=.*"]);
        assert_eq!(settings.history.keymap.delete_soft, "ctrl-x");
        assert_eq!(settings.history.keymap.delete_hard, "alt-x");
        assert_eq!(settings.history.keymap.toggle_scope, "ctrl-t");
    }

    #[test]
    fn explicit_default_value_still_overrides_an_earlier_value() {
        let base: Settings = serde_yaml::from_str("history:\n  defaultScope: directory\n").unwrap();
        let overlay: Settings = serde_yaml::from_str("history:\n  defaultScope: global\n").unwrap();
        assert_eq!(merge(base, overlay).history.default_scope, "global");
    }

    #[test]
    fn invalid_history_scope_is_rejected() {
        let error =
            serde_yaml::from_str::<Settings>("history:\n  defaultScope: universe\n").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported history defaultScope")
        );
    }

    #[test]
    fn cache_observes_edits_deletes_and_renames() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("one.yml");
        let second = directory.path().join("two.yml");
        fs::write(&first, "history:\n  defaultScope: session\n").unwrap();
        let mut cache = ConfigCache::default();
        assert_eq!(
            cache
                .load(&[ConfigSource::Yaml(first.clone())])
                .unwrap()
                .history
                .default_scope,
            "session"
        );
        fs::write(&first, "history:\n  defaultScope: global\n").unwrap();
        assert_eq!(
            cache
                .load(&[ConfigSource::Yaml(first.clone())])
                .unwrap()
                .history
                .default_scope,
            "global"
        );
        fs::rename(&first, &second).unwrap();
        assert!(cache.load(&[ConfigSource::Yaml(first)]).is_err());
        assert_eq!(
            cache
                .load(&[ConfigSource::Yaml(second)])
                .unwrap()
                .history
                .default_scope,
            "global"
        );
        assert_eq!(cache.load(&[]).unwrap(), Settings::default());
    }
}
