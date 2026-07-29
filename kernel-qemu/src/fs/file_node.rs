// AGENT: keep live inode state in FileNode while delegating encoded metadata
// images to a private component and file bytes to the QEMU block backend.
use super::*;
use crate::kernel::allocator::AllocatorState;

mod metadata;

use metadata::FileMetadata;

// AGENT: keep standalone/test file handles within the 1 MiB QEMU early heap;
// full-chain writeback preserves correctness when a single chain recycles slots.
const STANDALONE_BLOCK_CACHE_CHAINS: usize = 1;

// AGENT: distinguish regular path files from directory nodes for exec checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
}

// AGENT: track root block-device ownership so truncated FileNode data can
// return space to later files instead of only bumping a high-water mark.
pub struct FileBlockAllocator {
    state: Mutex<AllocatorState>,
}

impl FileBlockAllocator {
    // AGENT: bind the sequential block allocator to its device capacity once.
    pub fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(AllocatorState::new(limit)),
        }
    }

    // AGENT: rebuild exact block ownership from a validated on-disk bitmap so
    // later allocations cannot reuse superblock, metadata, or file-data blocks.
    pub(crate) fn from_allocated(limit: usize, allocated: &[usize]) -> Result<Self, &'static str> {
        let mut state = AllocatorState::new(limit);
        for &block in allocated {
            if state.reserve(block).is_none() {
                return Err("eio");
            }
        }
        Ok(Self {
            state: Mutex::new(state),
        })
    }

    // AGENT: prefer cleared blocks returned by FileBlock RAII drops, falling
    // back to the next never-used block inside the fixed RAM-device capacity.
    fn allocate_id(&self) -> Result<usize, &'static str> {
        self.state.lock().unwrap().allocate_from(0).ok_or("enospc")
    }

    // AGENT: FileBlock::drop returns already-cleared blocks here; duplicate
    // releases would mean block ownership escaped the FileNode map.
    fn release_owned(&self, block: usize) {
        let mut state = self.state.lock().unwrap();
        let released = state.release(block);
        debug_assert!(
            released,
            "file block released twice or outside allocator range"
        );
    }

    // AGENT: require recovered FileBlock wrappers to correspond to bitmap-owned
    // ids before live inode state adopts them.
    fn owns(&self, block: usize) -> bool {
        self.state.lock().unwrap().is_allocated(block)
    }

    // AGENT: snapshot current ownership for a stable block-bitmap commit.
    pub(crate) fn allocated_ids(&self) -> Vec<usize> {
        self.state.lock().unwrap().allocated_ids()
    }

    // AGENT: expose allocator reuse to focused fd/qemu-sync regressions without
    // making the free-list shape part of the normal filesystem API.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    fn stats(&self) -> (usize, usize) {
        self.state.lock().unwrap().stats()
    }
}

// AGENT: keep the shared block backend in one private object so FileStorage is
// a cloneable handle while FileBlock only adds single-block ownership.
struct FileStorageInner {
    cache: Arc<BlockCache>,
    device: Arc<dyn BlockDevice>,
    allocator: Arc<FileBlockAllocator>,
    reclaim_on_drop: AtomicBool,
}

impl FileStorageInner {
    // AGENT: centralize construction of the backend shared by handles and
    // owned file blocks.
    fn new(
        cache: Arc<BlockCache>,
        device: Arc<dyn BlockDevice>,
        allocator: Arc<FileBlockAllocator>,
    ) -> Self {
        Self {
            cache,
            device,
            allocator,
            reclaim_on_drop: AtomicBool::new(true),
        }
    }
}

// AGENT: own one backend block and return it to FileBlockAllocator when the
// FileNode block map drops it after truncation or node teardown.
struct FileBlock {
    id: usize,
    storage: Arc<FileStorageInner>,
}

impl FileBlock {
    // AGENT: bind block ownership to the backend used for cache-coherent zeroing
    // before the allocator is allowed to hand this id to another file.
    fn new(id: usize, storage: Arc<FileStorageInner>) -> Self {
        Self { id, storage }
    }

    // AGENT: keep callers from copying raw ids while still allowing cache I/O
    // to address the concrete block owned by this RAII wrapper.
    fn id(&self) -> usize {
        self.id
    }

    // AGENT: clear through BlockCache so reused blocks cannot expose stale file
    // contents from either the RAM backend or a resident dirty cache slot.
    fn clear_for_release(&self) -> Result<(), &'static str> {
        let zero = [0u8; BLOCK_CACHE_BLOCK_SIZE];
        self.storage.cache.write_block_cached(
            self.storage.device.as_ref(),
            ROOT_BLOCK_DEVICE,
            self.id,
            &zero,
        )
    }
}

impl Drop for FileBlock {
    // AGENT: make block release RAII-based; if best-effort clearing fails during
    // an implicit drop, leak the block instead of reusing stale contents.
    fn drop(&mut self) {
        if self.storage.reclaim_on_drop.load(Ordering::Acquire) && self.clear_for_release().is_ok()
        {
            self.storage.allocator.release_owned(self.id);
        }
    }
}

impl fmt::Debug for FileBlock {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FileBlock").field("id", &self.id).finish()
    }
}

// AGENT: share the live block backend between all handles opened from the same
// Kernel while keeping tests able to build standalone in-memory devices.
#[derive(Clone)]
pub struct FileStorage {
    inner: Arc<FileStorageInner>,
}

impl FileStorage {
    // AGENT: keep FileStorage as a cheap handle over the shared backend instead
    // of duplicating the backend fields in every owned block.
    pub fn new(
        cache: Arc<BlockCache>,
        device: Arc<dyn BlockDevice>,
        allocator: Arc<FileBlockAllocator>,
    ) -> Self {
        Self {
            inner: Arc::new(FileStorageInner::new(cache, device, allocator)),
        }
    }

    pub fn standalone() -> Self {
        let device = Arc::new(RamBlockDevice::empty());
        let allocator = Arc::new(FileBlockAllocator::new(device.block_count()));
        Self::new(
            Arc::new(BlockCache::new(STANDALONE_BLOCK_CACHE_CHAINS)),
            device,
            allocator,
        )
    }

    // AGENT: compare cloneable storage handles by their shared backend identity
    // so VFS regressions can prove a mounted path did not fall back to root.
    pub(crate) fn shares_backend_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    // AGENT: allocate an owned backend block tied to this storage's cache,
    // device, and allocator so FileNode truncation can release it by dropping.
    fn allocate_block(&self) -> Result<FileBlock, &'static str> {
        let id = self.inner.allocator.allocate_id()?;
        Ok(FileBlock::new(id, self.inner.clone()))
    }

    // AGENT: adopt one block already reserved by a recovered bitmap without
    // allocating it a second time or creating a second storage backend.
    fn adopt_block(&self, id: usize) -> Result<FileBlock, &'static str> {
        if id >= self.inner.device.block_count() || !self.inner.allocator.owns(id) {
            return Err("eio");
        }
        Ok(FileBlock::new(id, self.inner.clone()))
    }

    pub(crate) fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
        self.inner
            .cache
            .read_block_cached(self.inner.device.as_ref(), ROOT_BLOCK_DEVICE, block)
    }

    // AGENT: route file-block writes through BlockCache's write-back path.
    pub(crate) fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        self.inner.cache.write_block_cached(
            self.inner.device.as_ref(),
            ROOT_BLOCK_DEVICE,
            block,
            data,
        )
    }

    // AGENT: write every dirty cache slot to the device before issuing the
    // backend flush that makes completed writes stable across guest restart.
    pub(crate) fn flush(&self) -> Result<usize, &'static str> {
        let flushed = self.inner.cache.flush_dirty(self.inner.device.as_ref())?;
        self.inner.device.flush()?;
        Ok(flushed)
    }

    // AGENT: expose device capacity to the persistent-layout validator without
    // leaking the concrete RAM or VirtIO transport.
    pub(crate) fn block_count(&self) -> usize {
        self.inner.device.block_count()
    }

    // AGENT: preserve persistent disk contents when the final live FsInstance
    // is torn down; unlink/truncate reclamation still occurs while it is active.
    pub(crate) fn disarm_reclamation(&self) {
        self.inner.reclaim_on_drop.store(false, Ordering::Release);
    }

    // AGENT: snapshot the one allocator shared by all nodes in this filesystem.
    pub(crate) fn allocated_block_ids(&self) -> Vec<usize> {
        self.inner.allocator.allocated_ids()
    }

    // AGENT: expose allocator reuse to fd regressions without leaking raw block
    // ownership outside FileNode in normal builds.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(crate) fn allocator_stats(&self) -> (usize, usize) {
        self.inner.allocator.stats()
    }
}

// AGENT: FileNodeBlocks owns backend block RAII wrappers without encoding
// whether those blocks hold user data or FileNode metadata.
#[derive(Debug)]
struct FileNodeBlocks {
    blocks: Vec<FileBlock>,
}

impl FileNodeBlocks {
    fn empty() -> Self {
        Self { blocks: Vec::new() }
    }

    fn len(&self) -> usize {
        self.blocks.len()
    }

    fn push(&mut self, block: FileBlock) {
        self.blocks.push(block);
    }

    fn truncate(&mut self, len: usize) {
        self.blocks.truncate(len);
    }

    fn split_off(&mut self, at: usize) -> Vec<FileBlock> {
        self.blocks.split_off(at)
    }

    fn ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.blocks.iter().map(FileBlock::id)
    }

    // AGENT: reconstruct RAII block wrappers only after mount-time validation
    // has reserved every referenced id in the shared allocator bitmap.
    fn from_ids(backend: &FileStorage, ids: &[usize]) -> Result<Self, &'static str> {
        let mut blocks = Vec::with_capacity(ids.len());
        for &id in ids {
            blocks.push(backend.adopt_block(id)?);
        }
        Ok(Self { blocks })
    }
}

// AGENT: share one overflow-checked byte-to-block conversion between regular
// file allocation and the private metadata block store.
fn blocks_for_len(len: usize) -> Result<usize, &'static str> {
    if len == 0 {
        return Ok(0);
    }
    len.checked_add(BLOCK_CACHE_BLOCK_SIZE - 1)
        .map(|rounded| rounded / BLOCK_CACHE_BLOCK_SIZE)
        .ok_or("efbig")
}

// AGENT: keep the visible regular-file EOF under the same lock as its data
// blocks so readers never observe a length before the backing blocks exist.
#[derive(Debug)]
struct FileNodeData {
    blocks: FileNodeBlocks,
    byte_len: usize,
}

// AGENT: bind one visible direct-child name to a filesystem-local inode
// without storing a rename-sensitive full pathname in directory metadata.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub inode: InodeId,
}

// AGENT: carry one fully decoded FNMD image through mount-time validation before
// any live FileNode adopts its data or metadata block ownership.
pub(crate) struct RecoveredNodeState {
    pub(crate) kind: FileKind,
    pub(crate) executable: bool,
    pub(crate) byte_len: usize,
    pub(crate) data_blocks: Vec<usize>,
    pub(crate) entries: Vec<DirEntry>,
}

// AGENT: preserve insertion order for readdir while indexing direct children
// by name for component lookup inside the owning FsInstance namespace lock.
#[derive(Debug)]
struct DirectoryData {
    entries: Vec<DirEntry>,
    by_name: BTreeMap<String, InodeId>,
}

impl DirectoryData {
    fn empty() -> Self {
        Self {
            entries: Vec::new(),
            by_name: BTreeMap::new(),
        }
    }

    // AGENT: rebuild both ordered and indexed directory views from one validated
    // disk image while rejecting duplicate child names.
    fn from_entries(entries: Vec<DirEntry>) -> Result<Self, &'static str> {
        let mut by_name = BTreeMap::new();
        for entry in entries.iter() {
            if by_name.insert(entry.name.clone(), entry.inode).is_some() {
                return Err("eio");
            }
        }
        Ok(Self { entries, by_name })
    }
}

impl FileNodeData {
    fn empty() -> Self {
        Self {
            blocks: FileNodeBlocks::empty(),
            byte_len: 0,
        }
    }
}

// AGENT: this inode-like object owns live file and directory state, serializes
// each capacity-preflight/mutation/metadata-commit sequence, and delegates
// encoded metadata blocks to FileMetadata and file bytes to BlockCache.
pub struct FileNode {
    id: InodeId,
    pub kind: FileKind,
    pub executable: AtomicBool,
    mutation: Mutex<()>,
    storage: Mutex<FileNodeData>,
    metadata: FileMetadata,
    directory: Mutex<DirectoryData>,
}

impl FileNode {
    // AGENT: construct a regular inode only for FsInstance's inode allocator;
    // callers obtain managed Arc<FileNode> values from that filesystem object.
    pub(super) fn regular(id: InodeId, executable: bool) -> Self {
        Self {
            id,
            kind: FileKind::Regular,
            executable: AtomicBool::new(executable),
            mutation: Mutex::new(()),
            storage: Mutex::new(FileNodeData::empty()),
            metadata: FileMetadata::empty(),
            directory: Mutex::new(DirectoryData::empty()),
        }
    }

    // AGENT: construct a directory inode only for FsInstance with an ordered
    // directory-entry list and a direct-child BTreeMap index.
    pub(super) fn directory(id: InodeId) -> Self {
        Self {
            id,
            kind: FileKind::Directory,
            executable: AtomicBool::new(false),
            mutation: Mutex::new(()),
            storage: Mutex::new(FileNodeData::empty()),
            metadata: FileMetadata::empty(),
            directory: Mutex::new(DirectoryData::empty()),
        }
    }

    // AGENT: decode one inode image through the private FNMD implementation while
    // leaving cross-inode ownership and reachability checks to ChaosFs::mount.
    pub(crate) fn decode_persisted(
        backend: &FileStorage,
        metadata_blocks: &[usize],
    ) -> Result<RecoveredNodeState, &'static str> {
        FileMetadata::decode_from_blocks(backend, metadata_blocks)
    }

    // AGENT: construct one live inode only after the mount loader has validated
    // every block reference, directory edge, and bitmap ownership relation.
    pub(crate) fn recover(
        id: InodeId,
        backend: &FileStorage,
        metadata_blocks: &[usize],
        state: RecoveredNodeState,
    ) -> Result<Arc<Self>, &'static str> {
        let storage = FileNodeData {
            blocks: FileNodeBlocks::from_ids(backend, &state.data_blocks)?,
            byte_len: state.byte_len,
        };
        let directory = DirectoryData::from_entries(state.entries)?;
        let metadata = FileMetadata::from_ids(backend, metadata_blocks)?;
        Ok(Arc::new(Self {
            id,
            kind: state.kind,
            executable: AtomicBool::new(state.executable),
            mutation: Mutex::new(()),
            storage: Mutex::new(storage),
            metadata,
            directory: Mutex::new(directory),
        }))
    }

    // AGENT: serialize a stable inode image and return its backend locators for
    // the filesystem-wide inode table committed by FsInstance::flush.
    pub(crate) fn sync_metadata(&self, backend: &FileStorage) -> Result<Vec<usize>, &'static str> {
        let _mutation = self.mutation.lock().unwrap();
        let data_blocks = self.storage.lock().unwrap().blocks.len();
        self.reserve_data_layout(backend, data_blocks)?;
        self.persist_state(backend)?;
        Ok(self.metadata.block_ids())
    }

    // AGENT: expose the stable runtime inode identity allocated by FsInstance
    // for mountpoint keys and object-identity diagnostics.
    pub fn id(&self) -> InodeId {
        self.id
    }

    // AGENT: snapshot the stat fields supported by the current ChaosFs inode
    // model while making every not-yet-persisted field's zero policy explicit.
    pub fn file_attr(&self, fs_id: FsId) -> Result<FileAttr, &'static str> {
        let (byte_len, block_count) = {
            let storage = self.storage.lock().unwrap();
            (storage.byte_len, storage.blocks.len())
        };
        let mode = match self.kind {
            FileKind::Regular => {
                let permissions = if self.executable.load(Ordering::Relaxed) {
                    0o755
                } else {
                    0o644
                };
                S_IFREG | permissions
            }
            FileKind::Directory => S_IFDIR | 0o755,
        };
        Ok(FileAttr {
            dev: u64::try_from(fs_id).map_err(|_| "eoverflow")?,
            ino: self.id,
            mode,
            // ChaosFs has no hard-link operation yet, so every reachable inode
            // owns exactly one namespace binding in the first-stage model.
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            size: u64::try_from(byte_len).map_err(|_| "eoverflow")?,
            block_size: u32::try_from(BLOCK_CACHE_BLOCK_SIZE).map_err(|_| "eoverflow")?,
            blocks: u64::try_from(block_count).map_err(|_| "eoverflow")?,
            atime: FileTime::default(),
            mtime: FileTime::default(),
            ctime: FileTime::default(),
        })
    }

    // AGENT: look up one direct child through a component already validated by
    // the owning FsInstance namespace boundary.
    pub(super) fn lookup_child_inode(&self, name: ChildName<'_>) -> Result<InodeId, &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        self.directory
            .lock()
            .unwrap()
            .by_name
            .get(name.as_str())
            .copied()
            .ok_or("enoent")
    }

    // AGENT: serialize metadata reservation, child publication, persistence,
    // and rollback while keeping ordered directory indexes consistent.
    pub(super) fn insert_child(
        &self,
        backend: &FileStorage,
        name: ChildName<'_>,
        inode: InodeId,
    ) -> Result<(), &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        let _mutation = self.mutation.lock().unwrap();
        self.prepare_child_insert(backend, name)?;
        {
            let mut directory = self.directory.lock().unwrap();
            if directory.by_name.contains_key(name.as_str()) {
                return Err("eexist");
            }
            directory.entries.push(DirEntry {
                name: name.as_str().to_string(),
                inode,
            });
            directory.by_name.insert(name.as_str().to_string(), inode);
        }
        if let Err(error) = self.persist_state(backend) {
            let mut directory = self.directory.lock().unwrap();
            directory.by_name.remove(name.as_str());
            if let Some(index) = directory
                .entries
                .iter()
                .position(|entry| entry.name == name.as_str() && entry.inode == inode)
            {
                directory.entries.remove(index);
            }
            drop(directory);
            let _ = self.persist_state(backend);
            return Err(error);
        }
        Ok(())
    }

    // AGENT: serialize retarget, persistence, and rollback for one existing
    // child while preserving its directory iteration position.
    pub(super) fn replace_child_inode(
        &self,
        backend: &FileStorage,
        name: ChildName<'_>,
        expected: InodeId,
        replacement: InodeId,
    ) -> Result<(), &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        let _mutation = self.mutation.lock().unwrap();
        {
            let mut directory = self.directory.lock().unwrap();
            if directory.by_name.get(name.as_str()).copied() != Some(expected) {
                return Err("eio");
            }
            let entry = directory
                .entries
                .iter_mut()
                .find(|entry| entry.name == name.as_str() && entry.inode == expected)
                .ok_or("eio")?;
            entry.inode = replacement;
            directory
                .by_name
                .insert(name.as_str().to_string(), replacement);
        }
        if let Err(error) = self.persist_state(backend) {
            let mut directory = self.directory.lock().unwrap();
            if let Some(entry) = directory
                .entries
                .iter_mut()
                .find(|entry| entry.name == name.as_str() && entry.inode == replacement)
            {
                entry.inode = expected;
            }
            directory
                .by_name
                .insert(name.as_str().to_string(), expected);
            drop(directory);
            let _ = self.persist_state(backend);
            return Err(error);
        }
        Ok(())
    }

    // AGENT: fetch one directory entry by offset for handle-based iteration.
    pub fn dir_entry_at(&self, idx: usize) -> Result<String, &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        self.directory
            .lock()
            .unwrap()
            .entries
            .get(idx)
            .map(|entry| entry.name.clone())
            .ok_or("enoent")
    }

    // AGENT: expose the byte-precise regular-file EOF owned by this FileNode.
    pub fn len(&self) -> usize {
        self.storage.lock().unwrap().byte_len
    }

    // AGENT: expose allocated byte capacity for focused storage regressions
    // without making block-rounded capacity the public file length.
    pub(crate) fn allocated_len(&self) -> usize {
        self.storage
            .lock()
            .unwrap()
            .blocks
            .len()
            .saturating_mul(BLOCK_CACHE_BLOCK_SIZE)
    }

    // AGENT: expose metadata block growth to focused qemu-sync regressions
    // without leaking backend block ids into normal filesystem code.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(crate) fn metadata_block_count(&self) -> usize {
        self.metadata.block_count()
    }

    fn zero_range_locked(
        storage: &FileNodeData,
        backend: &FileStorage,
        start: usize,
        end: usize,
    ) -> Result<(), &'static str> {
        if start >= end {
            return Ok(());
        }

        let zero = [0u8; BLOCK_CACHE_BLOCK_SIZE];
        let mut copied = start;
        while copied < end {
            let file_block = copied / BLOCK_CACHE_BLOCK_SIZE;
            let block_off = copied % BLOCK_CACHE_BLOCK_SIZE;
            let n = min(end - copied, BLOCK_CACHE_BLOCK_SIZE - block_off);
            let block = storage.blocks.blocks[file_block].id();
            if block_off == 0 && n == BLOCK_CACHE_BLOCK_SIZE {
                backend.write_block(block, &zero)?;
            } else {
                let mut block_data = backend.read_block(block)?;
                block_data[block_off..block_off + n].fill(0);
                backend.write_block(block, &block_data)?;
            }
            copied += n;
        }
        Ok(())
    }

    // AGENT: read bytes from the contiguous block map while clipping at the
    // FileNode-owned byte length rather than allocated block capacity.
    pub(crate) fn read_bytes(
        &self,
        backend: &FileStorage,
        off: usize,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        let storage = self.storage.lock().unwrap();
        let file_len = storage.byte_len;
        if off >= file_len || buf.is_empty() {
            return Ok(0);
        }
        let total = min(buf.len(), file_len - off);
        let mut copied = 0usize;
        while copied < total {
            let abs = off.checked_add(copied).ok_or("efbig")?;
            let file_block = abs / BLOCK_CACHE_BLOCK_SIZE;
            let block_off = abs % BLOCK_CACHE_BLOCK_SIZE;
            let n = min(total - copied, BLOCK_CACHE_BLOCK_SIZE - block_off);
            let block = storage.blocks.blocks[file_block].id();
            let block_data = backend.read_block(block)?;
            buf[copied..copied + n].copy_from_slice(&block_data[block_off..block_off + n]);
            copied += n;
        }
        Ok(copied)
    }

    // AGENT: copy the complete byte-precise visible file contents out of the
    // block backend.
    pub(crate) fn read_all(&self, backend: &FileStorage) -> Result<Vec<u8>, &'static str> {
        let len = self.len();
        let mut data = vec![0; len];
        self.read_bytes(backend, 0, &mut data)?;
        Ok(data)
    }

    // AGENT: serialize metadata preflight, block-cache writes, EOF publication,
    // and metadata commit while zeroing any newly visible hole.
    pub(crate) fn write_bytes(
        &self,
        backend: &FileStorage,
        offset: Option<usize>,
        buf: &[u8],
    ) -> Result<usize, &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        let _mutation = self.mutation.lock().unwrap();
        self.prepare_write(backend, offset, buf.len())?;
        let mut storage = self.storage.lock().unwrap();
        let start = offset.unwrap_or(storage.byte_len);
        if buf.is_empty() {
            return Ok(start);
        }
        let end = start.checked_add(buf.len()).ok_or("efbig")?;
        let needed_blocks = blocks_for_len(end)?;
        let old_len = storage.byte_len;
        let mut layout_changed = false;

        while storage.blocks.len() < needed_blocks {
            storage.blocks.push(backend.allocate_block()?);
            layout_changed = true;
        }
        if start > old_len {
            Self::zero_range_locked(&storage, backend, old_len, start)?;
        }

        let mut copied = 0usize;
        while copied < buf.len() {
            let abs = start.checked_add(copied).ok_or("efbig")?;
            let file_block = abs / BLOCK_CACHE_BLOCK_SIZE;
            let block_off = abs % BLOCK_CACHE_BLOCK_SIZE;
            let n = min(buf.len() - copied, BLOCK_CACHE_BLOCK_SIZE - block_off);
            let block = storage.blocks.blocks[file_block].id();
            let mut block_data = if block_off == 0 && n == BLOCK_CACHE_BLOCK_SIZE {
                vec![0; BLOCK_CACHE_BLOCK_SIZE]
            } else {
                backend.read_block(block)?
            };
            block_data[block_off..block_off + n].copy_from_slice(&buf[copied..copied + n]);
            backend.write_block(block, &block_data)?;
            copied += n;
        }
        if end > storage.byte_len {
            storage.byte_len = end;
            layout_changed = true;
        }
        drop(storage);

        if layout_changed {
            self.persist_state(backend)?;
        }
        Ok(end)
    }

    // AGENT: seed a new file into the backend as already-synced contents.
    pub(crate) fn write_initial_bytes(
        &self,
        backend: &FileStorage,
        data: &[u8],
    ) -> Result<(), &'static str> {
        self.write_bytes(backend, Some(0), data)?;
        backend.flush()?;
        Ok(())
    }

    // AGENT: serialize resize preflight through metadata commit so truncation
    // cannot invalidate another mutation's reserved metadata capacity.
    pub(crate) fn set_data_len(
        &self,
        backend: &FileStorage,
        len: usize,
    ) -> Result<(), &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        let _mutation = self.mutation.lock().unwrap();
        let keep_blocks = blocks_for_len(len)?;
        self.prepare_resize(backend, keep_blocks, len)?;
        let changed = {
            let mut storage = self.storage.lock().unwrap();
            let old_len = storage.byte_len;
            let mut layout_changed = false;
            if storage.blocks.len() == keep_blocks {
                // Keep existing allocation for byte-precise shrink/grow inside
                // the same final block.
            } else {
                if keep_blocks < storage.blocks.len() {
                    for block in storage.blocks.blocks[keep_blocks..].iter() {
                        block.clear_for_release()?;
                    }
                    storage.blocks.truncate(keep_blocks);
                } else {
                    while storage.blocks.len() < keep_blocks {
                        storage.blocks.push(backend.allocate_block()?);
                    }
                }
                layout_changed = true;
            }
            if len > old_len {
                Self::zero_range_locked(&storage, backend, old_len, len)?;
            }
            if storage.byte_len != len {
                storage.byte_len = len;
                layout_changed = true;
            }
            layout_changed
        };
        if changed {
            self.persist_state(backend)?;
        }
        Ok(())
    }

    // AGENT: serialize growth preflight through metadata commit, eagerly
    // zeroing newly visible bytes before publishing the longer byte length.
    pub(crate) fn ensure_data_len_at_least(
        &self,
        backend: &FileStorage,
        len: usize,
    ) -> Result<(), &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        let _mutation = self.mutation.lock().unwrap();
        let needed_blocks = blocks_for_len(len)?;
        if !self.prepare_growth(backend, needed_blocks, len)? {
            return Ok(());
        }
        {
            let mut storage = self.storage.lock().unwrap();
            if storage.byte_len >= len {
                return Ok(());
            }
            let old_len = storage.byte_len;
            while storage.blocks.len() < needed_blocks {
                storage.blocks.push(backend.allocate_block()?);
            }
            Self::zero_range_locked(&storage, backend, old_len, len)?;
            storage.byte_len = len;
        }
        self.persist_state(backend)?;
        Ok(())
    }
}

// AGENT: retain FileNode diagnostics while delegating metadata allocation
// details to the private FileMetadata component.
impl fmt::Debug for FileNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let (byte_len, block_count) = {
            let storage = self.storage.lock().unwrap();
            (storage.byte_len, storage.blocks.len())
        };
        let metadata_block_count = self.metadata.block_count();
        let entry_count = self.directory.lock().unwrap().entries.len();
        f.debug_struct("FileNode")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("executable", &self.executable.load(Ordering::Relaxed))
            .field("byte_len", &byte_len)
            .field("blocks", &block_count)
            .field("metadata_blocks", &metadata_block_count)
            .field("entries", &entry_count)
            .finish()
    }
}
