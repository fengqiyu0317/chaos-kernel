// AGENT: bind one filesystem-local node namespace to exactly one FileStorage
// while the first VFS stage still indexes nodes by canonical relative paths.
use super::*;

pub type FsId = usize;
pub type InodeId = u64;

pub const ROOT_FS_ID: FsId = 1;
const ROOT_INODE_ID: InodeId = 1;

// AGENT: identify the filesystem implementation without conflating it with a
// particular mount attachment or concrete block-device transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FsKind {
    ChaosFs,
}

// AGENT: represent one filesystem instance and own all FileNodes together with
// the only FileStorage allowed to back their data and metadata blocks.
pub struct FsInstance {
    id: FsId,
    kind: FsKind,
    storage: FileStorage,
    root: Arc<FileNode>,
    nodes: RwLock<BTreeMap<String, Arc<FileNode>>>,
    next_inode: AtomicU64,
}

// AGENT: centralize filesystem-local lookup, inode allocation, and strict
// parent-directory insertion behind the FsInstance ownership boundary.
impl FsInstance {
    // AGENT: construct an empty ChaosFs namespace with a stable root inode and
    // retain the caller-selected storage as this instance's sole backend.
    pub fn new(id: FsId, storage: FileStorage) -> Arc<Self> {
        let root = Arc::new(FileNode::directory(ROOT_INODE_ID));
        let mut nodes = BTreeMap::new();
        nodes.insert(String::from("/"), root.clone());
        Arc::new(Self {
            id,
            kind: FsKind::ChaosFs,
            storage,
            root,
            nodes: RwLock::new(nodes),
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

    // AGENT: canonicalize one filesystem-internal absolute path while the first
    // stage still uses a flat map instead of directory-component dentries.
    pub(crate) fn normalize_path(path: &str) -> Result<String, &'static str> {
        if path.is_empty() {
            return Err("enoent");
        }
        if !path.starts_with('/') {
            return Err("einval");
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                component => parts.push(component),
            }
        }
        let mut normalized = String::from("/");
        for (index, component) in parts.iter().enumerate() {
            if index != 0 {
                normalized.push('/');
            }
            normalized.push_str(component);
        }
        Ok(normalized)
    }

    // AGENT: look up a managed node by its filesystem-internal canonical path.
    pub fn lookup(&self, path: &str) -> Result<Arc<FileNode>, &'static str> {
        let path = Self::normalize_path(path)?;
        self.nodes
            .read()
            .unwrap()
            .get(&path)
            .cloned()
            .ok_or("enoent")
    }

    // AGENT: prove that a proposed mountpoint is one of this instance's live
    // managed inode objects rather than a same-number inode from another fs.
    pub(crate) fn owns_node(&self, node: &Arc<FileNode>) -> bool {
        self.nodes
            .read()
            .unwrap()
            .values()
            .any(|candidate| Arc::ptr_eq(candidate, node))
    }

    // AGENT: allocate monotonically increasing runtime inode identities; disk
    // persistence and recovery deliberately remain outside this first stage.
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

    // AGENT: split a non-root internal path into its canonical parent key and
    // direct child name without interpreting symlinks or crossing mounts.
    fn parent_dir_entry(path: &str) -> Option<(String, String)> {
        if path == "/" {
            return None;
        }
        let slash = path.rfind('/')?;
        let name = &path[slash + 1..];
        if name.is_empty() {
            return None;
        }
        let parent = if slash == 0 { "/" } else { &path[..slash] };
        Some((parent.to_string(), name.to_string()))
    }

    // AGENT: validate an existing directory parent while the caller holds the
    // filesystem node-table write lock.
    fn require_parent_dir(
        nodes: &BTreeMap<String, Arc<FileNode>>,
        path: &str,
    ) -> Result<(Arc<FileNode>, String), &'static str> {
        let (parent, name) = Self::parent_dir_entry(path).ok_or("enoent")?;
        let parent = nodes.get(&parent).cloned().ok_or("enoent")?;
        if parent.kind != FileKind::Directory {
            return Err("enotdir");
        }
        Ok((parent, name))
    }

    // AGENT: publish one new node only after its parent metadata is updated
    // through this same filesystem instance's storage backend.
    fn insert_new_child_locked(
        &self,
        nodes: &mut BTreeMap<String, Arc<FileNode>>,
        path: String,
        node: Arc<FileNode>,
    ) -> Result<Arc<FileNode>, &'static str> {
        if nodes.contains_key(&path) {
            return Err("eexist");
        }
        let (parent, name) = Self::require_parent_dir(nodes, &path)?;
        parent.add_dir_entry(&self.storage, &name)?;
        nodes.insert(path, node.clone());
        Ok(node)
    }

    // AGENT: create a new regular inode atomically in this filesystem-local
    // namespace and reject every pre-existing path.
    pub fn create_regular(
        &self,
        path: &str,
        executable: bool,
    ) -> Result<Arc<FileNode>, &'static str> {
        let path = Self::normalize_path(path)?;
        let mut nodes = self.nodes.write().unwrap();
        let node = self.new_regular_node(executable);
        self.insert_new_child_locked(&mut nodes, path, node)
    }

    // AGENT: create a new directory inode atomically in this filesystem-local
    // namespace and preserve strict parent-directory semantics.
    pub fn create_directory(&self, path: &str) -> Result<Arc<FileNode>, &'static str> {
        let path = Self::normalize_path(path)?;
        let mut nodes = self.nodes.write().unwrap();
        let node = self.new_directory_node();
        self.insert_new_child_locked(&mut nodes, path, node)
    }

    // AGENT: combine existing-file checks and optional creation under one
    // filesystem-local table lock for transactional openat behavior.
    pub(crate) fn open_regular(
        &self,
        path: &str,
        create: bool,
        exclusive: bool,
    ) -> Result<Arc<FileNode>, &'static str> {
        let path = Self::normalize_path(path)?;
        let mut nodes = self.nodes.write().unwrap();
        if let Some(node) = nodes.get(&path).cloned() {
            if exclusive {
                return Err("eexist");
            }
            if node.kind != FileKind::Regular {
                return Err("eisdir");
            }
            return Ok(node);
        }
        if !create {
            return Err("enoent");
        }
        let node = self.new_regular_node(false);
        self.insert_new_child_locked(&mut nodes, path, node)
    }

    // AGENT: install or replace a regular inode, retaining a newly validated
    // parent across initial I/O so publication does not repeat the same check.
    pub(crate) fn install_regular(
        &self,
        path: &str,
        data: &[u8],
        executable: bool,
    ) -> Result<Arc<FileNode>, &'static str> {
        let path = Self::normalize_path(path)?;
        let mut nodes = self.nodes.write().unwrap();
        let new_parent = match nodes.get(&path) {
            Some(existing) if existing.kind != FileKind::Regular => return Err("eisdir"),
            Some(_) => None,
            None => Some(Self::require_parent_dir(&nodes, &path)?),
        };
        let node = self.new_regular_node(executable);
        node.write_initial_bytes(&self.storage, data)?;
        if let Some((parent, name)) = new_parent {
            parent.add_dir_entry(&self.storage, &name)?;
        }
        nodes.insert(path, node.clone());
        Ok(node)
    }

    // AGENT: establish a directory idempotently for kernel boot fixtures while
    // keeping user-requested mkdir on the strict create_directory interface.
    pub(crate) fn install_directory(&self, path: &str) -> Result<Arc<FileNode>, &'static str> {
        let path = Self::normalize_path(path)?;
        let mut nodes = self.nodes.write().unwrap();
        if let Some(existing) = nodes.get(&path).cloned() {
            return if existing.kind == FileKind::Directory {
                Ok(existing)
            } else {
                Err("eexist")
            };
        }
        let node = self.new_directory_node();
        self.insert_new_child_locked(&mut nodes, path, node)
    }
}
