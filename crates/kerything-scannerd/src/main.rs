use std::fs;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use kerything_core::daemon_model::{
    ScannerAuthorizeResult, ScannerStartScanParams, ScannerStartScanResult, ScannerStatusResult,
};
use kerything_core::ipc::{IpcFrame, params_as, read_frame, write_frame};
use kerything_core::scanner::{scan_device, validate_device_path};
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

fn serve(socket_path: PathBuf, _idle_timeout: Duration) -> anyhow::Result<()> {
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
                    if let Err(err) = handle_client(stream) {
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

fn handle_client(mut stream: UnixStream) -> anyhow::Result<()> {
    let peer = peer_credentials(&stream)?;
    let mut authorized = false;

    while let Some(frame) = read_frame(&mut stream)? {
        let id = frame.header.id;
        let Some(id) = id else {
            write_frame(
                &mut stream,
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
                    active: false,
                })?,
                Vec::new(),
            )),
            "scanner.cancel_scan" => Ok((
                json!({"cancelled": false, "message": "no cancellable scan is active on this connection"}),
                Vec::new(),
            )),
            "scanner.shutdown_idle" => Ok((json!({"accepted": true}), Vec::new())),
            "scanner.start_scan" => {
                if !authorized {
                    Err(anyhow::anyhow!("scanner connection is not authorized"))
                } else {
                    start_scan(&mut stream, &frame)
                }
            }
            other => Err(anyhow::anyhow!("unknown method: {other}")),
        };

        let response = match response {
            Ok((result, payload)) => IpcFrame::ok(id, result, payload),
            Err(err) => IpcFrame::error(Some(id), "request_failed", format!("{err:#}")),
        };
        write_frame(&mut stream, &response)?;

        if method == "scanner.shutdown_idle" {
            break;
        }
    }
    Ok(())
}

fn start_scan(stream: &mut UnixStream, frame: &IpcFrame) -> anyhow::Result<(Value, Vec<u8>)> {
    let params: ScannerStartScanParams = params_as(frame)?;
    anyhow::ensure!(
        params.fs_type.is_supported_for_scan(),
        "unsupported filesystem type {}",
        params.fs_type
    );
    let device = validate_device_path(&params.device_path)?;
    let mut progress = |done: u64, total: u64| {
        let total = total.max(1);
        let percent = (((done.min(total) * 100) + total / 2) / total).min(100) as u8;
        let _ = write_frame(
            &mut *stream,
            &IpcFrame::event(
                "scan_progress",
                json!({
                    "device_path": device.display().to_string(),
                    "percent": percent,
                }),
            ),
        );
    };
    let db = scan_device(&device, params.fs_type, &mut progress)?;
    let record_count = db.records.len();
    let mut payload = Vec::new();
    stream::write_scan_stream(&mut payload, &db)?;
    Ok((
        serde_json::to_value(ScannerStartScanResult { record_count })?,
        payload,
    ))
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
