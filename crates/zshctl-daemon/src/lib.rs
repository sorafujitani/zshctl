use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command as TokioCommand;
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::time::timeout;
use tracing::{debug, warn};
use zshctl_config::{ConfigCache, Settings};
use zshctl_core::completion::{CompletionRule, matching_rule, normalize_candidates};
use zshctl_core::snippet::{
    EditResult, Snippet, auto_snippet, insert_snippet_at_with_context, matches_snippet_context,
    matching_auto_snippet, matching_snippet, prepare_preprompt,
};
use zshctl_history::{
    DedupeStrategy, DeletedFilter, Entry as HistoryEntry, ExportFormat, HistoryStore, QueryFilter,
    Redactor, Scope as HistoryScope,
};
use zshctl_protocol::{
    DEFAULT_MAX_FRAME_SIZE, ErrorCode, Health, Operation, PROTOCOL_VERSION, ProtocolError, Request,
    Response, ResponseResult, read_frame, write_frame,
};

const CONNECT_TIMEOUT: Duration = Duration::from_millis(750);
const START_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONNECTIONS: usize = 128;
const MAX_COMMAND_OUTPUT: u64 = 4 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] zshctl_protocol::FrameError),
    #[error("daemon did not become healthy within {0:?}")]
    StartTimeout(Duration),
    #[error("unsafe runtime directory {path}: {reason}")]
    UnsafeRuntimeDirectory { path: PathBuf, reason: String },
    #[error("daemon returned an invalid health response")]
    InvalidHealthResponse,
    #[error("daemon protocol {daemon} is incompatible with client protocol {client}")]
    Incompatible { daemon: u16, client: u16 },
    #[error(
        "an existing daemon process is alive but did not pass its health probe (pid {pid}): {reason}"
    )]
    ExistingUnhealthy { pid: u32, reason: String },
}

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub directory: PathBuf,
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub pid: PathBuf,
}

impl RuntimePaths {
    pub fn resolve() -> Result<Self, DaemonError> {
        let directory = if let Some(value) = std::env::var_os("ZSHCTL_RUNTIME_DIR") {
            PathBuf::from(value)
        } else if let Some(value) = std::env::var_os("XDG_RUNTIME_DIR") {
            PathBuf::from(value).join("zshctl")
        } else {
            PathBuf::from(format!("/tmp/zshctl-{}", unsafe { libc::geteuid() }))
        };
        Self::from_directory(directory)
    }

    pub fn from_directory(directory: PathBuf) -> Result<Self, DaemonError> {
        ensure_secure_directory(&directory)?;
        Ok(Self {
            socket: directory.join("daemon.sock"),
            lock: directory.join("startup.lock"),
            pid: directory.join("daemon.pid"),
            directory,
        })
    }

    pub fn startup_lock(&self) -> Result<File, DaemonError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock)?;
        file.lock_exclusive()?;
        Ok(file)
    }

    fn startup_in_progress(&self) -> Result<bool, DaemonError> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                FileExt::unlock(&file)?;
                Ok(false)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(true),
            Err(error) => Err(error.into()),
        }
    }
}

fn ensure_secure_directory(path: &Path) -> Result<(), DaemonError> {
    if !path.exists() {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DaemonError::UnsafeRuntimeDirectory {
            path: path.to_path_buf(),
            reason: "must be a real directory, not a symlink".into(),
        });
    }
    let expected_uid = unsafe { libc::geteuid() };
    if metadata.uid() != expected_uid {
        return Err(DaemonError::UnsafeRuntimeDirectory {
            path: path.to_path_buf(),
            reason: format!("owned by uid {}, expected {expected_uid}", metadata.uid()),
        });
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(DaemonError::UnsafeRuntimeDirectory {
            path: path.to_path_buf(),
            reason: format!(
                "permissions {:o} allow group/other access",
                metadata.mode() & 0o777
            ),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DaemonStatus {
    Stopped {
        socket: PathBuf,
    },
    Starting {
        socket: PathBuf,
    },
    Healthy {
        socket: PathBuf,
        health: Health,
    },
    Stale {
        socket: PathBuf,
    },
    Incompatible {
        socket: PathBuf,
        daemon_protocol: u16,
    },
    Unhealthy {
        socket: PathBuf,
        reason: String,
    },
}

pub async fn status(paths: &RuntimePaths) -> Result<DaemonStatus, DaemonError> {
    if !paths.socket.exists() {
        return if paths.startup_in_progress()? {
            Ok(DaemonStatus::Starting {
                socket: paths.socket.clone(),
            })
        } else {
            Ok(DaemonStatus::Stopped {
                socket: paths.socket.clone(),
            })
        };
    }
    match probe_health(paths).await {
        Ok(health) if health.protocol_version == PROTOCOL_VERSION => Ok(DaemonStatus::Healthy {
            socket: paths.socket.clone(),
            health,
        }),
        Ok(health) => Ok(DaemonStatus::Incompatible {
            socket: paths.socket.clone(),
            daemon_protocol: health.protocol_version,
        }),
        Err(DaemonError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) =>
        {
            Ok(DaemonStatus::Stale {
                socket: paths.socket.clone(),
            })
        }
        Err(error) => Ok(DaemonStatus::Unhealthy {
            socket: paths.socket.clone(),
            reason: error.to_string(),
        }),
    }
}

pub async fn probe(paths: &RuntimePaths) -> Result<Health, DaemonError> {
    let health = probe_health(paths).await?;
    if health.protocol_version != PROTOCOL_VERSION {
        return Err(DaemonError::Incompatible {
            daemon: health.protocol_version,
            client: PROTOCOL_VERSION,
        });
    }
    Ok(health)
}

async fn probe_health(paths: &RuntimePaths) -> Result<Health, DaemonError> {
    let request = Request::new(Operation::Health, "probe", current_directory());
    let response = request_once(paths, &request, CONNECT_TIMEOUT).await?;
    match response.result {
        ResponseResult::Success { value } => {
            serde_json::from_value(value).map_err(|_| DaemonError::InvalidHealthResponse)
        }
        ResponseResult::Error { .. } => Err(DaemonError::InvalidHealthResponse),
    }
}

pub async fn request_once(
    paths: &RuntimePaths,
    request: &Request,
    duration: Duration,
) -> Result<Response, DaemonError> {
    let mut stream = timeout(duration, UnixStream::connect(&paths.socket))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "daemon connection timed out"))??;
    timeout(
        duration,
        write_frame(&mut stream, request, DEFAULT_MAX_FRAME_SIZE),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "daemon write timed out"))??;
    timeout(duration, read_frame(&mut stream, DEFAULT_MAX_FRAME_SIZE))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "daemon response timed out"))?
        .map_err(Into::into)
}

/// Executes a non-lifecycle request in the client process. This is an explicit
/// degraded mode for interactive shells whose daemon cannot be started.
pub async fn request_direct(paths: &RuntimePaths, request: Request) -> Response {
    if matches!(
        request.operation,
        Operation::Shutdown | Operation::Cancel { .. }
    ) {
        return Response::error(
            request.request_id,
            ProtocolError {
                code: ErrorCode::Validation,
                message: "lifecycle and cancellation operations require the daemon".into(),
                retryable: false,
            },
        );
    }
    dispatch(request, &DaemonState::new(paths.clone())).await
}

pub async fn start(paths: &RuntimePaths, executable: &Path) -> Result<Health, DaemonError> {
    let lock = paths.startup_lock()?;
    match probe(paths).await {
        Ok(health) => {
            drop(lock);
            return Ok(health);
        }
        Err(error @ DaemonError::Incompatible { .. }) => {
            drop(lock);
            return Err(error);
        }
        Err(error) => {
            if let Some(pid) = live_recorded_pid(paths) {
                drop(lock);
                return Err(DaemonError::ExistingUnhealthy {
                    pid,
                    reason: error.to_string(),
                });
            }
        }
    }
    if paths.socket.exists() {
        fs::remove_file(&paths.socket)?;
    }
    let mut command = Command::new(executable);
    command
        .args(["server", "run-spawned"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and the closure performs no memory
    // allocation or other work between fork and exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let child = command.spawn()?;
    fs::write(&paths.pid, format!("{}\n", child.id()))?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        match probe(paths).await {
            Ok(health) => {
                drop(lock);
                return Ok(health);
            }
            Err(DaemonError::Incompatible { daemon, client }) => {
                drop(lock);
                return Err(DaemonError::Incompatible { daemon, client });
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
    drop(lock);
    Err(DaemonError::StartTimeout(START_TIMEOUT))
}

fn live_recorded_pid(paths: &RuntimePaths) -> Option<u32> {
    let pid = fs::read_to_string(&paths.pid).ok()?.trim().parse().ok()?;
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 { Some(pid) } else { None }
}

pub async fn stop(paths: &RuntimePaths) -> Result<(), DaemonError> {
    if !paths.socket.exists() {
        return Ok(());
    }
    let request = Request::new(Operation::Shutdown, "control", current_directory());
    let _ = request_once(paths, &request, CONNECT_TIMEOUT).await?;
    let deadline = Instant::now() + START_TIMEOUT;
    while paths.socket.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if paths.socket.exists() {
        return Err(DaemonError::StartTimeout(START_TIMEOUT));
    }
    Ok(())
}

struct DaemonState {
    config_cache: Mutex<ConfigCache>,
    history: Mutex<Option<HistoryStore>>,
    inflight: Mutex<std::collections::HashMap<uuid::Uuid, tokio::task::AbortHandle>>,
}

impl DaemonState {
    fn new(_paths: RuntimePaths) -> Self {
        Self {
            config_cache: Mutex::new(ConfigCache::default()),
            history: Mutex::new(None),
            inflight: Mutex::new(std::collections::HashMap::new()),
        }
    }

    async fn settings(&self, request: &Request) -> Result<Settings, ProtocolError> {
        let environment = request_environment(request);
        let home = environment
            .get("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| feature_error(ErrorCode::Validation, "HOME is not set"))?;
        let cwd = PathBuf::from(&request.working_directory);
        let sources = zshctl_config::discover(&home, &cwd, &environment);
        self.config_cache
            .lock()
            .await
            .load(&sources)
            .map_err(|error| feature_error(ErrorCode::Validation, error))
    }

    async fn history(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<HistoryStore>>, ProtocolError> {
        let mut history = self.history.lock().await;
        if history.is_none() {
            let path = history_path().map_err(|error| feature_error(ErrorCode::Internal, error))?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| feature_error(ErrorCode::Internal, error))?;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            }
            *history = Some(
                HistoryStore::open(&path)
                    .map_err(|error| feature_error(ErrorCode::Internal, error))?,
            );
        }
        Ok(history)
    }
}

pub async fn run(paths: RuntimePaths) -> Result<(), DaemonError> {
    run_inner(paths, true).await
}

/// Runs a daemon spawned by [`start`], while the parent owns the startup lock.
/// This is separate from foreground `run` so concurrent starters cannot enter
/// the gap between process spawn and socket bind.
pub async fn run_spawned(paths: RuntimePaths) -> Result<(), DaemonError> {
    run_inner(paths, false).await
}

async fn run_inner(paths: RuntimePaths, acquire_lock: bool) -> Result<(), DaemonError> {
    let lock = if acquire_lock {
        Some(paths.startup_lock()?)
    } else {
        None
    };
    if probe(&paths).await.is_ok() {
        drop(lock);
        return Ok(());
    }
    if paths.socket.exists() {
        fs::remove_file(&paths.socket)?;
    }
    let listener = UnixListener::bind(&paths.socket)?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))?;
    let owned_inode = fs::metadata(&paths.socket)?.ino();
    fs::write(&paths.pid, format!("{}\n", std::process::id()))?;
    drop(lock);

    let state = Arc::new(DaemonState::new(paths.clone()));
    let shutdown = Arc::new(Notify::new());
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let termination = termination_signal();
    tokio::pin!(termination);
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = &mut termination => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = semaphore.clone().try_acquire_owned() else {
                    warn!("connection limit reached");
                    continue;
                };
                let shutdown = shutdown.clone();
                let state = state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(stream, shutdown, state).await {
                        debug!(%error, "client connection ended");
                    }
                });
            }
        }
    }
    drop(listener);
    if fs::metadata(&paths.socket)
        .map(|metadata| metadata.ino())
        .ok()
        == Some(owned_inode)
    {
        let _ = fs::remove_file(&paths.socket);
    }
    let _ = fs::remove_file(&paths.pid);
    Ok(())
}

async fn termination_signal() {
    let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        std::future::pending::<()>().await;
        return;
    };
    let Ok(mut interrupt) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
    else {
        std::future::pending::<()>().await;
        return;
    };
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
}

async fn serve_connection(
    mut stream: UnixStream,
    shutdown: Arc<Notify>,
    state: Arc<DaemonState>,
) -> Result<(), DaemonError> {
    loop {
        let request: Request = match read_frame(&mut stream, DEFAULT_MAX_FRAME_SIZE).await {
            Ok(request) => request,
            Err(zshctl_protocol::FrameError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        let should_shutdown = matches!(request.operation, Operation::Shutdown);
        let request_id = request.request_id;
        let response = if matches!(request.operation, Operation::Feature { .. }) {
            let state_for_task = state.clone();
            let task = tokio::spawn(async move { dispatch(request, &state_for_task).await });
            state
                .inflight
                .lock()
                .await
                .insert(request_id, task.abort_handle());
            let result = task.await;
            state.inflight.lock().await.remove(&request_id);
            match result {
                Ok(response) => response,
                Err(error) if error.is_cancelled() => Response::error(
                    request_id,
                    ProtocolError {
                        code: ErrorCode::Cancelled,
                        message: "request was cancelled".into(),
                        retryable: false,
                    },
                ),
                Err(error) => Response::error(
                    request_id,
                    ProtocolError {
                        code: ErrorCode::Internal,
                        message: format!("feature task failed: {error}"),
                        retryable: false,
                    },
                ),
            }
        } else {
            dispatch(request, &state).await
        };
        write_frame(&mut stream, &response, DEFAULT_MAX_FRAME_SIZE).await?;
        if should_shutdown {
            shutdown.notify_one();
            return Ok(());
        }
    }
}

async fn dispatch(request: Request, state: &DaemonState) -> Response {
    if request.protocol_version != PROTOCOL_VERSION {
        return Response::error(
            request.request_id,
            ProtocolError {
                code: ErrorCode::Incompatible,
                message: format!(
                    "client protocol {} is incompatible with daemon protocol {}",
                    request.protocol_version, PROTOCOL_VERSION
                ),
                retryable: false,
            },
        );
    }
    match request.operation.clone() {
        Operation::Health => Response::success(
            request.request_id,
            serde_json::to_value(health()).expect("health response is serializable"),
        ),
        Operation::Capabilities => Response::success(
            request.request_id,
            serde_json::json!({ "capabilities": health().capabilities }),
        ),
        Operation::Shutdown => Response::success(request.request_id, serde_json::json!({})),
        Operation::Cancel { request_id: target } => {
            let cancelled = if let Some(handle) = state.inflight.lock().await.remove(&target) {
                handle.abort();
                true
            } else {
                false
            };
            Response::success(
                request.request_id,
                serde_json::json!({ "cancelled": cancelled, "targetRequestId": target }),
            )
        }
        Operation::Feature { name, payload } => {
            let request_id = request.request_id;
            match dispatch_feature(&name, payload, &request, state).await {
                Ok(value) => Response::success(request_id, value),
                Err(error) => Response::error(request_id, error),
            }
        }
        Operation::Unknown => Response::error(
            request.request_id,
            ProtocolError {
                code: ErrorCode::UnknownOperation,
                message: "operation is not registered".into(),
                retryable: false,
            },
        ),
    }
}

async fn dispatch_feature(
    name: &str,
    payload: serde_json::Value,
    request: &Request,
    state: &DaemonState,
) -> Result<serde_json::Value, ProtocolError> {
    match name {
        "config.effective" => serde_json::to_value(state.settings(request).await?)
            .map_err(|error| feature_error(ErrorCode::Internal, error)),
        "snippet.next-placeholder" => {
            let buffer = required_string(&payload, "buffer")?;
            serde_json::to_value(zshctl_core::placeholder::next_placeholder(buffer))
                .map_err(|error| feature_error(ErrorCode::Internal, error))
        }
        "snippet.preprompt" => {
            let template = required_string(&payload, "template")?;
            serialize_edit(prepare_preprompt(template))
        }
        "snippet.preprompt-named" => {
            let settings = state.settings(request).await?;
            let name = required_string(&payload, "name")?;
            let Some(snippet) = settings.snippets.iter().find(|snippet| {
                snippet
                    .name
                    .as_deref()
                    .is_some_and(|candidate| candidate.trim() == name.trim())
            }) else {
                return serialize_edit(EditResult::Failure);
            };
            let template =
                evaluate_snippet(snippet, Path::new(&request.working_directory), request).await;
            serialize_edit(prepare_preprompt(&template))
        }
        "snippet.auto" => {
            let mut settings = state.settings(request).await?;
            let left = required_string(&payload, "lbuffer")?;
            let right = payload
                .get("rbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if let Some(index) = matching_auto_snippet(&settings.snippets, left, right) {
                if settings.snippets[index].evaluate {
                    let evaluated = evaluate_snippet(
                        &settings.snippets[index],
                        Path::new(&request.working_directory),
                        request,
                    )
                    .await;
                    settings.snippets[index].snippet = evaluated;
                    settings.snippets[index].evaluate = false;
                }
            }
            serialize_edit(auto_snippet(&settings.snippets, left, right))
        }
        "snippet.insert" => {
            let mut settings = state.settings(request).await?;
            let snippet_name = required_string(&payload, "name")?;
            let left = payload
                .get("lbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let right = payload
                .get("rbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let edit = if let Some(index) = matching_snippet(&settings.snippets, snippet_name) {
                insert_selected_snippet(&mut settings, index, left, right, left, right, request)
                    .await
            } else {
                EditResult::Failure
            };
            serialize_edit(edit)
        }
        "snippet.insert-id" => {
            let mut settings = state.settings(request).await?;
            let id = required_string(&payload, "id")?;
            let left = payload
                .get("lbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let right = payload
                .get("rbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let context_left = payload
                .get("context_lbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(left);
            let context_right = payload
                .get("context_rbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(right);
            let edit = if let Some((index, fingerprint)) =
                snippet_index(id).filter(|(index, _)| *index < settings.snippets.len())
            {
                if snippet_fingerprint(&settings.snippets[index]) != fingerprint {
                    EditResult::Failure
                } else {
                    insert_selected_snippet(
                        &mut settings,
                        index,
                        left,
                        right,
                        context_left,
                        context_right,
                        request,
                    )
                    .await
                }
            } else {
                EditResult::Failure
            };
            serialize_edit(edit)
        }
        "snippet.candidates" => {
            let settings = state.settings(request).await?;
            let left = required_string(&payload, "lbuffer")?;
            let right = payload
                .get("rbuffer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let items = settings
                .snippets
                .iter()
                .enumerate()
                .filter(|(_, snippet)| {
                    !snippet.snippet.contains('\n') && matches_snippet_context(snippet, left, right)
                })
                .map(|(index, snippet)| {
                    format!(
                        "{}\t{}\t{}",
                        snippet_id(index, snippet),
                        snippet_keyword_label(snippet),
                        snippet_display_label(snippet),
                    )
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({
                "status": "success",
                "options": "--prompt='Snippet> ' --height='40%' --layout=reverse --no-multi",
                "items": items,
            }))
        }
        "snippet.list" => {
            let settings = state.settings(request).await?;
            let snippets = settings
                .snippets
                .iter()
                .filter(|snippet| !snippet.snippet.contains('\n'))
                .collect::<Vec<_>>();
            let width = snippets
                .iter()
                .filter_map(|snippet| {
                    snippet
                        .name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                        .or_else(|| {
                            snippet
                                .keyword
                                .as_deref()
                                .filter(|keyword| !keyword.is_empty())
                        })
                })
                .map(|name| name.chars().count() + 1)
                .max()
                .unwrap_or(1);
            let items = snippets
                .iter()
                .map(|snippet| {
                    let name = snippet
                        .name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                        .or_else(|| {
                            snippet
                                .keyword
                                .as_deref()
                                .filter(|keyword| !keyword.is_empty())
                        });
                    let label = name.map(|name| format!("{name}:")).unwrap_or_default();
                    let keyword = if snippet.name.as_deref().is_some_and(|name| !name.is_empty()) {
                        snippet
                            .keyword
                            .as_deref()
                            .filter(|keyword| !keyword.is_empty())
                            .map(|keyword| format!("  [{keyword}]"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let text = snippet
                        .snippet
                        .replace('\\', "\\\\")
                        .replace('\r', "\\r")
                        .replace('\t', "\\t");
                    format!("{label:<width$}  {text}{keyword}")
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({
                "status": "success",
                "options": "--delimiter=':' --prompt='Snippet> ' --height='40%' --layout=reverse --no-multi",
                "items": items,
            }))
        }
        "completion.match" => {
            let settings = state.settings(request).await?;
            let buffer = required_string(&payload, "buffer")?;
            let rules = settings
                .completions
                .iter()
                .map(|completion| CompletionRule {
                    name: completion.name.clone(),
                    patterns: completion.patterns.clone(),
                    exclude_patterns: completion.exclude_patterns.clone(),
                })
                .collect::<Vec<_>>();
            let Some(rule) = matching_rule(&rules, buffer) else {
                return Ok(serde_json::Value::Null);
            };
            let completion = settings
                .completions
                .iter()
                .find(|completion| completion.name == rule.name)
                .expect("rule was built from completion");
            serde_json::to_value(completion)
                .map_err(|error| feature_error(ErrorCode::Internal, error))
        }
        "completion.source" => completion_source(payload, request, state).await,
        "ghq.list" => {
            let output = run_bounded_command(
                "ghq list --full-path",
                Path::new(&request.working_directory),
                request,
            )
            .await?;
            serde_json::to_value(normalize_candidates(
                output.lines().map(ToOwned::to_owned),
                100_000,
            ))
            .map_err(|error| feature_error(ErrorCode::Internal, error))
        }
        "history.log" => {
            let command = required_string(&payload, "command")?;
            if command.trim().is_empty() {
                return Err(feature_error(
                    ErrorCode::Validation,
                    "history command must not be empty",
                ));
            }
            let settings = state.settings(request).await?;
            let literals = payload
                .get("redact")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let redactor = Redactor::with_literals(&settings.history.redact, &literals)
                .map_err(|error| feature_error(ErrorCode::Validation, error))?;
            let entry = HistoryEntry {
                id: payload
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(Into::into),
                command: command.into(),
                cwd: payload
                    .get("pwd")
                    .or_else(|| payload.get("cwd"))
                    .and_then(serde_json::Value::as_str)
                    .map(Into::into),
                session_id: payload
                    .get("session")
                    .and_then(serde_json::Value::as_str)
                    .map(Into::into),
                occurred_at: payload
                    .get("ts")
                    .or_else(|| payload.get("occurredAt"))
                    .and_then(|value| {
                        value
                            .as_str()
                            .map(ToOwned::to_owned)
                            .or_else(|| value.as_i64().map(|value| format!("{value:020}")))
                    })
                    .unwrap_or_else(current_timestamp),
                exit_status: payload
                    .get("exit")
                    .or_else(|| payload.get("exitStatus"))
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                    .or(Some(0)),
                hostname: payload
                    .get("host")
                    .or_else(|| payload.get("hostname"))
                    .and_then(serde_json::Value::as_str)
                    .map(Into::into),
                user: payload
                    .get("user")
                    .and_then(serde_json::Value::as_str)
                    .map(Into::into),
                shell: payload
                    .get("shell")
                    .and_then(serde_json::Value::as_str)
                    .map(Into::into)
                    .or_else(|| Some("zsh".into())),
                repository_root: payload
                    .get("repoRoot")
                    .or_else(|| payload.get("repositoryRoot"))
                    .and_then(serde_json::Value::as_str)
                    .map(Into::into)
                    .or_else(|| {
                        payload
                            .get("pwd")
                            .or_else(|| payload.get("cwd"))
                            .and_then(serde_json::Value::as_str)
                            .and_then(|value| find_project_root(Path::new(value)))
                            .map(|path| path.to_string_lossy().into_owned())
                    }),
                duration_ms: payload
                    .get("durationMs")
                    .and_then(serde_json::Value::as_i64),
                metadata: payload
                    .get("metadata")
                    .and_then(|value| {
                        value
                            .as_str()
                            .map(|raw| {
                                serde_json::from_str(raw)
                                    .unwrap_or_else(|_| serde_json::json!({ "raw": raw }))
                            })
                            .or_else(|| (!value.is_null()).then(|| value.clone()))
                    })
                    .or_else(|| {
                        payload
                            .get("meta")
                            .filter(|value| !value.is_null())
                            .cloned()
                    }),
                deleted_at: None,
            };
            let history = state.history().await?;
            let stored = history
                .as_ref()
                .expect("history initialized")
                .log(&entry, &redactor)
                .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            Ok(serde_json::json!({ "stored": stored }))
        }
        "history.query" => {
            let settings = state.settings(request).await?;
            let scope_name = payload
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&settings.history.default_scope);
            if payload
                .get("toggleScope")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                let next = match scope_name {
                    "global" => "repository",
                    "repository" => "directory",
                    "directory" => "session",
                    _ => "global",
                };
                return Ok(serde_json::json!({ "scope": next }));
            }
            if scope_name == "all" {
                let limit = payload
                    .get("limit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(2_000)
                    .min(100_000) as usize;
                let deleted = parse_deleted_filter(&payload);
                let repository_root = payload
                    .get("repoRoot")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| {
                        find_project_root(Path::new(&request.working_directory))
                            .map(|path| path.to_string_lossy().into_owned())
                    })
                    .unwrap_or_else(|| request.working_directory.clone());
                let directory = payload
                    .get("directory")
                    .or_else(|| payload.get("cwd"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&request.working_directory);
                let session = payload
                    .get("session")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&request.shell_session_id);
                let history = state.history().await?;
                let store = history.as_ref().expect("history initialized");
                let mut scopes = serde_json::Map::new();
                for (name, scope) in [
                    ("global", HistoryScope::Global),
                    ("repository", HistoryScope::Repository(&repository_root)),
                    ("directory", HistoryScope::Directory(directory)),
                    ("session", HistoryScope::Session(session)),
                ] {
                    let entries = store
                        .query_filtered(QueryFilter {
                            scope,
                            limit,
                            deleted,
                            term: payload.get("term").and_then(serde_json::Value::as_str),
                            after: payload.get("after").and_then(serde_json::Value::as_str),
                            before: payload.get("before").and_then(serde_json::Value::as_str),
                            exit_status: payload
                                .get("exit")
                                .and_then(serde_json::Value::as_i64)
                                .and_then(|value| i32::try_from(value).ok()),
                            id: payload.get("id").and_then(serde_json::Value::as_str),
                        })
                        .map_err(|error| feature_error(ErrorCode::Internal, error))?;
                    scopes.insert(
                        name.into(),
                        serde_json::to_value(entries)
                            .map_err(|error| feature_error(ErrorCode::Internal, error))?,
                    );
                }
                return Ok(serde_json::json!({ "scopes": scopes }));
            }
            let scope_value = payload
                .get("scopeValue")
                .or_else(|| match scope_name {
                    "repository" => payload.get("repoRoot"),
                    "directory" => payload.get("directory").or_else(|| payload.get("cwd")),
                    "session" => payload.get("session"),
                    _ => None,
                })
                .and_then(serde_json::Value::as_str)
                .unwrap_or_else(|| match scope_name {
                    "directory" => &request.working_directory,
                    "session" => &request.shell_session_id,
                    _ => "",
                });
            let repository_root;
            let scope = match scope_name {
                "global" => HistoryScope::Global,
                "repository" => {
                    repository_root = if scope_value.is_empty() {
                        find_project_root(Path::new(&request.working_directory))
                            .unwrap_or_else(|| PathBuf::from(&request.working_directory))
                            .to_string_lossy()
                            .into_owned()
                    } else {
                        scope_value.to_owned()
                    };
                    HistoryScope::Repository(&repository_root)
                }
                "directory" => HistoryScope::Directory(scope_value),
                "session" => HistoryScope::Session(scope_value),
                _ => {
                    return Err(feature_error(
                        ErrorCode::Validation,
                        format!("unknown history scope {scope_name:?}"),
                    ));
                }
            };
            let limit = payload
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1_000)
                .min(100_000) as usize;
            let history = state.history().await?;
            let deleted = parse_deleted_filter(&payload);
            let entries = history
                .as_ref()
                .expect("history initialized")
                .query_filtered(QueryFilter {
                    scope,
                    limit,
                    deleted,
                    term: payload.get("term").and_then(serde_json::Value::as_str),
                    after: payload.get("after").and_then(serde_json::Value::as_str),
                    before: payload.get("before").and_then(serde_json::Value::as_str),
                    exit_status: payload
                        .get("exit")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|value| i32::try_from(value).ok()),
                    id: payload.get("id").and_then(serde_json::Value::as_str),
                })
                .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            serde_json::to_value(entries).map_err(|error| feature_error(ErrorCode::Internal, error))
        }
        "history.delete" => {
            let id = payload
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    feature_error(ErrorCode::Validation, "missing string field \"id\"")
                })?;
            let history = state.history().await?;
            let store = history.as_ref().expect("history initialized");
            let entry = store
                .get(id)
                .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            if entry.is_none() {
                return Err(feature_error(
                    ErrorCode::Validation,
                    format!("history record not found: {id}"),
                ));
            }
            let deleted = store
                .soft_delete_id(id, &current_timestamp())
                .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            if payload
                .get("hard")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                if let (Some(path), Some(entry)) =
                    (request.environment.get("HISTFILE"), entry.as_ref())
                {
                    prune_history_file(Path::new(path), &entry.command)
                        .map_err(|error| feature_error(ErrorCode::Internal, error))?;
                }
            }
            Ok(serde_json::json!({ "deleted": deleted }))
        }
        "history.export" => {
            let format = ExportFormat::parse(
                payload
                    .get("format")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("ndjson"),
            )
            .map_err(|error| feature_error(ErrorCode::Validation, error))?;
            let settings = state.settings(request).await?;
            let literals = payload
                .get("redact")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let export_redactor = Redactor::with_literals(&settings.history.redact, &literals)
                .map_err(|error| feature_error(ErrorCode::Validation, error))?;
            let scope_name = payload
                .get("scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&settings.history.default_scope);
            let repository_root;
            let scope = match scope_name {
                "global" => HistoryScope::Global,
                "repository" => {
                    repository_root = payload
                        .get("repoRoot")
                        .and_then(serde_json::Value::as_str)
                        .map(ToOwned::to_owned)
                        .or_else(|| {
                            find_project_root(Path::new(&request.working_directory))
                                .map(|path| path.to_string_lossy().into_owned())
                        })
                        .unwrap_or_else(|| request.working_directory.clone());
                    HistoryScope::Repository(&repository_root)
                }
                "directory" => HistoryScope::Directory(
                    payload
                        .get("directory")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&request.working_directory),
                ),
                "session" => HistoryScope::Session(
                    payload
                        .get("session")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&request.shell_session_id),
                ),
                _ => {
                    return Err(feature_error(
                        ErrorCode::Validation,
                        format!("unknown history scope {scope_name:?}"),
                    ));
                }
            };
            let deleted = parse_deleted_filter(&payload);
            let history = state.history().await?;
            let mut output = Vec::new();
            history
                .as_ref()
                .expect("history initialized")
                .export_formatted(
                    QueryFilter {
                        scope,
                        limit: payload
                            .get("limit")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(2_000)
                            .min(100_000) as usize,
                        deleted,
                        term: payload.get("term").and_then(serde_json::Value::as_str),
                        after: payload.get("after").and_then(serde_json::Value::as_str),
                        before: payload.get("before").and_then(serde_json::Value::as_str),
                        exit_status: payload
                            .get("exit")
                            .and_then(serde_json::Value::as_i64)
                            .and_then(|value| i32::try_from(value).ok()),
                        id: None,
                    },
                    format,
                    &mut output,
                )
                .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            let data = String::from_utf8(output)
                .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            Ok(serde_json::json!({ "data": export_redactor.apply(&data) }))
        }
        "history.import" => {
            let data = required_string(&payload, "data")?;
            let settings = state.settings(request).await?;
            let literals = payload
                .get("redact")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let redactor = Redactor::with_literals(&settings.history.redact, &literals)
                .map_err(|error| feature_error(ErrorCode::Validation, error))?;
            let format = ExportFormat::parse(
                payload
                    .get("format")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("ndjson"),
            )
            .map_err(|error| feature_error(ErrorCode::Validation, error))?;
            let dedupe = DedupeStrategy::parse(
                payload
                    .get("dedupe")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("off"),
            )
            .map_err(|error| feature_error(ErrorCode::Validation, error))?;
            let mut history = state.history().await?;
            let summary = history
                .as_mut()
                .expect("history initialized")
                .import_formatted(
                    format,
                    std::io::Cursor::new(data),
                    dedupe,
                    payload
                        .get("dryRun")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    &redactor,
                )
                .map_err(|error| feature_error(ErrorCode::Validation, error))?;
            serde_json::to_value(summary).map_err(|error| feature_error(ErrorCode::Internal, error))
        }
        "history.integrity" => {
            let history = state.history().await?;
            let result = history
                .as_ref()
                .expect("history initialized")
                .integrity_check()
                .map_err(|error| feature_error(ErrorCode::Internal, error))?;
            Ok(serde_json::json!({ "result": result }))
        }
        "history.fzf-config" => {
            let settings = state.settings(request).await?;
            Ok(serde_json::json!({
                "command": settings.history.fzf_command.unwrap_or_default(),
                "options": settings.history.fzf_options.join(" "),
                "togglePreview": settings.history.keymap.toggle_preview,
            }))
        }
        _ => Err(ProtocolError {
            code: ErrorCode::UnknownOperation,
            message: format!("feature operation {name:?} is not registered"),
            retryable: false,
        }),
    }
}

async fn completion_source(
    payload: serde_json::Value,
    request: &Request,
    state: &DaemonState,
) -> Result<serde_json::Value, ProtocolError> {
    let buffer = required_string(&payload, "buffer")?;
    let settings = state.settings(request).await?;
    let completion = settings
        .completions
        .iter()
        .enumerate()
        .find(|(_, completion)| {
            let rule = CompletionRule {
                name: completion.name.clone(),
                patterns: completion.patterns.clone(),
                exclude_patterns: completion.exclude_patterns.clone(),
            };
            matching_rule(std::slice::from_ref(&rule), buffer).is_some()
        });
    let Some((completion_index, completion)) = completion else {
        if !builtin_completion_disabled(request) {
            if let Some(source) = builtin_git_completion(buffer, request) {
                return Ok(source);
            }
        }
        return Ok(serde_json::json!({ "status": "no_match", "candidates": [] }));
    };
    let source_command = completion.source_command.clone();
    let options = merged_fzf_options(&completion.options, request);
    Ok(serde_json::json!({
        "status": "success",
        "name": format!("u{:04}", completion_index + 1),
        "candidates": [],
        "sourceCommand": source_command,
        "options": options,
        "preview": completion.preview,
        "callback": completion.callback,
        "callbackZero": completion.callback_zero,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawBuiltinCompletion {
    name: String,
    patterns: Vec<String>,
    #[serde(default)]
    exclude_patterns: Vec<String>,
    source_command: String,
    callback: Option<String>,
    callback_zero: bool,
    options_rendered: String,
}

#[derive(Deserialize)]
struct RawBuiltinManifest {
    sources: Vec<RawBuiltinCompletion>,
}

fn builtin_git_completion(buffer: &str, request: &Request) -> Option<serde_json::Value> {
    static SOURCES: OnceLock<Vec<RawBuiltinCompletion>> = OnceLock::new();
    let sources = SOURCES.get_or_init(|| {
        serde_json::from_str::<RawBuiltinManifest>(include_str!(
            "../../../spec/git-completions.json"
        ))
        .expect("checked-in Git completion snapshot is valid")
        .sources
    });
    let (index, source) = sources.iter().enumerate().find(|(_, source)| {
        source.patterns.iter().any(|pattern| {
            fancy_regex::Regex::new(pattern)
                .is_ok_and(|regex| regex.is_match(buffer).unwrap_or(false))
        }) && !source.exclude_patterns.iter().any(|pattern| {
            fancy_regex::Regex::new(pattern)
                .is_ok_and(|regex| regex.is_match(buffer).unwrap_or(false))
        })
    })?;
    let command = apply_git_command_overrides(&source.source_command, request);
    let options = apply_git_command_overrides(&source.options_rendered, request);
    Some(serde_json::json!({
        "status": "success",
        "name": format!("b{:04}", index + 1),
        "displayName": source.name,
        "candidates": [],
        "sourceCommand": command,
        "options": [],
        "optionsRendered": options,
        "preview": null,
        "callback": source.callback,
        "callbackZero": source.callback_zero,
    }))
}

fn apply_git_command_overrides(value: &str, request: &Request) -> String {
    let cat = request
        .environment
        .get("ZSHCTL_GIT_CAT")
        .cloned()
        .unwrap_or_else(|| "cat".into());
    let tree = request
        .environment
        .get("ZSHCTL_GIT_TREE")
        .cloned()
        .unwrap_or_else(|| "tree".into());
    value
        .replace("cat ", &format!("{cat} "))
        .replace("tree ", &format!("{tree} "))
}

async fn run_bounded_command(
    command: &str,
    cwd: &Path,
    request: &Request,
) -> Result<String, ProtocolError> {
    let mut child = TokioCommand::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .env_clear()
        .envs(&request.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| feature_error(ErrorCode::Internal, error))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| feature_error(ErrorCode::Internal, "command stdout is unavailable"))?;
    let future = async {
        let mut output = Vec::new();
        stdout
            .take(MAX_COMMAND_OUTPUT + 1)
            .read_to_end(&mut output)
            .await
            .map_err(|error| feature_error(ErrorCode::Internal, error))?;
        if output.len() as u64 > MAX_COMMAND_OUTPUT {
            let _ = child.kill().await;
            return Err(feature_error(
                ErrorCode::TooLarge,
                format!("command output exceeds {MAX_COMMAND_OUTPUT} bytes"),
            ));
        }
        let status = child
            .wait()
            .await
            .map_err(|error| feature_error(ErrorCode::Internal, error))?;
        if !status.success() {
            return Err(feature_error(
                ErrorCode::Validation,
                format!("source command exited with {status}"),
            ));
        }
        String::from_utf8(output).map_err(|error| feature_error(ErrorCode::Validation, error))
    };
    timeout(COMMAND_TIMEOUT, future)
        .await
        .map_err(|_| feature_error(ErrorCode::Timeout, "source command timed out"))?
}

async fn insert_selected_snippet(
    settings: &mut Settings,
    index: usize,
    left: &str,
    right: &str,
    context_left: &str,
    context_right: &str,
    request: &Request,
) -> EditResult {
    let matches_context = settings
        .snippets
        .get(index)
        .is_some_and(|snippet| matches_snippet_context(snippet, context_left, context_right));
    if !matches_context {
        return EditResult::Failure;
    }
    if settings.snippets[index].evaluate {
        let evaluated = evaluate_snippet(
            &settings.snippets[index],
            Path::new(&request.working_directory),
            request,
        )
        .await;
        settings.snippets[index].snippet = evaluated;
        settings.snippets[index].evaluate = false;
    }
    insert_snippet_at_with_context(
        &settings.snippets,
        index,
        left,
        right,
        context_left,
        context_right,
    )
}

fn snippet_id(index: usize, snippet: &Snippet) -> String {
    format!("s{:04}-{:016x}", index + 1, snippet_fingerprint(snippet))
}

fn snippet_index(id: &str) -> Option<(usize, u64)> {
    let (index, fingerprint) = id.trim().strip_prefix('s')?.split_once('-')?;
    let index = index.parse::<usize>().ok()?.checked_sub(1)?;
    let fingerprint = u64::from_str_radix(fingerprint, 16).ok()?;
    Some((index, fingerprint))
}

fn snippet_fingerprint(snippet: &Snippet) -> u64 {
    let serialized = serde_json::to_vec(snippet).expect("snippet is serializable");
    let mut hasher = DefaultHasher::new();
    serialized.hash(&mut hasher);
    hasher.finish()
}

fn snippet_keyword_label(snippet: &Snippet) -> String {
    snippet
        .keyword
        .as_deref()
        .map(sanitize_snippet_field)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

fn snippet_display_label(snippet: &Snippet) -> String {
    let keyword = snippet_keyword_label(snippet);
    let name = snippet
        .name
        .as_deref()
        .map(sanitize_snippet_field)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .filter(|name| name != &keyword);
    let body = sanitize_snippet_field(&snippet.snippet);
    if let Some(name) = name {
        format!("{name}:  {body}")
    } else {
        body
    }
}

fn sanitize_snippet_field(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

async fn evaluate_snippet(
    snippet: &zshctl_core::snippet::Snippet,
    cwd: &Path,
    request: &Request,
) -> String {
    if !snippet.evaluate {
        return snippet.snippet.clone();
    }
    // Evaluation errors produce an empty expansion while zshctl retains its
    // timeout and output-size safety boundary.
    run_bounded_command(&snippet.snippet, cwd, request)
        .await
        .map(|output| output.trim_end().to_owned())
        .unwrap_or_default()
}

fn required_string<'a>(
    payload: &'a serde_json::Value,
    field: &str,
) -> Result<&'a str, ProtocolError> {
    payload
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            feature_error(
                ErrorCode::Validation,
                format!("missing string field {field:?}"),
            )
        })
}

fn serialize_edit(edit: EditResult) -> Result<serde_json::Value, ProtocolError> {
    serde_json::to_value(edit).map_err(|error| feature_error(ErrorCode::Internal, error))
}

fn feature_error(code: ErrorCode, error: impl std::fmt::Display) -> ProtocolError {
    ProtocolError {
        code,
        message: error.to_string(),
        retryable: matches!(code, ErrorCode::Timeout),
    }
}

fn request_environment(request: &Request) -> std::collections::HashMap<String, String> {
    let mut environment = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    environment.extend(
        request
            .environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    environment
}

fn builtin_completion_disabled(request: &Request) -> bool {
    if let Some(value) = request.environment.get("ZSHCTL_DISABLE_BUILTIN_COMPLETION") {
        return !matches!(value.as_str(), "" | "0" | "false");
    }
    false
}

fn default_fzf_options(request: &Request) -> Vec<String> {
    let mut options = vec![
        "--bind=\"ctrl-d:preview-half-page-down,ctrl-u:preview-half-page-up,?:toggle-preview\""
            .into(),
        "--expect=\"alt-enter\"".into(),
        "--ansi".into(),
        "--height='80%'".into(),
        "--print0".into(),
        "--no-separator".into(),
    ];
    let value = request
        .environment
        .get("ZSHCTL_DEFAULT_FZF_OPTIONS")
        .cloned()
        .unwrap_or_default();
    options.extend(shell_words::split(&value).unwrap_or_default());
    options
}

fn merged_fzf_options(custom: &[String], request: &Request) -> Vec<String> {
    let mut options = default_fzf_options(request);
    for option in custom {
        let key = option.split('=').next().unwrap_or(option);
        if key == "--bind" {
            if let Some(existing) = options
                .iter_mut()
                .find(|value| value.starts_with("--bind="))
            {
                let old = existing
                    .strip_prefix("--bind=\"")
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or_default();
                let new = option
                    .strip_prefix("--bind=\"")
                    .and_then(|value| value.strip_suffix('"'))
                    .unwrap_or_default();
                *existing = format!("--bind=\"{old},{new}\"");
                continue;
            }
        }
        if let Some(index) = options
            .iter()
            .position(|value| value == key || value.starts_with(&format!("{key}=")))
        {
            options[index] = option.clone();
        } else {
            options.push(option.clone());
        }
    }
    options
}

fn find_project_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|directory| directory.join(".git").exists())
        .map(Path::to_path_buf)
}

fn health() -> Health {
    Health {
        pid: std::process::id(),
        build_identity: option_env!("ZSHCTL_BUILD_IDENTITY")
            .unwrap_or(env!("CARGO_PKG_VERSION"))
            .into(),
        protocol_version: PROTOCOL_VERSION,
        capabilities: vec![
            "health".into(),
            "shutdown".into(),
            "capabilities".into(),
            "config.effective".into(),
            "snippet.auto".into(),
            "snippet.insert".into(),
            "snippet.insert-id".into(),
            "snippet.candidates".into(),
            "snippet.list".into(),
            "snippet.next-placeholder".into(),
            "snippet.preprompt".into(),
            "snippet.preprompt-named".into(),
            "completion.match".into(),
            "completion.source".into(),
            "ghq.list".into(),
            "history.log".into(),
            "history.query".into(),
            "history.delete".into(),
            "history.export".into(),
            "history.import".into(),
            "history.integrity".into(),
            "history.fzf-config".into(),
        ],
    }
}

fn current_directory() -> String {
    std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn parse_deleted_filter(payload: &serde_json::Value) -> DeletedFilter {
    match payload.get("deleted") {
        Some(serde_json::Value::String(value)) if value == "include" => DeletedFilter::Include,
        Some(serde_json::Value::String(value)) if value == "only" => DeletedFilter::Only,
        Some(serde_json::Value::Bool(true)) => DeletedFilter::Include,
        _ => DeletedFilter::Exclude,
    }
}

fn prune_history_file(path: &Path, command: &str) -> Result<(), io::Error> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Ok(());
    };
    let retained = contents
        .lines()
        .filter(|line| {
            let recorded = line
                .strip_prefix(": ")
                .and_then(|line| line.split_once(';').map(|(_, command)| command))
                .unwrap_or(line);
            recorded != command
        })
        .collect::<Vec<_>>()
        .join("\n");
    let temporary = path.with_extension(format!("zshctl-{}.tmp", uuid::Uuid::new_v4()));
    let permissions = fs::metadata(path)?.permissions();
    fs::write(
        &temporary,
        if retained.is_empty() {
            retained
        } else {
            format!("{retained}\n")
        },
    )?;
    fs::set_permissions(&temporary, permissions)?;
    fs::rename(&temporary, path)
}

fn history_path() -> Result<PathBuf, io::Error> {
    if let Some(directory) = std::env::var_os("ZSHCTL_DATA_DIR") {
        return Ok(PathBuf::from(directory).join("history.sqlite3"));
    }
    if let Some(directory) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(directory).join("zshctl/history.sqlite3"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".local/share/zshctl/history.sqlite3"))
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

    #[test]
    fn runtime_directory_rejects_open_permissions() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let result = RuntimePaths::from_directory(temporary.path().to_path_buf());
        assert!(matches!(
            result,
            Err(DaemonError::UnsafeRuntimeDirectory { .. })
        ));
    }

    #[tokio::test]
    async fn dispatch_correlates_response_and_rejects_incompatible_version() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let state = DaemonState::new(paths);
        let mut request = Request::new(Operation::Health, "session", "/tmp");
        let request_id = request.request_id;
        request.protocol_version += 1;
        let response = dispatch(request, &state).await;
        assert_eq!(response.request_id, request_id);
        assert!(matches!(
            response.result,
            ResponseResult::Error {
                error: ProtocolError {
                    code: ErrorCode::Incompatible,
                    ..
                }
            }
        ));
    }

    #[tokio::test]
    async fn cancellation_aborts_only_the_correlated_request() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let state = DaemonState::new(paths);
        let target = uuid::Uuid::new_v4();
        let task = tokio::spawn(std::future::pending::<()>());
        state
            .inflight
            .lock()
            .await
            .insert(target, task.abort_handle());
        let request = Request::new(
            Operation::Cancel { request_id: target },
            "other-session",
            "/tmp",
        );
        let response = dispatch(request, &state).await;
        assert!(matches!(response.result, ResponseResult::Success { .. }));
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(state.inflight.lock().await.is_empty());
    }

    #[tokio::test]
    async fn preprompt_requests_remain_isolated_between_shell_sessions() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let state = DaemonState::new(paths);
        let first = Request::new(
            Operation::Feature {
                name: "snippet.preprompt".into(),
                payload: serde_json::json!({ "template": "first {{VALUE}}" }),
            },
            "shell-first",
            "/tmp",
        );
        let second = Request::new(
            Operation::Feature {
                name: "snippet.preprompt".into(),
                payload: serde_json::json!({ "template": "second {{VALUE}}" }),
            },
            "shell-second",
            "/tmp",
        );
        let (first, second) = tokio::join!(dispatch(first, &state), dispatch(second, &state));
        let value = |response: Response| match response.result {
            ResponseResult::Success { value } => value,
            ResponseResult::Error { error } => panic!("unexpected error: {error:?}"),
        };
        assert_eq!(value(first)["buffer"], "first  ");
        assert_eq!(value(second)["buffer"], "second  ");
    }

    #[tokio::test]
    async fn snippet_candidates_collect_context_matches_without_ui_filtering() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = temporary.path().join("snippets.yml");
        fs::write(
            &config,
            "snippets:\n  - name: ad_dev\n    keyword: deploy\n    snippet: \"printf ad_dev\"\n  - name: shared\n    keyword: cx_aws_dev\n    snippet: \"printf shared\"\n  - name: other\n    keyword: unrelated\n    snippet: \"printf pi_aws_dev\"\n  - name: multiline\n    keyword: multiline\n    snippet: |\n      printf one\n      printf two\n  - name: contextual\n    keyword: only_here\n    snippet: \"printf contextual\"\n    context:\n      lbuffer: \"^allowed$\"\n",
        )
        .unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let state = DaemonState::new(paths);

        for (lbuffer, rbuffer, expected_count) in [
            ("", "", 3),
            ("aws", "", 3),
            ("aws arg", "", 3),
            ("aws", "arg", 3),
            ("allowed", "", 4),
            ("blocked", "", 3),
        ] {
            let mut request = Request::new(
                Operation::Feature {
                    name: "snippet.candidates".into(),
                    payload: serde_json::json!({ "lbuffer": lbuffer, "rbuffer": rbuffer }),
                },
                "session",
                "/tmp",
            );
            request
                .environment
                .insert("HOME".into(), temporary.path().display().to_string());
            request
                .environment
                .insert("ZSHCTL_CONFIG".into(), config.display().to_string());
            let response = dispatch(request, &state).await;
            let ResponseResult::Success { value } = response.result else {
                panic!("candidate request failed");
            };
            let items = value["items"].as_array().expect("items must be an array");
            assert_eq!(
                items.len(),
                expected_count,
                "unexpected candidates for {lbuffer:?} {rbuffer:?}"
            );
        }

        let mut request = Request::new(
            Operation::Feature {
                name: "snippet.candidates".into(),
                payload: serde_json::json!({ "lbuffer": "aws", "rbuffer": "" }),
            },
            "session",
            "/tmp",
        );
        request
            .environment
            .insert("HOME".into(), temporary.path().display().to_string());
        request
            .environment
            .insert("ZSHCTL_CONFIG".into(), config.display().to_string());
        let response = dispatch(request, &state).await;
        let ResponseResult::Success { value } = response.result else {
            panic!("candidate request failed");
        };
        let items = value["items"].as_array().expect("items must be an array");
        let fields_for = |keyword: &str| {
            items.iter().find_map(|item| {
                let fields = item.as_str()?.split('\t').collect::<Vec<_>>();
                (fields.get(1) == Some(&keyword)).then_some(fields)
            })
        };
        let deploy = fields_for("deploy").expect("deploy candidate");
        assert_eq!(deploy.len(), 3);
        assert!(deploy[0].starts_with("s0001-"));
        assert_eq!(deploy[1], "deploy");
        assert_eq!(deploy[2], "ad_dev:  printf ad_dev");
        let shared = fields_for("cx_aws_dev").expect("cx_aws_dev candidate");
        assert_eq!(shared[2], "shared:  printf shared");
        let unrelated = fields_for("unrelated").expect("unrelated candidate");
        assert_eq!(unrelated[2], "other:  printf pi_aws_dev");
        assert!(
            !items
                .iter()
                .any(|item| { item.as_str().is_some_and(|item| item.contains("multiline")) })
        );
    }

    #[tokio::test]
    async fn snippet_candidate_ids_disambiguate_same_names_and_sanitize_tsv() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = temporary.path().join("snippets.yml");
        fs::write(
            &config,
            "snippets:\n  - name: 'same:name'\n    keyword: first\n    snippet: \"printf first\\tline\"\n  - name: 'same:name'\n    keyword: second\n    snippet: \"printf second\"\n  - name: same_keyword\n    keyword: same_keyword\n    snippet: \"printf duplicate\"\n",
        )
        .unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let state = DaemonState::new(paths);
        let mut candidates = Request::new(
            Operation::Feature {
                name: "snippet.candidates".into(),
                payload: serde_json::json!({ "lbuffer": "", "rbuffer": "" }),
            },
            "session",
            "/tmp",
        );
        candidates
            .environment
            .insert("HOME".into(), temporary.path().display().to_string());
        candidates
            .environment
            .insert("ZSHCTL_CONFIG".into(), config.display().to_string());
        let response = dispatch(candidates, &state).await;
        let ResponseResult::Success { value } = response.result else {
            panic!("candidate request failed");
        };
        let items = value["items"].as_array().expect("items must be an array");
        assert_eq!(items.len(), 3);
        let ids = items
            .iter()
            .map(|item| item.as_str().unwrap().split('\t').next().unwrap())
            .collect::<Vec<_>>();
        assert_ne!(ids[0], ids[1]);
        let fields = items
            .iter()
            .map(|item| item.as_str().unwrap().split('\t').collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert!(fields.iter().all(|fields| fields.len() == 3));
        assert_eq!(fields[0][1], "first");
        assert_eq!(fields[0][2], "same:name:  printf first line");
        assert_eq!(fields[1][1], "second");
        assert_eq!(fields[1][2], "same:name:  printf second");
        assert_eq!(fields[2][1], "same_keyword");
        assert_eq!(fields[2][2], "printf duplicate");

        let mut insert = Request::new(
            Operation::Feature {
                name: "snippet.insert-id".into(),
                payload: serde_json::json!({
                    "id": ids[1],
                    "lbuffer": "",
                    "rbuffer": "",
                    "context_lbuffer": "",
                    "context_rbuffer": ""
                }),
            },
            "session",
            "/tmp",
        );
        insert
            .environment
            .insert("HOME".into(), temporary.path().display().to_string());
        insert
            .environment
            .insert("ZSHCTL_CONFIG".into(), config.display().to_string());
        let response = dispatch(insert, &state).await;
        let ResponseResult::Success { value } = response.result else {
            panic!("insert request failed");
        };
        assert_eq!(value["status"], "success");
        assert_eq!(value["buffer"], "printf second ");
    }

    #[tokio::test]
    async fn context_mismatch_rejects_before_evaluate() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = temporary.path().join("snippets.yml");
        let marker = temporary.path().join("evaluated.marker");
        fs::write(
            &config,
            "snippets:\n  - name: guarded\n    keyword: guard\n    snippet: \"printf evaluated > evaluated.marker\"\n    evaluate: true\n    context:\n      lbuffer: \"^allowed$\"\n",
        )
        .unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let state = DaemonState::new(paths);
        let mut candidates = Request::new(
            Operation::Feature {
                name: "snippet.candidates".into(),
                payload: serde_json::json!({ "lbuffer": "allowed", "rbuffer": "" }),
            },
            "session",
            temporary.path().to_string_lossy().to_string(),
        );
        candidates
            .environment
            .insert("HOME".into(), temporary.path().display().to_string());
        candidates
            .environment
            .insert("ZSHCTL_CONFIG".into(), config.display().to_string());
        let response = dispatch(candidates, &state).await;
        let ResponseResult::Success { value } = response.result else {
            panic!("candidate request failed");
        };
        let id = value["items"][0]
            .as_str()
            .unwrap()
            .split('\t')
            .next()
            .unwrap()
            .to_owned();

        let mut insert = Request::new(
            Operation::Feature {
                name: "snippet.insert-id".into(),
                payload: serde_json::json!({
                    "id": id,
                    "lbuffer": "",
                    "rbuffer": "",
                    "context_lbuffer": "blocked",
                    "context_rbuffer": ""
                }),
            },
            "session",
            temporary.path().to_string_lossy().to_string(),
        );
        insert
            .environment
            .insert("HOME".into(), temporary.path().display().to_string());
        insert
            .environment
            .insert("ZSHCTL_CONFIG".into(), config.display().to_string());
        let response = dispatch(insert, &state).await;
        let ResponseResult::Success { value } = response.result else {
            panic!("insert request failed");
        };
        assert_eq!(value["status"], "failure");
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn stale_candidate_id_is_rejected_after_evaluated_snippet_changes() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let config = temporary.path().join("snippets.yml");
        fs::write(
            &config,
            "snippets:\n  - name: target\n    keyword: gs\n    snippet: \"printf old\"\n    evaluate: true\n",
        )
        .unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let state = DaemonState::new(paths);
        let set_environment = |request: &mut Request| {
            request
                .environment
                .insert("HOME".into(), temporary.path().display().to_string());
            request
                .environment
                .insert("ZSHCTL_CONFIG".into(), config.display().to_string());
        };

        let mut candidates = Request::new(
            Operation::Feature {
                name: "snippet.candidates".into(),
                payload: serde_json::json!({ "lbuffer": "gs", "rbuffer": "" }),
            },
            "session",
            "/tmp",
        );
        set_environment(&mut candidates);
        let candidates = dispatch(candidates, &state).await;
        let ResponseResult::Success { value } = candidates.result else {
            panic!("candidate request failed");
        };
        let id = value["items"][0]
            .as_str()
            .and_then(|item| item.split_once('\t'))
            .map(|(id, _)| id.to_owned())
            .expect("candidate must contain an id");

        fs::write(
            &config,
            "snippets:\n  - name: target\n    keyword: gs\n    snippet: \"printf new\"\n    evaluate: true\n",
        )
        .unwrap();
        let mut insert = Request::new(
            Operation::Feature {
                name: "snippet.insert-id".into(),
                payload: serde_json::json!({
                    "id": id,
                    "lbuffer": "gs",
                    "rbuffer": "",
                    "context_lbuffer": "",
                    "context_rbuffer": ""
                }),
            },
            "session",
            "/tmp",
        );
        set_environment(&mut insert);
        let insert = dispatch(insert, &state).await;
        let ResponseResult::Success { value } = insert.result else {
            panic!("insert request failed");
        };
        assert_eq!(value["status"], "failure");
    }

    #[tokio::test]
    async fn slow_live_daemon_socket_is_never_reclaimed() {
        let temporary = tempfile::tempdir().unwrap();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let paths = RuntimePaths::from_directory(temporary.path().to_path_buf()).unwrap();
        let listener = UnixListener::bind(&paths.socket).unwrap();
        fs::write(&paths.pid, format!("{}\n", std::process::id())).unwrap();
        let inode = fs::metadata(&paths.socket).unwrap().ino();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });

        let error = start(&paths, Path::new("/does/not/matter"))
            .await
            .unwrap_err();
        assert!(matches!(error, DaemonError::ExistingUnhealthy { .. }));
        assert_eq!(fs::metadata(&paths.socket).unwrap().ino(), inode);
        server.abort();
    }
}
