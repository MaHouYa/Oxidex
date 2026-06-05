pub mod btrfs;
pub mod ext4;
pub mod ntfs;

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::path::PathBuf;

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

pub fn validate_device_path(input: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(!input.is_empty(), "empty device path");
    let path = Path::new(input);
    anyhow::ensure!(path.is_absolute(), "device path must be absolute");
    anyhow::ensure!(input.starts_with("/dev/"), "device path must be under /dev");

    let resolved = fs::canonicalize(path)?;
    let resolved_string = resolved.to_string_lossy();
    anyhow::ensure!(
        resolved_string.starts_with("/dev/"),
        "resolved device path must remain under /dev"
    );

    let meta = fs::metadata(&resolved)?;
    anyhow::ensure!(
        meta.file_type().is_block_device(),
        "{} is not a block device",
        resolved.display()
    );
    anyhow::ensure!(
        meta.permissions().mode() & 0o002 == 0,
        "refusing world-writable device node {}",
        resolved.display()
    );
    Ok(resolved)
}
