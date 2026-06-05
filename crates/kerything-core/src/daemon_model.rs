use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::AppConfig;
use crate::device::DeviceInfo;
use crate::index::{SearchHit, SearchIndex, SearchRequest};
use crate::model::{FsType, SortDirection, SortKey};

pub type ScanJobId = u64;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceSummary {
    pub device_id: String,
    pub dev_node: String,
    pub fs_type: FsType,
    pub label: String,
    pub uuid: String,
    pub partuuid: String,
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
    pub summary: IndexSummary,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexCancelScanParams {
    pub job_id: Option<ScanJobId>,
    pub device_id: Option<String>,
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
    pub record_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScannerStatusResult {
    pub authorized: bool,
    pub active: bool,
}

impl DeviceSummary {
    pub fn from_device(device: &DeviceInfo, index: Option<&SearchIndex>) -> Self {
        Self {
            device_id: device.metadata.device_id.clone(),
            dev_node: device.metadata.dev_node.clone(),
            fs_type: device.metadata.fs_type,
            label: device.metadata.label.clone(),
            uuid: device.metadata.uuid.clone(),
            partuuid: device.metadata.partuuid.clone(),
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
        }
    }
}
