// AGENT: keep shared path-file metadata in FileNode while storing file bytes
// in the QEMU block backend instead of duplicating contents in the node.
use super::*;

const FILE_NODE_METADATA_MAGIC: &[u8; 4] = b"FNMD";

// AGENT: keep standalone/test file handles small; full-chain writeback now
// preserves correctness when a single chain has to recycle slots.
const STANDALONE_BLOCK_CACHE_CHAINS: usize = 10;

// AGENT: distinguish regular path files from directory nodes for exec checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
}

// AGENT: allocate root RamBlockDevice blocks for FileNode-backed regular files.
pub struct FileBlockAllocator {
    next: AtomicUsize,
}

impl FileBlockAllocator {
    pub fn new() -> Self {
        Self {
            next: AtomicUsize::new(0),
        }
    }

    fn allocate(&self, device: &RamBlockDevice) -> Result<usize, &'static str> {
        let block = self.next.fetch_add(1, Ordering::Relaxed);
        if block >= device.block_count() {
            return Err("enospc");
        }
        Ok(block)
    }
}

// AGENT: share the live block backend between all handles opened from the same
// Kernel while keeping tests able to build standalone in-memory devices.
#[derive(Clone)]
pub struct FileStorage {
    cache: Arc<BlockCache>,
    device: Arc<RamBlockDevice>,
    allocator: Arc<FileBlockAllocator>,
}

impl FileStorage {
    pub fn new(
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

    pub fn standalone() -> Self {
        Self::new(
            Arc::new(BlockCache::new(STANDALONE_BLOCK_CACHE_CHAINS)),
            Arc::new(RamBlockDevice::empty()),
            Arc::new(FileBlockAllocator::new()),
        )
    }

    fn allocate_block(&self) -> Result<usize, &'static str> {
        self.allocator.allocate(self.device.as_ref())
    }

    fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
        self.cache
            .read_block_cached(self.device.as_ref(), ROOT_BLOCK_DEVICE, block)
    }

    // AGENT: route file-block writes through BlockCache's write-back path.
    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        self.cache
            .write_block_cached(self.device.as_ref(), ROOT_BLOCK_DEVICE, block, data)
    }

    // AGENT: sync writes dirty cache slots back to the shared RAM block device.
    fn flush(&self) -> Result<usize, &'static str> {
        self.cache.flush_dirty(self.device.as_ref())
    }

    // AGENT: expose unified cache dirty state to focused fd regressions.
    pub fn dirty_count(&self) -> usize {
        self.cache.dirty_count()
    }
}

#[derive(Debug)]
struct FileNodeBlocks {
    len: usize,
    blocks: Vec<Option<usize>>,
}

impl FileNodeBlocks {
    fn empty() -> Self {
        Self {
            len: 0,
            blocks: Vec::new(),
        }
    }
}

// AGENT: FileNode owns metadata, directory entries, and the regular-file block
// map; actual bytes and unified dirty state live in the shared block cache.
pub struct FileNode {
    pub kind: FileKind,
    pub executable: AtomicBool,
    storage: Mutex<FileNodeBlocks>,
    metadata_block: Mutex<Option<usize>>,
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

    // AGENT: expose the visible byte length without exposing storage blocks.
    pub fn len(&self) -> usize {
        self.storage.lock().unwrap().len
    }

    fn ensure_block(
        storage: &mut FileNodeBlocks,
        backend: &FileStorage,
        file_block: usize,
    ) -> Result<(usize, bool), &'static str> {
        while storage.blocks.len() <= file_block {
            storage.blocks.push(None);
        }
        if let Some(block) = storage.blocks[file_block] {
            return Ok((block, false));
        }
        let block = backend.allocate_block()?;
        storage.blocks[file_block] = Some(block);
        Ok((block, true))
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
        Self::put_metadata_u64(&mut payload, &mut cursor, storage.len);
        Self::put_metadata_u64(&mut payload, &mut cursor, storage.blocks.len());
        Self::put_metadata_u64(&mut payload, &mut cursor, entries.len());

        for block in storage.blocks.iter() {
            let encoded = block.map(|nr| nr.saturating_add(1)).unwrap_or(0);
            Self::put_metadata_u64(&mut payload, &mut cursor, encoded);
        }
        for entry in entries.iter() {
            Self::put_metadata_u64(&mut payload, &mut cursor, entry.len());
            Self::put_metadata_bytes(&mut payload, &mut cursor, entry.as_bytes());
        }

        payload
    }

    fn ensure_metadata_block(&self, backend: &FileStorage) -> Result<usize, &'static str> {
        let mut metadata_block = self.metadata_block.lock().unwrap();
        if let Some(block) = *metadata_block {
            return Ok(block);
        }
        let block = backend.allocate_block()?;
        *metadata_block = Some(block);
        Ok(block)
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
        if end > storage.len {
            return Ok(true);
        }
        if len == 0 {
            return Ok(false);
        }
        let first = start / BLOCK_CACHE_BLOCK_SIZE;
        let last = (end - 1) / BLOCK_CACHE_BLOCK_SIZE;
        for file_block in first..=last {
            if !matches!(storage.blocks.get(file_block), Some(Some(_))) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    // AGENT: read bytes from the block backend through the node's block map,
    // treating sparse or unallocated regions as zero-filled file holes.
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
        if off >= storage.len || buf.is_empty() {
            return Ok(0);
        }
        let total = min(buf.len(), storage.len - off);
        let mut copied = 0usize;
        while copied < total {
            let abs = off.checked_add(copied).ok_or("efbig")?;
            let file_block = abs / BLOCK_CACHE_BLOCK_SIZE;
            let block_off = abs % BLOCK_CACHE_BLOCK_SIZE;
            let n = min(total - copied, BLOCK_CACHE_BLOCK_SIZE - block_off);
            if let Some(Some(block)) = storage.blocks.get(file_block) {
                let block_data = backend.read_block(*block)?;
                buf[copied..copied + n].copy_from_slice(&block_data[block_off..block_off + n]);
            } else {
                buf[copied..copied + n].fill(0);
            }
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
            let start = offset.unwrap_or(storage.len);
            if Self::write_may_change_metadata(&storage, start, buf.len())? {
                drop(storage);
                self.ensure_metadata_block(backend)?;
            }
        }
        let mut storage = self.storage.lock().unwrap();
        let start = offset.unwrap_or(storage.len);
        let end = start.checked_add(buf.len()).ok_or("efbig")?;
        let grew = end > storage.len;
        let mut metadata_changed = grew;

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
        if grew {
            storage.len = end;
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

    // AGENT: resize visible file length while keeping truncated stale blocks
    // unreachable from later reads.
    pub(crate) fn set_data_len(
        &self,
        backend: &FileStorage,
        len: usize,
    ) -> Result<(), &'static str> {
        if self.kind != FileKind::Regular {
            return Err("eisdir");
        }
        {
            let storage = self.storage.lock().unwrap();
            if storage.len != len {
                drop(storage);
                self.ensure_metadata_block(backend)?;
            }
        }
        let changed = {
            let mut storage = self.storage.lock().unwrap();
            if storage.len == len {
                false
            } else {
                if len < storage.len {
                    let keep_blocks = Self::blocks_for_len(len)?;
                    if keep_blocks > 0 {
                        let tail_off = len % BLOCK_CACHE_BLOCK_SIZE;
                        if tail_off != 0 {
                            if let Some(Some(block)) = storage.blocks.get(keep_blocks - 1) {
                                let mut block_data = backend.read_block(*block)?;
                                block_data[tail_off..].fill(0);
                                backend.write_block(*block, &block_data)?;
                            }
                        }
                    }
                    storage.blocks.truncate(keep_blocks);
                }
                storage.len = len;
                true
            }
        };
        if changed {
            self.mark_metadata_dirty(backend)?;
        }
        Ok(())
    }

    // AGENT: grow only the visible length; actual blocks are allocated lazily
    // when a later write stores non-hole bytes.
    pub(crate) fn ensure_data_len_at_least(
        &self,
        backend: &FileStorage,
        len: usize,
    ) -> Result<(), &'static str> {
        {
            let storage = self.storage.lock().unwrap();
            if storage.len < len {
                drop(storage);
                self.ensure_metadata_block(backend)?;
            }
        }
        let grew = {
            let mut storage = self.storage.lock().unwrap();
            if storage.len >= len {
                false
            } else {
                storage.len = len;
                true
            }
        };
        if grew {
            self.mark_metadata_dirty(backend)?;
        }
        Ok(())
    }

    // AGENT: data-only sync is intentionally equivalent to full sync in the
    // current QEMU file layer because dirty state is no longer split by kind.
    pub fn sync_data(&self, backend: &FileStorage) -> Result<(), &'static str> {
        backend.flush()?;
        Ok(())
    }

    // AGENT: full sync clears all cached dirty state.
    pub fn sync_all(&self, backend: &FileStorage) -> Result<(), &'static str> {
        backend.flush()?;
        Ok(())
    }
}

impl fmt::Debug for FileNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let storage = self.storage.lock().unwrap();
        f.debug_struct("FileNode")
            .field("kind", &self.kind)
            .field("executable", &self.executable.load(Ordering::Relaxed))
            .field("len", &storage.len)
            .field("blocks", &storage.blocks.len())
            .field("metadata_block", &*self.metadata_block.lock().unwrap())
            .field("entries", &self.dir_entries.lock().unwrap().len())
            .finish()
    }
}
