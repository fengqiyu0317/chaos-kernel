// AGENT: keep shared path-file metadata in FileNode while storing file bytes
// in the QEMU block backend instead of duplicating contents in the node.
use super::*;

const FILE_NODE_METADATA_MAGIC: &[u8; 4] = b"FNMD";

// AGENT: keep standalone/test file handles within the 1 MiB QEMU early heap;
// full-chain writeback preserves correctness when a single chain recycles slots.
const STANDALONE_BLOCK_CACHE_CHAINS: usize = 1;

// AGENT: distinguish regular path files from directory nodes for exec checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
}

// AGENT: track root RamBlockDevice block ownership so truncated FileNode data
// can return space to later files instead of only bumping a high-water mark.
pub struct FileBlockAllocator {
    state: Mutex<FileBlockAllocatorState>,
}

// AGENT: keep allocator state together because free-list reuse and next-block
// allocation must be updated atomically with respect to other file operations.
struct FileBlockAllocatorState {
    next: usize,
    free: BTreeSet<usize>,
}

impl FileBlockAllocator {
    // AGENT: start with an empty free list and allocate new blocks sequentially.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(FileBlockAllocatorState {
                next: 0,
                free: BTreeSet::new(),
            }),
        }
    }

    // AGENT: prefer cleared blocks returned by FileBlock RAII drops, falling
    // back to the next never-used block inside the fixed RAM-device capacity.
    fn allocate_id(&self, device: &RamBlockDevice) -> Result<usize, &'static str> {
        let mut state = self.state.lock().unwrap();
        if let Some(block) = state.free.iter().next().copied() {
            state.free.remove(&block);
            return Ok(block);
        }

        if state.next >= device.block_count() {
            return Err("enospc");
        }
        let block = state.next;
        state.next += 1;
        Ok(block)
    }

    // AGENT: FileBlock::drop returns already-cleared blocks here; duplicate
    // releases would mean block ownership escaped the FileNode map.
    fn release_owned(&self, block: usize) {
        let mut state = self.state.lock().unwrap();
        if block < state.next {
            let inserted = state.free.insert(block);
            debug_assert!(inserted, "file block released twice");
        } else {
            debug_assert!(false, "file block released outside allocator range");
        }
    }

    // AGENT: expose allocator reuse to focused fd/qemu-sync regressions without
    // making the free-list shape part of the normal filesystem API.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    fn stats(&self) -> (usize, usize) {
        let state = self.state.lock().unwrap();
        (state.next, state.free.len())
    }
}

// AGENT: keep the shared block backend in one private object so FileStorage is
// a cloneable handle while FileBlock only adds single-block ownership.
struct FileStorageInner {
    cache: Arc<BlockCache>,
    device: Arc<RamBlockDevice>,
    allocator: Arc<FileBlockAllocator>,
}

impl FileStorageInner {
    // AGENT: centralize construction of the backend shared by handles and
    // owned file blocks.
    fn new(
        cache: Arc<BlockCache>,
        device: Arc<RamBlockDevice>,
        allocator: Arc<FileBlockAllocator>,
    ) -> Self {
        Self {
            cache,
            device,
            allocator,
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
        if self.clear_for_release().is_ok() {
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
        device: Arc<RamBlockDevice>,
        allocator: Arc<FileBlockAllocator>,
    ) -> Self {
        Self {
            inner: Arc::new(FileStorageInner::new(cache, device, allocator)),
        }
    }

    pub fn standalone() -> Self {
        Self::new(
            Arc::new(BlockCache::new(STANDALONE_BLOCK_CACHE_CHAINS)),
            Arc::new(RamBlockDevice::empty()),
            Arc::new(FileBlockAllocator::new()),
        )
    }

    // AGENT: allocate an owned backend block tied to this storage's cache,
    // device, and allocator so FileNode truncation can release it by dropping.
    fn allocate_block(&self) -> Result<FileBlock, &'static str> {
        let id = self
            .inner
            .allocator
            .allocate_id(self.inner.device.as_ref())?;
        Ok(FileBlock::new(id, self.inner.clone()))
    }

    fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
        self.inner
            .cache
            .read_block_cached(self.inner.device.as_ref(), ROOT_BLOCK_DEVICE, block)
    }

    // AGENT: route file-block writes through BlockCache's write-back path.
    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        self.inner.cache.write_block_cached(
            self.inner.device.as_ref(),
            ROOT_BLOCK_DEVICE,
            block,
            data,
        )
    }

    // AGENT: sync writes dirty cache slots back to the shared RAM block device.
    fn flush(&self) -> Result<usize, &'static str> {
        self.inner.cache.flush_dirty(self.inner.device.as_ref())
    }

    // AGENT: expose allocator reuse to fd regressions without leaking raw block
    // ownership outside FileNode in normal builds.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(crate) fn allocator_stats(&self) -> (usize, usize) {
        self.inner.allocator.stats()
    }
}

// AGENT: FileNodeBlocks owns the data-block RAII wrappers for one regular file.
#[derive(Debug)]
struct FileNodeBlocks {
    blocks: Vec<FileBlock>,
}

impl FileNodeBlocks {
    fn empty() -> Self {
        Self { blocks: Vec::new() }
    }
}

// AGENT: FileNode owns metadata, directory entries, and the regular-file block
// map; actual bytes and unified dirty state live in the shared block cache.
pub struct FileNode {
    pub kind: FileKind,
    pub executable: AtomicBool,
    storage: Mutex<FileNodeBlocks>,
    metadata_block: Mutex<Option<FileBlock>>,
    dir_entries: Arc<Mutex<Vec<String>>>,
}

impl FileNode {
    // AGENT: create a regular file node whose contents will be read from the
    // caller-provided FileStorage.
    pub fn regular(executable: bool) -> Self {
        Self {
            kind: FileKind::Regular,
            executable: AtomicBool::new(executable),
            storage: Mutex::new(FileNodeBlocks::empty()),
            metadata_block: Mutex::new(None),
            dir_entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // AGENT: create a directory node with a real entry list for read_entry().
    pub fn directory() -> Self {
        Self {
            kind: FileKind::Directory,
            executable: AtomicBool::new(false),
            storage: Mutex::new(FileNodeBlocks::empty()),
            metadata_block: Mutex::new(None),
            dir_entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // AGENT: add one child name to a directory node without duplicating entries.
    pub fn add_dir_entry(&self, backend: &FileStorage, name: &str) -> Result<(), &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        {
            let entries = self.dir_entries.lock().unwrap();
            if entries.iter().any(|entry| entry == name) {
                return Ok(());
            }
        }
        self.ensure_metadata_block(backend)?;
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
            self.mark_metadata_dirty(backend)?;
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

    // AGENT: expose block-backed byte capacity without storing a separate file
    // length now that regular files use contiguous block allocation.
    pub fn len(&self) -> usize {
        self.storage
            .lock()
            .unwrap()
            .blocks
            .len()
            .saturating_mul(BLOCK_CACHE_BLOCK_SIZE)
    }

    // AGENT: allocate all missing logical file blocks up to file_block so the
    // node no longer needs sparse holes or a separate visible-length field.
    fn ensure_block(
        storage: &mut FileNodeBlocks,
        backend: &FileStorage,
        file_block: usize,
    ) -> Result<(usize, bool), &'static str> {
        let mut allocated = false;
        while storage.blocks.len() <= file_block {
            storage.blocks.push(backend.allocate_block()?);
            allocated = true;
        }
        Ok((storage.blocks[file_block].id(), allocated))
    }

    fn blocks_for_len(len: usize) -> Result<usize, &'static str> {
        if len == 0 {
            return Ok(0);
        }
        len.checked_add(BLOCK_CACHE_BLOCK_SIZE - 1)
            .map(|rounded| rounded / BLOCK_CACHE_BLOCK_SIZE)
            .ok_or("efbig")
    }

    fn put_metadata_bytes(payload: &mut [u8], cursor: &mut usize, bytes: &[u8]) {
        if *cursor < payload.len() {
            let n = min(bytes.len(), payload.len() - *cursor);
            payload[*cursor..*cursor + n].copy_from_slice(&bytes[..n]);
        }
        *cursor = (*cursor).saturating_add(bytes.len());
    }

    fn put_metadata_u64(payload: &mut [u8], cursor: &mut usize, value: usize) {
        let value = u64::try_from(value).unwrap_or(u64::MAX);
        Self::put_metadata_bytes(payload, cursor, &value.to_le_bytes());
    }

    fn metadata_payload(&self) -> Vec<u8> {
        let storage = self.storage.lock().unwrap();
        let entries = self.dir_entries.lock().unwrap();
        let mut payload = vec![0; BLOCK_CACHE_BLOCK_SIZE];
        let mut cursor = 0usize;

        Self::put_metadata_bytes(&mut payload, &mut cursor, FILE_NODE_METADATA_MAGIC);
        Self::put_metadata_bytes(
            &mut payload,
            &mut cursor,
            &[match self.kind {
                FileKind::Regular => 1,
                FileKind::Directory => 2,
            }],
        );
        Self::put_metadata_bytes(
            &mut payload,
            &mut cursor,
            &[self.executable.load(Ordering::Relaxed) as u8],
        );
        Self::put_metadata_u64(&mut payload, &mut cursor, storage.blocks.len());
        Self::put_metadata_u64(&mut payload, &mut cursor, entries.len());

        for block in storage.blocks.iter() {
            Self::put_metadata_u64(&mut payload, &mut cursor, block.id().saturating_add(1));
        }
        for entry in entries.iter() {
            Self::put_metadata_u64(&mut payload, &mut cursor, entry.len());
            Self::put_metadata_bytes(&mut payload, &mut cursor, entry.as_bytes());
        }

        payload
    }

    fn ensure_metadata_block(&self, backend: &FileStorage) -> Result<usize, &'static str> {
        let mut metadata_block = self.metadata_block.lock().unwrap();
        if let Some(block) = metadata_block.as_ref() {
            return Ok(block.id());
        }
        let block = backend.allocate_block()?;
        let id = block.id();
        *metadata_block = Some(block);
        Ok(id)
    }

    // AGENT: encode FileNode-owned metadata through BlockCache so metadata
    // changes use the same dirty state as regular file-block writes.
    fn mark_metadata_dirty(&self, backend: &FileStorage) -> Result<(), &'static str> {
        let block = self.ensure_metadata_block(backend)?;
        let payload = self.metadata_payload();
        backend.write_block(block, &payload)
    }

    fn write_may_change_metadata(
        storage: &FileNodeBlocks,
        start: usize,
        len: usize,
    ) -> Result<bool, &'static str> {
        let end = start.checked_add(len).ok_or("efbig")?;
        if len == 0 {
            return Ok(false);
        }
        Ok(Self::blocks_for_len(end)? > storage.blocks.len())
    }

    // AGENT: read bytes from the contiguous block map; EOF is derived from the
    // number of allocated file blocks rather than a stored byte length.
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
        let file_len = storage.blocks.len().saturating_mul(BLOCK_CACHE_BLOCK_SIZE);
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
            let block = storage.blocks[file_block].id();
            let block_data = backend.read_block(block)?;
            buf[copied..copied + n].copy_from_slice(&block_data[block_off..block_off + n]);
            copied += n;
        }
        Ok(copied)
    }

    // AGENT: copy the complete visible file contents out of the block backend.
    pub(crate) fn read_all(&self, backend: &FileStorage) -> Result<Vec<u8>, &'static str> {
        let len = self.len();
        let mut data = vec![0; len];
        self.read_bytes(backend, 0, &mut data)?;
        Ok(data)
    }

    // AGENT: write a byte range through the block cache and update only file
    // metadata in FileNode.
    pub(crate) fn write_bytes(
        &self,
        backend: &FileStorage,
        offset: Option<usize>,
        buf: &[u8],
    ) -> Result<usize, &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        {
            let storage = self.storage.lock().unwrap();
            let start = offset.unwrap_or(storage.blocks.len() * BLOCK_CACHE_BLOCK_SIZE);
            if Self::write_may_change_metadata(&storage, start, buf.len())? {
                drop(storage);
                self.ensure_metadata_block(backend)?;
            }
        }
        let mut storage = self.storage.lock().unwrap();
        let start = offset.unwrap_or(storage.blocks.len() * BLOCK_CACHE_BLOCK_SIZE);
        let end = start.checked_add(buf.len()).ok_or("efbig")?;
        let mut metadata_changed = false;

        let mut copied = 0usize;
        while copied < buf.len() {
            let abs = start.checked_add(copied).ok_or("efbig")?;
            let file_block = abs / BLOCK_CACHE_BLOCK_SIZE;
            let block_off = abs % BLOCK_CACHE_BLOCK_SIZE;
            let n = min(buf.len() - copied, BLOCK_CACHE_BLOCK_SIZE - block_off);
            let (block, allocated) = Self::ensure_block(&mut storage, backend, file_block)?;
            metadata_changed |= allocated;
            let mut block_data = if block_off == 0 && n == BLOCK_CACHE_BLOCK_SIZE {
                vec![0; BLOCK_CACHE_BLOCK_SIZE]
            } else {
                backend.read_block(block)?
            };
            block_data[block_off..block_off + n].copy_from_slice(&buf[copied..copied + n]);
            backend.write_block(block, &block_data)?;
            copied += n;
        }
        drop(storage);

        if metadata_changed {
            self.mark_metadata_dirty(backend)?;
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

    // AGENT: resize the contiguous block map; byte lengths round up to whole
    // blocks because FileNode no longer stores a separate visible byte length.
    pub(crate) fn set_data_len(
        &self,
        backend: &FileStorage,
        len: usize,
    ) -> Result<(), &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        let keep_blocks = Self::blocks_for_len(len)?;
        {
            let storage = self.storage.lock().unwrap();
            if storage.blocks.len() != keep_blocks {
                drop(storage);
                self.ensure_metadata_block(backend)?;
            }
        }
        let changed = {
            let mut storage = self.storage.lock().unwrap();
            if storage.blocks.len() == keep_blocks {
                false
            } else {
                if keep_blocks < storage.blocks.len() {
                    for block in storage.blocks[keep_blocks..].iter() {
                        block.clear_for_release()?;
                    }
                    storage.blocks.truncate(keep_blocks);
                } else {
                    while storage.blocks.len() < keep_blocks {
                        storage.blocks.push(backend.allocate_block()?);
                    }
                }
                true
            }
        };
        if changed {
            self.mark_metadata_dirty(backend)?;
        }
        Ok(())
    }

    // AGENT: grow the contiguous block map eagerly, replacing the old sparse
    // visible-length-only behavior.
    pub(crate) fn ensure_data_len_at_least(
        &self,
        backend: &FileStorage,
        len: usize,
    ) -> Result<(), &'static str> {
        let needed_blocks = Self::blocks_for_len(len)?;
        {
            let storage = self.storage.lock().unwrap();
            if storage.blocks.len() < needed_blocks {
                drop(storage);
                self.ensure_metadata_block(backend)?;
            }
        }
        let grew = {
            let mut storage = self.storage.lock().unwrap();
            if storage.blocks.len() >= needed_blocks {
                false
            } else {
                while storage.blocks.len() < needed_blocks {
                    storage.blocks.push(backend.allocate_block()?);
                }
                true
            }
        };
        if grew {
            self.mark_metadata_dirty(backend)?;
        }
        Ok(())
    }
}

impl fmt::Debug for FileNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let storage = self.storage.lock().unwrap();
        f.debug_struct("FileNode")
            .field("kind", &self.kind)
            .field("executable", &self.executable.load(Ordering::Relaxed))
            .field("blocks", &storage.blocks.len())
            .field("metadata_block", &*self.metadata_block.lock().unwrap())
            .field("entries", &self.dir_entries.lock().unwrap().len())
            .finish()
    }
}
