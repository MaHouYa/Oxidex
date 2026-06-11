use std::collections::HashMap;
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use notify::event::{ModifyKind, RenameMode};
use notify::{
    Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use oxidex_core::daemon_model::{
    ScanState, ScannerAuthorizeResult, ScannerCancelScanParams, ScannerJobId, ScannerJobSummary,
    ScannerLiveMetadata, ScannerStartScanParams, ScannerStartScanResult, ScannerStartWatchParams,
    ScannerStartWatchResult, ScannerStatusResult, ScannerStopWatchParams, ScannerTakeResultParams,
    ScannerTakeResultResult, ScannerWatchErrorEvent, ScannerWatchEvent, ScannerWatchEventKind,
    ScannerWatchId, ScannerWatchStoppedEvent, ScannerWatchSummary,
};
use oxidex_core::ipc::{IpcFrame, params_as, read_frame, write_frame};
use oxidex_core::model::{FsType, ScanDatabase};
use oxidex_core::scanner::{ScanCancellation, scan_device, validate_device_path};
use oxidex_core::{VERSION, stream};
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

const DEFAULT_SOCKET: &str = "/run/oxidex/scannerd.sock";

fn main() {
    if let Err(err) = run() {
        eprintln!("oxidex-scannerd: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut socket_path = PathBuf::from(DEFAULT_SOCKET);
    let mut foreground = false;
    let mut idle_timeout = 300u64;
    let mut debug_mode = false;
    let mut log_level: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--foreground" => foreground = true,
            "--debug" => debug_mode = true,
            "--socket" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--socket requires a path");
                };
                socket_path = PathBuf::from(value);
            }
            "--idle-timeout-seconds" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--idle-timeout-seconds requires a value");
                };
                idle_timeout = value.parse()?;
            }
            "--log-level" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--log-level requires a value");
                };
                log_level = Some(value);
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }
    init_terminal_logging("oxidex-scannerd", debug_mode, log_level.as_deref());
    tracing::info!(
        foreground,
        debug_mode,
        socket = %socket_path.display(),
        idle_timeout_seconds = idle_timeout,
        "starting oxidex-scannerd"
    );

    if !foreground {
        tracing::warn!(
            "running in foreground; service managers should pass --foreground explicitly"
        );
    }
    serve(socket_path, Duration::from_secs(idle_timeout))
}

fn print_usage() {
    eprintln!(
        "Usage: oxidex-scannerd [--foreground] [--debug] [--socket <path>] [--idle-timeout-seconds 300] [--log-level <level>]"
    );
    eprintln!("Levels: trace, debug, info, warn, error. RUST_LOG overrides --log-level.");
}

fn init_terminal_logging(binary: &str, debug: bool, log_level: Option<&str>) {
    let default_level = log_level.unwrap_or(if debug { "debug" } else { "info" });
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_level))
        .unwrap_or_else(|_| EnvFilter::new("warn"));
    if let Err(err) = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_ids(debug)
        .with_file(debug)
        .with_line_number(debug)
        .try_init()
    {
        eprintln!("{binary}: failed to initialize terminal logging: {err}");
    }
}

fn serve(socket_path: PathBuf, idle_timeout: Duration) -> anyhow::Result<()> {
    let listener = if let Some(listener) = inherited_systemd_listener()? {
        tracing::info!("using inherited systemd socket");
        verify_socket_security(&socket_path)?;
        listener
    } else {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if socket_path.exists() {
            fs::remove_file(&socket_path)?;
        }
        let listener = UnixListener::bind(&socket_path)?;
        configure_socket_permissions(&socket_path)?;
        verify_socket_security(&socket_path)?;
        tracing::info!(socket = %socket_path.display(), "listening");
        listener
    };

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                tracing::debug!("accepted scanner daemon client connection");
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, idle_timeout) {
                        tracing::warn!(error = %format!("{err:#}"), "scanner daemon client error");
                    }
                });
            }
            Err(err) => tracing::warn!(error = %err, "accept failed"),
        }
    }
    Ok(())
}

fn configure_socket_permissions(socket_path: &Path) -> anyhow::Result<()> {
    if let Some(gid) = group_gid("oxidex")? {
        let path = std::ffi::CString::new(socket_path.as_os_str().as_bytes())?;
        let rc = unsafe { libc::chown(path.as_ptr(), 0, gid) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        tracing::debug!(socket = %socket_path.display(), gid, "configured scanner socket group");
    } else {
        tracing::warn!(
            "group 'oxidex' does not exist; socket will stay root-owned and unprivileged oxidexd may be unable to connect"
        );
    }
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o660))?;
    tracing::debug!(socket = %socket_path.display(), mode = "0660", "configured scanner socket permissions");
    Ok(())
}

fn verify_socket_security(socket_path: &Path) -> anyhow::Result<()> {
    let meta = fs::metadata(socket_path)?;
    anyhow::ensure!(
        meta.file_type().is_socket(),
        "scanner path {} is not a Unix socket",
        socket_path.display()
    );
    let mode = meta.permissions().mode() & 0o777;
    anyhow::ensure!(
        mode & !0o660 == 0,
        "scanner socket {} has insecure mode {:o}; expected no broader than 0660",
        socket_path.display(),
        mode
    );
    if meta.uid() != 0 {
        tracing::warn!(
            socket = %socket_path.display(),
            uid = meta.uid(),
            "scanner socket is not root-owned"
        );
    }
    if let Some(gid) = group_gid("oxidex")? {
        if meta.gid() != gid {
            tracing::warn!(
                socket = %socket_path.display(),
                gid = meta.gid(),
                expected_gid = gid,
                "scanner socket group is not oxidex"
            );
        }
    } else {
        tracing::warn!("group 'oxidex' does not exist; scanner socket group cannot be verified");
    }
    Ok(())
}

fn group_gid(name: &str) -> anyhow::Result<Option<u32>> {
    let name = std::ffi::CString::new(name)?;
    let group = unsafe { libc::getgrnam(name.as_ptr()) };
    if group.is_null() {
        Ok(None)
    } else {
        Ok(Some(unsafe { (*group).gr_gid }))
    }
}

fn inherited_systemd_listener() -> anyhow::Result<Option<UnixListener>> {
    let Ok(pid) = std::env::var("LISTEN_PID") else {
        return Ok(None);
    };
    if pid.parse::<u32>().ok() != Some(std::process::id()) {
        return Ok(None);
    }
    let fds = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    if fds < 1 {
        return Ok(None);
    }
    let listener = unsafe { UnixListener::from_raw_fd(3) };
    Ok(Some(listener))
}

fn handle_client(mut stream: UnixStream, idle_timeout: Duration) -> anyhow::Result<()> {
    let peer = peer_credentials(&stream)?;
    tracing::debug!(
        peer_pid = peer.pid,
        peer_uid = peer.uid,
        peer_gid = peer.gid,
        "scanner client connected"
    );
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let jobs: Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>> = Arc::new(Mutex::new(HashMap::new()));
    let watch_summaries: Arc<Mutex<HashMap<ScannerWatchId, ScannerWatchSummary>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut watches: HashMap<ScannerWatchId, ScannerWatch> = HashMap::new();
    let next_job = Arc::new(AtomicU64::new(1));
    let next_watch = Arc::new(AtomicU64::new(1));
    let mut last_error: Option<String> = None;

    while let Some(frame) = read_frame(&mut stream)? {
        let id = frame.header.id;
        let Some(id) = id else {
            write_locked(
                &writer,
                &IpcFrame::error(None, "invalid_request", "request id is required"),
            )?;
            continue;
        };
        let method = frame.header.method.clone().unwrap_or_default();
        tracing::debug!(id, method = %method, "scanner request received");
        let response = match method.as_str() {
            "scanner.hello" => Ok((
                json!({"name": "oxidex-scannerd", "version": VERSION}),
                Vec::new(),
            )),
            "scanner.authorize" => {
                tracing::info!(
                    peer_pid = peer.pid,
                    peer_uid = peer.uid,
                    peer_gid = peer.gid,
                    "scanner.authorize is deprecated; socket access already granted"
                );
                Ok((
                    serde_json::to_value(ScannerAuthorizeResult {
                        authorized: true,
                        peer_uid: peer.uid,
                        peer_pid: peer.pid,
                    })?,
                    Vec::new(),
                ))
            }
            "scanner.status" => Ok((
                serde_json::to_value(ScannerStatusResult {
                    access_model: "unix_group_socket".into(),
                    peer_uid: peer.uid,
                    peer_gid: peer.gid,
                    active_jobs: job_summaries(&jobs),
                    active_watches: scanner_watch_summaries(&watch_summaries),
                    idle_timeout_seconds: idle_timeout.as_secs(),
                    last_error: last_error.clone(),
                })?,
                Vec::new(),
            )),
            "scanner.cancel_scan" => cancel_scan(&frame, &jobs),
            "scanner.shutdown_idle" => Ok((json!({"accepted": true}), Vec::new())),
            "scanner.start_scan" => start_scan(&frame, &writer, &jobs, &next_job),
            "scanner.take_result" => take_result(&frame, &jobs),
            "scanner.start_watch" => {
                start_watch(&frame, &writer, &mut watches, &watch_summaries, &next_watch)
            }
            "scanner.stop_watch" => stop_watch(&frame, &writer, &mut watches, &watch_summaries),
            "scanner.watch_status" => Ok((
                serde_json::to_value(scanner_watch_summaries(&watch_summaries))?,
                Vec::new(),
            )),
            other => Err(anyhow::anyhow!("unknown method: {other}")),
        };

        let response = match response {
            Ok((result, payload)) => {
                tracing::debug!(
                    id,
                    method = %method,
                    payload_bytes = payload.len(),
                    "scanner request completed"
                );
                IpcFrame::ok(id, result, payload)
            }
            Err(err) => {
                last_error = Some(format!("{err:#}"));
                tracing::warn!(
                    id,
                    method = %method,
                    error = %format!("{err:#}"),
                    "scanner request failed"
                );
                IpcFrame::error(Some(id), "request_failed", format!("{err:#}"))
            }
        };
        write_locked(&writer, &response)?;

        if method == "scanner.shutdown_idle" {
            break;
        }
    }
    tracing::debug!(
        peer_pid = peer.pid,
        peer_uid = peer.uid,
        "scanner client disconnected"
    );
    for (_, watch) in watches {
        let _ = write_locked(
            &writer,
            &IpcFrame::event(
                "scanner.watch_stopped",
                serde_json::to_value(ScannerWatchStoppedEvent {
                    watch_id: watch.summary.watch_id,
                    device_id: watch.summary.device_id,
                    reason: "client_disconnected".into(),
                })?,
            ),
        );
    }
    Ok(())
}

fn start_scan(
    frame: &IpcFrame,
    writer: &Arc<Mutex<UnixStream>>,
    jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>,
    next_job: &Arc<AtomicU64>,
) -> anyhow::Result<(Value, Vec<u8>)> {
    let params: ScannerStartScanParams = params_as(frame)?;
    anyhow::ensure!(
        params.fs_type.is_supported_for_scan(),
        "unsupported filesystem type {}",
        params.fs_type
    );
    let device = validate_device_path(&params.device_path)?;
    let job_id = next_job.fetch_add(1, Ordering::Relaxed);
    let cancellation = ScanCancellation::new();
    let device_path = device.display().to_string();
    let fs_type = params.fs_type;
    jobs.lock().unwrap().insert(
        job_id,
        ScannerJob {
            summary: ScannerJobSummary {
                job_id,
                device_path: device_path.clone(),
                fs_type,
                state: ScanState::Running,
                progress: 0,
                record_count: None,
                error: None,
            },
            cancellation: cancellation.clone(),
            payload: None,
        },
    );
    tracing::info!(
        job_id,
        device_path = %device_path,
        fs_type = fs_type.as_str(),
        "scanner job started"
    );

    let worker_jobs = jobs.clone();
    let worker_writer = writer.clone();
    thread::spawn(move || {
        let result = run_scan_job(
            job_id,
            &device,
            fs_type,
            &worker_jobs,
            &worker_writer,
            cancellation,
        );
        if let Err(err) = result {
            finish_job_failed(job_id, &worker_jobs, &worker_writer, format!("{err:#}"));
        }
    });

    Ok((
        serde_json::to_value(ScannerStartScanResult { job_id })?,
        Vec::new(),
    ))
}

fn run_scan_job(
    job_id: ScannerJobId,
    device: &Path,
    fs_type: FsType,
    jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>,
    writer: &Arc<Mutex<UnixStream>>,
    cancellation: ScanCancellation,
) -> anyhow::Result<()> {
    let device_path = device.display().to_string();
    tracing::debug!(
        job_id,
        device_path = %device_path,
        fs_type = fs_type.as_str(),
        "scanner worker running"
    );
    let mut progress = |done: u64, total: u64| {
        let total = total.max(1);
        let percent = (((done.min(total) * 100) + total / 2) / total).min(100) as u8;
        update_job_progress(job_id, jobs, percent);
        let _ = write_locked(
            writer,
            &IpcFrame::event(
                "scanner.scan_progress",
                json!({
                    "job_id": job_id,
                    "device_path": device_path,
                    "percent": percent,
                }),
            ),
        );
    };
    let db = scan_device(device, fs_type, &mut progress, &cancellation)?;
    finish_job_success(job_id, jobs, writer, db)
}

fn cancel_scan(
    frame: &IpcFrame,
    jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>,
) -> anyhow::Result<(Value, Vec<u8>)> {
    let params: ScannerCancelScanParams = params_as(frame)?;
    tracing::info!(job_id = params.job_id, "scanner job cancellation requested");
    let mut jobs = jobs.lock().unwrap();
    let Some(job) = jobs.get_mut(&params.job_id) else {
        return Ok((
            json!({"cancelled": false, "message": "unknown scanner job"}),
            Vec::new(),
        ));
    };
    job.cancellation.cancel();
    if job.summary.state == ScanState::Running {
        job.summary.state = ScanState::Cancelled;
        job.summary.error = Some("scan cancelled".into());
    }
    Ok((
        json!({"cancelled": true, "message": "cancel requested"}),
        Vec::new(),
    ))
}

fn take_result(
    frame: &IpcFrame,
    jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>,
) -> anyhow::Result<(Value, Vec<u8>)> {
    let params: ScannerTakeResultParams = params_as(frame)?;
    let mut jobs = jobs.lock().unwrap();
    let Some(job) = jobs.get_mut(&params.job_id) else {
        anyhow::bail!("unknown scanner job {}", params.job_id);
    };
    match job.summary.state {
        ScanState::Finished => {
            let payload = job
                .payload
                .take()
                .ok_or_else(|| anyhow::anyhow!("scanner result was already taken"))?;
            let record_count = job.summary.record_count.unwrap_or(0);
            tracing::debug!(
                job_id = params.job_id,
                record_count,
                payload_bytes = payload.len(),
                "scanner result taken"
            );
            Ok((
                serde_json::to_value(ScannerTakeResultResult { record_count })?,
                payload,
            ))
        }
        ScanState::Failed => anyhow::bail!(
            "{}",
            job.summary
                .error
                .clone()
                .unwrap_or_else(|| "scanner job failed".into())
        ),
        ScanState::Cancelled => anyhow::bail!("scanner job was cancelled"),
        ScanState::Queued | ScanState::Running => anyhow::bail!("scanner job is not finished"),
    }
}

fn update_job_progress(
    job_id: ScannerJobId,
    jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>,
    percent: u8,
) {
    if let Some(job) = jobs.lock().unwrap().get_mut(&job_id)
        && job.summary.state == ScanState::Running
    {
        job.summary.progress = percent;
    }
}

fn finish_job_success(
    job_id: ScannerJobId,
    jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>,
    writer: &Arc<Mutex<UnixStream>>,
    db: ScanDatabase,
) -> anyhow::Result<()> {
    let record_count = db.records.len();
    let mut payload = Vec::new();
    stream::write_scan_stream(&mut payload, &db)?;
    if let Some(job) = jobs.lock().unwrap().get_mut(&job_id) {
        if job.summary.state == ScanState::Cancelled {
            tracing::info!(job_id, "scanner job cancelled before success publish");
            let _ = write_locked(
                writer,
                &IpcFrame::event(
                    "scanner.scan_cancelled",
                    json!({"job_id": job_id, "device_path": job.summary.device_path}),
                ),
            );
            return Ok(());
        }
        job.summary.state = ScanState::Finished;
        job.summary.progress = 100;
        job.summary.record_count = Some(record_count);
        job.payload = Some(payload);
    }
    tracing::info!(job_id, record_count, "scanner job finished");
    write_locked(
        writer,
        &IpcFrame::event(
            "scanner.scan_finished",
            json!({"job_id": job_id, "record_count": record_count}),
        ),
    )?;
    Ok(())
}

fn finish_job_failed(
    job_id: ScannerJobId,
    jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>,
    writer: &Arc<Mutex<UnixStream>>,
    message: String,
) {
    let state = if message.contains("scan cancelled") {
        ScanState::Cancelled
    } else {
        ScanState::Failed
    };
    if let Some(job) = jobs.lock().unwrap().get_mut(&job_id) {
        job.summary.state = state;
        job.summary.error = Some(message.clone());
    }
    tracing::warn!(job_id, state = ?state, error = %message, "scanner job failed");
    let event = if state == ScanState::Cancelled {
        "scanner.scan_cancelled"
    } else {
        "scanner.scan_failed"
    };
    let _ = write_locked(
        writer,
        &IpcFrame::event(event, json!({"job_id": job_id, "message": message})),
    );
}

fn job_summaries(jobs: &Arc<Mutex<HashMap<ScannerJobId, ScannerJob>>>) -> Vec<ScannerJobSummary> {
    let mut summaries: Vec<_> = jobs
        .lock()
        .unwrap()
        .values()
        .map(|job| job.summary.clone())
        .collect();
    summaries.sort_by_key(|job| job.job_id);
    summaries
}

fn scanner_watch_summaries(
    watches: &Arc<Mutex<HashMap<ScannerWatchId, ScannerWatchSummary>>>,
) -> Vec<ScannerWatchSummary> {
    let mut summaries: Vec<_> = watches.lock().unwrap().values().cloned().collect();
    summaries.sort_by_key(|watch| watch.watch_id);
    summaries
}

fn write_locked(writer: &Arc<Mutex<UnixStream>>, frame: &IpcFrame) -> anyhow::Result<()> {
    write_frame(&mut *writer.lock().unwrap(), frame)
}

struct ScannerJob {
    summary: ScannerJobSummary,
    cancellation: ScanCancellation,
    payload: Option<Vec<u8>>,
}

struct ScannerWatch {
    _watcher: RecommendedWatcher,
    summary: ScannerWatchSummary,
}

fn start_watch(
    frame: &IpcFrame,
    writer: &Arc<Mutex<UnixStream>>,
    watches: &mut HashMap<ScannerWatchId, ScannerWatch>,
    watch_summaries: &Arc<Mutex<HashMap<ScannerWatchId, ScannerWatchSummary>>>,
    next_watch: &Arc<AtomicU64>,
) -> anyhow::Result<(Value, Vec<u8>)> {
    let params: ScannerStartWatchParams = params_as(frame)?;
    let (device_path, mount_point, watch_roots) = validate_watch_request(&params)?;
    let watch_id = next_watch.fetch_add(1, Ordering::Relaxed);
    let writer_for_callback = writer.clone();
    let summaries_for_callback = watch_summaries.clone();
    let callback_device_id = params.device_id.clone();
    let callback_mount = mount_point.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result: notify::Result<Event>| match result {
            Ok(event) => {
                for watch_event in
                    scanner_watch_events(watch_id, &callback_device_id, &callback_mount, event)
                {
                    let _ = write_locked(
                        &writer_for_callback,
                        &IpcFrame::event(
                            "scanner.watch_event",
                            serde_json::to_value(watch_event).unwrap_or(Value::Null),
                        ),
                    );
                }
            }
            Err(err) => {
                let message = err.to_string();
                if let Some(summary) = summaries_for_callback.lock().unwrap().get_mut(&watch_id) {
                    summary.state = "error".into();
                    summary.last_error = Some(message.clone());
                }
                let _ = write_locked(
                    &writer_for_callback,
                    &IpcFrame::event(
                        "scanner.watch_error",
                        serde_json::to_value(ScannerWatchErrorEvent {
                            watch_id: Some(watch_id),
                            device_id: callback_device_id.clone(),
                            message,
                        })
                        .unwrap_or(Value::Null),
                    ),
                );
            }
        },
        NotifyConfig::default(),
    )?;
    for watch_root in &watch_roots {
        watcher.watch(watch_root, RecursiveMode::Recursive)?;
    }
    let watched_directories = watch_roots.len();
    let summary = ScannerWatchSummary {
        watch_id,
        device_id: params.device_id.clone(),
        mount_point: mount_point.display().to_string(),
        state: "watching".into(),
        watched_directories,
        last_error: None,
    };
    watch_summaries
        .lock()
        .unwrap()
        .insert(watch_id, summary.clone());
    watches.insert(
        watch_id,
        ScannerWatch {
            _watcher: watcher,
            summary,
        },
    );
    tracing::info!(
        watch_id,
        device_id = %params.device_id,
        device_path = %device_path.display(),
        mount_point = %mount_point.display(),
        watched_directories,
        "scanner live watch started"
    );
    Ok((
        serde_json::to_value(ScannerStartWatchResult { watch_id })?,
        Vec::new(),
    ))
}

fn stop_watch(
    frame: &IpcFrame,
    writer: &Arc<Mutex<UnixStream>>,
    watches: &mut HashMap<ScannerWatchId, ScannerWatch>,
    watch_summaries: &Arc<Mutex<HashMap<ScannerWatchId, ScannerWatchSummary>>>,
) -> anyhow::Result<(Value, Vec<u8>)> {
    let params: ScannerStopWatchParams = params_as(frame)?;
    let Some(watch) = watches.remove(&params.watch_id) else {
        return Ok((
            json!({"stopped": false, "message": "unknown watch"}),
            Vec::new(),
        ));
    };
    watch_summaries.lock().unwrap().remove(&params.watch_id);
    write_locked(
        writer,
        &IpcFrame::event(
            "scanner.watch_stopped",
            serde_json::to_value(ScannerWatchStoppedEvent {
                watch_id: params.watch_id,
                device_id: watch.summary.device_id,
                reason: "stopped".into(),
            })?,
        ),
    )?;
    Ok((json!({"stopped": true}), Vec::new()))
}

fn validate_watch_request(
    params: &ScannerStartWatchParams,
) -> anyhow::Result<(PathBuf, PathBuf, Vec<PathBuf>)> {
    anyhow::ensure!(
        params.fs_type.is_supported_for_scan(),
        "unsupported filesystem type {}",
        params.fs_type
    );
    let device = validate_device_path(&params.device_path)?;
    anyhow::ensure!(
        !params.mount_point.trim().is_empty(),
        "mount point is empty"
    );
    let mount_point = PathBuf::from(&params.mount_point);
    anyhow::ensure!(
        mount_point.is_absolute(),
        "mount point must be an absolute path"
    );
    let mount_point = mount_point.canonicalize()?;
    let metadata = fs::metadata(&mount_point)?;
    anyhow::ensure!(
        metadata.is_dir(),
        "mount point {} is not a directory",
        mount_point.display()
    );
    anyhow::ensure!(
        mount_point_matches_requested_device(&mount_point, &device)?,
        "mount point {} is not listed in /proc/self/mountinfo for device {}",
        mount_point.display(),
        device.display()
    );
    let requested_roots = if params.watch_roots.is_empty() {
        vec![mount_point.clone()]
    } else {
        params.watch_roots.iter().map(PathBuf::from).collect()
    };
    let mut watch_roots = Vec::with_capacity(requested_roots.len());
    for watch_root in requested_roots {
        anyhow::ensure!(
            watch_root.is_absolute(),
            "watch root {} must be an absolute path",
            watch_root.display()
        );
        let watch_root = watch_root.canonicalize()?;
        anyhow::ensure!(
            watch_root.starts_with(&mount_point),
            "watch root {} is outside mount point {}",
            watch_root.display(),
            mount_point.display()
        );
        anyhow::ensure!(
            fs::metadata(&watch_root)?.is_dir(),
            "watch root {} is not a directory",
            watch_root.display()
        );
        anyhow::ensure!(
            watch_root_belongs_to_requested_mount(&watch_root, &mount_point, &device)?,
            "watch root {} belongs to a different mounted device than {}",
            watch_root.display(),
            device.display()
        );
        if !watch_roots.contains(&watch_root) {
            watch_roots.push(watch_root);
        }
    }
    anyhow::ensure!(!watch_roots.is_empty(), "no valid watch roots");
    Ok((device, mount_point, watch_roots))
}

fn scanner_watch_events(
    watch_id: ScannerWatchId,
    device_id: &str,
    mount_point: &Path,
    event: Event,
) -> Vec<ScannerWatchEvent> {
    if event.paths.is_empty() {
        return Vec::new();
    }
    match &event.kind {
        EventKind::Create(_) => event
            .paths
            .iter()
            .filter_map(|path| {
                Some(ScannerWatchEvent {
                    watch_id,
                    device_id: device_id.to_owned(),
                    kind: ScannerWatchEventKind::Created,
                    internal_path: path_to_internal_path(mount_point, path)?,
                    old_internal_path: None,
                    metadata: metadata_for_live_path(path),
                })
            })
            .collect(),
        EventKind::Remove(_) => event
            .paths
            .iter()
            .filter_map(|path| {
                Some(ScannerWatchEvent {
                    watch_id,
                    device_id: device_id.to_owned(),
                    kind: ScannerWatchEventKind::Removed,
                    internal_path: path_to_internal_path(mount_point, path)?,
                    old_internal_path: None,
                    metadata: None,
                })
            })
            .collect(),
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() >= 2 => {
            let Some(old_internal) = path_to_internal_path(mount_point, &event.paths[0]) else {
                return Vec::new();
            };
            let Some(new_internal) = path_to_internal_path(mount_point, &event.paths[1]) else {
                return Vec::new();
            };
            vec![ScannerWatchEvent {
                watch_id,
                device_id: device_id.to_owned(),
                kind: ScannerWatchEventKind::Renamed,
                internal_path: new_internal,
                old_internal_path: Some(old_internal),
                metadata: metadata_for_live_path(&event.paths[1]),
            }]
        }
        EventKind::Modify(_) => event
            .paths
            .iter()
            .filter_map(|path| {
                Some(ScannerWatchEvent {
                    watch_id,
                    device_id: device_id.to_owned(),
                    kind: ScannerWatchEventKind::Metadata,
                    internal_path: path_to_internal_path(mount_point, path)?,
                    old_internal_path: None,
                    metadata: metadata_for_live_path(path),
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn metadata_for_live_path(path: &Path) -> Option<ScannerLiveMetadata> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let file_type = metadata.file_type();
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    Some(ScannerLiveMetadata {
        size: metadata.len(),
        mtime,
        is_dir: file_type.is_dir(),
        is_symlink: file_type.is_symlink(),
    })
}

fn path_to_internal_path(mount_point: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(mount_point).ok()?;
    let text = relative.to_string_lossy();
    if text.is_empty() {
        Some("/".into())
    } else {
        Some(format!("/{}", text.trim_start_matches('/')))
    }
}

fn mount_point_matches_requested_device(mount_point: &Path, device: &Path) -> anyhow::Result<bool> {
    let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
        return Ok(true);
    };
    Ok(mountinfo_text_matches_requested_device(
        &text,
        mount_point,
        device,
    ))
}

fn watch_root_belongs_to_requested_mount(
    watch_root: &Path,
    mount_point: &Path,
    device: &Path,
) -> anyhow::Result<bool> {
    let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
        return Ok(true);
    };
    Ok(mountinfo_text_watch_root_belongs_to_requested_mount(
        &text,
        watch_root,
        mount_point,
        device,
    ))
}

fn mountinfo_text_matches_requested_device(text: &str, mount_point: &Path, device: &Path) -> bool {
    let wanted_mount = mount_point.to_string_lossy();
    let canonical_device = device
        .canonicalize()
        .unwrap_or_else(|_| device.to_path_buf());
    for line in text.lines() {
        let Some((prefix, suffix)) = line.split_once(" - ") else {
            continue;
        };
        let mut fields = prefix.split_whitespace();
        let mount = fields.nth(4).map(decode_mountinfo_path);
        if mount.as_deref() != Some(wanted_mount.as_ref()) {
            continue;
        }

        let mut suffix_fields = suffix.split_whitespace();
        let _fs_type = suffix_fields.next();
        let Some(source) = suffix_fields.next().map(decode_mountinfo_path) else {
            return true;
        };
        if !source.starts_with("/dev/") {
            return true;
        }
        let source_path = PathBuf::from(source);
        let Ok(canonical_source) = source_path.canonicalize() else {
            return true;
        };
        return canonical_source == canonical_device;
    }
    false
}

fn mountinfo_text_watch_root_belongs_to_requested_mount(
    text: &str,
    watch_root: &Path,
    requested_mount: &Path,
    device: &Path,
) -> bool {
    let canonical_device = device
        .canonicalize()
        .unwrap_or_else(|_| device.to_path_buf());
    let mut deepest: Option<(PathBuf, String)> = None;
    for line in text.lines() {
        let Some((prefix, suffix)) = line.split_once(" - ") else {
            continue;
        };
        let mut fields = prefix.split_whitespace();
        let Some(mount) = fields.nth(4).map(decode_mountinfo_path) else {
            continue;
        };
        let mount_path = PathBuf::from(&mount);
        if !path_contains_or_equals(&mount_path, watch_root) {
            continue;
        }
        if deepest
            .as_ref()
            .map(|(current, _)| mount_path.as_os_str().len() > current.as_os_str().len())
            .unwrap_or(true)
        {
            deepest = Some((mount_path, suffix.to_owned()));
        }
    }

    let Some((deepest_mount, suffix)) = deepest else {
        return true;
    };
    if deepest_mount == requested_mount {
        return true;
    }

    let mut suffix_fields = suffix.split_whitespace();
    let _fs_type = suffix_fields.next();
    let Some(source) = suffix_fields.next().map(decode_mountinfo_path) else {
        return true;
    };
    if !source.starts_with("/dev/") {
        return true;
    }
    let source_path = PathBuf::from(source);
    let Ok(canonical_source) = source_path.canonicalize() else {
        return true;
    };
    canonical_source == canonical_device
}

fn path_contains_or_equals(root: &Path, path: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn decode_mountinfo_path(path: &str) -> String {
    path.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[derive(Clone, Copy, Debug)]
struct PeerCredentials {
    pid: i32,
    uid: u32,
    gid: u32,
}

fn peer_credentials(stream: &UnixStream) -> anyhow::Result<PeerCredentials> {
    let fd = stream.as_raw_fd();
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(PeerCredentials {
        pid: cred.pid,
        uid: cred.uid,
        gid: cred.gid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_path_strips_mount_root() {
        assert_eq!(
            path_to_internal_path(Path::new("/mnt/data"), Path::new("/mnt/data/home/a.txt")),
            Some("/home/a.txt".into())
        );
        assert_eq!(
            path_to_internal_path(Path::new("/"), Path::new("/home/a.txt")),
            Some("/home/a.txt".into())
        );
        assert_eq!(
            path_to_internal_path(Path::new("/mnt/data"), Path::new("/mnt/data")),
            Some("/".into())
        );
        assert_eq!(
            path_to_internal_path(Path::new("/mnt/data"), Path::new("/other/a.txt")),
            None
        );
    }

    #[test]
    fn mountinfo_match_checks_device_source_when_possible() {
        let text = "36 25 1:5 / /mnt/data rw,relatime - ext4 /dev/null rw\n";
        assert!(mountinfo_text_matches_requested_device(
            text,
            Path::new("/mnt/data"),
            Path::new("/dev/null")
        ));
        assert!(!mountinfo_text_matches_requested_device(
            text,
            Path::new("/mnt/data"),
            Path::new("/dev/zero")
        ));
        assert!(!mountinfo_text_matches_requested_device(
            text,
            Path::new("/mnt/missing"),
            Path::new("/dev/null")
        ));
    }

    #[test]
    fn mountinfo_match_decodes_escaped_mount_point() {
        let text = "36 25 1:5 / /mnt/Oxidex\\040Data rw,relatime - ext4 /dev/null rw\n";
        assert!(mountinfo_text_matches_requested_device(
            text,
            Path::new("/mnt/Oxidex Data"),
            Path::new("/dev/null")
        ));
    }

    #[test]
    fn watch_root_mountinfo_allows_subroot_on_same_mount() {
        let text = "36 25 1:5 / /mnt/data rw,relatime - ext4 /dev/null rw\n";
        assert!(mountinfo_text_watch_root_belongs_to_requested_mount(
            text,
            Path::new("/mnt/data/home"),
            Path::new("/mnt/data"),
            Path::new("/dev/null")
        ));
    }

    #[test]
    fn watch_root_mountinfo_rejects_nested_different_device() {
        let text = "\
36 25 1:5 / / rw,relatime - ext4 /dev/null rw\n\
37 36 1:3 / /home rw,relatime - ext4 /dev/zero rw\n";
        assert!(!mountinfo_text_watch_root_belongs_to_requested_mount(
            text,
            Path::new("/home/hiroshi"),
            Path::new("/"),
            Path::new("/dev/null")
        ));
    }
}
