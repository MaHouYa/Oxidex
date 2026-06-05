use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub const ROOT_PARENT: u32 = u32::MAX;
pub const FLAG_IS_DIR: u8 = 1 << 0;
pub const FLAG_IS_SYMLINK: u8 = 1 << 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
pub enum FsType {
    Ntfs,
    Ext4,
    Btrfs,
}

impl FsType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ntfs => "ntfs",
            Self::Ext4 => "ext4",
            Self::Btrfs => "btrfs",
        }
    }

    pub fn is_supported_for_scan(self) -> bool {
        matches!(self, Self::Ntfs | Self::Ext4 | Self::Btrfs)
    }
}

impl fmt::Display for FsType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for FsType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "ntfs" => Ok(Self::Ntfs),
            "ext4" => Ok(Self::Ext4),
            "btrfs" => Ok(Self::Btrfs),
            other => Err(format!("unsupported filesystem type: {other}")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScanRecord {
    pub parent: u32,
    pub size: u64,
    pub mtime: i64,
    pub name_offset: u32,
    pub name_len: u32,
    pub flags: u8,
}

impl ScanRecord {
    pub fn is_dir(&self) -> bool {
        self.flags & FLAG_IS_DIR != 0
    }

    pub fn is_symlink(&self) -> bool {
        self.flags & FLAG_IS_SYMLINK != 0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ScanDatabase {
    pub fs_type: FsType,
    pub records: Vec<ScanRecord>,
    pub string_pool: Vec<u8>,
}

impl ScanDatabase {
    pub fn new(fs_type: FsType) -> Self {
        Self {
            fs_type,
            records: Vec::new(),
            string_pool: Vec::new(),
        }
    }

    pub fn push_record(
        &mut self,
        parent: u32,
        name: &str,
        size: u64,
        mtime: i64,
        is_dir: bool,
        is_symlink: bool,
    ) -> anyhow::Result<u32> {
        let name_offset = u32::try_from(self.string_pool.len())?;
        let name_len = u32::try_from(name.len())?;
        self.string_pool.extend_from_slice(name.as_bytes());

        let mut flags = 0u8;
        if is_dir {
            flags |= FLAG_IS_DIR;
        }
        if is_symlink {
            flags |= FLAG_IS_SYMLINK;
        }

        let idx = u32::try_from(self.records.len())?;
        self.records.push(ScanRecord {
            parent,
            size,
            mtime,
            name_offset,
            name_len,
            flags,
        });
        Ok(idx)
    }

    pub fn name(&self, idx: u32) -> &str {
        let rec = &self.records[idx as usize];
        let start = rec.name_offset as usize;
        let end = start + rec.name_len as usize;
        std::str::from_utf8(&self.string_pool[start..end]).unwrap_or("")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceMetadata {
    pub device_id: String,
    pub dev_node: String,
    pub fs_type: FsType,
    pub label: String,
    pub uuid: String,
    pub partuuid: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortKey {
    Name,
    Path,
    Size,
    Mtime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortDirection {
    Asc,
    Desc,
}
