// AGENT
use super::*;

// AGENT: identify one filesystem object by its mount view and managed inode;
// pathname text and per-open state deliberately remain outside this type.
#[derive(Clone)]
pub struct FInstance {
    pub mount: Arc<Mount>,
    pub node: Arc<FileNode>,
    mount_pin: MountPin,
}

// AGENT: keep stateless inode operations on stable file-object identity so
// callers never need a second node or FileStorage field.
impl FInstance {
    // AGENT: admit only mount/node pairs whose filesystem owns the managed inode.
    pub fn new(mount: Arc<Mount>, node: Arc<FileNode>) -> Self {
        assert!(
            mount.fs().owns_node(&node),
            "FInstance node must belong to its mount filesystem"
        );
        let mount_pin = MountPin::try_new(mount.clone())
            .expect("FInstance mount must still be attached when first pinned");
        Self {
            mount,
            node,
            mount_pin,
        }
    }

    // AGENT: consume the pin acquired under MountTable's read lock so crossing
    // a visible mount never releases topology protection before active_refs rises.
    pub(crate) fn from_mount_pin(mount_pin: MountPin, node: Arc<FileNode>) -> Self {
        let mount = mount_pin.mount().clone();
        assert!(
            mount.fs().owns_node(&node),
            "FInstance node must belong to its mount filesystem"
        );
        Self {
            mount,
            node,
            mount_pin,
        }
    }

    // AGENT: derive another inode identity inside an already-pinned mount even
    // after lazy detach; cloning the existing pin avoids a detach race window.
    pub(crate) fn with_node(&self, node: Arc<FileNode>) -> Self {
        assert!(
            self.mount.fs().owns_node(&node),
            "FInstance node must belong to its mount filesystem"
        );
        Self {
            mount: self.mount.clone(),
            node,
            mount_pin: self.mount_pin.clone(),
        }
    }

    // AGENT: lend the only storage backend permitted to serve this filesystem's
    // managed node without caching another FileStorage handle.
    pub(crate) fn storage(&self) -> &FileStorage {
        self.mount.fs().storage()
    }

    // AGENT: expose the FileNode-owned byte-precise EOF through stable identity.
    pub fn len(&self) -> usize {
        self.node.len()
    }

    // AGENT: bind stat device identity to the shared filesystem rather than one
    // mount attachment, then delegate inode-owned fields to FileNode.
    pub fn file_attr(&self) -> Result<FileAttr, &'static str> {
        self.node.file_attr(self.mount.fs().id())
    }

    // AGENT: copy from a regular node at an explicit offset without touching
    // open-file-description state.
    fn copy_from_node_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.node.read_bytes(self.storage(), off, buf)
    }

    // AGENT: keep positioned reads as pure mount-backed object operations; fd
    // permission and offset handling remain above FInstance.
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.copy_from_node_at(off, buf)
    }

    // AGENT: keep positioned writes as pure mount-backed object operations; fd
    // permission and offset handling remain above FInstance.
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        self.node.write_bytes(self.storage(), Some(off), buf)?;
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

    // AGENT: truncate the referenced object through its owning filesystem's
    // storage while leaving fd write-permission checks to OpenFileDesc.
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        let len = usize::try_from(len).map_err(|_| "efbig")?;
        self.node.set_data_len(self.storage(), len)?;
        Ok(())
    }

    // AGENT: inspect one directory entry by explicit index; FHandle owns the
    // per-open iteration offset.
    pub fn read_entry(&self, idx: usize) -> Result<String, &'static str> {
        self.node.dir_entry_at(idx)
    }

    // AGENT: expose supported regular-file ioctl results without embedding an
    // open-description offset in stable file-object identity.
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

    // AGENT: grow one regular object through the FileNode single-lock helper;
    // descriptor access checks remain in the fd layer.
    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if self.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if len == 0 {
            return Err("einval");
        }
        let needed = offset.checked_add(len).ok_or("efbig")?;
        self.node.ensure_data_len_at_least(self.storage(), needed)?;
        Ok(())
    }
}

// AGENT: format stable object identity rather than a rename-sensitive pathname.
impl fmt::Debug for FInstance {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FInstance")
            .field("mount", &self.mount.id())
            .field("fs", &self.mount.fs().id())
            .field("inode", &self.node.id())
            .finish()
    }
}
