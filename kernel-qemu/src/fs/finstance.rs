// AGENT
use super::*;

// AGENT: regular file instances only identify the backing file object. Per-open
// offset lives in FHandle, while access/status flags live in OpenFileDesc.
#[derive(Clone)]
pub struct FInstance {
    pub path: String,
    path_ref: PathRef,
}

impl FInstance {
    // AGENT: create a fresh managed standalone regular node for device-like
    // instances without admitting an arbitrary node/storage pairing.
    pub fn new(path: &str) -> Self {
        Self::with_data(path, Vec::new())
    }

    // AGENT: create a managed standalone filesystem, seed one regular inode,
    // and derive the handle backend from its root mount.
    pub fn with_data(path: &str, d: Vec<u8>) -> Self {
        let storage = FileStorage::standalone();
        let fs = FsInstance::new(0, storage);
        let node = fs
            .install_regular("/file", &d, false)
            .expect("standalone RAM file seed should fit");
        let mount = MountTable::new(fs).root();
        Self::from_resolved(ResolvedPath {
            path_ref: PathRef { mount, node },
            display_path: path.to_string(),
        })
    }

    // AGENT: create a managed standalone directory instance for focused fd
    // iteration and fallocate error regressions.
    pub fn directory(path: &str) -> Self {
        let fs = FsInstance::new(0, FileStorage::standalone());
        let node = fs
            .install_directory("/dir")
            .expect("standalone directory should install");
        let mount = MountTable::new(fs).root();
        Self::from_resolved(ResolvedPath {
            path_ref: PathRef { mount, node },
            display_path: path.to_string(),
        })
    }

    // AGENT: derive a backing file object only from mount-plus-node identity so
    // the selected storage always belongs to the node's FsInstance.
    pub fn from_path(path: PathRef) -> Self {
        assert!(
            path.mount.fs().owns_node(&path.node),
            "PathRef node must belong to its mount filesystem"
        );
        Self {
            path: String::new(),
            path_ref: path,
        }
    }

    // AGENT: preserve a canonical external name for diagnostics while deriving
    // all object and storage identity from ResolvedPath::path_ref.
    pub fn from_resolved(path: ResolvedPath) -> Self {
        let display_path = path.display_path;
        let mut instance = Self::from_path(path.path_ref);
        instance.path = display_path;
        instance
    }

    // AGENT: expose the immutable mount-plus-node identity without flattening it
    // back into independently replaceable node and storage fields.
    pub fn path_ref(&self) -> &PathRef {
        &self.path_ref
    }

    // AGENT: lend the node through the retained PathRef so every caller keeps
    // the inode tied to the filesystem instance that owns it.
    pub fn node(&self) -> &Arc<FileNode> {
        &self.path_ref.node
    }

    // AGENT: derive backend identity from the retained mount for each operation
    // instead of caching a second FileStorage handle in FInstance.
    pub(crate) fn storage(&self) -> &FileStorage {
        self.path_ref.mount.fs().storage()
    }

    // AGENT: duplicate only the file object reference; open-description state is
    // intentionally not part of FInstance.
    pub fn dup(&self) -> Self {
        FInstance {
            path: self.path.clone(),
            path_ref: self.path_ref.clone(),
        }
    }

    // AGENT: expose the FileNode-owned byte-precise EOF through regular instances.
    pub fn len(&self) -> usize {
        self.node().len()
    }

    // AGENT: copy from a regular file node at an explicit offset without
    // touching descriptor state.
    fn copy_from_node_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.node().read_bytes(self.storage(), off, buf)
    }

    // AGENT: direct positioned reads are pure file-object reads; fd permission
    // checks belong to OpenFileDesc.
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.copy_from_node_at(off, buf)
    }

    // AGENT: direct positioned writes are pure file-object writes; fd permission
    // checks belong to OpenFileDesc.
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        self.node().write_bytes(self.storage(), Some(off), buf)?;
        Ok(buf.len())
    }

    // AGENT: copy a regular-file byte range without changing descriptor state.
    pub(super) fn copy_chunk_at(&self, off: usize, count: usize) -> Result<Vec<u8>, &'static str> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut data = vec![0; count];
        let n = self.copy_from_node_at(off, &mut data)?;
        data.truncate(n);
        Ok(data)
    }

    // AGENT: direct truncation mutates only the backing file object; write
    // permission checks belong to OpenFileDesc.
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        let len = usize::try_from(len).map_err(|_| "efbig")?;
        self.node().set_data_len(self.storage(), len)?;
        Ok(())
    }
    // AGENT: direct directory inspection stays stateless and uses the caller's
    // explicit entry index; fd-level iteration advances OpenFileDesc.
    pub fn read_entry(&self, idx: usize) -> Result<String, &'static str> {
        self.node().dir_entry_at(idx)
    }
    // AGENT: regular files only report supported ioctl results; unknown
    // requests must not be silently treated as success.
    pub(super) fn io_ctl_with_offset(
        &self,
        cmd: usize,
        offset: u64,
    ) -> Result<usize, &'static str> {
        match cmd {
            FIONREAD | TIOCINQ => {
                let len = self.len() as u64;
                usize::try_from(len.saturating_sub(offset)).map_err(|_| "eoverflow")
            }
            _ => Err("enotty"),
        }
    }

    // AGENT: direct allocation validates regular-file semantics and grows the
    // node through the single-lock FileNode helper.
    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if self.node().kind != FileKind::Regular {
            return Err("enodev");
        }
        if len == 0 {
            return Err("einval");
        }
        let needed = offset.checked_add(len).ok_or("efbig")?;
        self.node()
            .ensure_data_len_at_least(self.storage(), needed)?;
        Ok(())
    }
}

impl fmt::Debug for FInstance {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FI").field("path", &self.path).finish()
    }
}
