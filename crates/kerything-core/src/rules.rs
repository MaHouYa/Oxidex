use globset::{GlobBuilder, GlobMatcher};

use crate::config::{PathRule, RuleKind};
use crate::index::{IndexedRecord, SearchIndex};
use crate::model::ROOT_PARENT;

#[derive(Clone, Debug)]
pub struct CompiledRules {
    includes: Vec<CompiledRule>,
    excludes: Vec<CompiledRule>,
}

#[derive(Clone, Debug)]
struct CompiledRule {
    pattern: String,
    matcher: GlobMatcher,
    full_path: bool,
}

pub fn compile_rules(rules: &[PathRule]) -> anyhow::Result<CompiledRules> {
    let mut includes = Vec::new();
    let mut excludes = Vec::new();

    for rule in rules {
        let pattern = rule.pattern.trim();
        anyhow::ensure!(!pattern.is_empty(), "rule pattern must not be empty");
        let full_path = pattern.starts_with('/');
        let matcher = GlobBuilder::new(pattern)
            .literal_separator(full_path)
            .build()
            .map_err(|err| anyhow::anyhow!("invalid path rule '{pattern}': {err}"))?
            .compile_matcher();
        let compiled = CompiledRule {
            pattern: pattern.to_owned(),
            matcher,
            full_path,
        };
        match rule.kind {
            RuleKind::Include => includes.push(compiled),
            RuleKind::Exclude => excludes.push(compiled),
        }
    }

    Ok(CompiledRules { includes, excludes })
}

impl CompiledRules {
    pub fn is_empty(&self) -> bool {
        self.includes.is_empty() && self.excludes.is_empty()
    }

    pub fn matches(&self, internal_path: &str, name: &str) -> bool {
        let included = if self.includes.is_empty() {
            true
        } else {
            self.includes
                .iter()
                .any(|rule| rule.matches(internal_path, name))
        };
        included
            && !self
                .excludes
                .iter()
                .any(|rule| rule.matches(internal_path, name))
    }
}

impl CompiledRule {
    fn matches(&self, internal_path: &str, name: &str) -> bool {
        if self.full_path {
            return self.matcher.is_match(internal_path);
        }

        if self.matcher.is_match(name) {
            return true;
        }

        internal_path
            .trim_start_matches('/')
            .split('/')
            .filter(|component| !component.is_empty())
            .any(|component| self.matcher.is_match(component))
    }
}

pub fn filter_index(index: SearchIndex, rules: &CompiledRules) -> anyhow::Result<SearchIndex> {
    if rules.is_empty() {
        return Ok(index);
    }

    let len = index.records.len();
    let mut selected = vec![false; len];
    for (record_idx, selected_record) in selected.iter_mut().enumerate() {
        let name = index.name(record_idx as u32);
        let path = index.internal_path(record_idx as u32);
        *selected_record = rules.matches(path, name);
    }

    let mut keep = selected.clone();
    for (record_idx, selected_record) in selected.iter().copied().enumerate() {
        if !selected_record {
            continue;
        }

        let mut parent = index.records[record_idx].parent;
        let mut depth = 0usize;
        while parent != ROOT_PARENT && (parent as usize) < len && depth < len {
            keep[parent as usize] = true;
            let next = index.records[parent as usize].parent;
            if next == parent {
                break;
            }
            parent = next;
            depth += 1;
        }
    }

    let mut old_to_new = vec![None; len];
    let kept_count = keep.iter().filter(|&&value| value).count();
    let mut records = Vec::with_capacity(kept_count);
    let mut string_pool = Vec::with_capacity(index.string_pool.len().min(kept_count * 32));
    let mut folded_pool = Vec::with_capacity(index.folded_pool.len().min(kept_count * 32));

    for (old_idx, should_keep) in keep.iter().copied().enumerate() {
        if !should_keep {
            continue;
        }
        let new_idx = u32::try_from(records.len())?;
        old_to_new[old_idx] = Some(new_idx);

        let old_record = &index.records[old_idx];
        let name = index.name(old_idx as u32);
        let folded = name.to_lowercase();
        let name_offset = u32::try_from(string_pool.len())?;
        let name_len = u32::try_from(name.len())?;
        string_pool.extend_from_slice(name.as_bytes());
        let folded_offset = u32::try_from(folded_pool.len())?;
        let folded_len = u32::try_from(folded.len())?;
        folded_pool.extend_from_slice(folded.as_bytes());

        records.push(IndexedRecord {
            parent: old_record.parent,
            size: old_record.size,
            mtime: old_record.mtime,
            name_offset,
            name_len,
            folded_offset,
            folded_len,
            flags: old_record.flags,
        });
    }

    for (old_idx, should_keep) in keep.iter().copied().enumerate() {
        if !should_keep {
            continue;
        }
        let new_idx = old_to_new[old_idx].expect("kept records are mapped");
        let old_parent = index.records[old_idx].parent;
        let new_parent = if old_parent == ROOT_PARENT {
            ROOT_PARENT
        } else {
            old_to_new
                .get(old_parent as usize)
                .and_then(|value| *value)
                .unwrap_or(ROOT_PARENT)
        };
        records[new_idx as usize].parent = new_parent;
    }

    let mut filtered = SearchIndex {
        metadata: index.metadata,
        generation: index.generation.saturating_add(1),
        last_indexed_time: index.last_indexed_time,
        records,
        string_pool,
        folded_pool,
        internal_paths: Vec::new(),
        flat_index: Vec::new(),
        order_by_name: Vec::new(),
        order_by_path: Vec::new(),
        order_by_size: Vec::new(),
        order_by_mtime: Vec::new(),
    };
    filtered.rebuild_accelerators();
    Ok(filtered)
}

impl std::fmt::Display for CompiledRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PathRule, RuleKind};
    use crate::index::SearchIndex;
    use crate::model::{DeviceMetadata, FsType, ROOT_PARENT, ScanDatabase};

    fn sample_index() -> SearchIndex {
        let mut scan = ScanDatabase::new(FsType::Ext4);
        scan.push_record(ROOT_PARENT, "", 0, 0, true, false)
            .unwrap();
        let home = scan.push_record(0, "home", 0, 0, true, false).unwrap();
        let hiroshi = scan
            .push_record(home, "hiroshi", 0, 0, true, false)
            .unwrap();
        let src = scan.push_record(hiroshi, "src", 0, 0, true, false).unwrap();
        scan.push_record(src, "main.rs", 12, 1, false, false)
            .unwrap();
        let modules = scan
            .push_record(src, "node_modules", 0, 0, true, false)
            .unwrap();
        scan.push_record(modules, "left-pad.js", 9, 2, false, false)
            .unwrap();
        let var = scan.push_record(0, "var", 0, 0, true, false).unwrap();
        let cache = scan.push_record(var, "cache", 0, 0, true, false).unwrap();
        scan.push_record(cache, "pkg.tar", 99, 3, false, false)
            .unwrap();

        SearchIndex::from_scan(
            DeviceMetadata {
                device_id: "uuid:test".into(),
                dev_node: "/dev/test".into(),
                fs_type: FsType::Ext4,
                label: "Test".into(),
                uuid: "test".into(),
                partuuid: String::new(),
            },
            scan,
            123,
        )
        .unwrap()
    }

    #[test]
    fn excludes_name_component_and_descendants() {
        let rules = compile_rules(&[PathRule {
            kind: RuleKind::Exclude,
            pattern: "node_modules".into(),
        }])
        .unwrap();
        let filtered = filter_index(sample_index(), &rules).unwrap();
        assert!(!(0..filtered.records.len() as u32).any(|idx| filtered.name(idx) == "left-pad.js"));
        assert!(
            !(0..filtered.records.len() as u32).any(|idx| filtered.name(idx) == "node_modules")
        );
        assert!((0..filtered.records.len() as u32).any(|idx| filtered.name(idx) == "main.rs"));
    }

    #[test]
    fn include_keeps_ancestors_for_paths() {
        let rules = compile_rules(&[PathRule {
            kind: RuleKind::Include,
            pattern: "/home/hiroshi/src/**".into(),
        }])
        .unwrap();
        let filtered = filter_index(sample_index(), &rules).unwrap();
        let paths: Vec<_> = (0..filtered.records.len() as u32)
            .map(|idx| filtered.internal_path(idx).to_owned())
            .collect();
        assert!(paths.iter().any(|path| path == "/home"));
        assert!(paths.iter().any(|path| path == "/home/hiroshi"));
        assert!(paths.iter().any(|path| path == "/home/hiroshi/src/main.rs"));
        assert!(!paths.iter().any(|path| path == "/var/cache/pkg.tar"));
    }

    #[test]
    fn excludes_win_over_includes() {
        let rules = compile_rules(&[
            PathRule {
                kind: RuleKind::Include,
                pattern: "/home/**".into(),
            },
            PathRule {
                kind: RuleKind::Exclude,
                pattern: "*.rs".into(),
            },
        ])
        .unwrap();
        let filtered = filter_index(sample_index(), &rules).unwrap();
        assert!(!(0..filtered.records.len() as u32).any(|idx| filtered.name(idx) == "main.rs"));
    }
}
