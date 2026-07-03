// AGENT: split FileNode storage semantics out of fd.rs so fd.rs can focus on
// descriptor and handle behavior while file node mutation stays in one module.
use super::*;

// AGENT: distinguish regular path files from directory nodes for exec checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
}

// AGENT: track unsynced changes in the in-memory file node so sync methods
// report real state transitions instead of being empty success stubs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDirty {
    pub data: bool,
    pub metadata: bool,
}

impl FileDirty {
    pub const fn clean() -> Self {
        Self {
            data: false,
            metadata: false,
        }
    }
}

// AGENT: share file contents, executable metadata, and simple directory entries
// across all handles.
pub struct FileNode {
    pub kind: FileKind,
    pub executable: AtomicBool,
    pub data: Arc<Mutex<Vec<u8>>>,
    dirty: Mutex<FileDirty>,
    dir_entries: Arc<Mutex<Vec<String>>>,
}

impl FileNode {
    // AGENT: create a regular in-memory file node with stable shared contents.
    pub fn regular(data: Vec<u8>, executable: bool) -> Self {
        Self {
            kind: FileKind::Regular,
            executable: AtomicBool::new(executable),
            data: Arc::new(Mutex::new(data)),
            dirty: Mutex::new(FileDirty::clean()),
            dir_entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // AGENT: create a directory node with a real entry list for read_entry().
    pub fn directory() -> Self {
        Self {
            kind: FileKind::Directory,
            executable: AtomicBool::new(false),
            data: Arc::new(Mutex::new(Vec::new())),
            dirty: Mutex::new(FileDirty::clean()),
            dir_entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // AGENT: add one child name to a directory node without duplicating entries.
    pub fn add_dir_entry(&self, name: &str) -> Result<(), &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        if name.is_empty() || name.contains('/') || name.bytes().any(|b| b == 0) {
            return Err("einval");
        }
        let inserted = {
            let mut entries = self.dir_entries.lock().unwrap();
            if entries.iter().any(|entry| entry == name) {
                false
            } else {
                entries.push(name.to_string());
                true
            }
        };
        if inserted {
            self.dirty.lock().unwrap().metadata = true;
        }
        Ok(())
    }

    // AGENT: fetch one directory entry by offset for handle-based iteration.
    pub fn dir_entry_at(&self, idx: usize) -> Result<String, &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        self.dir_entries
            .lock()
            .unwrap()
            .get(idx)
            .cloned()
            .ok_or("enoent")
    }

    // AGENT: check one directory-local child name without claiming to resolve
    // full paths; Kernel::lookup_path owns global path resolution.
    pub fn has_dir_entry(&self, name: &str) -> Result<bool, &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        Ok(self
            .dir_entries
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry == name))
    }

    // AGENT: expose dirty state for focused tests and future flush decisions.
    pub fn dirty_state(&self) -> FileDirty {
        *self.dirty.lock().unwrap()
    }

    // AGENT: mark content writes dirty and record metadata changes when size grew.
    pub(crate) fn note_write(&self, metadata_changed: bool) {
        let mut dirty = self.dirty.lock().unwrap();
        dirty.data = true;
        dirty.metadata |= metadata_changed;
    }

    // AGENT: mark operations such as truncate/fallocate that change file size.
    pub(crate) fn note_resize(&self) {
        let mut dirty = self.dirty.lock().unwrap();
        dirty.data = true;
        dirty.metadata = true;
    }

    // AGENT: write a byte range while centralizing growth checks and dirty
    // accounting for all FileNode-backed write paths.
    pub(crate) fn write_bytes(
        &self,
        offset: Option<usize>,
        buf: &[u8],
    ) -> Result<usize, &'static str> {
        let mut data = self.data.lock().unwrap();
        let start = offset.unwrap_or_else(|| data.len());
        let end = start.checked_add(buf.len()).ok_or("efbig")?;
        let grew = end > data.len();
        if grew {
            data.resize(end, 0);
        }
        if !buf.is_empty() {
            data[start..end].copy_from_slice(buf);
        }
        drop(data);
        if grew || !buf.is_empty() {
            self.note_write(grew);
        }
        Ok(end)
    }

    // AGENT: resize file contents and mark both data and metadata dirty only
    // when the visible file length actually changes.
    pub(crate) fn set_data_len(&self, len: usize) {
        let changed = {
            let mut data = self.data.lock().unwrap();
            if data.len() == len {
                false
            } else {
                data.resize(len, 0);
                true
            }
        };
        if changed {
            self.note_resize();
        }
    }

    // AGENT: grow file contents under one data lock so allocation cannot race
    // with another writer and accidentally shrink a larger file.
    pub(crate) fn ensure_data_len_at_least(&self, len: usize) {
        let grew = {
            let mut data = self.data.lock().unwrap();
            if data.len() >= len {
                false
            } else {
                data.resize(len, 0);
                true
            }
        };
        if grew {
            self.note_resize();
        }
    }

    // AGENT: data-only sync clears dirty file contents but leaves metadata dirty.
    pub fn sync_data(&self) -> Result<(), &'static str> {
        self.dirty.lock().unwrap().data = false;
        Ok(())
    }

    // AGENT: full sync clears both content and metadata dirty bits.
    pub fn sync_all(&self) -> Result<(), &'static str> {
        *self.dirty.lock().unwrap() = FileDirty::clean();
        Ok(())
    }
}

impl fmt::Debug for FileNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FileNode")
            .field("kind", &self.kind)
            .field("executable", &self.executable.load(Ordering::Relaxed))
            .field("len", &self.data.lock().unwrap().len())
            .field("dirty", &self.dirty_state())
            .field("entries", &self.dir_entries.lock().unwrap().len())
            .finish()
    }
}
