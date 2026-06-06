use std::collections::HashMap;
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::thread;
use std::time::Duration;

use oxidex_core::daemon_model::{
    ScanState, ScannerAuthorizeResult, ScannerCancelScanParams, ScannerJobId, ScannerJobSummary,
    ScannerStartScanParams, ScannerStartScanResult, ScannerStatusResult, ScannerTakeResultParams,
    ScannerTakeResultResult,
};
use oxidex_core::ipc::{IpcFrame, params_as, read_frame, write_frame};
use oxidex_core::model::{FsType, ScanDatabase};
use oxidex_core::scanner::{ScanCancellation, scan_device, validate_device_path};
use oxidex_core::{VERSION, stream};
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

const DEFAULT_SOCKET: &str = "/run/oxidex/scannerd.sock";
const POLKIT_ACTION: &str = "org.mahouya.oxidex.connect-scanner";

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
    let next_job = Arc::new(AtomicU64::new(1));
    let mut authorized = false;
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
        tracing::debug!(id, method = %method, authorized, "scanner request received");
        let response = match method.as_str() {
            "scanner.hello" => Ok((
                json!({"name": "oxidex-scannerd", "version": VERSION}),
                Vec::new(),
            )),
            "scanner.authorize" => {
                authorize_peer(&peer)?;
                authorized = true;
                tracing::info!(
                    peer_pid = peer.pid,
                    peer_uid = peer.uid,
                    peer_gid = peer.gid,
                    "scanner client authorized"
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
                    authorized,
                    active_jobs: job_summaries(&jobs),
                    idle_timeout_seconds: idle_timeout.as_secs(),
                    last_error: last_error.clone(),
                })?,
                Vec::new(),
            )),
            "scanner.cancel_scan" => cancel_scan(&frame, &jobs),
            "scanner.shutdown_idle" => Ok((json!({"accepted": true}), Vec::new())),
            "scanner.start_scan" => {
                if !authorized {
                    Err(anyhow::anyhow!("scanner connection is not authorized"))
                } else {
                    start_scan(&frame, &writer, &jobs, &next_job)
                }
            }
            "scanner.take_result" => {
                if !authorized {
                    Err(anyhow::anyhow!("scanner connection is not authorized"))
                } else {
                    take_result(&frame, &jobs)
                }
            }
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

fn write_locked(writer: &Arc<Mutex<UnixStream>>, frame: &IpcFrame) -> anyhow::Result<()> {
    write_frame(&mut *writer.lock().unwrap(), frame)
}

struct ScannerJob {
    summary: ScannerJobSummary,
    cancellation: ScanCancellation,
    payload: Option<Vec<u8>>,
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

fn authorize_peer(peer: &PeerCredentials) -> anyhow::Result<()> {
    if std::env::var_os("OXIDEX_SCANNERD_SKIP_POLKIT").is_some() {
        tracing::warn!(
            "OXIDEX_SCANNERD_SKIP_POLKIT is set; allowing scanner client without Polkit"
        );
        return Ok(());
    }
    if peer.uid == 0 {
        tracing::debug!(
            peer_pid = peer.pid,
            "allowing root scanner client without Polkit prompt"
        );
        return Ok(());
    }

    tracing::debug!(
        action = POLKIT_ACTION,
        peer_pid = peer.pid,
        peer_uid = peer.uid,
        "running pkcheck"
    );
    let output = Command::new("pkcheck")
        .arg("--action-id")
        .arg(POLKIT_ACTION)
        .arg("--process")
        .arg(peer.pid.to_string())
        .arg("--allow-user-interaction")
        .output()
        .map_err(|err| anyhow::anyhow!("failed to run pkcheck: {err}"))?;

    if output.status.success() {
        tracing::debug!(
            peer_pid = peer.pid,
            peer_uid = peer.uid,
            "pkcheck authorized peer"
        );
        return Ok(());
    }

    let diagnostics = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = format!("{}{}", diagnostics.trim(), stdout.trim());
    tracing::warn!(
        peer_pid = peer.pid,
        peer_uid = peer.uid,
        status = %output.status,
        details = %details,
        "pkcheck denied peer"
    );
    if details.contains("is not registered") || details.contains("not registered") {
        anyhow::bail!(
            "Polkit action {POLKIT_ACTION} is not registered. Install org.mahouya.oxidex.policy to /usr/share/polkit-1/actions/ and restart polkit, then reconnect. pkcheck said: {details}"
        );
    }

    anyhow::bail!(
        "Polkit authorization denied for pid {} uid {} gid {}. pkcheck said: {}",
        peer.pid,
        peer.uid,
        peer.gid,
        if details.is_empty() {
            output.status.to_string()
        } else {
            details
        }
    );
}
