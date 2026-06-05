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

use kerything_core::daemon_model::{
    ScanState, ScannerAuthorizeResult, ScannerCancelScanParams, ScannerJobId, ScannerJobSummary,
    ScannerStartScanParams, ScannerStartScanResult, ScannerStatusResult, ScannerTakeResultParams,
    ScannerTakeResultResult,
};
use kerything_core::ipc::{IpcFrame, params_as, read_frame, write_frame};
use kerything_core::model::{FsType, ScanDatabase};
use kerything_core::scanner::{ScanCancellation, scan_device, validate_device_path};
use kerything_core::{VERSION, stream};
use serde_json::{Value, json};

const DEFAULT_SOCKET: &str = "/run/kerything/scannerd.sock";
const POLKIT_ACTION: &str = "net.reikooters.kerything.connect-scanner";

fn main() {
    if let Err(err) = run() {
        eprintln!("kerything-scannerd: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut socket_path = PathBuf::from(DEFAULT_SOCKET);
    let mut foreground = false;
    let mut idle_timeout = 300u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--foreground" => foreground = true,
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
                let _ = args.next();
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
    }

    if !foreground {
        eprintln!(
            "kerything-scannerd: running in foreground; service managers should pass --foreground explicitly"
        );
    }
    serve(socket_path, Duration::from_secs(idle_timeout))
}

fn print_usage() {
    eprintln!(
        "Usage: kerything-scannerd [--foreground] [--socket <path>] [--idle-timeout-seconds 300] [--log-level <level>]"
    );
}

fn serve(socket_path: PathBuf, idle_timeout: Duration) -> anyhow::Result<()> {
    let listener = if let Some(listener) = inherited_systemd_listener()? {
        eprintln!("kerything-scannerd: using inherited systemd socket");
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
        eprintln!("kerything-scannerd: listening on {}", socket_path.display());
        listener
    };

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, idle_timeout) {
                        eprintln!("kerything-scannerd: client error: {err:#}");
                    }
                });
            }
            Err(err) => eprintln!("kerything-scannerd: accept failed: {err}"),
        }
    }
    Ok(())
}

fn configure_socket_permissions(socket_path: &Path) -> anyhow::Result<()> {
    if let Some(gid) = group_gid("kerything")? {
        let path = std::ffi::CString::new(socket_path.as_os_str().as_bytes())?;
        let rc = unsafe { libc::chown(path.as_ptr(), 0, gid) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    } else {
        eprintln!(
            "kerything-scannerd: group 'kerything' does not exist; socket will stay root-owned and unprivileged kerythingd may be unable to connect"
        );
    }
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o660))?;
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
        let response = match method.as_str() {
            "scanner.hello" => Ok((
                json!({"name": "kerything-scannerd", "version": VERSION}),
                Vec::new(),
            )),
            "scanner.authorize" => {
                authorize_peer(&peer)?;
                authorized = true;
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
            Ok((result, payload)) => IpcFrame::ok(id, result, payload),
            Err(err) => {
                last_error = Some(format!("{err:#}"));
                IpcFrame::error(Some(id), "request_failed", format!("{err:#}"))
            }
        };
        write_locked(&writer, &response)?;

        if method == "scanner.shutdown_idle" {
            break;
        }
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
    if std::env::var_os("KERYTHING_SCANNERD_SKIP_POLKIT").is_some() {
        return Ok(());
    }
    if peer.uid == 0 {
        return Ok(());
    }

    let output = Command::new("pkcheck")
        .arg("--action-id")
        .arg(POLKIT_ACTION)
        .arg("--process")
        .arg(peer.pid.to_string())
        .arg("--allow-user-interaction")
        .output()
        .map_err(|err| anyhow::anyhow!("failed to run pkcheck: {err}"))?;

    if output.status.success() {
        return Ok(());
    }

    let diagnostics = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = format!("{}{}", diagnostics.trim(), stdout.trim());
    if details.contains("is not registered") || details.contains("not registered") {
        anyhow::bail!(
            "Polkit action {POLKIT_ACTION} is not registered. Install net.reikooters.kerything.policy to /usr/share/polkit-1/actions/ and restart polkit, then reconnect. pkcheck said: {details}"
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
