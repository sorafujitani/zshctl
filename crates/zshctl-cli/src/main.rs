use anyhow::{Context, bail};
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;
use zshctl_daemon::{DaemonStatus, RuntimePaths};
use zshctl_protocol::{Operation, Request, Response, ResponseResult};

#[derive(Debug, Parser)]
#[command(
    name = "zshctl",
    version,
    about = "Fast, stateful shell workflows for Zsh"
)]
struct Cli {
    /// Run a line-oriented zshctl feature mode.
    #[arg(long)]
    mode: Option<String>,
    #[arg(long = "input.lbuffer", default_value = "")]
    input_lbuffer: String,
    #[arg(long = "input.rbuffer", default_value = "")]
    input_rbuffer: String,
    #[arg(long = "input.snippet", default_value = "")]
    input_snippet: String,
    #[arg(long = "input.snippet-id", default_value = "")]
    input_snippet_id: String,
    #[arg(long = "input.context-lbuffer", default_value = "")]
    input_context_lbuffer: String,
    #[arg(long = "input.context-rbuffer", default_value = "")]
    input_context_rbuffer: String,
    #[arg(long = "input.template", default_value = "")]
    input_template: String,
    #[arg(long = "input.dir")]
    input_dir: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage the shared per-user daemon.
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Print protocol capabilities as JSON.
    Capabilities,
    /// Send a feature request. Intended for thin shell adapters and diagnostics.
    Request {
        name: String,
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    /// Query and update Smart History.
    History {
        #[command(subcommand)]
        command: Box<HistoryCommand>,
    },
}

#[derive(Debug, Subcommand)]
enum ServerCommand {
    /// Run the daemon in the foreground.
    Run,
    #[command(hide = true)]
    RunSpawned,
    /// Start the daemon if it is not healthy.
    Start,
    /// Stop the daemon.
    Stop,
    /// Stop and start the daemon.
    Restart,
    /// Print daemon state as JSON.
    Status,
}

#[derive(Debug, Subcommand)]
enum HistoryCommand {
    /// Record one command unless redaction rules exclude it.
    Log(Box<HistoryLogArgs>),
    /// Query entries as JSON, newest first.
    Query(Box<HistoryQueryArgs>),
    /// Delete exactly one history row by ID.
    Delete {
        id: Option<String>,
        #[arg(long = "id")]
        id_flag: Option<String>,
        /// Also prune the command from HISTFILE; the database row remains soft-deleted.
        #[arg(long)]
        hard: bool,
    },
    /// Export all defined fields as JSON Lines.
    Export(Box<HistoryExportArgs>),
    /// Transactionally import JSON Lines from stdin.
    Import(Box<HistoryImportArgs>),
    /// Run SQLite's integrity check.
    Integrity,
    /// Print NUL-delimited fzf settings for the Smart History widget.
    FzfConfig,
}

#[derive(Debug, Args)]
struct HistoryLogArgs {
    command: Option<String>,
    #[arg(long, visible_alias = "command")]
    cmd: Option<String>,
    #[arg(long, visible_alias = "pwd")]
    cwd: Option<String>,
    #[arg(long, visible_alias = "exit")]
    exit_status: Option<i32>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long)]
    shell: Option<String>,
    #[arg(long)]
    ts: Option<String>,
    #[arg(long, visible_alias = "durationMs")]
    duration_ms: Option<i64>,
    #[arg(long, visible_alias = "repoRoot")]
    repo_root: Option<String>,
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    meta: Option<String>,
    #[arg(long)]
    redact: Vec<String>,
}

#[derive(Debug, Args)]
struct HistoryQueryArgs {
    #[arg(long)]
    scope: Option<String>,
    #[arg(long)]
    scope_value: Option<String>,
    #[arg(long)]
    cwd: Option<String>,
    #[arg(long)]
    directory: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    term: Option<String>,
    #[arg(long)]
    after: Option<String>,
    #[arg(long)]
    before: Option<String>,
    #[arg(long = "exit")]
    exit_status: Option<i32>,
    #[arg(long)]
    id: Option<String>,
    #[arg(long)]
    format: Option<String>,
    #[arg(long)]
    deleted: Option<String>,
    #[arg(long, visible_alias = "repoRoot")]
    repo_root: Option<String>,
    #[arg(long)]
    toggle_scope: bool,
    #[arg(long, default_value_t = 1_000)]
    limit: usize,
    #[arg(long)]
    commands: bool,
}

#[derive(Debug, Args)]
struct HistoryExportArgs {
    #[arg(long, default_value = "ndjson")]
    format: String,
    #[arg(long = "out")]
    output_path: Option<String>,
    #[arg(long)]
    scope: Option<String>,
    #[arg(long)]
    directory: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long, visible_alias = "repoRoot")]
    repo_root: Option<String>,
    #[arg(long, default_value_t = 2_000)]
    limit: usize,
    #[arg(long)]
    term: Option<String>,
    #[arg(long)]
    after: Option<String>,
    #[arg(long)]
    before: Option<String>,
    #[arg(long = "exit")]
    exit_status: Option<i32>,
    #[arg(long)]
    deleted: Option<String>,
    #[arg(long)]
    redact: Vec<String>,
}

#[derive(Debug, Args)]
struct HistoryImportArgs {
    #[arg(long, default_value = "ndjson")]
    format: String,
    #[arg(long = "in")]
    input_path: Option<String>,
    #[arg(long, default_value = "off")]
    dedupe: String,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    redact: Vec<String>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = execute().await {
        eprintln!("zshctl: {error:#}");
        std::process::exit(1);
    }
}

async fn execute() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init()
        .ok();
    let cli = Cli::parse();
    let paths = RuntimePaths::resolve()?;
    if let Some(mode) = &cli.mode {
        return execute_mode(mode, &cli, &paths).await;
    }
    let Some(command) = cli.command else {
        bail!("a command or --mode is required");
    };
    match command {
        Command::Server { command } => match command {
            ServerCommand::Run => zshctl_daemon::run(paths).await?,
            ServerCommand::RunSpawned => zshctl_daemon::run_spawned(paths).await?,
            ServerCommand::Start => {
                let executable = std::env::current_exe().context("resolve current executable")?;
                let health = zshctl_daemon::start(&paths, &executable).await?;
                println!("{}", serde_json::to_string(&health)?);
            }
            ServerCommand::Stop => zshctl_daemon::stop(&paths).await?,
            ServerCommand::Restart => {
                zshctl_daemon::stop(&paths).await?;
                let executable = std::env::current_exe().context("resolve current executable")?;
                let health = zshctl_daemon::start(&paths, &executable).await?;
                println!("{}", serde_json::to_string(&health)?);
            }
            ServerCommand::Status => {
                let status = zshctl_daemon::status(&paths).await?;
                println!("{}", serde_json::to_string(&status)?);
                if !matches!(status, DaemonStatus::Healthy { .. }) {
                    std::process::exit(3);
                }
            }
        },
        Command::Capabilities => {
            let response = send(&paths, Operation::Capabilities).await?;
            println!("{}", serde_json::to_string(&response)?);
        }
        Command::Request { name, payload } => {
            let payload = serde_json::from_str(&payload).context("parse --payload JSON")?;
            let response = send(&paths, Operation::Feature { name, payload }).await?;
            println!("{}", serde_json::to_string(&response)?);
            if matches!(response.result, ResponseResult::Error { .. }) {
                std::process::exit(1);
            }
        }
        Command::History { command } => execute_history(*command, &paths).await?,
    }
    Ok(())
}

async fn execute_history(command: HistoryCommand, paths: &RuntimePaths) -> anyhow::Result<()> {
    let mut output_commands = false;
    let mut export_path = None;
    let mut query_format = None;
    let mut query_id = None;
    let mut query_toggle = false;
    let mut query_scope = None;
    let (name, payload) = match command {
        HistoryCommand::Log(args) => {
            let HistoryLogArgs {
                command,
                cmd,
                cwd,
                exit_status,
                session,
                host,
                user,
                shell,
                ts,
                duration_ms,
                repo_root,
                id,
                meta,
                redact,
            } = *args;
            (
                "history.log",
                json!({
                "command": command.or(cmd).unwrap_or_default(), "pwd": cwd,
                "exit": exit_status, "session": session, "host": host,
                "user": user, "shell": shell, "ts": ts, "durationMs": duration_ms,
                    "repoRoot": repo_root, "id": id, "meta": meta, "redact": redact,
                }),
            )
        }
        HistoryCommand::Query(args) => {
            let HistoryQueryArgs {
                scope,
                scope_value,
                cwd,
                directory,
                session,
                term,
                after,
                before,
                exit_status,
                id,
                format,
                deleted,
                repo_root,
                toggle_scope,
                limit,
                commands,
            } = *args;
            output_commands = commands;
            query_format = format.clone().or_else(|| Some("lines".into()));
            query_id = id.clone();
            query_toggle = toggle_scope;
            query_scope = scope.clone();
            (
                "history.query",
                json!({
                    "scope": scope, "scopeValue": scope_value, "cwd": cwd,
                    "directory": directory, "session": session, "term": term,
                    "after": after, "before": before, "exit": exit_status,
                    "id": id, "format": format, "deleted": deleted,
                    "repoRoot": repo_root, "toggleScope": toggle_scope, "limit": limit
                }),
            )
        }
        HistoryCommand::Delete { id, id_flag, hard } => (
            "history.delete",
            json!({ "id": id_flag.or(id).unwrap_or_default(), "hard": hard }),
        ),
        HistoryCommand::Export(args) => {
            export_path = args.output_path.clone();
            (
                "history.export",
                json!({
                    "format": args.format, "scope": args.scope, "directory": args.directory,
                    "session": args.session, "repoRoot": args.repo_root, "limit": args.limit,
                    "term": args.term, "after": args.after, "before": args.before,
                    "exit": args.exit_status, "deleted": args.deleted, "redact": args.redact,
                }),
            )
        }
        HistoryCommand::Import(args) => {
            let mut data = String::new();
            if let Some(path) = &args.input_path {
                data = std::fs::read_to_string(path)
                    .with_context(|| format!("read history import {path}"))?;
            } else {
                std::io::stdin().read_to_string(&mut data)?;
            }
            (
                "history.import",
                json!({ "data": data, "format": args.format,
                "dedupe": args.dedupe, "dryRun": args.dry_run, "redact": args.redact }),
            )
        }
        HistoryCommand::Integrity => ("history.integrity", json!({})),
        HistoryCommand::FzfConfig => ("history.fzf-config", json!({})),
    };
    let response = send(
        paths,
        Operation::Feature {
            name: name.into(),
            payload,
        },
    )
    .await?;
    match response.result {
        ResponseResult::Success { value } if name == "history.export" => {
            let data = value["data"].as_str().unwrap_or_default();
            if let Some(path) = export_path {
                std::fs::write(&path, data)
                    .with_context(|| format!("write history export {path}"))?;
                println!("success");
            } else {
                print!("{data}");
            }
        }
        ResponseResult::Success { .. } if name == "history.log" || name == "history.delete" => {
            println!("success");
        }
        ResponseResult::Success { value } if name == "history.import" => {
            println!("success");
            println!(
                "added={} skipped={} total={}",
                value["added"].as_u64().unwrap_or_default(),
                value["skipped"].as_u64().unwrap_or_default(),
                value["total"].as_u64().unwrap_or_default()
            );
        }
        ResponseResult::Success { value } if name == "history.query" && output_commands => {
            let entries = history_response_entries(&value);
            for entry in &entries {
                if let Some(command) = entry["command"].as_str() {
                    println!("{command}");
                }
            }
        }
        ResponseResult::Success { value } if name == "history.query" && query_toggle => {
            println!("success");
            println!("{}", value["scope"].as_str().unwrap_or("global"));
        }
        ResponseResult::Success { value }
            if name == "history.query" && query_format.as_deref() == Some("json") =>
        {
            println!("success");
            let body = if query_scope.as_deref() == Some("all") {
                value
            } else if query_id.is_some() {
                value
                    .as_array()
                    .and_then(|items| items.first())
                    .cloned()
                    .unwrap_or(Value::Null)
            } else {
                json!({ "scope": query_scope.as_deref().unwrap_or("global"), "items": value })
            };
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        ResponseResult::Success { value }
            if name == "history.query"
                && matches!(query_format.as_deref(), Some("lines" | "smart-lines")) =>
        {
            println!("success");
            if let Some(scopes) = value.get("scopes").and_then(Value::as_object) {
                for (scope, entries) in scopes {
                    for entry in entries.as_array().into_iter().flatten() {
                        let mut fields = smart_history_line(entry)
                            .split('\u{00a0}')
                            .map(ToOwned::to_owned)
                            .collect::<Vec<_>>();
                        if fields.len() > 1 {
                            fields[1] = format!("[{scope}] {}", fields[1]);
                        }
                        println!("{}", fields.join("\u{00a0}"));
                    }
                }
            } else {
                for entry in value.as_array().into_iter().flatten() {
                    println!("{}", smart_history_line(entry));
                }
            }
        }
        ResponseResult::Success { value } if name == "history.fzf-config" => {
            println!(
                "success\0{}\0{}\0{}",
                value["command"].as_str().unwrap_or_default(),
                value["options"].as_str().unwrap_or_default(),
                value["togglePreview"].as_str().unwrap_or("?")
            );
        }
        ResponseResult::Success { value } => println!("{}", serde_json::to_string(&value)?),
        ResponseResult::Error { error } => {
            bail!("{}: {}", serde_json::to_string(&error.code)?, error.message)
        }
    }
    Ok(())
}

async fn send(paths: &RuntimePaths, operation: Operation) -> anyhow::Result<Response> {
    let request_timeout = client_timeout();
    let mut request = Request::new(
        operation,
        std::env::var("ZSHCTL_SESSION_ID").unwrap_or_else(|_| "cli".into()),
        std::env::current_dir()?.to_string_lossy(),
    );
    request.environment = client_environment();
    if daemon_disabled() {
        return Ok(zshctl_daemon::request_direct(paths, request).await);
    }
    match zshctl_daemon::request_once(paths, &request, request_timeout).await {
        Ok(response) => Ok(response),
        Err(first_error) => {
            let executable = std::env::current_exe().context("resolve current executable")?;
            let retry = async {
                zshctl_daemon::start(paths, &executable)
                    .await
                    .with_context(|| {
                        format!("daemon unavailable after initial error: {first_error}")
                    })?;
                zshctl_daemon::request_once(paths, &request, request_timeout)
                    .await
                    .context("daemon request failed after one safe start retry")
            }
            .await;
            match retry {
                Ok(response) => Ok(response),
                Err(error) if direct_fallback_enabled() => {
                    eprintln!(
                        "zshctl: daemon unavailable; using explicit direct fallback: {error:#}"
                    );
                    Ok(zshctl_daemon::request_direct(paths, request).await)
                }
                Err(error) => Err(error),
            }
        }
    }
}

fn client_timeout() -> Duration {
    std::env::var("ZSHCTL_CLIENT_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| Duration::from_secs_f64(value.clamp(0.01, 30.0)))
        .unwrap_or_else(|| Duration::from_secs(2))
}

fn direct_fallback_enabled() -> bool {
    std::env::var("ZSHCTL_DIRECT_FALLBACK")
        .ok()
        .is_some_and(|value| !matches!(value.trim(), "" | "0" | "false"))
}

fn daemon_disabled() -> bool {
    std::env::var("ZSHCTL_DISABLE_DAEMON")
        .is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false"))
}

fn client_environment() -> std::collections::BTreeMap<String, String> {
    std::env::vars()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "PWD" | "HOME" | "PATH" | "SHELL" | "USER" | "TERM" | "HISTFILE" | "GHQ_ROOT"
            ) || key.starts_with("ZSHCTL_")
        })
        .collect()
}

fn smart_history_line(entry: &Value) -> String {
    const DELIMITER: char = '\u{00a0}';
    let clean = |value: &str| value.replace('\t', "    ").replace(DELIMITER, " ");
    let command = clean(entry["command"].as_str().unwrap_or_default());
    let raw_command = entry["command"]
        .as_str()
        .unwrap_or_default()
        .replace('\t', "\u{001f}")
        .replace(DELIMITER, " ");
    let directory = clean(entry["pwd"].as_str().unwrap_or_default());
    let exit = match entry["exit"].as_i64() {
        Some(0) => "\u{1b}[32m✔\u{1b}[0m",
        Some(_) => "\u{1b}[31m✘\u{1b}[0m",
        None => "\u{1b}[2m·\u{1b}[0m",
    };
    let duration = entry["duration_ms"]
        .as_i64()
        .filter(|value| *value > 0)
        .map(|value| {
            if value < 1_000 {
                format!("{value}ms")
            } else {
                format!("{:.1}s", value as f64 / 1_000.0)
            }
        })
        .unwrap_or_default();
    [
        entry["id"].as_str().unwrap_or_default().to_owned(),
        format!(
            "\u{1b}[2m{}\u{1b}[0m",
            history_time_ago(entry["ts"].as_str().unwrap_or_default())
        ),
        exit.into(),
        format!("  {command}"),
        (!directory.is_empty())
            .then(|| format!("  \u{1b}[2m{directory}\u{1b}[0m"))
            .unwrap_or_default(),
        (!duration.is_empty())
            .then(|| format!("  {duration}"))
            .unwrap_or_default(),
        raw_command,
    ]
    .join(&DELIMITER.to_string())
}

fn history_response_entries(value: &Value) -> Vec<&Value> {
    if let Some(entries) = value.as_array() {
        return entries.iter().collect();
    }
    value
        .get("scopes")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|scopes| scopes.values())
        .flat_map(|entries| entries.as_array().into_iter().flatten())
        .collect()
}

fn history_time_ago(timestamp: &str) -> String {
    let Ok(then) =
        time::OffsetDateTime::parse(timestamp, &time::format_description::well_known::Rfc3339)
    else {
        return "0s".into();
    };
    let seconds = (time::OffsetDateTime::now_utc() - then)
        .whole_seconds()
        .max(0);
    match seconds {
        0..=1 => "1s".into(),
        2..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        86_400..=604_799 => format!("{}d", seconds / 86_400),
        604_800..=2_591_999 => format!("{}w", seconds / 604_800),
        2_592_000..=31_535_999 => format!("{}mo", seconds / 2_592_000),
        _ => format!("{}y", seconds / 31_536_000),
    }
}

async fn execute_mode(mode: &str, cli: &Cli, paths: &RuntimePaths) -> anyhow::Result<()> {
    if mode == "chdir" {
        let directory = cli
            .input_dir
            .as_deref()
            .context("--input.dir is required")?;
        std::env::set_current_dir(directory)
            .with_context(|| format!("change directory to {directory}"))?;
        return Ok(());
    }
    let (feature, payload) = match mode {
        "auto-snippet" => (
            "snippet.auto",
            json!({ "lbuffer": cli.input_lbuffer, "rbuffer": cli.input_rbuffer }),
        ),
        "insert-snippet" => (
            "snippet.insert",
            json!({
                "name": cli.input_snippet,
                "lbuffer": cli.input_lbuffer,
                "rbuffer": cli.input_rbuffer,
            }),
        ),
        "insert-snippet-id" => (
            "snippet.insert-id",
            json!({
                "id": cli.input_snippet_id,
                "lbuffer": cli.input_lbuffer,
                "rbuffer": cli.input_rbuffer,
                "context_lbuffer": cli.input_context_lbuffer,
                "context_rbuffer": cli.input_context_rbuffer,
            }),
        ),
        "preprompt" => (
            "snippet.preprompt",
            json!({ "template": cli.input_template }),
        ),
        "preprompt-snippet" => (
            "snippet.preprompt-named",
            json!({ "name": cli.input_snippet }),
        ),
        "next-placeholder" => (
            "snippet.next-placeholder",
            json!({ "buffer": format!("{}{}", cli.input_lbuffer, cli.input_rbuffer) }),
        ),
        "completion" => (
            "completion.source",
            json!({
                "buffer": cli.input_lbuffer,
                "lbuffer": cli.input_lbuffer,
                "rbuffer": cli.input_rbuffer
            }),
        ),
        "snippet-list" => ("snippet.list", json!({})),
        "snippet-candidates" => (
            "snippet.candidates",
            json!({
                "lbuffer": cli.input_lbuffer,
                "rbuffer": cli.input_rbuffer,
            }),
        ),
        "ghq-list" => ("ghq.list", json!({})),
        "pid" => {
            let response = send(paths, Operation::Health).await?;
            if let ResponseResult::Success { value } = response.result {
                println!("{}", value["pid"].as_u64().unwrap_or_default());
                return Ok(());
            }
            bail!("daemon health request failed")
        }
        _ => {
            println!("failure");
            println!("{mode} mode is not exist");
            return Ok(());
        }
    };
    let response = send(
        paths,
        Operation::Feature {
            name: feature.into(),
            payload,
        },
    )
    .await?;
    print_mode_response(mode, response)
}

fn print_mode_response(mode: &str, response: Response) -> anyhow::Result<()> {
    let value = match response.result {
        ResponseResult::Success { value } => value,
        ResponseResult::Error { error } => {
            println!("failure");
            eprintln!("zshctl: {:?}: {}", error.code, error.message);
            return Ok(());
        }
    };
    if mode == "completion" {
        if value["status"] != "success" {
            println!("failure");
            return Ok(());
        }
        let command = value["sourceCommand"]
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or(candidates_command(&value["candidates"])?);
        println!("success");
        println!("{command}");
        let mut options = value["options"].as_array().cloned().unwrap_or_default();
        if let Some(preview) = value["preview"].as_str() {
            options.insert(
                2.min(options.len()),
                Value::String(format!("--preview=\"{}\"", preview.replace('"', "\\\""))),
            );
        }
        if let Some(rendered) = value["optionsRendered"].as_str() {
            println!("{rendered}");
        } else {
            println!("{}", render_options(&options));
        }
        println!("{}", value["callback"].as_str().unwrap_or_default());
        println!(
            "{}",
            if value["callbackZero"].as_bool().unwrap_or(false) {
                "zero"
            } else {
                ""
            }
        );
        println!(
            "{}",
            if !value["callback"].is_null() {
                "shell"
            } else {
                "none"
            }
        );
        println!("{}", value["name"].as_str().unwrap_or_default());
        return Ok(());
    }
    if matches!(mode, "snippet-list" | "snippet-candidates") {
        if value["status"] != "success" {
            println!("failure");
            return Ok(());
        }
        if mode == "snippet-candidates" {
            println!("success");
        }
        println!("{}", value["options"].as_str().unwrap_or_default());
        for item in value["items"].as_array().into_iter().flatten() {
            if let Some(item) = item.as_str() {
                println!("{item}");
            }
        }
        return Ok(());
    }
    if mode == "ghq-list" {
        for candidate in value.as_array().into_iter().flatten() {
            if let Some(path) = candidate["value"].as_str() {
                println!("{path}");
            }
        }
        return Ok(());
    }
    if mode == "next-placeholder" {
        let Some(edit) = value.as_object() else {
            println!("failure");
            return Ok(());
        };
        println!("success");
        println!("{}", edit["buffer"].as_str().unwrap_or_default());
        println!("{}", edit["cursor"].as_u64().unwrap_or_default());
        return Ok(());
    }
    if value["status"] != "success" {
        println!("failure");
        return Ok(());
    }
    println!("success");
    println!("{}", value["buffer"].as_str().unwrap_or_default());
    println!("{}", value["cursor"].as_u64().unwrap_or_default());
    Ok(())
}

fn candidates_command(candidates: &Value) -> anyhow::Result<String> {
    let values = candidates
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|candidate| candidate.as_str().or_else(|| candidate["value"].as_str()))
        .map(shell_single_quote)
        .collect::<Vec<_>>();
    Ok(format!("printf '%s\\0' {}", values.join(" ")))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn render_options(values: &[Value]) -> String {
    values
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}
