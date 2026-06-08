use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use oxidex_core::config::{
    AppConfig, LanguageMode, ThemeMode, config_path as default_config_path, load_config_from_path,
    save_config_to_path, validate_config,
};
use oxidex_core::daemon_model::{
    ConfigGetResult, ConfigSetParams, ConfigValidateParams, DaemonDoctorResult, DaemonStatus,
    DeviceSummary, IndexCancelScanParams, IndexCancelScanResult, IndexForgetParams,
    IndexJobStatusParams, IndexStartScanParams, IndexStartScanResult, IndexSummary,
    ResolvePathParams, ResolvedPath, ScanJobSummary, ScanState, ScannerCancelScanParams,
    ScannerLiveMetadata, ScannerStartScanParams, ScannerStartScanResult, ScannerStartWatchParams,
    ScannerStartWatchResult, ScannerStatusDetail, ScannerStopWatchParams, ScannerTakeResultParams,
    ScannerWatchErrorEvent, ScannerWatchEvent, ScannerWatchEventKind, ScannerWatchId,
    ScannerWatchStoppedEvent, SearchExplainParams, SearchQueryParams, SearchQueryResult,
    SearchResultRow, WatchSummary,
};
use oxidex_core::device::{DeviceInfo, list_known_devices};
use oxidex_core::doctor::{DoctorOptions, run_local_doctor};
use oxidex_core::index::{
    LiveRecordMetadata, LiveUpdateEvent, SearchHit, SearchIndex, explain_search_query,
    merge_search_request, parse_search_query,
};
use oxidex_core::ipc::{IpcFrame, params_as, read_frame, result_as, write_frame};
use oxidex_core::model::{ScanDatabase, SortDirection, SortKey};
use oxidex_core::rules::{compile_rules, filter_index};
use oxidex_core::scanner::ScanCancellation;
use oxidex_core::{VERSION, snapshot, stream};
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

const DEFAULT_SCANNER_SOCKET: &str = "/run/oxidex/scannerd.sock";

fn main() {
    if let Err(err) = run() {
        eprintln!("oxidexd: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut socket_path = default_daemon_socket_path()?;
    let mut config_path = default_config_path()?;
    let mut foreground = false;
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
            "--config" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--config requires a path");
                };
                config_path = PathBuf::from(value);
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
    init_terminal_logging("oxidexd", debug_mode, log_level.as_deref());
    tracing::info!(
        foreground,
        debug_mode,
        socket = %socket_path.display(),
        config = %config_path.display(),
        scanner_socket = DEFAULT_SCANNER_SOCKET,
        "starting oxidexd"
    );

    if !foreground {
        tracing::warn!(
            "running in foreground; service managers should pass --foreground explicitly"
        );
    }

    let state = Arc::new(Mutex::new(DaemonState::load(
        config_path,
        DEFAULT_SCANNER_SOCKET.into(),
    )?));
    start_watch_manager(state.clone());
    serve(socket_path, state)
}

fn print_usage() {
    eprintln!(
        "Usage: oxidexd [--foreground] [--debug] [--socket <path>] [--config <path>] [--log-level <level>]"
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

fn serve(socket_path: PathBuf, state: Arc<Mutex<DaemonState>>) -> anyhow::Result<()> {
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
        tracing::info!(socket = %socket_path.display(), "listening");
        listener
    };

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                tracing::debug!("accepted daemon client connection");
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, state) {
                        tracing::warn!(error = %format!("{err:#}"), "daemon client error");
                    }
                });
            }
            Err(err) => tracing::warn!(error = %err, "accept failed"),
        }
    }
    Ok(())
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

fn handle_client(mut stream: UnixStream, state: Arc<Mutex<DaemonState>>) -> anyhow::Result<()> {
    ensure_same_uid_client(&stream)?;
    tracing::debug!("daemon client passed same-UID check");
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
        tracing::debug!(id, method = %method, "daemon request received");
        let response = match handle_request(frame, &state) {
            Ok((result, payload)) => {
                tracing::debug!(id, method = %method, payload_bytes = payload.len(), "daemon request completed");
                IpcFrame::ok(id, result, payload)
            }
            Err(err) => {
                tracing::warn!(id, method = %method, error = %format!("{err:#}"), "daemon request failed");
                IpcFrame::error(Some(id), "request_failed", format!("{err:#}"))
            }
        };
        write_frame(&mut stream, &response)?;
    }
    tracing::debug!("daemon client disconnected");
    Ok(())
}

fn ensure_same_uid_client(stream: &UnixStream) -> anyhow::Result<()> {
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
    let current_uid = unsafe { libc::geteuid() };
    anyhow::ensure!(
        cred.uid == current_uid,
        "rejecting daemon client uid {}; expected {}",
        cred.uid,
        current_uid
    );
    Ok(())
}

fn handle_request(
    frame: IpcFrame,
    state: &Arc<Mutex<DaemonState>>,
) -> anyhow::Result<(Value, Vec<u8>)> {
    let method = frame
        .header
        .method
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing request method"))?;
    match method {
        "daemon.hello" => Ok((
            json!({
                "name": "oxidexd",
                "version": VERSION,
                "protocol": 1,
            }),
            Vec::new(),
        )),
        "daemon.status" => {
            let mut state = state.lock().unwrap();
            state.refresh_devices();
            let result = DaemonStatus {
                version: VERSION.to_owned(),
                index_count: state.indexes.len(),
                device_count: state.devices.len(),
                scanner_socket: state.scanner_socket.display().to_string(),
                scanner_status: Some(state.scanner_status.clone()),
            };
            Ok((serde_json::to_value(result)?, Vec::new()))
        }
        "daemon.doctor" => {
            let state = state.lock().unwrap();
            let mut report = run_local_doctor(&DoctorOptions {
                scanner_socket: Some(state.scanner_socket.clone()),
                ..DoctorOptions::default()
            });
            report.add(
                oxidex_core::doctor::DoctorCategory::Daemon,
                oxidex_core::doctor::DoctorSeverity::Ok,
                "daemon_running",
                "oxidexd accepted this doctor request",
            );
            Ok((
                serde_json::to_value(DaemonDoctorResult { report })?,
                Vec::new(),
            ))
        }
        "device.list" => {
            let mut state = state.lock().unwrap();
            state.refresh_devices();
            Ok((serde_json::to_value(state.device_summaries())?, Vec::new()))
        }
        "index.list" => {
            let state = state.lock().unwrap();
            Ok((serde_json::to_value(state.index_summaries())?, Vec::new()))
        }
        "index.forget" => {
            let params: IndexForgetParams = params_as(&frame)?;
            let mut state = state.lock().unwrap();
            state
                .indexes
                .retain(|idx| idx.metadata.device_id != params.device_id);
            snapshot::delete_index(&params.device_id)?;
            state.index_states.remove(&params.device_id);
            state.watch_summaries.remove(&params.device_id);
            Ok((serde_json::to_value(state.index_summaries())?, Vec::new()))
        }
        "index.start_scan" => {
            let params: IndexStartScanParams = params_as(&frame)?;
            let result = start_scan_job(state.clone(), params.device_id)?;
            Ok((serde_json::to_value(result)?, Vec::new()))
        }
        "index.job_list" => {
            let state = state.lock().unwrap();
            Ok((serde_json::to_value(state.job_summaries())?, Vec::new()))
        }
        "index.job_status" => {
            let params: IndexJobStatusParams = params_as(&frame)?;
            let state = state.lock().unwrap();
            let job = state
                .jobs
                .get(&params.job_id)
                .map(|job| job.summary.clone())
                .ok_or_else(|| anyhow::anyhow!("unknown scan job {}", params.job_id))?;
            Ok((serde_json::to_value(job)?, Vec::new()))
        }
        "index.cancel_scan" => {
            let params: IndexCancelScanParams = params_as(&frame)?;
            let result = cancel_scan_job(state, &params);
            Ok((serde_json::to_value(result)?, Vec::new()))
        }
        "search.query" => {
            let params: SearchQueryParams = params_as(&frame)?;
            let mut state = state.lock().unwrap();
            state.refresh_devices();
            let request = match params.request {
                Some(request) => request,
                None => parse_search_query(&params.query)?,
            };
            let limit = params
                .max_results
                .unwrap_or(state.config.search.max_results);
            let hits = merge_search_request(
                &state.indexes,
                params.device_filter.as_deref(),
                &request,
                params.sort_key,
                params.sort_direction,
            );
            let truncated = hits.len() > limit;
            let rows = state.rows_for_hits(hits.into_iter().take(limit));
            Ok((
                serde_json::to_value(SearchQueryResult { rows, truncated })?,
                Vec::new(),
            ))
        }
        "search.explain" => {
            let params: SearchExplainParams = params_as(&frame)?;
            Ok((
                serde_json::to_value(explain_search_query(&params.query)?)?,
                Vec::new(),
            ))
        }
        "config.get" => {
            let state = state.lock().unwrap();
            Ok((
                serde_json::to_value(ConfigGetResult {
                    path: state.config_path.display().to_string(),
                    config: state.config.clone(),
                })?,
                Vec::new(),
            ))
        }
        "config.set" => {
            let params: ConfigSetParams = params_as(&frame)?;
            let mut state = state.lock().unwrap();
            let mut next_config = state.config.clone();
            apply_config_set(&mut next_config, &params)?;
            validate_config(&next_config)?;
            save_config_to_path(&state.config_path, &next_config)?;
            state.config = next_config;
            let path = state.config_path.display().to_string();
            Ok((
                serde_json::to_value(ConfigGetResult {
                    path,
                    config: state.config.clone(),
                })?,
                Vec::new(),
            ))
        }
        "config.validate" => {
            let params: ConfigValidateParams = params_as(&frame)?;
            validate_config(&params.config)?;
            Ok((json!({"valid": true}), Vec::new()))
        }
        "config.reload" => {
            let mut state = state.lock().unwrap();
            state.config = load_config_from_path(&state.config_path)?;
            Ok((
                serde_json::to_value(ConfigGetResult {
                    path: state.config_path.display().to_string(),
                    config: state.config.clone(),
                })?,
                Vec::new(),
            ))
        }
        "open.resolve_path" => {
            let params: ResolvePathParams = params_as(&frame)?;
            let mut state = state.lock().unwrap();
            state.refresh_devices();
            let resolved = state.resolve_path(&params)?;
            Ok((serde_json::to_value(resolved)?, Vec::new()))
        }
        "watch.status" => {
            let state = state.lock().unwrap();
            let mut summaries: Vec<_> = state.watch_summaries.values().cloned().collect();
            summaries.sort_by(|a, b| a.device_id.cmp(&b.device_id));
            Ok((serde_json::to_value(summaries)?, Vec::new()))
        }
        other => anyhow::bail!("unknown method: {other}"),
    }
}

struct DaemonState {
    config_path: PathBuf,
    config: AppConfig,
    indexes: Vec<SearchIndex>,
    index_states: HashMap<String, snapshot::IndexStateV1>,
    devices: Vec<DeviceInfo>,
    scanner_socket: PathBuf,
    scanner_status: ScannerStatusDetail,
    jobs: HashMap<u64, DaemonScanJob>,
    watch_summaries: HashMap<String, WatchSummary>,
    next_job_id: u64,
}

struct DaemonScanJob {
    summary: ScanJobSummary,
    cancellation: ScanCancellation,
}

impl DaemonState {
    fn load(config_path: PathBuf, scanner_socket: PathBuf) -> anyhow::Result<Self> {
        let config = load_config_from_path(&config_path)?;
        let indexes = snapshot::load_all_indexes()?;
        let index_states: HashMap<String, snapshot::IndexStateV1> =
            snapshot::load_all_index_states()?
                .into_iter()
                .map(|mut state| {
                    if normalize_watch_setup_sidecar(&mut state) {
                        let _ = snapshot::save_index_state(&state);
                    }
                    (state.device_id.clone(), state)
                })
                .collect();
        let devices = list_known_devices().unwrap_or_default();
        tracing::debug!(
            config = %config_path.display(),
            indexes = indexes.len(),
            index_states = index_states.len(),
            devices = devices.len(),
            "loaded daemon state"
        );
        Ok(Self {
            config_path,
            config,
            indexes,
            index_states,
            devices,
            scanner_status: ScannerStatusDetail {
                socket: scanner_socket.display().to_string(),
                reachable: false,
                access_model: "unix_group_socket".into(),
                peer_uid: None,
                peer_gid: None,
                last_error: None,
            },
            scanner_socket,
            jobs: HashMap::new(),
            watch_summaries: HashMap::new(),
            next_job_id: 1,
        })
    }

    fn refresh_devices(&mut self) {
        self.devices = list_known_devices().unwrap_or_default();
    }

    fn sort_indexes(&mut self) {
        self.indexes
            .sort_by(|a, b| a.metadata.device_id.cmp(&b.metadata.device_id));
    }

    fn next_job_id(&mut self) -> u64 {
        let id = self.next_job_id;
        self.next_job_id = self.next_job_id.saturating_add(1);
        id
    }

    fn index_summaries(&self) -> Vec<IndexSummary> {
        self.indexes
            .iter()
            .map(|index| {
                let stale = self
                    .index_states
                    .get(&index.metadata.device_id)
                    .and_then(|state| state.stale_reason.as_ref())
                    .is_some();
                let summary = IndexSummary::from_index(index, stale);
                self.index_states
                    .get(&index.metadata.device_id)
                    .map(|state| summary.clone().with_state(state.summary()))
                    .unwrap_or(summary)
            })
            .collect()
    }

    fn job_summaries(&self) -> Vec<ScanJobSummary> {
        let mut jobs: Vec<_> = self.jobs.values().map(|job| job.summary.clone()).collect();
        jobs.sort_by_key(|job| job.job_id);
        jobs
    }

    fn device_summaries(&self) -> Vec<DeviceSummary> {
        self.devices
            .iter()
            .map(|device| {
                let index = self
                    .indexes
                    .iter()
                    .find(|index| index.metadata.device_id == device.metadata.device_id);
                DeviceSummary::from_device(device, index)
            })
            .collect()
    }

    fn rows_for_hits(&self, hits: impl Iterator<Item = SearchHit>) -> Vec<SearchResultRow> {
        hits.filter_map(|hit| {
            let index = self
                .indexes
                .iter()
                .find(|index| index.metadata.device_id == hit.device_id)?;
            let rec_idx = hit.record_idx;
            let record = index.records.get(rec_idx as usize)?;
            let device = self
                .devices
                .iter()
                .find(|device| device.metadata.device_id == hit.device_id);
            let (mounted, mount_point) = device
                .map(|device| (device.mounted, device.primary_mount_point.as_str()))
                .unwrap_or((false, ""));
            Some(SearchResultRow {
                hit,
                name: index.name(rec_idx).to_owned(),
                display_path: index.display_path(rec_idx, mounted, mount_point),
                internal_path: index.internal_path(rec_idx).to_owned(),
                device_label: device_label(index),
                fs_type: index.metadata.fs_type,
                size: record.size,
                mtime: record.mtime,
                is_dir: record.is_dir(),
                is_symlink: record.is_symlink(),
                mounted,
                last_indexed_time: index.last_indexed_time,
            })
        })
        .collect()
    }

    fn resolve_path(&self, params: &ResolvePathParams) -> anyhow::Result<ResolvedPath> {
        let index = self
            .indexes
            .iter()
            .find(|index| index.metadata.device_id == params.device_id)
            .ok_or_else(|| anyhow::anyhow!("index not loaded for {}", params.device_id))?;
        anyhow::ensure!(
            (params.record_idx as usize) < index.records.len(),
            "record index out of bounds"
        );
        let device = self
            .devices
            .iter()
            .find(|device| device.metadata.device_id == params.device_id);
        let (mounted, mount_point) = device
            .map(|device| (device.mounted, device.primary_mount_point.as_str()))
            .unwrap_or((false, ""));
        Ok(ResolvedPath {
            device_id: params.device_id.clone(),
            record_idx: params.record_idx,
            mounted,
            path: index.display_path(params.record_idx, mounted, mount_point),
        })
    }
}

fn start_scan_job(
    state: Arc<Mutex<DaemonState>>,
    device_id: String,
) -> anyhow::Result<IndexStartScanResult> {
    let (job_id, device, cancellation) = {
        let mut state = state.lock().unwrap();
        state.refresh_devices();
        let device = state
            .devices
            .iter()
            .find(|device| device.metadata.device_id == device_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown device {device_id}"))?;
        if let Some(device_config) = state
            .config
            .devices
            .iter()
            .find(|entry| entry.device_id == device.metadata.device_id)
        {
            anyhow::ensure!(device_config.enabled, "device is disabled in config");
        }

        let job_id = state.next_job_id();
        let cancellation = ScanCancellation::new();
        state.jobs.insert(
            job_id,
            DaemonScanJob {
                summary: ScanJobSummary {
                    job_id,
                    device_id: device_id.clone(),
                    state: ScanState::Queued,
                    progress: 0,
                    message: "Queued".into(),
                    started_time: None,
                    finished_time: None,
                    result: None,
                    error: None,
                },
                cancellation: cancellation.clone(),
            },
        );
        tracing::info!(
            job_id,
            device_id = %device_id,
            dev_node = %device.metadata.dev_node,
            fs_type = device.metadata.fs_type.as_str(),
            "queued scan job"
        );

        (job_id, device, cancellation)
    };

    let worker_state = ScanWorkerState {
        state: state.clone(),
        device,
        job_id,
        cancellation: cancellation.clone(),
    };
    thread::spawn(move || run_scan_worker(worker_state));

    Ok(IndexStartScanResult {
        job_id,
        device_id,
        state: ScanState::Queued,
    })
}

struct ScanWorkerState {
    state: Arc<Mutex<DaemonState>>,
    device: DeviceInfo,
    job_id: u64,
    cancellation: ScanCancellation,
}

fn run_scan_worker(worker: ScanWorkerState) {
    if wait_for_scan_slot(&worker).is_err() {
        return;
    }

    let start = Instant::now();
    tracing::info!(
        job_id = worker.job_id,
        device_id = %worker.device.metadata.device_id,
        "scan worker started"
    );
    let (config, scanner_socket) = {
        let state = worker.state.lock().unwrap();
        (state.config.clone(), state.scanner_socket.clone())
    };

    let mut progress = |percent| update_daemon_job_progress(&worker.state, worker.job_id, percent);
    let result = scan_and_index_device(
        &worker.device,
        &config,
        &scanner_socket,
        &worker.cancellation,
        &mut progress,
    );

    match result {
        Ok((index, scanner_label)) => {
            finish_daemon_job_success(worker, index, scanner_label, start)
        }
        Err(err) => {
            tracing::warn!(
                job_id = worker.job_id,
                device_id = %worker.device.metadata.device_id,
                error = %format!("{err:#}"),
                "scan worker failed"
            );
            finish_daemon_job_failed(worker, format!("{err:#}"), start)
        }
    }
}

fn wait_for_scan_slot(worker: &ScanWorkerState) -> anyhow::Result<()> {
    loop {
        {
            let mut state = worker.state.lock().unwrap();
            let max_parallel = state.config.indexing.max_parallel_scans.max(1);
            let running = state
                .jobs
                .values()
                .filter(|job| job.summary.state == ScanState::Running)
                .count();
            let Some(job) = state.jobs.get_mut(&worker.job_id) else {
                return Err(anyhow::anyhow!("scan job disappeared"));
            };
            if job.summary.state == ScanState::Cancelled || worker.cancellation.is_cancelled() {
                job.summary.state = ScanState::Cancelled;
                job.summary.finished_time = Some(now_unix());
                job.summary.message = "Cancelled before start".into();
                return Err(anyhow::anyhow!("scan cancelled"));
            }
            if running < max_parallel {
                job.summary.state = ScanState::Running;
                job.summary.started_time = Some(now_unix());
                job.summary.message = "Running".into();
                tracing::debug!(job_id = worker.job_id, "scan job acquired running slot");
                return Ok(());
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn update_daemon_job_progress(state: &Arc<Mutex<DaemonState>>, job_id: u64, percent: u8) {
    if let Some(job) = state.lock().unwrap().jobs.get_mut(&job_id)
        && job.summary.state == ScanState::Running
    {
        job.summary.progress = percent;
        job.summary.message = format!("Scanning: {percent}%");
    }
}

fn finish_daemon_job_success(
    worker: ScanWorkerState,
    index: SearchIndex,
    scanner_label: String,
    start: Instant,
) {
    if worker.cancellation.is_cancelled() {
        finish_daemon_job_failed(worker, "scan cancelled".into(), start);
        return;
    }

    let device_id = index.metadata.device_id.clone();
    let entry_count = index.records.len();
    let saved = snapshot::save_index(&index);
    match saved {
        Ok(path) => {
            let snapshot_size = fs::metadata(&path).ok().map(|meta| meta.len());
            let mut state_sidecar = snapshot::IndexStateV1::new(device_id.clone());
            state_sidecar.last_success_time = Some(now_unix());
            state_sidecar.last_scan_duration_ms = Some(start.elapsed().as_millis() as u64);
            state_sidecar.last_scanner = Some(scanner_label.clone());
            state_sidecar.last_entry_count = Some(entry_count);
            state_sidecar.snapshot_size = snapshot_size;
            state_sidecar.live_watch_state = Some("pending".into());
            let _ = snapshot::save_index_state(&state_sidecar);

            let mut state = worker.state.lock().unwrap();
            state
                .indexes
                .retain(|idx| idx.metadata.device_id != device_id);
            state.indexes.push(index);
            state.sort_indexes();
            state
                .index_states
                .insert(device_id.clone(), state_sidecar.clone());
            state.scanner_status.reachable = scanner_label == "scannerd";
            state.scanner_status.access_model = "unix_group_socket".into();
            state.scanner_status.last_error = None;
            let result_summary = state
                .indexes
                .iter()
                .find(|index| index.metadata.device_id == device_id)
                .map(|index| {
                    IndexSummary::from_index(index, false).with_state(state_sidecar.summary())
                });
            if let Some(job) = state.jobs.get_mut(&worker.job_id) {
                job.summary.state = ScanState::Finished;
                job.summary.progress = 100;
                job.summary.finished_time = Some(now_unix());
                job.summary.message = format!("Indexed {entry_count} entries");
                job.summary.result = result_summary;
            }
            tracing::info!(
                job_id = worker.job_id,
                device_id = %device_id,
                entries = entry_count,
                scanner = %scanner_label,
                snapshot = %path.display(),
                duration_ms = start.elapsed().as_millis(),
                "scan job finished"
            );
        }
        Err(err) => finish_daemon_job_failed(worker, format!("{err:#}"), start),
    }
}

fn finish_daemon_job_failed(worker: ScanWorkerState, message: String, start: Instant) {
    let cancelled = worker.cancellation.is_cancelled() || message.contains("scan cancelled");
    let device_id = worker.device.metadata.device_id.clone();
    let mut state_sidecar = snapshot::load_index_state(&device_id)
        .ok()
        .flatten()
        .unwrap_or_else(|| snapshot::IndexStateV1::new(device_id.clone()));
    state_sidecar.last_failure_time = Some(now_unix());
    state_sidecar.last_scan_duration_ms = Some(start.elapsed().as_millis() as u64);
    state_sidecar.last_error = Some(message.clone());
    state_sidecar.stale_reason = Some(if cancelled {
        "scan_cancelled".into()
    } else {
        "scan_failed".into()
    });
    let _ = snapshot::save_index_state(&state_sidecar);

    let mut state = worker.state.lock().unwrap();
    state.index_states.insert(device_id.clone(), state_sidecar);
    state.scanner_status.last_error = Some(message.clone());
    if let Some(job) = state.jobs.get_mut(&worker.job_id) {
        job.summary.state = if cancelled {
            ScanState::Cancelled
        } else {
            ScanState::Failed
        };
        job.summary.finished_time = Some(now_unix());
        job.summary.message = message.clone();
        job.summary.error = Some(message);
    }
    tracing::warn!(
        job_id = worker.job_id,
        device_id = %device_id,
        cancelled,
        duration_ms = start.elapsed().as_millis(),
        "scan job failed"
    );
}

fn cancel_scan_job(
    state: &Arc<Mutex<DaemonState>>,
    params: &IndexCancelScanParams,
) -> IndexCancelScanResult {
    let mut state = state.lock().unwrap();
    let job_id = params.job_id.or_else(|| {
        params.device_id.as_ref().and_then(|device_id| {
            state
                .jobs
                .values()
                .find(|job| {
                    job.summary.device_id == *device_id
                        && matches!(job.summary.state, ScanState::Queued | ScanState::Running)
                })
                .map(|job| job.summary.job_id)
        })
    });
    let Some(job_id) = job_id else {
        return IndexCancelScanResult {
            cancelled: false,
            message: "no matching queued or running scan job".into(),
        };
    };
    let Some(job) = state.jobs.get_mut(&job_id) else {
        return IndexCancelScanResult {
            cancelled: false,
            message: "unknown scan job".into(),
        };
    };
    job.cancellation.cancel();
    if job.summary.state == ScanState::Queued {
        job.summary.state = ScanState::Cancelled;
        job.summary.finished_time = Some(now_unix());
    }
    job.summary.message = "Cancel requested".into();
    IndexCancelScanResult {
        cancelled: true,
        message: format!("cancel requested for scan job {job_id}"),
    }
}

fn start_watch_manager(state: Arc<Mutex<DaemonState>>) {
    thread::spawn(move || watch_manager_loop(state));
}

struct DirtyState {
    first_dirty: Instant,
    last_dirty: Instant,
}

struct ScannerWatchConnection {
    stream: UnixStream,
    next_id: u64,
    active: HashMap<String, ScannerWatchId>,
}

fn watch_manager_loop(state: Arc<Mutex<DaemonState>>) {
    let mut connection: Option<ScannerWatchConnection> = None;
    let mut dirty: HashMap<String, DirtyState> = HashMap::new();
    let mut last_reconcile = Instant::now() - Duration::from_secs(60);

    loop {
        if connection.is_none() {
            match connect_scanner_watch_session(&state) {
                Ok(conn) => connection = Some(conn),
                Err(err) => {
                    mark_scanner_connection_error(&state, format!("{err:#}"));
                    flush_dirty_indexes(&state, &mut dirty);
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
            }
        }

        let mut drop_connection = false;
        if let Some(conn) = connection.as_mut() {
            if let Err(err) = drain_scanner_watch_frames(conn, &state, &mut dirty) {
                let active = conn.active.clone();
                mark_active_watches_desynced(&state, &active, format!("{err:#}"));
                drop_connection = true;
            } else if last_reconcile.elapsed() >= Duration::from_secs(5) {
                if let Err(err) = reconcile_scanner_watches(&state, conn, &mut dirty) {
                    let active = conn.active.clone();
                    mark_active_watches_desynced(&state, &active, format!("{err:#}"));
                    drop_connection = true;
                }
                last_reconcile = Instant::now();
            }
        }
        if drop_connection {
            connection = None;
        }

        flush_dirty_indexes(&state, &mut dirty);
        thread::sleep(Duration::from_millis(200));
    }
}

#[derive(Clone)]
struct DesiredWatch {
    device_id: String,
    device_path: String,
    fs_type: oxidex_core::model::FsType,
    mount_point: String,
    watched_directories: usize,
}

fn connect_scanner_watch_session(
    state: &Arc<Mutex<DaemonState>>,
) -> anyhow::Result<ScannerWatchConnection> {
    let socket_path = state.lock().unwrap().scanner_socket.clone();
    let stream = UnixStream::connect(&socket_path).map_err(|err| {
        if err.kind() == ErrorKind::PermissionDenied {
            anyhow::anyhow!(
                "permission denied connecting to scanner daemon socket {}; ensure it is root:oxidex mode 0660 and this user is in the oxidex group",
                socket_path.display()
            )
        } else {
            anyhow::Error::from(err)
        }
    })?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    {
        let mut state = state.lock().unwrap();
        state.scanner_status.reachable = true;
        state.scanner_status.access_model = "unix_group_socket".into();
        state.scanner_status.last_error = None;
    }
    tracing::debug!(socket = %socket_path.display(), "scanner watch session connected");
    Ok(ScannerWatchConnection {
        stream,
        next_id: 1,
        active: HashMap::new(),
    })
}

fn reconcile_scanner_watches(
    state: &Arc<Mutex<DaemonState>>,
    conn: &mut ScannerWatchConnection,
    dirty: &mut HashMap<String, DirtyState>,
) -> anyhow::Result<()> {
    let desired = desired_watches(state);
    let desired_ids: HashSet<_> = desired
        .iter()
        .map(|watch| watch.device_id.clone())
        .collect();
    let current_ids: Vec<_> = conn.active.keys().cloned().collect();
    for device_id in current_ids {
        if !desired_ids.contains(&device_id) {
            if let Some(watch_id) = conn.active.remove(&device_id) {
                let params = ScannerStopWatchParams { watch_id };
                let _ = scanner_watch_request(
                    conn,
                    "scanner.stop_watch",
                    serde_json::to_value(params)?,
                    state,
                    dirty,
                );
            }
            state.lock().unwrap().watch_summaries.remove(&device_id);
        }
    }

    for desired in desired {
        if conn.active.contains_key(&desired.device_id) {
            let dirty = state
                .lock()
                .unwrap()
                .watch_summaries
                .get(&desired.device_id)
                .map(|summary| summary.dirty)
                .unwrap_or(false);
            update_watch_summary(
                state,
                WatchSummary {
                    device_id: desired.device_id,
                    mounted: true,
                    enabled: true,
                    state: "watching".into(),
                    watched_directories: desired.watched_directories,
                    dirty,
                    last_error: None,
                },
            );
            continue;
        }
        let params = ScannerStartWatchParams {
            device_id: desired.device_id.clone(),
            device_path: desired.device_path.clone(),
            fs_type: desired.fs_type,
            mount_point: desired.mount_point.clone(),
        };
        match scanner_watch_request(
            conn,
            "scanner.start_watch",
            serde_json::to_value(params)?,
            state,
            dirty,
        )
        .and_then(|frame| result_as::<ScannerStartWatchResult>(&frame))
        {
            Ok(result) => {
                conn.active
                    .insert(desired.device_id.clone(), result.watch_id);
                update_watch_summary(
                    state,
                    WatchSummary {
                        device_id: desired.device_id,
                        mounted: true,
                        enabled: true,
                        state: "watching".into(),
                        watched_directories: desired.watched_directories,
                        dirty: false,
                        last_error: None,
                    },
                );
            }
            Err(err) => mark_watch_setup_unavailable(state, &desired.device_id, format!("{err:#}")),
        }
    }
    Ok(())
}

fn desired_watches(state: &Arc<Mutex<DaemonState>>) -> Vec<DesiredWatch> {
    let mut state = state.lock().unwrap();
    state.refresh_devices();
    if !state.config.indexing.watch_mounted {
        return Vec::new();
    }
    state
        .indexes
        .iter()
        .filter_map(|index| {
            if state
                .index_states
                .get(&index.metadata.device_id)
                .and_then(|state| state.stale_reason.as_deref())
                .map(|reason| reason.starts_with("watch_"))
                .unwrap_or(false)
            {
                return None;
            }
            if state
                .index_states
                .get(&index.metadata.device_id)
                .and_then(|state| state.live_watch_state.as_deref())
                .is_some_and(|watch_state| watch_state == "unavailable")
            {
                return None;
            }
            let device = state
                .devices
                .iter()
                .find(|device| device.metadata.device_id == index.metadata.device_id)?;
            if !device.mounted || device.primary_mount_point.trim().is_empty() {
                return None;
            }
            Some(DesiredWatch {
                device_id: index.metadata.device_id.clone(),
                device_path: device.metadata.dev_node.clone(),
                fs_type: device.metadata.fs_type,
                mount_point: device.primary_mount_point.clone(),
                watched_directories: index
                    .records
                    .iter()
                    .filter(|record| record.is_dir())
                    .count(),
            })
        })
        .collect()
}

fn scanner_watch_request(
    conn: &mut ScannerWatchConnection,
    method: &str,
    params: Value,
    state: &Arc<Mutex<DaemonState>>,
    dirty: &mut HashMap<String, DirtyState>,
) -> anyhow::Result<IpcFrame> {
    let id = conn.next_id;
    conn.next_id = conn.next_id.saturating_add(1);
    write_frame(&mut conn.stream, &IpcFrame::request(id, method, params))?;
    loop {
        let Some(frame) = read_scanner_watch_frame(&mut conn.stream)? else {
            continue;
        };
        if frame.header.id == Some(id) {
            return Ok(frame);
        }
        handle_scanner_watch_frame(state, dirty, frame)?;
    }
}

fn drain_scanner_watch_frames(
    conn: &mut ScannerWatchConnection,
    state: &Arc<Mutex<DaemonState>>,
    dirty: &mut HashMap<String, DirtyState>,
) -> anyhow::Result<()> {
    while let Some(frame) = read_scanner_watch_frame(&mut conn.stream)? {
        handle_scanner_watch_frame(state, dirty, frame)?;
    }
    Ok(())
}

fn read_scanner_watch_frame(stream: &mut UnixStream) -> anyhow::Result<Option<IpcFrame>> {
    match read_frame(&mut *stream) {
        Ok(frame) => Ok(frame),
        Err(err) if is_timeout_error(&err) => Ok(None),
        Err(err) => Err(err),
    }
}

fn is_timeout_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .map(|err| matches!(err.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock))
        .unwrap_or(false)
}

fn handle_scanner_watch_frame(
    state: &Arc<Mutex<DaemonState>>,
    dirty: &mut HashMap<String, DirtyState>,
    frame: IpcFrame,
) -> anyhow::Result<()> {
    let Some(event) = frame.header.event.as_deref() else {
        return Ok(());
    };
    match event {
        "scanner.watch_event" => {
            let event: ScannerWatchEvent =
                serde_json::from_value(frame.header.params.unwrap_or(Value::Null))?;
            if apply_scanner_watch_event(state, &event)? {
                let now = Instant::now();
                dirty
                    .entry(event.device_id.clone())
                    .and_modify(|entry| entry.last_dirty = now)
                    .or_insert(DirtyState {
                        first_dirty: now,
                        last_dirty: now,
                    });
                set_watch_dirty(state, &event.device_id, true);
            }
        }
        "scanner.watch_error" => {
            let event: ScannerWatchErrorEvent =
                serde_json::from_value(frame.header.params.unwrap_or(Value::Null))?;
            mark_watch_error(state, &event.device_id, event.message);
        }
        "scanner.watch_stopped" => {
            let event: ScannerWatchStoppedEvent =
                serde_json::from_value(frame.header.params.unwrap_or(Value::Null))?;
            state
                .lock()
                .unwrap()
                .watch_summaries
                .entry(event.device_id.clone())
                .and_modify(|summary| {
                    summary.state = "stopped".into();
                    summary.last_error = Some(event.reason.clone());
                });
        }
        _ => {}
    }
    Ok(())
}

fn apply_scanner_watch_event(
    state: &Arc<Mutex<DaemonState>>,
    event: &ScannerWatchEvent,
) -> anyhow::Result<bool> {
    match event.kind {
        ScannerWatchEventKind::Created => {
            let Some(metadata) = event.metadata.clone() else {
                return Ok(false);
            };
            apply_live_created(
                state,
                &event.device_id,
                event.internal_path.clone(),
                live_metadata(metadata),
            )
        }
        ScannerWatchEventKind::Removed => {
            let mut state = state.lock().unwrap();
            let Some(index) = state
                .indexes
                .iter_mut()
                .find(|index| index.metadata.device_id == event.device_id)
            else {
                return Ok(false);
            };
            index.apply_live_event(LiveUpdateEvent::Removed {
                internal_path: event.internal_path.clone(),
            })
        }
        ScannerWatchEventKind::Renamed => {
            let Some(old_internal) = event.old_internal_path.clone() else {
                return Ok(false);
            };
            let Some(metadata) = event.metadata.clone() else {
                let mut state = state.lock().unwrap();
                let Some(index) = state
                    .indexes
                    .iter_mut()
                    .find(|index| index.metadata.device_id == event.device_id)
                else {
                    return Ok(false);
                };
                return index.apply_live_event(LiveUpdateEvent::Removed {
                    internal_path: old_internal,
                });
            };
            if !rules_allow(
                &state.lock().unwrap(),
                &event.device_id,
                &event.internal_path,
            )? {
                let mut state = state.lock().unwrap();
                let Some(index) = state
                    .indexes
                    .iter_mut()
                    .find(|index| index.metadata.device_id == event.device_id)
                else {
                    return Ok(false);
                };
                return index.apply_live_event(LiveUpdateEvent::Removed {
                    internal_path: old_internal,
                });
            }
            let mut state = state.lock().unwrap();
            let Some(index) = state
                .indexes
                .iter_mut()
                .find(|index| index.metadata.device_id == event.device_id)
            else {
                return Ok(false);
            };
            index.apply_live_event(LiveUpdateEvent::Renamed {
                old_internal_path: old_internal,
                new_internal_path: event.internal_path.clone(),
                metadata: live_metadata(metadata),
            })
        }
        ScannerWatchEventKind::Metadata => {
            let Some(metadata) = event.metadata.clone() else {
                return Ok(false);
            };
            let mut state = state.lock().unwrap();
            let Some(index) = state
                .indexes
                .iter_mut()
                .find(|index| index.metadata.device_id == event.device_id)
            else {
                return Ok(false);
            };
            index.apply_live_event(LiveUpdateEvent::Metadata {
                internal_path: event.internal_path.clone(),
                metadata: live_metadata(metadata),
            })
        }
    }
}

fn live_metadata(metadata: ScannerLiveMetadata) -> LiveRecordMetadata {
    LiveRecordMetadata {
        size: metadata.size,
        mtime: metadata.mtime,
        is_dir: metadata.is_dir,
        is_symlink: metadata.is_symlink,
    }
}

fn apply_live_created(
    state: &Arc<Mutex<DaemonState>>,
    device_id: &str,
    internal_path: String,
    metadata: LiveRecordMetadata,
) -> anyhow::Result<bool> {
    if !rules_allow(&state.lock().unwrap(), device_id, &internal_path)? {
        return Ok(false);
    }
    let mut state = state.lock().unwrap();
    let Some(index) = state
        .indexes
        .iter_mut()
        .find(|index| index.metadata.device_id == device_id)
    else {
        return Ok(false);
    };
    index.apply_live_event(LiveUpdateEvent::Created {
        internal_path,
        metadata,
    })
}

fn rules_allow(state: &DaemonState, device_id: &str, internal_path: &str) -> anyhow::Result<bool> {
    let Some(device_config) = state
        .config
        .devices
        .iter()
        .find(|device| device.device_id == device_id)
    else {
        return Ok(true);
    };
    if device_config.rules.is_empty() {
        return Ok(true);
    }
    let rules = compile_rules(&device_config.rules)?;
    Ok(rules.matches(internal_path, internal_name(internal_path)))
}

fn internal_name(internal_path: &str) -> &str {
    internal_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
}

fn mark_scanner_connection_error(state: &Arc<Mutex<DaemonState>>, message: String) {
    let mut state = state.lock().unwrap();
    state.scanner_status.reachable = false;
    state.scanner_status.access_model = "unix_group_socket".into();
    state.scanner_status.last_error = Some(message);
}

fn mark_active_watches_desynced(
    state: &Arc<Mutex<DaemonState>>,
    active: &HashMap<String, ScannerWatchId>,
    message: String,
) {
    mark_scanner_connection_error(state, message.clone());
    for device_id in active.keys() {
        mark_watch_error(state, device_id, message.clone());
    }
}

fn flush_dirty_indexes(state: &Arc<Mutex<DaemonState>>, dirty: &mut HashMap<String, DirtyState>) {
    let (flush_after, max_dirty, due): (Duration, Duration, Vec<String>) = {
        let state = state.lock().unwrap();
        let flush_after = Duration::from_secs(state.config.indexing.live_update_flush_seconds);
        let max_dirty = Duration::from_secs(state.config.indexing.live_update_max_dirty_seconds);
        let due = dirty
            .iter()
            .filter(|(_, entry)| {
                entry.last_dirty.elapsed() >= flush_after
                    || entry.first_dirty.elapsed() >= max_dirty
            })
            .map(|(device_id, _)| device_id.clone())
            .collect();
        (flush_after, max_dirty, due)
    };
    let _ = (flush_after, max_dirty);

    for device_id in due {
        let index = {
            let state = state.lock().unwrap();
            state
                .indexes
                .iter()
                .find(|index| index.metadata.device_id == device_id)
                .cloned()
        };
        let Some(index) = index else {
            dirty.remove(&device_id);
            continue;
        };
        match snapshot::save_index(&index) {
            Ok(path) => {
                let mut state = state.lock().unwrap();
                let snapshot_size = fs::metadata(&path).ok().map(|meta| meta.len());
                let state_sidecar = state
                    .index_states
                    .entry(device_id.clone())
                    .or_insert_with(|| snapshot::IndexStateV1::new(device_id.clone()));
                state_sidecar.snapshot_size = snapshot_size;
                state_sidecar.live_watch_state = Some("watching".into());
                let _ = snapshot::save_index_state(state_sidecar);
                set_watch_dirty_locked(&mut state, &device_id, false);
                dirty.remove(&device_id);
            }
            Err(err) => mark_watch_error(state, &device_id, format!("{err:#}")),
        }
    }
}

fn update_watch_summary(state: &Arc<Mutex<DaemonState>>, summary: WatchSummary) {
    state
        .lock()
        .unwrap()
        .watch_summaries
        .insert(summary.device_id.clone(), summary);
}

fn set_watch_dirty(state: &Arc<Mutex<DaemonState>>, device_id: &str, dirty: bool) {
    set_watch_dirty_locked(&mut state.lock().unwrap(), device_id, dirty);
}

fn set_watch_dirty_locked(state: &mut DaemonState, device_id: &str, dirty: bool) {
    if let Some(summary) = state.watch_summaries.get_mut(device_id) {
        summary.dirty = dirty;
    }
}

fn mark_watch_error(state: &Arc<Mutex<DaemonState>>, device_id: &str, message: String) {
    tracing::warn!(
        device_id,
        error = %message,
        "mounted live watcher desynchronized"
    );
    let mut state = state.lock().unwrap();
    let sidecar = state
        .index_states
        .entry(device_id.to_owned())
        .or_insert_with(|| snapshot::IndexStateV1::new(device_id.to_owned()));
    sidecar.stale_reason = Some("watch_desync".into());
    sidecar.live_watch_state = Some("error".into());
    sidecar.last_error = Some(message.clone());
    let _ = snapshot::save_index_state(sidecar);
    state
        .watch_summaries
        .entry(device_id.to_owned())
        .and_modify(|summary| {
            summary.state = "error".into();
            summary.last_error = Some(message.clone());
        })
        .or_insert(WatchSummary {
            device_id: device_id.to_owned(),
            mounted: true,
            enabled: true,
            state: "error".into(),
            watched_directories: 0,
            dirty: false,
            last_error: Some(message),
        });
}

fn mark_watch_setup_unavailable(state: &Arc<Mutex<DaemonState>>, device_id: &str, message: String) {
    tracing::warn!(
        device_id,
        error = %message,
        "mounted live watcher unavailable"
    );
    let mut state = state.lock().unwrap();
    let sidecar = state
        .index_states
        .entry(device_id.to_owned())
        .or_insert_with(|| snapshot::IndexStateV1::new(device_id.to_owned()));
    if sidecar.stale_reason.as_deref() == Some("watch_desync")
        && sidecar
            .last_error
            .as_deref()
            .is_some_and(is_permission_denied_message)
    {
        sidecar.stale_reason = None;
        sidecar.last_error = None;
    }
    sidecar.live_watch_state = Some("unavailable".into());
    let _ = snapshot::save_index_state(sidecar);
    state
        .watch_summaries
        .entry(device_id.to_owned())
        .and_modify(|summary| {
            summary.state = "unavailable".into();
            summary.last_error = Some(message.clone());
        })
        .or_insert(WatchSummary {
            device_id: device_id.to_owned(),
            mounted: true,
            enabled: true,
            state: "unavailable".into(),
            watched_directories: 0,
            dirty: false,
            last_error: Some(message),
        });
}

fn normalize_watch_setup_sidecar(state: &mut snapshot::IndexStateV1) -> bool {
    if state.stale_reason.as_deref() == Some("watch_desync")
        && state.live_watch_state.as_deref() == Some("error")
        && state
            .last_error
            .as_deref()
            .is_some_and(is_permission_denied_message)
    {
        state.stale_reason = None;
        state.last_error = None;
        state.live_watch_state = Some("unavailable".into());
        true
    } else {
        false
    }
}

fn is_permission_denied_message(message: &str) -> bool {
    message.to_ascii_lowercase().contains("permission denied")
}

fn device_label(index: &SearchIndex) -> String {
    if !index.metadata.label.trim().is_empty() {
        format!(
            "{} ({})",
            index.metadata.label.trim(),
            index.metadata.device_id
        )
    } else {
        index.metadata.device_id.clone()
    }
}

fn scan_and_index_device(
    device: &DeviceInfo,
    config: &AppConfig,
    scanner_socket: &Path,
    cancellation: &ScanCancellation,
    progress: &mut dyn FnMut(u8),
) -> anyhow::Result<(SearchIndex, String)> {
    if let Some(device_config) = config
        .devices
        .iter()
        .find(|entry| entry.device_id == device.metadata.device_id)
    {
        anyhow::ensure!(device_config.enabled, "device is disabled in config");
    }

    let scan = scan_with_scannerd(scanner_socket, device, cancellation, progress)?;
    tracing::debug!(
        device_id = %device.metadata.device_id,
        "scan completed through scanner daemon"
    );

    let mut index = SearchIndex::from_scan(device.metadata.clone(), scan, now_unix())?;
    if let Some(device_config) = config
        .devices
        .iter()
        .find(|entry| entry.device_id == device.metadata.device_id)
    {
        let rules = compile_rules(&device_config.rules)?;
        index = filter_index(index, &rules)?;
    }
    Ok((index, "scannerd".to_owned()))
}

fn scan_with_scannerd(
    socket_path: &Path,
    device: &DeviceInfo,
    cancellation: &ScanCancellation,
    progress: &mut dyn FnMut(u8),
) -> anyhow::Result<ScanDatabase> {
    tracing::debug!(
        socket = %socket_path.display(),
        device_id = %device.metadata.device_id,
        "connecting to scanner daemon"
    );
    let mut stream = UnixStream::connect(socket_path).map_err(|err| {
        if err.kind() == ErrorKind::PermissionDenied {
            anyhow::anyhow!(
                "permission denied connecting to scanner daemon socket {}; ensure the socket is owned by root:oxidex with mode 0660 and the user running oxidexd is in the oxidex group",
                socket_path.display()
            )
        } else {
            anyhow::Error::from(err)
        }
    })?;
    let params = ScannerStartScanParams {
        device_path: device.metadata.dev_node.clone(),
        fs_type: device.metadata.fs_type,
    };
    write_frame(
        &mut stream,
        &IpcFrame::request(1, "scanner.start_scan", serde_json::to_value(params)?),
    )?;
    let frame = read_until_response(&mut stream, 1, progress)?;
    let started: ScannerStartScanResult = result_as(&frame)?;
    tracing::info!(
        device_id = %device.metadata.device_id,
        scanner_job_id = started.job_id,
        "scanner daemon job started"
    );
    let mut next_id = 2u64;

    loop {
        if cancellation.is_cancelled() {
            let params = ScannerCancelScanParams {
                job_id: started.job_id,
            };
            write_frame(
                &mut stream,
                &IpcFrame::request(
                    next_id,
                    "scanner.cancel_scan",
                    serde_json::to_value(params)?,
                ),
            )?;
            let _ = read_until_response(&mut stream, next_id, progress);
            anyhow::bail!("scan cancelled");
        }

        let take_id = next_id.saturating_add(1);
        next_id = take_id;
        write_frame(
            &mut stream,
            &IpcFrame::request(
                take_id,
                "scanner.take_result",
                serde_json::to_value(ScannerTakeResultParams {
                    job_id: started.job_id,
                })?,
            ),
        )?;
        let frame = read_until_response(&mut stream, take_id, progress)?;
        if frame.header.ok == Some(true) {
            let _result: oxidex_core::daemon_model::ScannerTakeResultResult = result_as(&frame)?;
            tracing::debug!(
                device_id = %device.metadata.device_id,
                scanner_job_id = started.job_id,
                payload_bytes = frame.payload.len(),
                "scanner daemon result received"
            );
            return stream::read_scan_stream(&frame.payload[..]);
        }
        let message = frame
            .header
            .error
            .as_ref()
            .map(|err| err.message.clone())
            .unwrap_or_else(|| "scanner job not ready".into());
        if !message.contains("not finished") && !message.contains("not ready") {
            anyhow::bail!("{message}");
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn read_until_response(
    stream: &mut UnixStream,
    id: u64,
    progress: &mut dyn FnMut(u8),
) -> anyhow::Result<IpcFrame> {
    loop {
        let Some(frame) = read_frame(&mut *stream)? else {
            anyhow::bail!("scanner daemon closed connection");
        };
        if let Some(event) = frame.header.event.as_deref()
            && event == "scanner.scan_progress"
            && let Some(params) = frame.header.params.as_ref()
            && let Some(percent) = params.get("percent").and_then(|value| value.as_u64())
        {
            progress(percent.min(100) as u8);
        }
        if frame.header.id == Some(id) {
            return Ok(frame);
        }
    }
}

fn apply_config_set(config: &mut AppConfig, params: &ConfigSetParams) -> anyhow::Result<()> {
    match params.path.as_str() {
        "ui.theme" => {
            let value = params
                .value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("ui.theme must be a string"))?;
            config.ui.theme = match value {
                "system" => ThemeMode::System,
                "light" => ThemeMode::Light,
                "dark" => ThemeMode::Dark,
                other => anyhow::bail!("unsupported theme '{other}'"),
            };
        }
        "ui.language" => {
            let value = params
                .value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("ui.language must be a string"))?;
            config.ui.language = match value {
                "system" => LanguageMode::System,
                "en_us" | "en-US" | "en" => LanguageMode::EnUs,
                "zh_cn" | "zh-CN" | "zh" => LanguageMode::ZhCn,
                other => anyhow::bail!("unsupported language '{other}'"),
            };
        }
        "ui.show_filter_panel" => {
            config.ui.show_filter_panel = params
                .value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("ui.show_filter_panel must be a boolean"))?;
        }
        "ui.remember_window_size" => {
            config.ui.remember_window_size = params
                .value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("ui.remember_window_size must be a boolean"))?;
        }
        "ui.cjk_font_fallback" => {
            config.ui.cjk_font_fallback = params
                .value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("ui.cjk_font_fallback must be a boolean"))?;
        }
        "ui.cjk_preferred_font" => {
            config.ui.cjk_preferred_font = params
                .value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("ui.cjk_preferred_font must be a string"))?
                .trim()
                .to_owned();
        }
        "search.default_sort" => {
            let value = params
                .value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("search.default_sort must be a string"))?;
            config.search.default_sort = parse_sort_key(value)?;
        }
        "search.default_direction" => {
            let value = params
                .value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("search.default_direction must be a string"))?;
            config.search.default_direction = parse_sort_direction(value)?;
        }
        "search.max_results" => {
            config.search.max_results =
                params.value.as_u64().ok_or_else(|| {
                    anyhow::anyhow!("search.max_results must be a positive integer")
                })? as usize;
        }
        "indexing.scan_on_startup" => {
            config.indexing.scan_on_startup = params
                .value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("indexing.scan_on_startup must be a boolean"))?;
        }
        "indexing.auto_rescan_removable" => {
            config.indexing.auto_rescan_removable = params.value.as_bool().ok_or_else(|| {
                anyhow::anyhow!("indexing.auto_rescan_removable must be a boolean")
            })?;
        }
        "indexing.max_parallel_scans" => {
            config.indexing.max_parallel_scans = params.value.as_u64().ok_or_else(|| {
                anyhow::anyhow!("indexing.max_parallel_scans must be a positive integer")
            })? as usize;
        }
        "indexing.watch_mounted" => {
            config.indexing.watch_mounted = params
                .value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("indexing.watch_mounted must be a boolean"))?;
        }
        "indexing.live_update_flush_seconds" => {
            config.indexing.live_update_flush_seconds = params.value.as_u64().ok_or_else(|| {
                anyhow::anyhow!("indexing.live_update_flush_seconds must be a positive integer")
            })?;
        }
        "indexing.live_update_max_dirty_seconds" => {
            config.indexing.live_update_max_dirty_seconds =
                params.value.as_u64().ok_or_else(|| {
                    anyhow::anyhow!(
                        "indexing.live_update_max_dirty_seconds must be a positive integer"
                    )
                })?;
        }
        "rofi.max_results" => {
            config.rofi.max_results = params
                .value
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("rofi.max_results must be a positive integer"))?
                as usize;
        }
        "rofi.show_device_label" => {
            config.rofi.show_device_label = params
                .value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("rofi.show_device_label must be a boolean"))?;
        }
        "rofi.show_full_path" => {
            config.rofi.show_full_path = params
                .value
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("rofi.show_full_path must be a boolean"))?;
        }
        other => anyhow::bail!(
            "config.set currently supports scalar settings only; unsupported path '{other}'"
        ),
    }
    Ok(())
}

fn parse_sort_key(value: &str) -> anyhow::Result<SortKey> {
    match value {
        "relevance" | "rank" => Ok(SortKey::Relevance),
        "name" => Ok(SortKey::Name),
        "path" => Ok(SortKey::Path),
        "size" => Ok(SortKey::Size),
        "mtime" | "modified" | "date" => Ok(SortKey::Mtime),
        other => anyhow::bail!("unsupported sort key '{other}'"),
    }
}

fn parse_sort_direction(value: &str) -> anyhow::Result<SortDirection> {
    match value {
        "asc" | "ascending" => Ok(SortDirection::Asc),
        "desc" | "descending" => Ok(SortDirection::Desc),
        other => anyhow::bail!("unsupported sort direction '{other}'"),
    }
}

fn default_daemon_socket_path() -> anyhow::Result<PathBuf> {
    let dirs = xdg::BaseDirectories::with_prefix("oxidex");
    Ok(dirs.get_runtime_file("oxidexd.sock")?)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_watch_setup_sidecar_becomes_unavailable_not_stale() {
        let mut state = snapshot::IndexStateV1::new("partuuid:test");
        state.stale_reason = Some("watch_desync".into());
        state.live_watch_state = Some("error".into());
        state.last_error = Some("Permission denied (os error 13)".into());

        assert!(normalize_watch_setup_sidecar(&mut state));
        assert_eq!(state.stale_reason, None);
        assert_eq!(state.last_error, None);
        assert_eq!(state.live_watch_state.as_deref(), Some("unavailable"));
    }

    #[test]
    fn non_watch_sidecar_error_is_not_normalized() {
        let mut state = snapshot::IndexStateV1::new("partuuid:test");
        state.stale_reason = Some("scan_failed".into());
        state.live_watch_state = Some("error".into());
        state.last_error = Some("Permission denied (os error 13)".into());

        assert!(!normalize_watch_setup_sidecar(&mut state));
        assert_eq!(state.stale_reason.as_deref(), Some("scan_failed"));
        assert_eq!(
            state.last_error.as_deref(),
            Some("Permission denied (os error 13)")
        );
    }
}
