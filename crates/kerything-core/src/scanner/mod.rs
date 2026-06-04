pub mod btrfs;
pub mod ext4;
pub mod ntfs;

use std::path::Path;

use crate::model::{FsType, ScanDatabase};

pub type ProgressCallback<'a> = dyn FnMut(u64, u64) + 'a;

pub fn scan_device(
    path: &Path,
    fs_type: FsType,
    progress: &mut ProgressCallback<'_>,
) -> anyhow::Result<ScanDatabase> {
    match fs_type {
        FsType::Ntfs => ntfs::scan(path, progress),
        FsType::Ext4 => ext4::scan(path, progress),
        FsType::Btrfs => btrfs::scan(path, progress),
    }
}
