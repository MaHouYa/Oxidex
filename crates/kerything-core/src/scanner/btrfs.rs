use std::collections::VecDeque;
use std::fs::File;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use btrfs_fs::{FileKind, Filesystem};

use crate::model::{FsType, ROOT_PARENT, ScanDatabase};
use crate::scanner::ProgressCallback;

pub fn scan(path: &Path, progress: &mut ProgressCallback<'_>) -> anyhow::Result<ScanDatabase> {
    progress(0, 1);
    let file = File::open(path)?;
    let fs = Filesystem::open(file)?;
    if fs.superblock().num_devices > 1 {
        anyhow::bail!("multi-device Btrfs filesystems are not supported by the V2 basic scanner");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|err| anyhow::anyhow!("failed to create Btrfs scanner runtime: {err}"))?;
    runtime.block_on(scan_default_root(fs, progress))
}

async fn scan_default_root(
    fs: Filesystem<File>,
    progress: &mut ProgressCallback<'_>,
) -> anyhow::Result<ScanDatabase> {
    let root = fs.root();
    let mut db = ScanDatabase::new(FsType::Btrfs);
    db.push_record(ROOT_PARENT, "", 0, 0, true, false)?;

    let subvols = fs.list_subvolumes().await?;
    if subvols.len() > 1 {
        eprintln!(
            "Btrfs V2 basic scanner indexes only the default root; {} additional subvolume{} will be listed as boundaries but not traversed.",
            subvols.len() - 1,
            if subvols.len() == 2 { "" } else { "s" }
        );
    }

    let mut queue = VecDeque::from([(root, 0u32)]);
    let mut seen = 0u64;

    while let Some((dir, parent_idx)) = queue.pop_front() {
        let entries = fs.readdirplus(dir, 0).await?;
        for (entry, stat) in entries {
            if entry.name == b"." || entry.name == b".." {
                continue;
            }

            let name = String::from_utf8_lossy(&entry.name).into_owned();
            if name.is_empty() {
                continue;
            }

            let crosses_subvolume = entry.ino.subvol != root.subvol;
            let is_dir = stat.kind == FileKind::Directory || crosses_subvolume;
            let is_symlink = stat.kind == FileKind::Symlink;
            let idx = db.push_record(
                parent_idx,
                &name,
                stat.size,
                system_time_to_unix(stat.mtime),
                is_dir,
                is_symlink,
            )?;

            seen += 1;
            if seen & 4095 == 0 {
                progress(seen, seen.saturating_add(1));
            }

            if is_dir && !crosses_subvolume {
                queue.push_back((entry.ino, idx));
            }
        }
    }

    progress(1, 1);
    Ok(db)
}

fn system_time_to_unix(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn fixture_image_scans_when_mkfs_btrfs_is_available() -> anyhow::Result<()> {
        let base = std::env::temp_dir().join(format!("kerything-btrfs-{}", std::process::id()));
        let src = base.join("src");
        let nested = src.join("nested");
        fs::create_dir_all(&nested)?;
        fs::write(src.join("hello.txt"), b"hello")?;
        fs::write(nested.join("main.rs"), b"fn main() {}")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink("hello.txt", src.join("link"))?;

        let image = base.join("test.img");
        let file = fs::File::create(&image)?;
        file.set_len(128 * 1024 * 1024)?;
        drop(file);

        let mkfs = Command::new("mkfs.btrfs")
            .arg("-f")
            .arg("--rootdir")
            .arg(&src)
            .arg(&image)
            .status();

        let Ok(status) = mkfs else {
            let _ = fs::remove_dir_all(&base);
            return Ok(());
        };
        if !status.success() {
            let _ = fs::remove_dir_all(&base);
            return Ok(());
        }

        let mut progress = |_: u64, _: u64| {};
        let result = scan(&image, &mut progress);
        let _ = fs::remove_dir_all(&base);

        let db = result?;
        assert_eq!(db.fs_type, FsType::Btrfs);
        assert!((0..db.records.len() as u32).any(|idx| db.name(idx) == "hello.txt"));
        assert!((0..db.records.len() as u32).any(|idx| db.name(idx) == "main.rs"));
        Ok(())
    }

    #[test]
    fn subvolume_fixture_does_not_recurse_beyond_default_root() -> anyhow::Result<()> {
        let base =
            std::env::temp_dir().join(format!("kerything-btrfs-subvol-{}", std::process::id()));
        let src = base.join("src");
        let sub = src.join("sub");
        fs::create_dir_all(&sub)?;
        fs::write(src.join("top.txt"), b"top")?;
        fs::write(sub.join("inside.txt"), b"inside")?;

        let image = base.join("test.img");
        let file = fs::File::create(&image)?;
        file.set_len(128 * 1024 * 1024)?;
        drop(file);

        let mkfs = Command::new("mkfs.btrfs")
            .arg("-f")
            .arg("--rootdir")
            .arg(&src)
            .arg("--subvol")
            .arg("sub")
            .arg(&image)
            .status();

        let Ok(status) = mkfs else {
            let _ = fs::remove_dir_all(&base);
            return Ok(());
        };
        if !status.success() {
            let _ = fs::remove_dir_all(&base);
            return Ok(());
        }

        let mut progress = |_: u64, _: u64| {};
        let result = scan(&image, &mut progress);
        let _ = fs::remove_dir_all(&base);

        let db = result?;
        assert!((0..db.records.len() as u32).any(|idx| db.name(idx) == "sub"));
        assert!(!(0..db.records.len() as u32).any(|idx| db.name(idx) == "inside.txt"));
        Ok(())
    }
}
