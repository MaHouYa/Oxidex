use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, ErrorKind, Read};
use std::os::fd::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kerything_core::config::{
    AppConfig, ThemeMode, config_path as default_config_path, load_config_from_path,
    save_config_to_path, validate_config,
};
use kerything_core::daemon_model::{
    ConfigGetResult, ConfigSetParams, ConfigValidateParams, DaemonStatus, DeviceSummary,
    IndexForgetParams, IndexStartScanParams, IndexStartScanResult, IndexSummary, ResolvePathParams,
    ResolvedPath, ScannerAuthorizeResult, ScannerStartScanParams, ScannerStartScanResult,
    SearchQueryParams, SearchQueryResult, SearchResultRow,
};
use kerything_core::device::{DeviceInfo, list_known_devices};
use kerything_core::index::{SearchHit, SearchIndex, merge_search_request, parse_search_query};
use kerything_core::ipc::{IpcFrame, params_as, read_frame, result_as, write_frame};
use kerything_core::model::{ScanDatabase, SortDirection, SortKey};
use kerything_core::rules::{compile_rules, filter_index};
use kerything_core::{VERSION, snapshot, stream};
use serde_json::{Value, json};

const DEFAULT_SCANNER_SOCKET: &str = "/run/kerything/scannerd.sock";

fn main() {
    if let Err(err) = run() {
        eprintln!("kerythingd: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut socket_path = default_daemon_socket_path()?;
    let mut config_path = default_config_path()?;
    let mut foreground = false;
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
            "--config" => {
                let Some(value) = args.next() else {
                    anyhow::bail!("--config requires a path");
                };
                config_path = PathBuf::from(value);
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
            "kerythingd: running in foreground; service managers should pass --foreground explicitly"
        );
    }

    let state = Arc::new(Mutex::new(DaemonState::load(
        config_path,
        DEFAULT_SCANNER_SOCKET.into(),
    )?));
    serve(socket_path, state)
}

fn print_usage() {
    eprintln!(
        "Usage: kerythingd [--foreground] [--socket <path>] [--config <path>] [--log-level <level>]"
    );
}

fn serve(socket_path: PathBuf, state: Arc<Mutex<DaemonState>>) -> anyhow::Result<()> {
    let listener = if let Some(listener) = inherited_systemd_listener()? {
        eprintln!("kerythingd: using inherited systemd socket");
        listener
    } else {
        if let Some(parent) = socket_path.parent() {
            fs::create_dir_all(parent)?;
        }
        if socket_path.exists() {
            fs::remove_file(&socket_path)?;
        }
        let listener = UnixListener::bind(&socket_path)?;
        eprintln!("kerythingd: listening on {}", socket_path.display());
        listener
    };

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, state) {
                        eprintln!("kerythingd: client error: {err:#}");
                    }
                });
            }
            Err(err) => eprintln!("kerythingd: accept failed: {err}"),
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
    while let Some(frame) = read_frame(&mut stream)? {
        let id = frame.header.id;
        let Some(id) = id else {
            write_frame(
                &mut stream,
                &IpcFrame::error(None, "invalid_request", "request id is required"),
            )?;
            continue;
        };
        let response = match handle_request(frame, &state) {
            Ok((result, payload)) => IpcFrame::ok(id, result, payload),
            Err(err) => IpcFrame::error(Some(id), "request_failed", format!("{err:#}")),
        };
        write_frame(&mut stream, &response)?;
    }
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
                "name": "kerythingd",
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
            };
            Ok((serde_json::to_value(result)?, Vec::new()))
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
            Ok((serde_json::to_value(state.index_summaries())?, Vec::new()))
        }
        "index.start_scan" => {
            let params: IndexStartScanParams = params_as(&frame)?;
            let (device, config, scanner_socket, job_id) = {
                let mut state = state.lock().unwrap();
                state.refresh_devices();
                let device = state
                    .devices
                    .iter()
                    .find(|device| device.metadata.device_id == params.device_id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("unknown device {}", params.device_id))?;
                let config = state.config.clone();
                let scanner_socket = state.scanner_socket.clone();
                let job_id = state.next_job_id();
                (device, config, scanner_socket, job_id)
            };

            let index = scan_and_index_device(&device, &config, &scanner_socket)?;
            snapshot::save_index(&index)?;
            let summary = IndexSummary::from_index(&index, false);
            {
                let mut state = state.lock().unwrap();
                state
                    .indexes
                    .retain(|idx| idx.metadata.device_id != params.device_id);
                state.indexes.push(index);
                state.sort_indexes();
            }
            Ok((
                serde_json::to_value(IndexStartScanResult { job_id, summary })?,
                Vec::new(),
            ))
        }
        "index.cancel_scan" => Ok((
            json!({"cancelled": false, "message": "no asynchronous scan is active in this daemon build"}),
            Vec::new(),
        )),
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
            apply_config_set(&mut state.config, &params)?;
            validate_config(&state.config)?;
            save_config_to_path(&state.config_path, &state.config)?;
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
        other => anyhow::bail!("unknown method: {other}"),
    }
}

struct DaemonState {
    config_path: PathBuf,
    config: AppConfig,
    indexes: Vec<SearchIndex>,
    devices: Vec<DeviceInfo>,
    scanner_socket: PathBuf,
    next_job_id: u64,
}

impl DaemonState {
    fn load(config_path: PathBuf, scanner_socket: PathBuf) -> anyhow::Result<Self> {
        let config = load_config_from_path(&config_path)?;
        let indexes = snapshot::load_all_indexes()?;
        let devices = list_known_devices().unwrap_or_default();
        Ok(Self {
            config_path,
            config,
            indexes,
            devices,
            scanner_socket,
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
            .map(|index| IndexSummary::from_index(index, false))
            .collect()
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
) -> anyhow::Result<SearchIndex> {
    if let Some(device_config) = config
        .devices
        .iter()
        .find(|entry| entry.device_id == device.metadata.device_id)
    {
        anyhow::ensure!(device_config.enabled, "device is disabled in config");
    }

    let scan = match scan_with_scannerd(scanner_socket, device) {
        Ok(scan) => scan,
        Err(scanner_err) => {
            eprintln!(
                "kerythingd: scanner daemon unavailable or failed ({scanner_err:#}); falling back to scanner helper"
            );
            scan_with_helper(device)?
        }
    };

    let mut index = SearchIndex::from_scan(device.metadata.clone(), scan, now_unix())?;
    if let Some(device_config) = config
        .devices
        .iter()
        .find(|entry| entry.device_id == device.metadata.device_id)
    {
        let rules = compile_rules(&device_config.rules)?;
        index = filter_index(index, &rules)?;
    }
    Ok(index)
}

fn scan_with_scannerd(socket_path: &Path, device: &DeviceInfo) -> anyhow::Result<ScanDatabase> {
    let mut stream = UnixStream::connect(socket_path).map_err(|err| {
        if err.kind() == ErrorKind::PermissionDenied {
            anyhow::anyhow!(
                "permission denied connecting to scanner daemon socket {}; ensure the socket is owned by root:kerything with mode 0660 and the user running kerythingd is in the kerything group",
                socket_path.display()
            )
        } else {
            anyhow::Error::from(err)
        }
    })?;
    let authorize = IpcFrame::request(1, "scanner.authorize", json!({}));
    write_frame(&mut stream, &authorize)?;
    let Some(frame) = read_frame(&mut stream)? else {
        anyhow::bail!("scanner daemon closed connection during authorization");
    };
    let auth: ScannerAuthorizeResult = result_as(&frame)?;
    anyhow::ensure!(auth.authorized, "scanner daemon authorization failed");

    let params = ScannerStartScanParams {
        device_path: device.metadata.dev_node.clone(),
        fs_type: device.metadata.fs_type,
    };
    write_frame(
        &mut stream,
        &IpcFrame::request(2, "scanner.start_scan", serde_json::to_value(params)?),
    )?;
    loop {
        let Some(frame) = read_frame(&mut stream)? else {
            anyhow::bail!("scanner daemon closed connection during scan");
        };
        if frame.header.id == Some(2) {
            let _: ScannerStartScanResult = result_as(&frame)?;
            return stream::read_scan_stream(&frame.payload[..]);
        }
    }
}

fn scan_with_helper(device: &DeviceInfo) -> anyhow::Result<ScanDatabase> {
    let helper = helper_path();
    let mut child = Command::new("pkexec")
        .arg(helper)
        .arg(&device.metadata.dev_node)
        .arg(device.metadata.fs_type.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("helper stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("helper stderr unavailable"))?;

    let stdout_handle = thread::spawn(move || {
        let mut data = Vec::new();
        stdout.read_to_end(&mut data).map(|_| data)
    });

    let stderr_handle = thread::spawn(move || -> std::io::Result<String> {
        let reader = std::io::BufReader::new(stderr);
        let mut diagnostics = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() && !line.trim().starts_with("KERYTHING_PROGRESS ") {
                diagnostics.push(line);
            }
        }
        Ok(diagnostics.join("\n"))
    });

    loop {
        if let Some(status) = child.try_wait()? {
            let output = stdout_handle
                .join()
                .map_err(|_| anyhow::anyhow!("helper stdout reader panicked"))??;
            let diagnostics = stderr_handle
                .join()
                .map_err(|_| anyhow::anyhow!("helper stderr reader panicked"))??;
            if !status.success() {
                if diagnostics.trim().is_empty() {
                    anyhow::bail!("scanner helper failed with status {status}");
                }
                anyhow::bail!("scanner helper failed with status {status}: {diagnostics}");
            }
            return stream::read_scan_stream(&output[..]);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn helper_path() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from("kerything-scanner-helper");
    };
    if let Some(dir) = exe.parent() {
        let sibling = dir.join("kerything-scanner-helper");
        if sibling.exists() {
            return sibling;
        }
    }
    PathBuf::from("kerything-scanner-helper")
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
        other => anyhow::bail!(
            "config.set currently supports scalar settings only; unsupported path '{other}'"
        ),
    }
    Ok(())
}

fn parse_sort_key(value: &str) -> anyhow::Result<SortKey> {
    match value {
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
    let dirs = xdg::BaseDirectories::with_prefix("kerything");
    Ok(dirs.get_runtime_file("kerythingd.sock")?)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[allow(dead_code)]
fn indexed_device_ids(indexes: &[SearchIndex]) -> HashSet<String> {
    indexes
        .iter()
        .map(|index| index.metadata.device_id.clone())
        .collect()
}
