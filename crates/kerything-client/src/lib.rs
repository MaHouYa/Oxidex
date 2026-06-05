use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use kerything_core::daemon_model::{
    ConfigGetResult, ConfigSetParams, DaemonDoctorResult, DaemonStatus, DeviceSummary,
    IndexCancelScanParams, IndexCancelScanResult, IndexForgetParams, IndexJobStatusParams,
    IndexStartScanParams, IndexStartScanResult, IndexSummary, ResolvePathParams, ResolvedPath,
    ScanJobId, ScanJobSummary, SearchExplainParams, SearchQueryParams, SearchQueryResult,
    WatchSummary,
};
use kerything_core::index::SearchExplanation;
use kerything_core::ipc::{IpcFrame, read_frame, result_as, write_frame};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

pub struct KerythingClient {
    stream: UnixStream,
    next_id: u64,
}

impl KerythingClient {
    pub fn connect_default() -> anyhow::Result<Self> {
        Self::connect(default_daemon_socket_path()?)
    }

    pub fn connect(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(path)?,
            next_id: 1,
        })
    }

    pub fn request<P, R>(&mut self, method: &str, params: &P) -> anyhow::Result<R>
    where
        P: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let params = serde_json::to_value(params)?;
        write_frame(&mut self.stream, &IpcFrame::request(id, method, params))?;

        loop {
            let Some(frame) = read_frame(&mut self.stream)? else {
                anyhow::bail!("daemon closed the connection");
            };
            if frame.header.id == Some(id) {
                return result_as(&frame);
            }
            // Events are intentionally ignored by the synchronous client API.
        }
    }

    pub fn request_frame<P>(&mut self, method: &str, params: &P) -> anyhow::Result<IpcFrame>
    where
        P: Serialize + ?Sized,
    {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let params = serde_json::to_value(params)?;
        write_frame(&mut self.stream, &IpcFrame::request(id, method, params))?;

        loop {
            let Some(frame) = read_frame(&mut self.stream)? else {
                anyhow::bail!("daemon closed the connection");
            };
            if frame.header.id == Some(id) {
                if frame.header.ok != Some(true) {
                    if let Some(error) = &frame.header.error {
                        anyhow::bail!("{}: {}", error.code, error.message);
                    }
                    anyhow::bail!("daemon request failed");
                }
                return Ok(frame);
            }
        }
    }

    pub fn hello(&mut self) -> anyhow::Result<Value> {
        self.request("daemon.hello", &json!({}))
    }

    pub fn status(&mut self) -> anyhow::Result<DaemonStatus> {
        self.request("daemon.status", &json!({}))
    }

    pub fn devices(&mut self) -> anyhow::Result<Vec<DeviceSummary>> {
        self.request("device.list", &json!({}))
    }

    pub fn indexes(&mut self) -> anyhow::Result<Vec<IndexSummary>> {
        self.request("index.list", &json!({}))
    }

    pub fn forget_index(&mut self, device_id: &str) -> anyhow::Result<Vec<IndexSummary>> {
        self.request(
            "index.forget",
            &IndexForgetParams {
                device_id: device_id.to_owned(),
            },
        )
    }

    pub fn start_scan(&mut self, device_id: &str) -> anyhow::Result<IndexStartScanResult> {
        self.request(
            "index.start_scan",
            &IndexStartScanParams {
                device_id: device_id.to_owned(),
            },
        )
    }

    pub fn jobs(&mut self) -> anyhow::Result<Vec<ScanJobSummary>> {
        self.request("index.job_list", &json!({}))
    }

    pub fn job_status(&mut self, job_id: ScanJobId) -> anyhow::Result<ScanJobSummary> {
        self.request("index.job_status", &IndexJobStatusParams { job_id })
    }

    pub fn cancel_scan(
        &mut self,
        job_id: Option<ScanJobId>,
        device_id: Option<&str>,
    ) -> anyhow::Result<IndexCancelScanResult> {
        self.request(
            "index.cancel_scan",
            &IndexCancelScanParams {
                job_id,
                device_id: device_id.map(str::to_owned),
            },
        )
    }

    pub fn search(&mut self, params: &SearchQueryParams) -> anyhow::Result<SearchQueryResult> {
        self.request("search.query", params)
    }

    pub fn explain(&mut self, query: &str) -> anyhow::Result<SearchExplanation> {
        self.request(
            "search.explain",
            &SearchExplainParams {
                query: query.to_owned(),
            },
        )
    }

    pub fn doctor(&mut self) -> anyhow::Result<DaemonDoctorResult> {
        self.request("daemon.doctor", &json!({}))
    }

    pub fn watch_status(&mut self) -> anyhow::Result<Vec<WatchSummary>> {
        self.request("watch.status", &json!({}))
    }

    pub fn config_get(&mut self) -> anyhow::Result<ConfigGetResult> {
        self.request("config.get", &json!({}))
    }

    pub fn config_set(&mut self, path: &str, value: Value) -> anyhow::Result<ConfigGetResult> {
        self.request(
            "config.set",
            &ConfigSetParams {
                path: path.to_owned(),
                value,
            },
        )
    }

    pub fn resolve_path(
        &mut self,
        device_id: &str,
        record_idx: u32,
    ) -> anyhow::Result<ResolvedPath> {
        self.request(
            "open.resolve_path",
            &ResolvePathParams {
                device_id: device_id.to_owned(),
                record_idx,
            },
        )
    }
}

pub fn default_daemon_socket_path() -> anyhow::Result<PathBuf> {
    let dirs = xdg::BaseDirectories::with_prefix("kerything");
    Ok(dirs.get_runtime_file("kerythingd.sock")?)
}
