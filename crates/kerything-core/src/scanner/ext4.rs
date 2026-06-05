use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use ext4::{Enhanced, FileType};

use crate::model::{FsType, ROOT_PARENT, ScanDatabase};
use crate::scanner::{ProgressCallback, ScanCancellation};

pub fn scan(
    path: &Path,
    progress: &mut ProgressCallback<'_>,
    cancellation: &ScanCancellation,
) -> anyhow::Result<ScanDatabase> {
    progress(0, 1);
    let file = File::open(path)?;
    // Kerything is a read-only filename indexer. Directory metadata checksums are useful
    // for fsck, but a mismatch should not make search indexing fail outright.
    let options = ext4::Options {
        checksums: ext4::Checksums::Ignored,
        load_xattrs: false,
    };
    let volume = ext4::SuperBlock::new_with_options(file, &options)?;
    let root = volume.root()?;

    let mut db = ScanDatabase::new(FsType::Ext4);
    db.push_record(ROOT_PARENT, "", 0, 0, true, false)?;

    let mut path_to_record = HashMap::new();
    path_to_record.insert("/".to_owned(), 0u32);
    let mut seen = 0u64;

    volume.walk(&root, "/", &mut |_, raw_path, inode, enhanced| {
        cancellation.ensure_not_cancelled()?;
        let path = normalize_path(raw_path);
        if path == "/" {
            return Ok(true);
        }

        let (parent_path, name) = split_path(&path);
        if name.is_empty() {
            return Ok(true);
        }

        let parent = path_to_record
            .get(parent_path)
            .copied()
            .unwrap_or(ROOT_PARENT);
        let is_dir = matches!(enhanced, Enhanced::Directory(_))
            || inode.stat.extracted_type == FileType::Directory;
        let is_symlink = matches!(enhanced, Enhanced::SymbolicLink(_))
            || inode.stat.extracted_type == FileType::SymbolicLink;

        let idx = db.push_record(
            parent,
            name,
            inode.stat.size,
            inode.stat.mtime.epoch_secs as i64,
            is_dir,
            is_symlink,
        )?;
        path_to_record.insert(path, idx);
        seen += 1;
        if seen & 4095 == 0 {
            progress(seen, seen.saturating_add(1));
        }
        Ok(true)
    })?;

    progress(1, 1);
    Ok(db)
}

fn normalize_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut prev_slash = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev_slash {
                out.push(ch);
            }
            prev_slash = true;
        } else {
            out.push(ch);
            prev_slash = false;
        }
    }
    if out.is_empty() { "/".to_owned() } else { out }
}

fn split_path(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return ("/", "");
    }
    let Some(pos) = trimmed.rfind('/') else {
        return ("/", trimmed);
    };
    let parent = if pos == 0 { "/" } else { &trimmed[..pos] };
    let name = &trimmed[pos + 1..];
    (parent, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn metadata_csum_seed_image_scans_when_mkfs_ext4_is_available() -> anyhow::Result<()> {
        let image = std::env::temp_dir().join(format!(
            "kerything-ext4-csum-seed-{}.img",
            std::process::id()
        ));
        let file = fs::File::create(&image)?;
        file.set_len(64 * 1024 * 1024)?;
        drop(file);

        let mkfs = Command::new("mkfs.ext4")
            .arg("-q")
            .arg("-F")
            .arg("-O")
            .arg("metadata_csum,metadata_csum_seed")
            .arg(&image)
            .status();

        let Ok(status) = mkfs else {
            let _ = fs::remove_file(&image);
            return Ok(());
        };
        if !status.success() {
            let _ = fs::remove_file(&image);
            return Ok(());
        }

        let mut progress = |_: u64, _: u64| {};
        let result = scan(&image, &mut progress, &ScanCancellation::new());
        let _ = fs::remove_file(&image);

        let db = result?;
        assert_eq!(db.fs_type, FsType::Ext4);
        assert!(!db.records.is_empty());
        Ok(())
    }
}
