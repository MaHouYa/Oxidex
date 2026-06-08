use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{config_path, load_config_from_path};
use crate::snapshot;

pub const DEFAULT_SCANNER_SOCKET: &str = "/run/oxidex/scannerd.sock";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorSeverity {
    Ok,
    Info,
    Warn,
    Fail,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCategory {
    Daemon,
    Scanner,
    Systemd,
    Config,
    Indexes,
    Security,
    Packaging,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DoctorCheck {
    pub category: DoctorCategory,
    pub severity: DoctorSeverity,
    pub name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

#[derive(Clone, Debug, Default)]
pub struct DoctorOptions {
    pub daemon_socket: Option<PathBuf>,
    pub scanner_socket: Option<PathBuf>,
    pub scanner_only: bool,
    pub security_only: bool,
}

impl DoctorReport {
    pub fn add(
        &mut self,
        category: DoctorCategory,
        severity: DoctorSeverity,
        name: impl Into<String>,
        message: impl Into<String>,
    ) {
        self.checks.push(DoctorCheck {
            category,
            severity,
            name: name.into(),
            message: message.into(),
            detail: None,
        });
    }

    pub fn add_detail(
        &mut self,
        category: DoctorCategory,
        severity: DoctorSeverity,
        name: impl Into<String>,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.checks.push(DoctorCheck {
            category,
            severity,
            name: name.into(),
            message: message.into(),
            detail: Some(detail.into()),
        });
    }

    pub fn merge(&mut self, other: DoctorReport) {
        self.checks.extend(other.checks);
    }
}

pub fn run_local_doctor(options: &DoctorOptions) -> DoctorReport {
    let mut report = DoctorReport::default();
    let scanner_socket = options
        .scanner_socket
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SCANNER_SOCKET));

    if !options.scanner_only && !options.security_only {
        check_config(&mut report);
        check_indexes(&mut report);
        if let Some(path) = &options.daemon_socket {
            check_daemon_socket(&mut report, path);
        }
        check_systemd_units(&mut report);
    }

    check_scanner_socket(&mut report, &scanner_socket);
    check_security(&mut report, &scanner_socket);
    report
}

fn check_config(report: &mut DoctorReport) {
    match config_path() {
        Ok(path) => match load_config_from_path(&path) {
            Ok(_) => report.add(
                DoctorCategory::Config,
                DoctorSeverity::Ok,
                "config",
                format!("Config is valid at {}", path.display()),
            ),
            Err(err) => report.add_detail(
                DoctorCategory::Config,
                DoctorSeverity::Fail,
                "config",
                format!("Config failed to load at {}", path.display()),
                format!("{err:#}"),
            ),
        },
        Err(err) => report.add_detail(
            DoctorCategory::Config,
            DoctorSeverity::Fail,
            "config_path",
            "Could not resolve XDG config path",
            format!("{err:#}"),
        ),
    }
}

fn check_indexes(report: &mut DoctorReport) {
    match snapshot::index_dir() {
        Ok(path) => {
            if path.exists() {
                match snapshot::load_all_indexes() {
                    Ok(indexes) => report.add(
                        DoctorCategory::Indexes,
                        DoctorSeverity::Ok,
                        "indexes",
                        format!(
                            "Loaded {} snapshot{} from {}",
                            indexes.len(),
                            plural(indexes.len()),
                            path.display()
                        ),
                    ),
                    Err(err) => report.add_detail(
                        DoctorCategory::Indexes,
                        DoctorSeverity::Fail,
                        "indexes",
                        format!("Could not load indexes from {}", path.display()),
                        format!("{err:#}"),
                    ),
                }
            } else if fs::create_dir_all(&path).is_ok() {
                report.add(
                    DoctorCategory::Indexes,
                    DoctorSeverity::Ok,
                    "index_dir",
                    format!("Index directory can be created at {}", path.display()),
                );
            } else {
                report.add(
                    DoctorCategory::Indexes,
                    DoctorSeverity::Fail,
                    "index_dir",
                    format!("Index directory is not writable: {}", path.display()),
                );
            }
        }
        Err(err) => report.add_detail(
            DoctorCategory::Indexes,
            DoctorSeverity::Fail,
            "index_dir",
            "Could not resolve XDG data index path",
            format!("{err:#}"),
        ),
    }
}

fn check_daemon_socket(report: &mut DoctorReport, path: &Path) {
    if !path.exists() {
        report.add(
            DoctorCategory::Daemon,
            DoctorSeverity::Warn,
            "daemon_socket",
            format!("Daemon socket is not present at {}", path.display()),
        );
        return;
    }

    match UnixStream::connect(path) {
        Ok(_) => report.add(
            DoctorCategory::Daemon,
            DoctorSeverity::Ok,
            "daemon_socket",
            format!("Daemon socket is reachable at {}", path.display()),
        ),
        Err(err) => report.add_detail(
            DoctorCategory::Daemon,
            DoctorSeverity::Fail,
            "daemon_socket",
            format!(
                "Daemon socket exists but is not reachable at {}",
                path.display()
            ),
            err.to_string(),
        ),
    }
}

fn check_scanner_socket(report: &mut DoctorReport, path: &Path) {
    let Ok(meta) = fs::metadata(path) else {
        report.add(
            DoctorCategory::Scanner,
            DoctorSeverity::Warn,
            "scanner_socket",
            format!("Scanner socket is not present at {}", path.display()),
        );
        return;
    };

    let mode = meta.permissions().mode() & 0o777;
    let severity = if mode & !0o660 == 0 {
        DoctorSeverity::Ok
    } else {
        DoctorSeverity::Fail
    };
    report.add(
        DoctorCategory::Scanner,
        severity,
        "scanner_socket",
        format!("Scanner socket mode is {:o} at {}", mode, path.display()),
    );

    if meta.file_type().is_socket() {
        report.add(
            DoctorCategory::Scanner,
            DoctorSeverity::Ok,
            "scanner_socket_type",
            "Scanner path is a Unix socket",
        );
    } else {
        report.add(
            DoctorCategory::Scanner,
            DoctorSeverity::Fail,
            "scanner_socket_type",
            "Scanner path is not a Unix socket",
        );
    }

    if meta.uid() == 0 {
        report.add(
            DoctorCategory::Scanner,
            DoctorSeverity::Ok,
            "scanner_socket_owner",
            "Scanner socket is root-owned",
        );
    } else {
        report.add(
            DoctorCategory::Scanner,
            DoctorSeverity::Warn,
            "scanner_socket_owner",
            format!("Scanner socket uid is {}, expected root", meta.uid()),
        );
    }

    if let Some(gid) = group_gid("oxidex") {
        if meta.gid() == gid {
            report.add(
                DoctorCategory::Scanner,
                DoctorSeverity::Ok,
                "scanner_socket_group",
                "Scanner socket group is oxidex",
            );
        } else {
            report.add(
                DoctorCategory::Scanner,
                DoctorSeverity::Warn,
                "scanner_socket_group",
                format!(
                    "Scanner socket gid is {}, expected oxidex ({gid})",
                    meta.gid()
                ),
            );
        }
    } else {
        report.add(
            DoctorCategory::Scanner,
            DoctorSeverity::Warn,
            "scanner_group",
            "System group oxidex does not exist",
        );
    }
}

fn check_security(report: &mut DoctorReport, scanner_socket: &Path) {
    if let Some(gid) = group_gid("oxidex") {
        let groups = process_groups();
        if groups.contains(&gid) {
            report.add(
                DoctorCategory::Security,
                DoctorSeverity::Ok,
                "scanner_group_membership",
                "Current process is in the oxidex group",
            );
        } else {
            report.add(
                DoctorCategory::Security,
                DoctorSeverity::Warn,
                "scanner_group_membership",
                "Current user is not in the oxidex group; scanner socket connection will fail unless another access rule is configured",
            );
        }
    }

    if let Ok(meta) = fs::metadata(scanner_socket) {
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o002 == 0 {
            report.add(
                DoctorCategory::Security,
                DoctorSeverity::Ok,
                "scanner_socket_not_world_writable",
                "Scanner socket is not world-writable",
            );
        } else {
            report.add(
                DoctorCategory::Security,
                DoctorSeverity::Fail,
                "scanner_socket_not_world_writable",
                "Scanner socket is world-writable",
            );
        }
    }
}

fn check_systemd_units(report: &mut DoctorReport) {
    for (category, path) in [
        (
            "user_unit",
            Path::new("/usr/lib/systemd/user/oxidexd.socket"),
        ),
        (
            "system_unit",
            Path::new("/usr/lib/systemd/system/oxidex-scannerd.socket"),
        ),
    ] {
        report.add(
            DoctorCategory::Systemd,
            if path.exists() {
                DoctorSeverity::Ok
            } else {
                DoctorSeverity::Info
            },
            category,
            format!(
                "{} {}",
                path.display(),
                if path.exists() {
                    "exists"
                } else {
                    "is not installed in /usr"
                }
            ),
        );
    }
}

fn group_gid(name: &str) -> Option<u32> {
    let text = fs::read_to_string("/etc/group").ok()?;
    for line in text.lines() {
        let mut parts = line.split(':');
        if parts.next()? == name {
            parts.next()?;
            return parts.next()?.parse().ok();
        }
    }
    None
}

fn process_groups() -> Vec<u32> {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return Vec::new();
    };
    for line in status.lines() {
        if let Some(groups) = line.strip_prefix("Groups:") {
            return groups
                .split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect();
        }
    }
    Vec::new()
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
