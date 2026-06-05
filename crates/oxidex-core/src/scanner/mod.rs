pub mod btrfs;
pub mod ext4;
pub mod ntfs;

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::model::{FsType, ScanDatabase};

pub type ProgressCallback<'a> = dyn FnMut(u64, u64) + 'a;

#[derive(Clone, Debug, Default)]
pub struct ScanCancellation {
    inner: Arc<AtomicBool>,
}

impl ScanCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::Relaxed)
    }

    pub fn ensure_not_cancelled(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.is_cancelled(), "scan cancelled");
        Ok(())
    }
}

pub fn scan_device(
    path: &Path,
    fs_type: FsType,
    progress: &mut ProgressCallback<'_>,
    cancellation: &ScanCancellation,
) -> anyhow::Result<ScanDatabase> {
    match fs_type {
        FsType::Ntfs => ntfs::scan(path, progress, cancellation),
        FsType::Ext4 => ext4::scan(path, progress, cancellation),
        FsType::Btrfs => btrfs::scan(path, progress, cancellation),
    }
}

pub fn scan_device_uncancelled(
    path: &Path,
    fs_type: FsType,
    progress: &mut ProgressCallback<'_>,
) -> anyhow::Result<ScanDatabase> {
    scan_device(path, fs_type, progress, &ScanCancellation::new())
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
