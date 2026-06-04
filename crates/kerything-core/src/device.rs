use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::model::{DeviceMetadata, FsType};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceInfo {
    pub metadata: DeviceMetadata,
    pub mounted: bool,
    pub mount_points: Vec<String>,
    pub primary_mount_point: String,
}

#[derive(Default)]
struct Candidate {
    dev_node: String,
    uuid: String,
    partuuid: String,
    label: String,
    fs_type: Option<FsType>,
}

pub fn list_known_devices() -> anyhow::Result<Vec<DeviceInfo>> {
    let mount_info = read_mount_info().unwrap_or_default();
    let mut by_dev: HashMap<String, Candidate> = HashMap::new();

    add_symlink_dir(&mut by_dev, "/dev/disk/by-partuuid", LinkKind::PartUuid)?;
    add_symlink_dir(&mut by_dev, "/dev/disk/by-uuid", LinkKind::Uuid)?;
    add_symlink_dir(&mut by_dev, "/dev/disk/by-label", LinkKind::Label)?;
    add_udev_devices(&mut by_dev)?;

    for cand in by_dev.values_mut() {
        if let Ok(meta) = fs::metadata(&cand.dev_node) {
            let key = format!("b{}:{}", meta.rdev() >> 8, meta.rdev() & 0xff);
            if let Some(udev) = read_udev_data(&key) {
                if cand.uuid.is_empty() {
                    cand.uuid = udev.get("ID_FS_UUID").cloned().unwrap_or_default();
                }
                if cand.partuuid.is_empty() {
                    cand.partuuid = udev.get("ID_PART_ENTRY_UUID").cloned().unwrap_or_default();
                }
                if cand.label.is_empty() {
                    cand.label = udev.get("ID_FS_LABEL").cloned().unwrap_or_default();
                }
                if cand.fs_type.is_none() {
                    cand.fs_type = udev
                        .get("ID_FS_TYPE")
                        .and_then(|s| FsType::from_str(s).ok());
                }
            }
        }
    }

    let mut out = Vec::new();
    for cand in by_dev.into_values() {
        let Some(fs_type) = cand.fs_type else {
            continue;
        };

        let device_id = if !cand.partuuid.is_empty() {
            format!("partuuid:{}", cand.partuuid.to_ascii_lowercase())
        } else if !cand.uuid.is_empty() {
            format!("uuid:{}", cand.uuid.to_ascii_lowercase())
        } else {
            format!("dev:{}", cand.dev_node)
        };

        let mut mount_points = Vec::new();
        for mi in &mount_info {
            if !mi.mount_source.starts_with("/dev/") {
                continue;
            }
            if let Ok(src) = fs::canonicalize(&mi.mount_source)
                && src == Path::new(&cand.dev_node)
            {
                mount_points.push(mi.mount_point.clone());
            }
        }
        mount_points.sort();
        mount_points.dedup();

        let primary_mount_point = pick_primary_mount_point(&mount_points);
        out.push(DeviceInfo {
            metadata: DeviceMetadata {
                device_id,
                dev_node: cand.dev_node,
                fs_type,
                label: cand.label,
                uuid: cand.uuid.to_ascii_lowercase(),
                partuuid: cand.partuuid.to_ascii_lowercase(),
            },
            mounted: !mount_points.is_empty(),
            mount_points,
            primary_mount_point,
        });
    }

    out.sort_by(|a, b| {
        a.metadata
            .label
            .cmp(&b.metadata.label)
            .then_with(|| a.metadata.dev_node.cmp(&b.metadata.dev_node))
    });
    Ok(out)
}

pub fn find_device_by_id(device_id: &str) -> anyhow::Result<Option<DeviceInfo>> {
    Ok(list_known_devices()?
        .into_iter()
        .find(|dev| dev.metadata.device_id == device_id))
}

pub fn current_mount_for(device_id: &str) -> anyhow::Result<Option<String>> {
    Ok(find_device_by_id(device_id)?
        .and_then(|dev| (!dev.primary_mount_point.is_empty()).then_some(dev.primary_mount_point)))
}

#[derive(Clone, Copy)]
enum LinkKind {
    Uuid,
    PartUuid,
    Label,
}

fn add_symlink_dir(
    by_dev: &mut HashMap<String, Candidate>,
    dir: &str,
    kind: LinkKind,
) -> anyhow::Result<()> {
    let path = Path::new(dir);
    if !path.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(real) = fs::canonicalize(entry.path()) else {
            continue;
        };
        let dev_node = real.to_string_lossy().to_string();
        let cand = by_dev.entry(dev_node.clone()).or_insert_with(|| Candidate {
            dev_node,
            ..Candidate::default()
        });

        match kind {
            LinkKind::Uuid if cand.uuid.is_empty() => cand.uuid = name,
            LinkKind::PartUuid if cand.partuuid.is_empty() => cand.partuuid = name,
            LinkKind::Label if cand.label.is_empty() => cand.label = name,
            _ => {}
        }
    }
    Ok(())
}

fn add_udev_devices(by_dev: &mut HashMap<String, Candidate>) -> anyhow::Result<()> {
    let dir = Path::new("/run/udev/data");
    if !dir.is_dir() {
        return Ok(());
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let key = entry.file_name().to_string_lossy().to_string();
        if !key.starts_with('b') {
            continue;
        }

        let Some(udev) = read_udev_data(&key) else {
            continue;
        };
        let Some(dev_name) = udev.get("DEVNAME") else {
            continue;
        };
        let Ok(real) = fs::canonicalize(dev_name) else {
            continue;
        };
        let dev_node = real.to_string_lossy().to_string();
        let cand = by_dev.entry(dev_node.clone()).or_insert_with(|| Candidate {
            dev_node,
            ..Candidate::default()
        });

        if cand.uuid.is_empty() {
            cand.uuid = udev.get("ID_FS_UUID").cloned().unwrap_or_default();
        }
        if cand.partuuid.is_empty() {
            cand.partuuid = udev.get("ID_PART_ENTRY_UUID").cloned().unwrap_or_default();
        }
        if cand.label.is_empty() {
            cand.label = udev.get("ID_FS_LABEL").cloned().unwrap_or_default();
        }
        if cand.fs_type.is_none() {
            cand.fs_type = udev
                .get("ID_FS_TYPE")
                .and_then(|s| FsType::from_str(s).ok());
        }
    }

    Ok(())
}

fn read_udev_data(key: &str) -> Option<HashMap<String, String>> {
    let path = Path::new("/run/udev/data").join(key);
    let content = fs::read_to_string(path).ok()?;
    let mut out = HashMap::new();
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("E:") else {
            continue;
        };
        let Some((k, v)) = rest.split_once('=') else {
            continue;
        };
        out.insert(k.to_owned(), v.to_owned());
    }
    Some(out)
}

#[derive(Clone, Debug)]
struct MountInfoEntry {
    mount_point: String,
    mount_source: String,
}

fn read_mount_info() -> anyhow::Result<Vec<MountInfoEntry>> {
    let content = fs::read_to_string("/proc/self/mountinfo")?;
    let mut out = Vec::new();
    for line in content.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left_fields: Vec<_> = left.split_whitespace().collect();
        let right_fields: Vec<_> = right.split_whitespace().collect();
        if left_fields.len() < 5 || right_fields.len() < 2 {
            continue;
        }
        out.push(MountInfoEntry {
            mount_point: unescape_mountinfo(left_fields[4]),
            mount_source: unescape_mountinfo(right_fields[1]),
        });
    }
    Ok(out)
}

fn unescape_mountinfo(s: &str) -> String {
    s.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

fn pick_primary_mount_point(mount_points: &[String]) -> String {
    let mut best = String::new();
    for mp in mount_points {
        if (mp == "/mnt" || mp.starts_with("/mnt/") || mp == "/media" || mp.starts_with("/media/"))
            && (best.is_empty() || mp.len() < best.len())
        {
            best = mp.clone();
        }
    }
    if !best.is_empty() {
        return best;
    }
    mount_points
        .iter()
        .min_by_key(|mp| mp.len())
        .cloned()
        .unwrap_or_default()
}
