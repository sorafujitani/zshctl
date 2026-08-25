use regex::Regex;
use rusqlite::{Connection, OptionalExtension, Transaction, params, params_from_iter};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("history database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("history I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid history record at line {line}: {source}")]
    InvalidImport {
        line: usize,
        source: serde_json::Error,
    },
    #[error("invalid history record at line {line}: {message}")]
    InvalidImportLine { line: usize, message: String },
    #[error("unsupported history format {0:?}")]
    UnsupportedFormat(String),
    #[error("invalid redaction pattern {pattern:?}: {source}")]
    InvalidRedaction {
        pattern: String,
        source: regex::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default = "current_timestamp", rename = "ts")]
    pub occurred_at: String,
    pub command: String,
    #[serde(default, rename = "exit")]
    pub exit_status: Option<i32>,
    #[serde(default, rename = "pwd")]
    pub cwd: Option<String>,
    #[serde(default, rename = "session")]
    pub session_id: Option<String>,
    #[serde(default, rename = "host")]
    pub hostname: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default, rename = "repo_root")]
    pub repository_root: Option<String>,
    #[serde(default)]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default, rename = "meta")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    Global,
    Repository(&'a str),
    Directory(&'a str),
    Session(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletedFilter {
    Exclude,
    Include,
    Only,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Ndjson,
    Zsh,
    Bash,
    Fish,
    AtuinJson,
}

impl ExportFormat {
    pub fn parse(value: &str) -> Result<Self, HistoryError> {
        match value {
            "ndjson" => Ok(Self::Ndjson),
            "zsh" => Ok(Self::Zsh),
            "bash" => Ok(Self::Bash),
            "fish" => Ok(Self::Fish),
            "atuin-json" => Ok(Self::AtuinJson),
            _ => Err(HistoryError::UnsupportedFormat(value.into())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupeStrategy {
    Off,
    Strict,
    Loose,
}

impl DedupeStrategy {
    pub fn parse(value: &str) -> Result<Self, HistoryError> {
        match value {
            "off" => Ok(Self::Off),
            "strict" => Ok(Self::Strict),
            "loose" => Ok(Self::Loose),
            _ => Err(HistoryError::InvalidImportLine {
                line: 0,
                message: format!("unknown dedupe strategy {value:?}"),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportSummary {
    pub added: usize,
    pub skipped: usize,
    pub total: usize,
}

pub struct QueryFilter<'a> {
    pub scope: Scope<'a>,
    pub limit: usize,
    pub deleted: DeletedFilter,
    pub term: Option<&'a str>,
    pub after: Option<&'a str>,
    pub before: Option<&'a str>,
    pub exit_status: Option<i32>,
    pub id: Option<&'a str>,
}

pub struct Redactor {
    patterns: Vec<Regex>,
}

impl Redactor {
    pub fn new(patterns: &[String]) -> Result<Self, HistoryError> {
        let patterns = patterns
            .iter()
            .map(|pattern| {
                Regex::new(pattern).map_err(|source| HistoryError::InvalidRedaction {
                    pattern: pattern.clone(),
                    source,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { patterns })
    }

    pub fn with_literals(patterns: &[String], literals: &[String]) -> Result<Self, HistoryError> {
        let mut combined = patterns.to_vec();
        combined.extend(literals.iter().map(|value| regex::escape(value)));
        Self::new(&combined)
    }

    pub fn allows(&self, command: &str) -> bool {
        !self
            .patterns
            .iter()
            .any(|pattern| pattern.is_match(command))
    }

    pub fn apply(&self, command: &str) -> String {
        self.patterns
            .iter()
            .fold(command.to_owned(), |value, pattern| {
                pattern.replace_all(&value, "***").into_owned()
            })
    }
}

pub struct HistoryStore {
    connection: Connection,
}

impl HistoryStore {
    pub fn open(path: &Path) -> Result<Self, HistoryError> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;",
        )?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='history')",
            [],
            |row| row.get(0),
        )?;
        if exists {
            let compatible: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('history') WHERE name='ts')",
                [],
                |row| row.get(0),
            )?;
            if !compatible {
                migrate_legacy(&mut connection)?;
            }
        } else {
            create_schema(&connection)?;
        }
        connection.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_command_ts ON history(TRIM(command), ts DESC)",
            [],
        )?;
        Ok(Self { connection })
    }

    pub fn log(&self, entry: &Entry, redactor: &Redactor) -> Result<bool, HistoryError> {
        let command = redactor.apply(&entry.command);
        let id = entry
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        self.connection.execute(
            "INSERT INTO history(id,ts,command,exit,pwd,session,host,user,shell,repo_root,deleted_at,duration_ms,meta)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![id, entry.occurred_at, command, entry.exit_status, entry.cwd,
                entry.session_id, entry.hostname, entry.user, entry.shell.as_deref().unwrap_or("zsh"), entry.repository_root,
                entry.deleted_at, entry.duration_ms,
                entry.metadata.as_ref().map(serde_json::Value::to_string)],
        )?;
        Ok(true)
    }

    pub fn query(&self, scope: Scope<'_>, limit: usize) -> Result<Vec<Entry>, HistoryError> {
        self.query_filtered(QueryFilter {
            scope,
            limit,
            deleted: DeletedFilter::Exclude,
            term: None,
            after: None,
            before: None,
            exit_status: None,
            id: None,
        })
    }

    pub fn query_filtered(&self, filter: QueryFilter<'_>) -> Result<Vec<Entry>, HistoryError> {
        let mut clauses = Vec::new();
        let mut values = Vec::<rusqlite::types::Value>::new();
        match filter.deleted {
            DeletedFilter::Exclude => clauses.push("deleted_at IS NULL".to_owned()),
            DeletedFilter::Only => clauses.push("deleted_at IS NOT NULL".to_owned()),
            DeletedFilter::Include => {}
        }
        match filter.scope {
            Scope::Global => {}
            Scope::Repository(value) => {
                push_filter(&mut clauses, &mut values, "repo_root = ?", value.to_owned())
            }
            Scope::Directory(value) => {
                push_filter(&mut clauses, &mut values, "pwd = ?", value.to_owned())
            }
            Scope::Session(value) => {
                push_filter(&mut clauses, &mut values, "session = ?", value.to_owned())
            }
        }
        if let Some(value) = filter.term {
            push_filter(
                &mut clauses,
                &mut values,
                "command LIKE ?",
                format!("%{value}%"),
            );
        }
        if let Some(value) = filter.after {
            push_filter(&mut clauses, &mut values, "ts >= ?", value.to_owned());
        }
        if let Some(value) = filter.before {
            push_filter(&mut clauses, &mut values, "ts <= ?", value.to_owned());
        }
        if let Some(value) = filter.exit_status {
            values.push(value.into());
            clauses.push(format!("exit = ?{}", values.len()));
        }
        if let Some(value) = filter.id {
            push_filter(&mut clauses, &mut values, "id = ?", value.to_owned());
        }
        let where_clause = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT id,ts,command,exit,pwd,session,host,user,shell,repo_root,deleted_at,duration_ms,meta
               FROM history {where_clause} ORDER BY ts DESC"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), map_entry)?;
        let limit = filter.limit.min(100_000);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut seen = std::collections::HashSet::new();
        let mut entries = Vec::with_capacity(limit.min(1_000));
        for entry in rows {
            let entry = entry?;
            let trimmed = entry.command.trim();
            let key = if trimmed.is_empty() {
                entry.id.clone().unwrap_or_default()
            } else {
                trimmed.to_owned()
            };
            if seen.insert(key) {
                entries.push(entry);
                if entries.len() == limit {
                    break;
                }
            }
        }
        Ok(entries)
    }

    pub fn hard_delete_id(&self, id: &str) -> Result<bool, HistoryError> {
        Ok(self
            .connection
            .execute("DELETE FROM history WHERE id=?1", [id])?
            == 1)
    }

    pub fn soft_delete_id(&self, id: &str, deleted_at: &str) -> Result<bool, HistoryError> {
        Ok(self.connection.execute(
            "UPDATE history SET deleted_at=?2 WHERE id=?1 AND deleted_at IS NULL",
            params![id, deleted_at],
        )? == 1)
    }

    pub fn get(&self, id: &str) -> Result<Option<Entry>, HistoryError> {
        self.connection.query_row(
            "SELECT id,ts,command,exit,pwd,session,host,user,shell,repo_root,deleted_at,duration_ms,meta FROM history WHERE id=?1",
            [id], map_entry,
        ).optional().map_err(Into::into)
    }

    pub fn export_jsonl(&self, mut output: impl Write) -> Result<usize, HistoryError> {
        let entries = self.query(Scope::Global, 100_000)?;
        for entry in &entries {
            serde_json::to_writer(&mut output, entry).map_err(std::io::Error::other)?;
            output.write_all(b"\n")?;
        }
        Ok(entries.len())
    }

    pub fn export_formatted(
        &self,
        filter: QueryFilter<'_>,
        format: ExportFormat,
        mut output: impl Write,
    ) -> Result<usize, HistoryError> {
        let entries = self.query_filtered(filter)?;
        for entry in &entries {
            match format {
                ExportFormat::Ndjson => {
                    serde_json::to_writer(&mut output, entry).map_err(std::io::Error::other)?
                }
                ExportFormat::Zsh | ExportFormat::Bash => {
                    let epoch = parse_epoch(&entry.occurred_at);
                    let duration = entry.duration_ms.unwrap_or(0).max(0) / 1_000;
                    write!(output, ": {epoch}:{duration};{}", entry.command)?;
                }
                ExportFormat::Fish => {
                    let epoch = parse_epoch(&entry.occurred_at);
                    let escaped = entry
                        .command
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('\n', "\\n")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t");
                    write!(output, "- cmd: \"{escaped}\"\n  when: {epoch}")?;
                }
                ExportFormat::AtuinJson => serde_json::to_writer(
                    &mut output,
                    &serde_json::json!({
                        "id": entry.id,
                        "timestamp": entry.occurred_at,
                        "duration": entry.duration_ms.unwrap_or(0).max(0) * 1_000_000,
                        "exit": entry.exit_status.unwrap_or(0),
                        "command": entry.command,
                        "cwd": entry.cwd,
                    }),
                )
                .map_err(std::io::Error::other)?,
            }
            output.write_all(b"\n")?;
        }
        Ok(entries.len())
    }

    pub fn import_jsonl(
        &mut self,
        input: impl BufRead,
        redactor: &Redactor,
    ) -> Result<usize, HistoryError> {
        let mut entries = Vec::new();
        for (index, line) in input.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str::<Entry>(&line).map_err(|source| {
                HistoryError::InvalidImport {
                    line: index + 1,
                    source,
                }
            })?);
        }
        let transaction = self.connection.transaction()?;
        let count = import_transaction(&transaction, &entries, redactor)?;
        transaction.commit()?;
        Ok(count)
    }

    pub fn import_formatted(
        &mut self,
        format: ExportFormat,
        input: impl BufRead,
        dedupe: DedupeStrategy,
        dry_run: bool,
        redactor: &Redactor,
    ) -> Result<ImportSummary, HistoryError> {
        let entries = parse_formatted(format, input)?;
        let total = entries.len();
        if dry_run {
            return Ok(ImportSummary {
                added: 0,
                skipped: total,
                total,
            });
        }
        let transaction = self.connection.transaction()?;
        let (added, skipped) =
            import_transaction_deduped(&transaction, &entries, redactor, dedupe)?;
        transaction.commit()?;
        Ok(ImportSummary {
            added,
            skipped,
            total,
        })
    }

    pub fn integrity_check(&self) -> Result<String, HistoryError> {
        Ok(self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
    }
}

fn push_filter(
    clauses: &mut Vec<String>,
    values: &mut Vec<rusqlite::types::Value>,
    clause: &str,
    value: String,
) {
    values.push(value.into());
    clauses.push(clause.replace('?', &format!("?{}", values.len())));
}

fn create_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "CREATE TABLE history(id TEXT PRIMARY KEY,ts TEXT NOT NULL,command TEXT NOT NULL,exit INTEGER,
          pwd TEXT,session TEXT,host TEXT,user TEXT,shell TEXT NOT NULL,repo_root TEXT,deleted_at TEXT,
          duration_ms INTEGER,meta TEXT);
         CREATE INDEX idx_history_ts ON history(ts DESC);
         CREATE INDEX idx_history_repo ON history(repo_root);
         CREATE INDEX idx_history_pwd ON history(pwd);
         CREATE INDEX idx_history_session ON history(session);
         CREATE INDEX idx_history_deleted ON history(deleted_at);",
    )
}

fn migrate_legacy(connection: &mut Connection) -> Result<(), rusqlite::Error> {
    let transaction = connection.transaction()?;
    transaction.execute("ALTER TABLE history RENAME TO history_legacy", [])?;
    transaction.execute_batch(
        "DROP INDEX IF EXISTS history_occurred_at;
         DROP INDEX IF EXISTS history_cwd;
         DROP INDEX IF EXISTS history_session;",
    )?;
    create_schema(&transaction)?;
    transaction.execute_batch(
        "INSERT INTO history(id,ts,command,exit,pwd,session,host,shell,deleted_at)
         SELECT CAST(id AS TEXT),printf('%020d',occurred_at),command,exit_status,cwd,session_id,hostname,'zsh',
           CASE WHEN deleted_at IS NULL THEN NULL ELSE printf('%020d',deleted_at) END FROM history_legacy;
         DROP TABLE history_legacy;",
    )?;
    transaction.commit()
}

fn map_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: row.get(0)?,
        occurred_at: row.get(1)?,
        command: row.get(2)?,
        exit_status: row.get(3)?,
        cwd: row.get(4)?,
        session_id: row.get(5)?,
        hostname: row.get(6)?,
        user: row.get(7)?,
        shell: row.get(8)?,
        repository_root: row.get(9)?,
        deleted_at: row.get(10)?,
        duration_ms: row.get(11)?,
        metadata: row
            .get::<_, Option<String>>(12)?
            .and_then(|value| serde_json::from_str(&value).ok()),
    })
}

fn import_transaction(
    transaction: &Transaction<'_>,
    entries: &[Entry],
    redactor: &Redactor,
) -> Result<usize, HistoryError> {
    let mut inserted = 0;
    for entry in entries {
        let command = redactor.apply(&entry.command);
        let id = entry
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        transaction.execute(
            "INSERT INTO history(id,ts,command,exit,pwd,session,host,user,shell,repo_root,deleted_at,duration_ms,meta)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![id,entry.occurred_at,command,entry.exit_status,entry.cwd,entry.session_id,
              entry.hostname,entry.user,entry.shell.as_deref().unwrap_or("zsh"),entry.repository_root,entry.deleted_at,entry.duration_ms,
              entry.metadata.as_ref().map(serde_json::Value::to_string)],
        )?;
        inserted += 1;
    }
    Ok(inserted)
}

fn import_transaction_deduped(
    transaction: &Transaction<'_>,
    entries: &[Entry],
    redactor: &Redactor,
    dedupe: DedupeStrategy,
) -> Result<(usize, usize), HistoryError> {
    let mut added = 0;
    let mut skipped = 0;
    for entry in entries {
        let id = entry
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if dedupe != DedupeStrategy::Off
            && transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM history WHERE id=?1)",
                [&id],
                |row| row.get::<_, bool>(0),
            )?
        {
            skipped += 1;
            continue;
        }
        let command = redactor.apply(&entry.command);
        transaction.execute(
            "INSERT INTO history(id,ts,command,exit,pwd,session,host,user,shell,repo_root,deleted_at,duration_ms,meta)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![id,entry.occurred_at,command,entry.exit_status,entry.cwd,entry.session_id,
              entry.hostname,entry.user,entry.shell.as_deref().unwrap_or("zsh"),entry.repository_root,
              entry.deleted_at,entry.duration_ms,entry.metadata.as_ref().map(serde_json::Value::to_string)],
        )?;
        added += 1;
    }
    Ok((added, skipped))
}

fn parse_formatted(format: ExportFormat, input: impl BufRead) -> Result<Vec<Entry>, HistoryError> {
    let lines = input.lines().collect::<Result<Vec<_>, _>>()?;
    match format {
        ExportFormat::Ndjson => lines
            .into_iter()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                serde_json::from_str(&line).map_err(|source| HistoryError::InvalidImport {
                    line: index + 1,
                    source,
                })
            })
            .collect(),
        ExportFormat::Zsh | ExportFormat::Bash => {
            let pattern = Regex::new(r"^:\s*(\d+):(\d+);(.*)$").expect("constant regex");
            lines
                .into_iter()
                .enumerate()
                .filter(|(_, line)| !line.trim().is_empty())
                .map(|(index, line)| {
                    if let Some(captures) = pattern.captures(&line) {
                        let epoch = captures[1].parse::<i64>().map_err(|error| {
                            HistoryError::InvalidImportLine {
                                line: index + 1,
                                message: error.to_string(),
                            }
                        })?;
                        let duration = captures[2].parse::<i64>().unwrap_or(0).max(0) * 1_000;
                        Ok(import_entry(
                            captures[3].to_owned(),
                            timestamp_from_epoch(epoch),
                            Some(duration),
                        ))
                    } else {
                        Ok(import_entry(
                            line.trim().to_owned(),
                            "1970-01-01T00:00:00Z".into(),
                            None,
                        ))
                    }
                })
                .collect()
        }
        ExportFormat::AtuinJson => lines
            .into_iter()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
            .map(|(index, line)| {
                let value: serde_json::Value =
                    serde_json::from_str(&line).map_err(|source| HistoryError::InvalidImport {
                        line: index + 1,
                        source,
                    })?;
                Ok(Entry {
                    id: value
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(Into::into),
                    occurred_at: value
                        .get("timestamp")
                        .and_then(serde_json::Value::as_str)
                        .map(Into::into)
                        .unwrap_or_else(current_timestamp),
                    command: value
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&line)
                        .into(),
                    exit_status: value
                        .get("exit")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                    cwd: value
                        .get("cwd")
                        .and_then(serde_json::Value::as_str)
                        .map(Into::into),
                    session_id: None,
                    hostname: None,
                    user: None,
                    shell: value
                        .get("shell")
                        .and_then(serde_json::Value::as_str)
                        .map(Into::into)
                        .or_else(|| Some("zsh".into())),
                    repository_root: None,
                    deleted_at: None,
                    duration_ms: value
                        .get("duration")
                        .and_then(serde_json::Value::as_i64)
                        .map(|value| value / 1_000_000),
                    metadata: None,
                })
            })
            .collect(),
        ExportFormat::Fish => Err(HistoryError::UnsupportedFormat("fish import".into())),
    }
}

fn import_entry(command: String, occurred_at: String, duration_ms: Option<i64>) -> Entry {
    Entry {
        id: None,
        occurred_at,
        command,
        exit_status: None,
        cwd: None,
        session_id: None,
        hostname: None,
        user: None,
        shell: Some("zsh".into()),
        repository_root: None,
        deleted_at: None,
        duration_ms,
        metadata: None,
    }
}

fn parse_epoch(timestamp: &str) -> i64 {
    time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
        .map(|value| value.unix_timestamp())
        .unwrap_or(0)
}

fn timestamp_from_epoch(epoch: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(epoch)
        .ok()
        .and_then(|value| {
            value
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(current_timestamp)
}

fn current_timestamp() -> String {
    let now = time::OffsetDateTime::now_utc();
    let millis = now.nanosecond() / 1_000_000;
    now.replace_nanosecond(0)
        .ok()
        .and_then(|value| {
            value
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map(|value| format!("{}.{millis:03}Z", value.trim_end_matches('Z')))
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn entry(command: &str, ts: &str, session: &str) -> Entry {
        Entry {
            id: None,
            occurred_at: ts.into(),
            command: command.into(),
            exit_status: Some(0),
            cwd: Some("/tmp".into()),
            session_id: Some(session.into()),
            hostname: None,
            user: None,
            shell: Some("zsh".into()),
            repository_root: None,
            deleted_at: None,
            duration_ms: None,
            metadata: None,
        }
    }

    #[test]
    fn records_queries_and_deduplicates_newest() {
        let store = HistoryStore::open(Path::new(":memory:")).unwrap();
        let redactor = Redactor::new(&[]).unwrap();
        store.log(&entry("same", "0001", "a"), &redactor).unwrap();
        store.log(&entry("same", "0002", "b"), &redactor).unwrap();
        assert_eq!(store.query(Scope::Global, 10).unwrap().len(), 1);
        assert_eq!(store.query(Scope::Session("a"), 10).unwrap().len(), 1);
    }

    #[test]
    fn redaction_and_import_are_transactional() {
        let mut store = HistoryStore::open(Path::new(":memory:")).unwrap();
        let redactor = Redactor::new(&["secret".into()]).unwrap();
        assert!(store.log(&entry("secret", "1", "a"), &redactor).unwrap());
        assert_eq!(store.query(Scope::Global, 10).unwrap()[0].command, "***");
        assert!(
            store
                .import_jsonl(Cursor::new("{\"command\":\"ok\"}\nnot-json\n"), &redactor)
                .is_err()
        );
        assert_eq!(store.query(Scope::Global, 10).unwrap().len(), 1);
    }

    #[test]
    fn deletion_and_repository_scope_are_explicit() {
        let store = HistoryStore::open(Path::new(":memory:")).unwrap();
        let redactor = Redactor::new(&[]).unwrap();
        let mut item = entry("inside", "1", "a");
        item.repository_root = Some("/repo".into());
        store.log(&item, &redactor).unwrap();
        let id = store.query(Scope::Repository("/repo"), 10).unwrap()[0]
            .id
            .clone()
            .unwrap();
        assert!(store.soft_delete_id(&id, "2").unwrap());
        assert!(store.query(Scope::Global, 10).unwrap().is_empty());
        assert!(store.hard_delete_id(&id).unwrap());
    }

    #[test]
    fn ndjson_round_trip_preserves_public_fields() {
        let first = HistoryStore::open(Path::new(":memory:")).unwrap();
        let redactor = Redactor::new(&[]).unwrap();
        let mut item = entry("echo", "2026-01-01T00:00:00Z", "s");
        item.metadata = Some(serde_json::json!({"key":"value"}));
        first.log(&item, &redactor).unwrap();
        let mut bytes = Vec::new();
        first.export_jsonl(&mut bytes).unwrap();
        let mut second = HistoryStore::open(Path::new(":memory:")).unwrap();
        second.import_jsonl(Cursor::new(bytes), &redactor).unwrap();
        assert_eq!(
            second.query(Scope::Global, 1).unwrap()[0].metadata,
            item.metadata
        );
    }

    #[test]
    fn formatted_exports_and_imports_preserve_supported_fields() {
        let first = HistoryStore::open(Path::new(":memory:")).unwrap();
        let redactor = Redactor::new(&[]).unwrap();
        let mut item = entry("echo hello", "2026-01-01T00:00:00Z", "s");
        item.duration_ms = Some(2_000);
        first.log(&item, &redactor).unwrap();
        let filter = || QueryFilter {
            scope: Scope::Global,
            limit: 10,
            deleted: DeletedFilter::Exclude,
            term: None,
            after: None,
            before: None,
            exit_status: None,
            id: None,
        };
        let mut zsh = Vec::new();
        first
            .export_formatted(filter(), ExportFormat::Zsh, &mut zsh)
            .unwrap();
        assert_eq!(
            String::from_utf8(zsh.clone()).unwrap(),
            ": 1767225600:2;echo hello\n"
        );
        let mut second = HistoryStore::open(Path::new(":memory:")).unwrap();
        let summary = second
            .import_formatted(
                ExportFormat::Zsh,
                Cursor::new(zsh),
                DedupeStrategy::Off,
                false,
                &redactor,
            )
            .unwrap();
        assert_eq!(
            summary,
            ImportSummary {
                added: 1,
                skipped: 0,
                total: 1
            }
        );
        assert_eq!(
            second.query(Scope::Global, 10).unwrap()[0].command,
            "echo hello"
        );

        let mut atuin = Vec::new();
        first
            .export_formatted(filter(), ExportFormat::AtuinJson, &mut atuin)
            .unwrap();
        let mut third = HistoryStore::open(Path::new(":memory:")).unwrap();
        third
            .import_formatted(
                ExportFormat::AtuinJson,
                Cursor::new(atuin.clone()),
                DedupeStrategy::Strict,
                false,
                &redactor,
            )
            .unwrap();
        let repeated = third
            .import_formatted(
                ExportFormat::AtuinJson,
                Cursor::new(atuin),
                DedupeStrategy::Strict,
                false,
                &redactor,
            )
            .unwrap();
        assert_eq!(repeated.skipped, 1);
    }

    #[test]
    fn legacy_schema_migration_is_atomic_and_queryable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(
            "CREATE TABLE history(id INTEGER PRIMARY KEY AUTOINCREMENT,command TEXT NOT NULL,cwd TEXT NOT NULL,session_id TEXT NOT NULL,occurred_at INTEGER NOT NULL,exit_status INTEGER,hostname TEXT,deleted_at INTEGER);
             CREATE INDEX history_occurred_at ON history(occurred_at DESC);
             CREATE INDEX history_cwd ON history(cwd);
             CREATE INDEX history_session ON history(session_id);
             INSERT INTO history(command,cwd,session_id,occurred_at,exit_status) VALUES('legacy','/tmp','old',42,0);",
        ).unwrap();
        drop(connection);
        let store = HistoryStore::open(&path).unwrap();
        let rows = store.query(Scope::Global, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].command, "legacy");
        assert_eq!(store.integrity_check().unwrap(), "ok");
    }
}
