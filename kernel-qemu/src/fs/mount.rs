// AGENT: model filesystem attachments by mount and inode identity instead of
// rewriting absolute path strings into synthetic device-prefixed keys.
use super::*;

pub type MountId = usize;
const ROOT_MOUNT_ID: MountId = 1;

// AGENT: retain an explicit mount-flags value even though the first object-VFS
// stage accepts only the empty set at the syscall boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MountFlags {
    bits: usize,
}

// AGENT: keep flag construction and inspection explicit for later remount and
// policy work without implementing those semantics in this stage.
impl MountFlags {
    // AGENT: construct the only mount flag set supported in this first stage.
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    // AGENT: expose the retained flag bits without making the field mutable.
    pub const fn bits(self) -> usize {
        self.bits
    }
}

// AGENT: represent one attachment of an FsInstance into the mount topology;
// parent is weak so the topology cannot form an Arc reference cycle.
pub struct Mount {
    id: MountId,
    fs: Arc<FsInstance>,
    parent: Option<Weak<Mount>>,
    mountpoint: Option<Arc<FileNode>>,
    flags: MountFlags,
}

// AGENT: expose immutable mount identity and ownership links while leaving all
// topology mutation in MountTable.
impl Mount {
    // AGENT: construct the unique root attachment with no parent or mountpoint.
    fn root(fs: Arc<FsInstance>) -> Arc<Self> {
        Arc::new(Self {
            id: ROOT_MOUNT_ID,
            fs,
            parent: None,
            mountpoint: None,
            flags: MountFlags::empty(),
        })
    }

    // AGENT: construct a child attachment whose parent lifetime is controlled
    // by MountTable or extant FInstance values rather than a strong back-edge.
    fn attached(
        id: MountId,
        fs: Arc<FsInstance>,
        parent: &Arc<Mount>,
        mountpoint: Arc<FileNode>,
        flags: MountFlags,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            fs,
            parent: Some(Arc::downgrade(parent)),
            mountpoint: Some(mountpoint),
            flags,
        })
    }

    // AGENT: expose stable attachment identity for topology keys and diagnostics.
    pub fn id(&self) -> MountId {
        self.id
    }

    // AGENT: expose the filesystem shared by every path through this attachment.
    pub fn fs(&self) -> &Arc<FsInstance> {
        &self.fs
    }

    // AGENT: upgrade the non-owning parent edge while the parent remains alive.
    pub fn parent(&self) -> Option<Arc<Mount>> {
        self.parent.as_ref().and_then(Weak::upgrade)
    }

    // AGENT: clone the stable inode object at which this child was attached.
    pub fn mountpoint(&self) -> Option<Arc<FileNode>> {
        self.mountpoint.clone()
    }

    // AGENT: return immutable attachment flags reserved for later VFS stages.
    pub fn flags(&self) -> MountFlags {
        self.flags
    }
}

// AGENT: key a mount stack by the parent view and stable mountpoint inode rather
// than by a rename-sensitive full pathname.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MountPointKey {
    pub parent_mount: MountId,
    pub inode: InodeId,
}

// AGENT: own the root mount and every attached child strongly, with each
// mountpoint value storing bottom-to-top stacking order.
pub struct MountTable {
    root: Arc<Mount>,
    children: RwLock<BTreeMap<MountPointKey, Vec<Arc<Mount>>>>,
    next_mount_id: AtomicUsize,
}

// AGENT: centralize mount membership, stacking, lookup, and detach semantics in
// an identity-based topology independent from pathname parsing.
impl MountTable {
    // AGENT: attach the supplied root filesystem as the topology root.
    pub fn new(root_fs: Arc<FsInstance>) -> Self {
        Self {
            root: Mount::root(root_fs),
            children: RwLock::new(BTreeMap::new()),
            next_mount_id: AtomicUsize::new(ROOT_MOUNT_ID + 1),
        }
    }

    // AGENT: return the stable root attachment used to begin every absolute
    // path resolution.
    pub fn root(&self) -> Arc<Mount> {
        self.root.clone()
    }

    // AGENT: test active membership under the caller-held children lock so a
    // stale detached parent cannot receive new topology entries.
    fn contains_mount_locked(
        &self,
        children: &BTreeMap<MountPointKey, Vec<Arc<Mount>>>,
        mount: &Arc<Mount>,
    ) -> bool {
        Arc::ptr_eq(&self.root, mount)
            || children
                .values()
                .flatten()
                .any(|candidate| Arc::ptr_eq(candidate, mount))
    }

    // AGENT: allocate monotonically increasing mount identities without using
    // filesystem IDs or source path strings as attachment identity.
    fn allocate_mount_id(&self) -> MountId {
        self.next_mount_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("runtime mount id space exhausted")
    }

    // AGENT: attach one filesystem above a managed directory mountpoint and
    // push it on the existing stack rather than replacing lower attachments.
    pub fn attach(
        &self,
        parent: &Arc<Mount>,
        mountpoint: Arc<FileNode>,
        fs: Arc<FsInstance>,
        flags: MountFlags,
    ) -> Result<Arc<Mount>, &'static str> {
        if mountpoint.kind != FileKind::Directory || fs.root().kind != FileKind::Directory {
            return Err("enotdir");
        }
        if !parent.fs().owns_node(&mountpoint) {
            return Err("exdev");
        }
        let mut children = self.children.write().unwrap();
        if !self.contains_mount_locked(&children, parent) {
            return Err("einval");
        }
        let mount = Mount::attached(
            self.allocate_mount_id(),
            fs,
            parent,
            mountpoint.clone(),
            flags,
        );
        children
            .entry(MountPointKey {
                parent_mount: parent.id(),
                inode: mountpoint.id(),
            })
            .or_default()
            .push(mount.clone());
        Ok(mount)
    }

    // AGENT: return the currently visible top attachment for one parent view and
    // inode, leaving lower stacked mounts intact.
    pub fn mounted_on(&self, parent: &Arc<Mount>, node: &Arc<FileNode>) -> Option<Arc<Mount>> {
        if !parent.fs().owns_node(node) {
            return None;
        }
        self.children
            .read()
            .unwrap()
            .get(&MountPointKey {
                parent_mount: parent.id(),
                inode: node.id(),
            })
            .and_then(|stack| stack.last().cloned())
    }

    // AGENT: pop and return only the visible attachment so existing FInstance and
    // explicit mount holders remain valid after topology removal.
    pub fn detach_top(
        &self,
        parent: &Arc<Mount>,
        node: &Arc<FileNode>,
    ) -> Result<Arc<Mount>, &'static str> {
        if !parent.fs().owns_node(node) {
            return Err("exdev");
        }
        let mut children = self.children.write().unwrap();
        if !self.contains_mount_locked(&children, parent) {
            return Err("einval");
        }
        let key = MountPointKey {
            parent_mount: parent.id(),
            inode: node.id(),
        };
        let (mount, remove_key) = {
            let stack = children.get_mut(&key).ok_or("einval")?;
            let mount = stack.pop().ok_or("einval")?;
            (mount, stack.is_empty())
        };
        if remove_key {
            children.remove(&key);
        }
        Ok(mount)
    }

    // AGENT: report active non-root attachments, counting every stacked mount.
    pub fn mount_count(&self) -> usize {
        self.children.read().unwrap().values().map(Vec::len).sum()
    }
}

// AGENT: keep identity-topology regressions next to the mount implementation.
#[cfg(any(test, feature = "qemu-sync-selftest"))]
#[path = "mount_tests.rs"]
pub mod tests;
