// AGENT: bind one filesystem-local inode namespace to exactly one FileStorage;
// directory nodes, rather than canonical full-path strings, own child names.
use super::*;

pub type FsId = usize;
pub type InodeId = u64;

pub const ROOT_FS_ID: FsId = 1;
const ROOT_INODE_ID: InodeId = 1;

// AGENT: prove that a directory-operation argument is exactly one ordinary
// child component before FileNode may use it as a namespace key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChildName<'a>(&'a str);

// AGENT: centralize direct-child construction so FsInstance and FileNode
// operations consume an already-validated component type.
impl<'a> ChildName<'a> {
    // AGENT: reject empty, navigation, and multi-component strings before they
    // can become an unreachable or ambiguous directory entry.
    pub(crate) fn new(name: &'a str) -> Result<Self, &'static str> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err("einval");
        }
        Ok(Self(name))
    }

    // AGENT: lend the validated component bytes without allowing callers to
    // construct another unchecked ChildName.
    pub(crate) fn as_str(self) -> &'a str {
        self.0
    }
}

// AGENT: identify the filesystem implementation without conflating it with a
// particular mount attachment or concrete block-device transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsKind {
    ChaosFs,
}

// AGENT: Parse the userspace filesystem type without coupling it to a
// particular device or mount attachment.
impl FsKind {
    pub fn from_name(name: &str) -> Result<Self, &'static str> {
        match name {
            "chaosfs" => Ok(Self::ChaosFs),
            _ => Err("enodev"),
        }
    }
}

// AGENT: own live runtime inodes by filesystem-local stable identity together
// with the only FileStorage allowed to back their data and metadata blocks.
pub struct FsInstance {
    id: FsId,
    kind: FsKind,
    storage: FileStorage,
    root: Arc<FileNode>,
    inodes: RwLock<BTreeMap<InodeId, Arc<FileNode>>>,
    next_inode: AtomicU64,
}

// AGENT: centralize inode lookup, direct-child traversal, allocation, and
// namespace mutation behind the FsInstance ownership boundary.
impl FsInstance {
    // AGENT: construct an empty ChaosFs namespace with a stable root inode and
    // retain the caller-selected storage as this instance's sole backend.
    pub fn new(id: FsId, storage: FileStorage) -> Arc<Self> {
        let root = Arc::new(FileNode::directory(ROOT_INODE_ID));
        let mut inodes = BTreeMap::new();
        inodes.insert(ROOT_INODE_ID, root.clone());
        Arc::new(Self {
            id,
            kind: FsKind::ChaosFs,
            storage,
            root,
            inodes: RwLock::new(inodes),
            next_inode: AtomicU64::new(ROOT_INODE_ID + 1),
        })
    }

    // AGENT: expose the runtime filesystem identity used by VFS allocation and
    // diagnostics without treating a source pathname as filesystem identity.
    pub fn id(&self) -> FsId {
        self.id
    }

    // AGENT: expose the implementation kind independently from mount flags.
    pub fn kind(&self) -> FsKind {
        self.kind
    }

    // AGENT: return the stable root inode owned by this filesystem instance.
    pub fn root(&self) -> Arc<FileNode> {
        self.root.clone()
    }

    // AGENT: lend the sole storage backend for managed-node I/O without moving
    // cache, device, or allocator ownership back into Kernel.
    pub fn storage(&self) -> &FileStorage {
        &self.storage
    }

    // AGENT: look up one live managed inode by filesystem-local identity.
    pub fn lookup_inode(&self, inode: InodeId) -> Result<Arc<FileNode>, &'static str> {
        self.inodes
            .read()
            .unwrap()
            .get(&inode)
            .cloned()
            .ok_or("enoent")
    }

    // AGENT: validate that a parent object is the live directory registered at
    // its inode number in this filesystem rather than a foreign lookalike.
    fn require_owned_directory(
        inodes: &BTreeMap<InodeId, Arc<FileNode>>,
        parent: &Arc<FileNode>,
    ) -> Result<(), &'static str> {
        let owned = inodes.get(&parent.id()).ok_or("exdev")?;
        if !Arc::ptr_eq(owned, parent) {
            return Err("exdev");
        }
        if parent.kind != FileKind::Directory {
            return Err("enotdir");
        }
        Ok(())
    }

    // AGENT: look up exactly one direct child through its parent directory
    // object without constructing or interpreting a full pathname.
    pub(crate) fn lookup_child(
        &self,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
    ) -> Result<Arc<FileNode>, &'static str> {
        let inodes = self.inodes.read().unwrap();
        Self::require_owned_directory(&inodes, parent)?;
        let inode = parent.lookup_child_inode(name)?;
        inodes.get(&inode).cloned().ok_or("eio")
    }

    // AGENT: prove that a proposed mountpoint is the live inode object currently
    // registered at its identity in this filesystem instance.
    pub(crate) fn owns_node(&self, node: &Arc<FileNode>) -> bool {
        self.inodes
            .read()
            .unwrap()
            .get(&node.id())
            .is_some_and(|owned| Arc::ptr_eq(owned, node))
    }

    // AGENT: allocate monotonically increasing runtime inode identities; disk
    // persistence and recovery deliberately remain outside this VFS stage.
    fn allocate_inode_id(&self) -> InodeId {
        self.next_inode
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("runtime inode id space exhausted")
    }

    // AGENT: allocate one regular node owned by this instance.
    fn new_regular_node(&self, executable: bool) -> Arc<FileNode> {
        Arc::new(FileNode::regular(self.allocate_inode_id(), executable))
    }

    // AGENT: allocate one directory node owned by this instance.
    fn new_directory_node(&self) -> Arc<FileNode> {
        Arc::new(FileNode::directory(self.allocate_inode_id()))
    }

    // AGENT: publish a new child binding and then register its inode while the
    // caller holds the filesystem namespace write lock.
    fn insert_new_child_locked(
        &self,
        inodes: &mut BTreeMap<InodeId, Arc<FileNode>>,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
        node: Arc<FileNode>,
    ) -> Result<Arc<FileNode>, &'static str> {
        match parent.lookup_child_inode(name) {
            Ok(_) => return Err("eexist"),
            Err("enoent") => {}
            Err(error) => return Err(error),
        }
        parent.insert_child(&self.storage, name, node.id())?;
        let replaced = inodes.insert(node.id(), node.clone());
        debug_assert!(
            replaced.is_none(),
            "fresh inode identity already registered"
        );
        Ok(node)
    }

    // AGENT: retarget one existing child binding and replace its registered
    // inode while the caller holds the filesystem namespace write lock.
    fn replace_child_locked(
        &self,
        inodes: &mut BTreeMap<InodeId, Arc<FileNode>>,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
        existing: Arc<FileNode>,
        replacement: Arc<FileNode>,
    ) -> Result<Arc<FileNode>, &'static str> {
        parent.replace_child_inode(&self.storage, name, existing.id(), replacement.id())?;
        let removed = inodes.remove(&existing.id());
        debug_assert!(
            removed
                .as_ref()
                .is_some_and(|owned| Arc::ptr_eq(owned, &existing)),
            "replaced inode was not registered"
        );
        let replaced = inodes.insert(replacement.id(), replacement.clone());
        debug_assert!(
            replaced.is_none(),
            "fresh replacement inode identity already registered"
        );
        Ok(replacement)
    }

    // AGENT: create a new regular inode atomically under one parent directory
    // and reject every pre-existing direct-child name.
    pub(crate) fn create_regular_at(
        &self,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
        executable: bool,
    ) -> Result<Arc<FileNode>, &'static str> {
        let mut inodes = self.inodes.write().unwrap();
        Self::require_owned_directory(&inodes, parent)?;
        match parent.lookup_child_inode(name) {
            Ok(_) => return Err("eexist"),
            Err("enoent") => {}
            Err(error) => return Err(error),
        }
        let node = self.new_regular_node(executable);
        self.insert_new_child_locked(&mut inodes, parent, name, node)
    }

    // AGENT: create a new directory inode atomically under one live directory
    // while preserving strict mkdir EEXIST semantics.
    pub(crate) fn create_directory_at(
        &self,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
    ) -> Result<Arc<FileNode>, &'static str> {
        let mut inodes = self.inodes.write().unwrap();
        Self::require_owned_directory(&inodes, parent)?;
        match parent.lookup_child_inode(name) {
            Ok(_) => return Err("eexist"),
            Err("enoent") => {}
            Err(error) => return Err(error),
        }
        let node = self.new_directory_node();
        self.insert_new_child_locked(&mut inodes, parent, name, node)
    }

    // AGENT: combine direct-child lookup and optional creation under one
    // namespace lock for transactional openat and O_EXCL behavior.
    pub(crate) fn open_regular_at(
        &self,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
        create: bool,
        exclusive: bool,
    ) -> Result<Arc<FileNode>, &'static str> {
        let mut inodes = self.inodes.write().unwrap();
        Self::require_owned_directory(&inodes, parent)?;
        match parent.lookup_child_inode(name) {
            Ok(inode) => {
                let node = inodes.get(&inode).cloned().ok_or("eio")?;
                if exclusive {
                    return Err("eexist");
                }
                if node.kind != FileKind::Regular {
                    return Err("eisdir");
                }
                Ok(node)
            }
            Err("enoent") if create => {
                let node = self.new_regular_node(false);
                self.insert_new_child_locked(&mut inodes, parent, name, node)
            }
            Err(error) => Err(error),
        }
    }

    // AGENT: install or replace one kernel-owned regular inode while retaining
    // the direct-child iteration position and using this filesystem's backend.
    pub(crate) fn install_regular_at(
        &self,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
        data: &[u8],
        executable: bool,
    ) -> Result<Arc<FileNode>, &'static str> {
        let mut inodes = self.inodes.write().unwrap();
        Self::require_owned_directory(&inodes, parent)?;
        let existing = match parent.lookup_child_inode(name) {
            Ok(inode) => Some(inodes.get(&inode).cloned().ok_or("eio")?),
            Err("enoent") => None,
            Err(error) => return Err(error),
        };
        if existing
            .as_ref()
            .is_some_and(|node| node.kind != FileKind::Regular)
        {
            return Err("eisdir");
        }

        let node = self.new_regular_node(executable);
        node.write_initial_bytes(&self.storage, data)?;
        if let Some(existing) = existing {
            self.replace_child_locked(&mut inodes, parent, name, existing, node)
        } else {
            self.insert_new_child_locked(&mut inodes, parent, name, node)
        }
    }

    // AGENT: establish a directory idempotently for kernel boot fixtures while
    // keeping user-requested mkdir on create_directory_at.
    pub(crate) fn install_directory_at(
        &self,
        parent: &Arc<FileNode>,
        name: ChildName<'_>,
    ) -> Result<Arc<FileNode>, &'static str> {
        let mut inodes = self.inodes.write().unwrap();
        Self::require_owned_directory(&inodes, parent)?;
        match parent.lookup_child_inode(name) {
            Ok(inode) => {
                let existing = inodes.get(&inode).cloned().ok_or("eio")?;
                if existing.kind == FileKind::Directory {
                    Ok(existing)
                } else {
                    Err("eexist")
                }
            }
            Err("enoent") => {
                let node = self.new_directory_node();
                self.insert_new_child_locked(&mut inodes, parent, name, node)
            }
            Err(error) => Err(error),
        }
    }
}
