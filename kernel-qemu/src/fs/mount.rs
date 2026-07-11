use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::irq_lock::RwLock;

// AGENT: Keep one canonical mount-point to target binding.
pub struct MountEntry {
    pub prefix: String,
    pub target: String,
}

// AGENT: Own mount bindings and path-resolution policy independently from I/O
// scheduling and simulated disk behavior.
pub struct MountTable {
    pub entries: RwLock<Vec<MountEntry>>,
}

// AGENT: Preserve the existing mount-table behavior after extracting it from
// the mixed mount_io_disk module.
impl MountTable {
    // AGENT: Construct an empty mount table.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    // AGENT: accept only non-root absolute mount points and store them in one
    // canonical form so bind, unmount, and has_prefix agree.
    fn normalize_prefix(pfx: &str) -> Option<String> {
        if !pfx.starts_with('/') {
            return None;
        }
        let normalized = Self::canonicalize_path(pfx);
        if normalized == "/" {
            None
        } else {
            Some(normalized)
        }
    }

    // AGENT: collapse duplicate slashes and dot components before mount lookup.
    fn canonicalize_path(path: &str) -> String {
        let absolute = path.starts_with('/');
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if !parts.is_empty() {
                        parts.pop();
                    } else if !absolute {
                        parts.push("..");
                    }
                }
                part => parts.push(part),
            }
        }

        let mut normalized = String::new();
        if absolute {
            normalized.push('/');
        }
        for (idx, part) in parts.iter().enumerate() {
            if idx > 0 {
                normalized.push('/');
            }
            normalized.push_str(part);
        }
        if normalized.is_empty() && absolute {
            normalized.push('/');
        }
        normalized
    }

    // AGENT: require a directory-boundary match so /mnt does not also match
    // /mnt2; mount prefixes are already canonical and non-root.
    fn prefix_matches_path(prefix: &str, path: &str) -> bool {
        if !path.starts_with(prefix) {
            return false;
        }
        path.len() == prefix.len() || path.as_bytes().get(prefix.len()) == Some(&b'/')
    }

    // AGENT: canonicalize mount bindings, keep one target per prefix, preserve
    // longest-prefix-first lookup, and report invalid syscall-facing inputs.
    pub fn mount(&self, pfx: &str, tgt: &str) -> Result<(), &'static str> {
        let prefix = Self::normalize_prefix(pfx).ok_or("einval")?;
        if tgt.is_empty() {
            return Err("einval");
        }
        let mut e = self.entries.write().unwrap();
        if let Some(existing) = e.iter_mut().find(|m| m.prefix == prefix) {
            existing.target = tgt.to_string();
            return Ok(());
        }
        let insert_at = e
            .iter()
            .position(|m| m.prefix.len() < prefix.len())
            .unwrap_or(e.len());
        e.insert(
            insert_at,
            MountEntry {
                prefix,
                target: tgt.to_string(),
            },
        );
        Ok(())
    }

    // AGENT: retain the original compatibility helper for in-kernel callers
    // that intentionally ignore invalid mount requests.
    pub fn bind(&self, pfx: &str, tgt: &str) {
        let _ = self.mount(pfx, tgt);
    }

    // AGENT: Resolve one longest mount prefix without recursively remapping the
    // remaining path through unrelated mounts.
    pub fn resolve(&self, path: &str) -> Result<String, &'static str> {
        let canonical = Self::canonicalize_path(path);
        let matched = {
            let tbl = self.entries.read().unwrap();
            Self::find_mount_id_locked(&tbl, &canonical).map(|idx| {
                let m = &tbl[idx];
                let rest = if canonical.len() == m.prefix.len() {
                    "/".to_string()
                } else {
                    canonical[m.prefix.len()..].to_string()
                };
                (m.target.clone(), rest)
            })
        };

        Ok(match matched {
            Some((dev, rest)) => {
                let mut result = String::with_capacity(dev.len() + 1 + rest.len());
                result.push_str(&dev);
                result.push(':');
                result.push_str(&rest);
                result
            }
            None => canonical,
        })
    }

    // AGENT: normalize and remove exactly one syscall-facing mount point,
    // reporting invalid or absent bindings instead of silently succeeding.
    pub fn umount(&self, pfx: &str) -> Result<(), &'static str> {
        let prefix = Self::normalize_prefix(pfx).ok_or("einval")?;
        let mut e = self.entries.write().unwrap();
        let before = e.len();
        let mut index = 0;
        while index < e.len() {
            if e[index].prefix == prefix {
                e.remove(index);
            } else {
                index += 1;
            }
        }
        if e.len() == before {
            Err("einval")
        } else {
            Ok(())
        }
    }

    // AGENT: retain the original boolean compatibility helper on top of the
    // syscall-facing exact unmount operation.
    pub fn unmount(&self, pfx: &str) -> bool {
        self.umount(pfx).is_ok()
    }

    // AGENT: Return a detached snapshot of the current mount bindings.
    pub fn list_mounts(&self) -> Vec<(String, String)> {
        let tbl = self.entries.read().unwrap();
        let mut result = Vec::with_capacity(tbl.len());
        for m in tbl.iter() {
            result.push((m.prefix.clone(), m.target.clone()));
        }
        result
    }

    // AGENT: Scan a caller-held mount table snapshot in longest-prefix-first
    // order, returning the first complete path-component prefix without taking
    // another lock.
    fn find_mount_id_locked(tbl: &[MountEntry], path: &str) -> Option<usize> {
        for (idx, m) in tbl.iter().enumerate() {
            if Self::prefix_matches_path(&m.prefix, path) {
                return Some(idx);
            }
        }
        None
    }

    // AGENT: Keep the legacy helper API while delegating to the non-locking
    // scanner under a single read guard.
    fn find_mount_id(&self, path: &str) -> Option<usize> {
        let canonical = Self::canonicalize_path(path);
        let tbl = self.entries.read().unwrap();
        Self::find_mount_id_locked(&tbl, &canonical)
    }

    // AGENT: Clone the matching mount entry while holding one read lock so the
    // saved index cannot race with concurrent bind or unmount operations.
    pub fn find_mount(&self, path: &str) -> Option<MountEntry> {
        let canonical = Self::canonicalize_path(path);
        let tbl = self.entries.read().unwrap();
        let best_match_idx = Self::find_mount_id_locked(&tbl, &canonical);
        best_match_idx.map(|idx| {
            let m = &tbl[idx];
            MountEntry {
                prefix: m.prefix.clone(),
                target: m.target.clone(),
            }
        })
    }

    // AGENT: Report the number of active mount bindings.
    pub fn mount_count(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    // AGENT: query prefixes through the same canonical form used by bind.
    pub fn has_prefix(&self, pfx: &str) -> bool {
        let Some(prefix) = Self::normalize_prefix(pfx) else {
            return false;
        };
        self.entries
            .read()
            .unwrap()
            .iter()
            .any(|m| m.prefix == prefix)
    }
}

// AGENT: Keep mount-table regressions next to the extracted mount module.
#[cfg(any(test, feature = "qemu-sync-selftest"))]
#[path = "mount_tests.rs"]
pub mod tests;
