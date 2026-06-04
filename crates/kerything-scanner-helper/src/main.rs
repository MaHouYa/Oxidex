use std::env;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use kerything_core::model::FsType;

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() == 2 && args[1] == "--version" {
        println!("kerything-scanner-helper v{}", kerything_core::VERSION);
        return Ok(());
    }

    if args.len() != 3 {
        print_usage(&args[0]);
        std::process::exit(64);
    }

    let fs_type = FsType::from_str(&args[2]).map_err(anyhow::Error::msg)?;
    let device = validate_device_path(&args[1]).inspect_err(|_| {
        print_usage(&args[0]);
    })?;

    let mut reporter = ProgressReporter::new();
    eprintln!("Scanning {} ({})", device.display(), fs_type);
    let db = kerything_core::scanner::scan_device(&device, fs_type, &mut |done, total| {
        reporter.report(done, total);
    })?;
    reporter.report(1, 1);

    let stdout = io::stdout();
    let mut lock = stdout.lock();
    kerything_core::stream::write_scan_stream(&mut lock, &db)?;
    lock.flush()?;
    Ok(())
}

fn print_usage(argv0: &str) {
    eprintln!(
        "Usage:\n  {argv0} --version\n  {argv0} <devicePath> <fsType>\nWhere:\n  <devicePath> is a block device path under /dev\n  <fsType> is one of: ntfs, ext4, btrfs"
    );
}

fn validate_device_path(input: &str) -> anyhow::Result<PathBuf> {
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

struct ProgressReporter {
    next_emit: Instant,
    last_pct: Option<u8>,
}

impl ProgressReporter {
    fn new() -> Self {
        Self {
            next_emit: Instant::now(),
            last_pct: None,
        }
    }

    fn report(&mut self, done: u64, total: u64) {
        let total = total.max(1);
        let done = done.min(total);
        let pct = ((done * 100 + total / 2) / total).min(100) as u8;

        if pct != 100 && Instant::now() < self.next_emit {
            return;
        }
        if self.last_pct == Some(pct) {
            return;
        }
        self.last_pct = Some(pct);
        self.next_emit = Instant::now() + Duration::from_millis(100);
        eprintln!("KERYTHING_PROGRESS {pct}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_device_path_rejects_empty_path() {
        assert!(validate_device_path("").is_err());
    }

    #[test]
    fn validate_device_path_rejects_relative_path() {
        assert!(validate_device_path("sda1").is_err());
    }

    #[test]
    fn validate_device_path_rejects_non_dev_path() {
        assert!(validate_device_path("/tmp/not-a-device").is_err());
    }

    #[test]
    fn fs_type_parser_accepts_v2_btrfs_name() {
        assert_eq!(FsType::from_str("btrfs").unwrap(), FsType::Btrfs);
    }
}
