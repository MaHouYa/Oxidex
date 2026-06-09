use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::AppConfig;
use crate::device::DeviceInfo;
use crate::doctor::DoctorReport;
use crate::index::{SearchHit, SearchIndex, SearchRequest};
use crate::model::{FsType, SortDirection, SortKey};

pub type ScanJobId = u64;
pub type ScannerJobId = u64;
pub type ScannerWatchId = u64;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClientRequest {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClientResponse {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonEvent {
    ConfigChanged,
    IndexesChanged,
    ScanStarted {
        job_id: ScanJobId,
        device_id: String,
    },
    ScanProgress {
        job_id: ScanJobId,
        device_id: String,
        percent: u8,
    },
    ScanFinished {
        job_id: ScanJobId,
        device_id: String,
    },
    ScanFailed {
        job_id: ScanJobId,
        device_id: String,
        message: String,
    },
    DeviceListRefreshed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    Queued,
    Running,
    Finished,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DaemonStatus {
    pub version: String,
    pub index_count: usize,
    pub device_count: usize,
    pub scanner_socket: String,
    pub scanner_status: Option<ScannerStatusDetail>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DaemonDoctorResult {
    pub report: DoctorReport,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceSummary {
    pub device_id: String,
    pub dev_node: String,
    pub fs_type: Option<FsType>,
    pub fs_type_name: String,
    pub label: String,
    pub uuid: String,
    pub partuuid: String,
    pub scan_supported: bool,
    pub scan_unavailable_reason: Option<String>,
    pub mounted: bool,
    pub mount_points: Vec<String>,
    pub primary_mount_point: String,
    pub indexed: bool,
    pub entry_count: Option<usize>,
    pub last_indexed_time: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexSummary {
    pub device_id: String,
    pub dev_node: String,
    pub fs_type: FsType,
    pub label: String,
    pub uuid: String,
    pub partuuid: String,
    pub entry_count: usize,
    pub last_indexed_time: i64,
    pub stale: bool,
    pub state: Option<IndexStateSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct IndexStateSummary {
    pub last_success_time: Option<i64>,
    pub last_failure_time: Option<i64>,
    pub last_error: Option<String>,
    pub last_scan_duration_ms: Option<u64>,
    pub last_scanner: Option<String>,
    pub snapshot_size: Option<u64>,
    pub stale_reason: Option<String>,
    pub live_watch_state: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WatchSummary {
    pub device_id: String,
    pub mounted: bool,
    pub enabled: bool,
    pub state: String,
    pub watched_directories: usize,
    pub dirty: bool,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexHealth {
    pub summary: IndexSummary,
    pub mounted: bool,
    pub mount_point: String,
    pub snapshot_size: Option<u64>,
    pub watch: Option<WatchSummary>,
    pub btrfs_note: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvedPath {
    pub device_id: String,
    pub record_idx: u32,
    pub mounted: bool,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchResultRow {
    pub hit: SearchHit,
    pub name: String,
    pub display_path: String,
    pub internal_path: String,
    pub device_label: String,
    pub fs_type: FsType,
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub mounted: bool,
    pub last_indexed_time: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchQueryParams {
    pub query: String,
    #[serde(default)]
    pub request: Option<SearchRequest>,
    #[serde(default)]
    pub device_filter: Option<String>,
    pub sort_key: SortKey,
    pub sort_direction: SortDirection,
    #[serde(default)]
    pub max_results: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchQueryResult {
    pub rows: Vec<SearchResultRow>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchExplainParams {
    pub query: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexForgetParams {
    pub device_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexStartScanParams {
    pub device_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexStartScanResult {
    pub job_id: ScanJobId,
    pub device_id: String,
    pub state: ScanState,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScanJobSummary {
    pub job_id: ScanJobId,
    pub device_id: String,
    pub state: ScanState,
    pub progress: u8,
    pub message: String,
    pub started_time: Option<i64>,
    pub finished_time: Option<i64>,
    pub result: Option<IndexSummary>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexCancelScanParams {
    pub job_id: Option<ScanJobId>,
    pub device_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexJobStatusParams {
    pub job_id: ScanJobId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexCancelScanResult {
    pub cancelled: bool,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConfigGetResult {
    pub path: String,
    pub config: AppConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConfigSetParams {
    pub path: String,
    pub value: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConfigValidateParams {
    pub config: AppConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ResolvePathParams {
    pub device_id: String,
    pub record_idx: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerHelloResult {
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerAuthorizeResult {
    pub authorized: bool,
    pub peer_uid: u32,
    pub peer_pid: i32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerStartScanParams {
    pub device_path: String,
    pub fs_type: FsType,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerStartScanResult {
    pub job_id: ScannerJobId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerTakeResultParams {
    pub job_id: ScannerJobId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerTakeResultResult {
    pub record_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerCancelScanParams {
    pub job_id: ScannerJobId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerStatusResult {
    pub access_model: String,
    pub peer_uid: u32,
    pub peer_gid: u32,
    pub active_jobs: Vec<ScannerJobSummary>,
    pub active_watches: Vec<ScannerWatchSummary>,
    pub idle_timeout_seconds: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ScannerStatusDetail {
    pub socket: String,
    pub reachable: bool,
    pub access_model: String,
    pub peer_uid: Option<u32>,
    pub peer_gid: Option<u32>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerJobSummary {
    pub job_id: ScannerJobId,
    pub device_path: String,
    pub fs_type: FsType,
    pub state: ScanState,
    pub progress: u8,
    pub record_count: Option<usize>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerStartWatchParams {
    pub device_id: String,
    pub device_path: String,
    pub fs_type: FsType,
    pub mount_point: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerStartWatchResult {
    pub watch_id: ScannerWatchId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerStopWatchParams {
    pub watch_id: ScannerWatchId,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerWatchSummary {
    pub watch_id: ScannerWatchId,
    pub device_id: String,
    pub mount_point: String,
    pub state: String,
    pub watched_directories: usize,
    pub last_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScannerWatchEventKind {
    Created,
    Removed,
    Renamed,
    Metadata,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerLiveMetadata {
    pub size: u64,
    pub mtime: i64,
    pub is_dir: bool,
    pub is_symlink: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerWatchEvent {
    pub watch_id: ScannerWatchId,
    pub device_id: String,
    pub kind: ScannerWatchEventKind,
    pub internal_path: String,
    pub old_internal_path: Option<String>,
    pub metadata: Option<ScannerLiveMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerWatchErrorEvent {
    pub watch_id: Option<ScannerWatchId>,
    pub device_id: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerWatchStoppedEvent {
    pub watch_id: ScannerWatchId,
    pub device_id: String,
    pub reason: String,
}

impl DeviceSummary {
    pub fn from_device(device: &DeviceInfo, index: Option<&SearchIndex>) -> Self {
        Self {
            device_id: device.device_id.clone(),
            dev_node: device.dev_node.clone(),
            fs_type: device.fs_type,
            fs_type_name: device.fs_type_name.clone(),
            label: device.label.clone(),
            uuid: device.uuid.clone(),
            partuuid: device.partuuid.clone(),
            scan_supported: device.scan_supported,
            scan_unavailable_reason: device.scan_unavailable_reason.clone(),
            mounted: device.mounted,
            mount_points: device.mount_points.clone(),
            primary_mount_point: device.primary_mount_point.clone(),
            indexed: index.is_some(),
            entry_count: index.map(|index| index.records.len()),
            last_indexed_time: index.map(|index| index.last_indexed_time),
        }
    }
}

impl IndexSummary {
    pub fn from_index(index: &SearchIndex, stale: bool) -> Self {
        Self {
            device_id: index.metadata.device_id.clone(),
            dev_node: index.metadata.dev_node.clone(),
            fs_type: index.metadata.fs_type,
            label: index.metadata.label.clone(),
            uuid: index.metadata.uuid.clone(),
            partuuid: index.metadata.partuuid.clone(),
            entry_count: index.records.len(),
            last_indexed_time: index.last_indexed_time,
            stale,
            state: None,
        }
    }

    pub fn with_state(mut self, state: IndexStateSummary) -> Self {
        self.stale |= state.stale_reason.is_some();
        self.state = Some(state);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceInfo;

    #[test]
    fn device_summary_supports_unsupported_filesystems() {
        let device = DeviceInfo {
            metadata: None,
            device_id: "dev:/dev/sr0".into(),
            dev_node: "/dev/sr0".into(),
            fs_type: None,
            fs_type_name: "iso9660".into(),
            label: "Install Media".into(),
            uuid: "2026-06-01".into(),
            partuuid: String::new(),
            scan_supported: false,
            scan_unavailable_reason: Some("Unsupported filesystem: iso9660".into()),
            mounted: true,
            mount_points: vec!["/run/media/install".into()],
            primary_mount_point: "/run/media/install".into(),
        };

        let summary = DeviceSummary::from_device(&device, None);
        let json = serde_json::to_value(&summary).unwrap();

        assert_eq!(summary.fs_type, None);
        assert_eq!(summary.fs_type_name, "iso9660");
        assert!(!summary.scan_supported);
        assert_eq!(
            summary.scan_unavailable_reason.as_deref(),
            Some("Unsupported filesystem: iso9660")
        );
        assert!(json.get("fs_type").unwrap().is_null());
    }
}
