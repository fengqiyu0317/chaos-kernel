// AGENT: keep shared path-file metadata in FileNode while storing file bytes
// in the QEMU block backend instead of duplicating contents in the node.
use super::*;
use crate::kernel::allocator::AllocatorState;

const FILE_NODE_METADATA_MAGIC: &[u8; 4] = b"FNMD";
const FILE_NODE_METADATA_HEADER_LEN: usize = 4 + 1 + 1 + 8 + 8 + 8;

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

    // AGENT: allocate an owned backend block tied to this storage's cache,
    // device, and allocator so FileNode truncation can release it by dropping.
    fn allocate_block(&self) -> Result<FileBlock, &'static str> {
        let id = self.inner.allocator.allocate_id()?;
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

    // AGENT: write every dirty cache slot to the device before issuing the
    // backend flush that makes completed writes stable across guest restart.
    fn flush(&self) -> Result<usize, &'static str> {
        let flushed = self.inner.cache.flush_dirty(self.inner.device.as_ref())?;
        self.inner.device.flush()?;
        Ok(flushed)
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
}

// AGENT: keep the visible regular-file EOF under the same lock as its data
// blocks so readers never observe a length before the backing blocks exist.
#[derive(Debug)]
struct FileNodeData {
    blocks: FileNodeBlocks,
    byte_len: usize,
}

impl FileNodeData {
    fn empty() -> Self {
        Self {
            blocks: FileNodeBlocks::empty(),
            byte_len: 0,
        }
    }
}

// AGENT: this inode-like shared file object owns file type, metadata, directory
// entries, byte length, and the regular-file block map; per-open offset and
// status flags remain in the fd/OFD layer, while actual bytes and unified dirty
// state live in the shared block cache.
pub struct FileNode {
    pub kind: FileKind,
    pub executable: AtomicBool,
    storage: Mutex<FileNodeData>,
    metadata_blocks: Mutex<FileNodeBlocks>,
    dir_entries: Arc<Mutex<Vec<String>>>,
}

impl FileNode {
    // AGENT: create a regular file node whose contents will be read from the
    // caller-provided FileStorage.
    pub fn regular(executable: bool) -> Self {
        Self {
            kind: FileKind::Regular,
            executable: AtomicBool::new(executable),
            storage: Mutex::new(FileNodeData::empty()),
            metadata_blocks: Mutex::new(FileNodeBlocks::empty()),
            dir_entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // AGENT: create a directory node with a real entry list for read_entry().
    pub fn directory() -> Self {
        Self {
            kind: FileKind::Directory,
            executable: AtomicBool::new(false),
            storage: Mutex::new(FileNodeData::empty()),
            metadata_blocks: Mutex::new(FileNodeBlocks::empty()),
            dir_entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // AGENT: add one child name to a directory node without duplicating entries.
    pub fn add_dir_entry(&self, backend: &FileStorage, name: &str) -> Result<(), &'static str> {
        if self.kind != FileKind::Directory {
            return Err("enotdir");
        }
        {
            let data_blocks = self.storage.lock().unwrap().blocks.len();
            let entries = self.dir_entries.lock().unwrap();
            if entries.iter().any(|entry| entry == name) {
                return Ok(());
            }
            let entry_name_bytes = Self::entry_name_bytes(&entries)?
                .checked_add(name.len())
                .ok_or("efbig")?;
            let entry_count = entries.len().checked_add(1).ok_or("efbig")?;
            let payload_len =
                Self::metadata_payload_len(data_blocks, entry_count, entry_name_bytes)?;
            drop(entries);
            self.ensure_metadata_capacity(backend, payload_len)?;
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
        self.metadata_blocks.lock().unwrap().len()
    }

    fn blocks_for_len(len: usize) -> Result<usize, &'static str> {
        if len == 0 {
            return Ok(0);
        }
        len.checked_add(BLOCK_CACHE_BLOCK_SIZE - 1)
            .map(|rounded| rounded / BLOCK_CACHE_BLOCK_SIZE)
            .ok_or("efbig")
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

    fn checked_metadata_add(lhs: usize, rhs: usize) -> Result<usize, &'static str> {
        lhs.checked_add(rhs).ok_or("efbig")
    }

    fn entry_name_bytes(entries: &[String]) -> Result<usize, &'static str> {
        let mut total = 0usize;
        for entry in entries.iter() {
            total = Self::checked_metadata_add(total, entry.len())?;
        }
        Ok(total)
    }

    fn metadata_payload_len(
        data_blocks: usize,
        entry_count: usize,
        entry_name_bytes: usize,
    ) -> Result<usize, &'static str> {
        let data_block_bytes = data_blocks.checked_mul(8).ok_or("efbig")?;
        let entry_len_bytes = entry_count.checked_mul(8).ok_or("efbig")?;
        let len = Self::checked_metadata_add(FILE_NODE_METADATA_HEADER_LEN, data_block_bytes)?;
        let len = Self::checked_metadata_add(len, entry_len_bytes)?;
        Self::checked_metadata_add(len, entry_name_bytes)
    }

    fn put_metadata_bytes(payload: &mut Vec<u8>, bytes: &[u8]) {
        payload.extend_from_slice(bytes);
    }

    fn put_metadata_u64(payload: &mut Vec<u8>, value: usize) -> Result<(), &'static str> {
        let value = u64::try_from(value).map_err(|_| "efbig")?;
        Self::put_metadata_bytes(payload, &value.to_le_bytes());
        Ok(())
    }

    fn metadata_payload(&self) -> Result<Vec<u8>, &'static str> {
        let storage = self.storage.lock().unwrap();
        let entries = self.dir_entries.lock().unwrap();
        let entry_name_bytes = Self::entry_name_bytes(&entries)?;
        let payload_len =
            Self::metadata_payload_len(storage.blocks.len(), entries.len(), entry_name_bytes)?;
        let mut payload = Vec::with_capacity(payload_len);

        Self::put_metadata_bytes(&mut payload, FILE_NODE_METADATA_MAGIC);
        Self::put_metadata_bytes(
            &mut payload,
            &[match self.kind {
                FileKind::Regular => 1,
                FileKind::Directory => 2,
            }],
        );
        Self::put_metadata_bytes(
            &mut payload,
            &[self.executable.load(Ordering::Relaxed) as u8],
        );
        Self::put_metadata_u64(&mut payload, storage.byte_len)?;
        Self::put_metadata_u64(&mut payload, storage.blocks.len())?;
        Self::put_metadata_u64(&mut payload, entries.len())?;

        for block in storage.blocks.ids() {
            let stored_id = block.checked_add(1).ok_or("efbig")?;
            Self::put_metadata_u64(&mut payload, stored_id)?;
        }
        for entry in entries.iter() {
            Self::put_metadata_u64(&mut payload, entry.len())?;
            Self::put_metadata_bytes(&mut payload, entry.as_bytes());
        }
        debug_assert_eq!(payload.len(), payload_len);

        Ok(payload)
    }

    fn ensure_metadata_capacity(
        &self,
        backend: &FileStorage,
        payload_len: usize,
    ) -> Result<usize, &'static str> {
        let needed_blocks = Self::blocks_for_len(payload_len.max(1))?;
        let mut metadata_blocks = self.metadata_blocks.lock().unwrap();
        if metadata_blocks.len() >= needed_blocks {
            return Ok(needed_blocks);
        }
        let mut allocated = Vec::new();
        while metadata_blocks.len() + allocated.len() < needed_blocks {
            allocated.push(backend.allocate_block()?);
        }
        metadata_blocks.blocks.append(&mut allocated);
        Ok(needed_blocks)
    }

    fn ensure_metadata_capacity_for_data_blocks(
        &self,
        backend: &FileStorage,
        data_blocks: usize,
    ) -> Result<(), &'static str> {
        let entries = self.dir_entries.lock().unwrap();
        let entry_name_bytes = Self::entry_name_bytes(&entries)?;
        let payload_len = Self::metadata_payload_len(data_blocks, entries.len(), entry_name_bytes)?;
        drop(entries);
        self.ensure_metadata_capacity(backend, payload_len)?;
        Ok(())
    }

    // AGENT: encode FileNode-owned metadata through BlockCache so metadata
    // changes use the same dirty state as regular file-block writes, spanning
    // as many backend blocks as the serialized metadata needs.
    fn mark_metadata_dirty(&self, backend: &FileStorage) -> Result<(), &'static str> {
        let payload = self.metadata_payload()?;
        let needed_blocks = self.ensure_metadata_capacity(backend, payload.len())?;
        let extra_blocks = {
            let mut metadata_blocks = self.metadata_blocks.lock().unwrap();
            for idx in 0..needed_blocks {
                let start = idx * BLOCK_CACHE_BLOCK_SIZE;
                let end = min(start + BLOCK_CACHE_BLOCK_SIZE, payload.len());
                let mut block_payload = [0u8; BLOCK_CACHE_BLOCK_SIZE];
                if start < end {
                    block_payload[..end - start].copy_from_slice(&payload[start..end]);
                }
                backend.write_block(metadata_blocks.blocks[idx].id(), &block_payload)?;
            }
            if metadata_blocks.len() > needed_blocks {
                metadata_blocks.split_off(needed_blocks)
            } else {
                Vec::new()
            }
        };
        drop(extra_blocks);
        Ok(())
    }

    fn write_may_change_metadata(
        storage: &FileNodeData,
        start: usize,
        len: usize,
    ) -> Result<bool, &'static str> {
        let end = start.checked_add(len).ok_or("efbig")?;
        if len == 0 {
            return Ok(false);
        }
        Ok(Self::blocks_for_len(end)? > storage.blocks.len() || end > storage.byte_len)
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

    // AGENT: write a byte range through the block cache, zero any newly visible
    // hole, and update the FileNode-owned byte length under the storage lock.
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
            let start = offset.unwrap_or(storage.byte_len);
            if Self::write_may_change_metadata(&storage, start, buf.len())? {
                let end = start.checked_add(buf.len()).ok_or("efbig")?;
                let needed_blocks = Self::blocks_for_len(end)?;
                drop(storage);
                self.ensure_metadata_capacity_for_data_blocks(backend, needed_blocks)?;
            }
        }
        let mut storage = self.storage.lock().unwrap();
        let start = offset.unwrap_or(storage.byte_len);
        if buf.is_empty() {
            return Ok(start);
        }
        let end = start.checked_add(buf.len()).ok_or("efbig")?;
        let needed_blocks = Self::blocks_for_len(end)?;
        let old_len = storage.byte_len;
        let mut metadata_changed = false;

        while storage.blocks.len() < needed_blocks {
            storage.blocks.push(backend.allocate_block()?);
            metadata_changed = true;
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
            metadata_changed = true;
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

    // AGENT: resize visible EOF and the contiguous block map together so
    // truncation releases blocks while in-block shrink/grow keeps byte precision.
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
            if storage.blocks.len() != keep_blocks || storage.byte_len != len {
                drop(storage);
                self.ensure_metadata_capacity_for_data_blocks(backend, keep_blocks)?;
            }
        }
        let changed = {
            let mut storage = self.storage.lock().unwrap();
            let old_len = storage.byte_len;
            let mut metadata_changed = false;
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
                metadata_changed = true;
            }
            if len > old_len {
                Self::zero_range_locked(&storage, backend, old_len, len)?;
            }
            if storage.byte_len != len {
                storage.byte_len = len;
                metadata_changed = true;
            }
            metadata_changed
        };
        if changed {
            self.mark_metadata_dirty(backend)?;
        }
        Ok(())
    }

    // AGENT: grow visible EOF and the contiguous block map eagerly, zeroing newly
    // visible bytes before publishing the longer byte length.
    pub(crate) fn ensure_data_len_at_least(
        &self,
        backend: &FileStorage,
        len: usize,
    ) -> Result<(), &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        let needed_blocks = Self::blocks_for_len(len)?;
        {
            let storage = self.storage.lock().unwrap();
            if storage.byte_len >= len {
                return Ok(());
            }
            if storage.blocks.len() < needed_blocks || storage.byte_len < len {
                drop(storage);
                self.ensure_metadata_capacity_for_data_blocks(backend, needed_blocks)?;
            }
        }
        let grew = {
            let mut storage = self.storage.lock().unwrap();
            if storage.byte_len >= len {
                return Ok(());
            }
            let old_len = storage.byte_len;
            let mut metadata_changed = false;
            while storage.blocks.len() < needed_blocks {
                storage.blocks.push(backend.allocate_block()?);
                metadata_changed = true;
            }
            Self::zero_range_locked(&storage, backend, old_len, len)?;
            storage.byte_len = len;
            metadata_changed = true;
            metadata_changed
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
            .field("byte_len", &storage.byte_len)
            .field("blocks", &storage.blocks.len())
            .field(
                "metadata_blocks",
                &self.metadata_blocks.lock().unwrap().len(),
            )
            .field("entries", &self.dir_entries.lock().unwrap().len())
            .finish()
    }
}
