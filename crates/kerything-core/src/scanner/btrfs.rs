use std::path::Path;

use crate::model::ScanDatabase;
use crate::scanner::ProgressCallback;

pub fn scan(_path: &Path, _progress: &mut ProgressCallback<'_>) -> anyhow::Result<ScanDatabase> {
    anyhow::bail!(
        "Btrfs scanning is planned for v2. Device discovery and snapshot metadata already support btrfs, but the raw metadata-tree scanner is not implemented yet."
    )
}
