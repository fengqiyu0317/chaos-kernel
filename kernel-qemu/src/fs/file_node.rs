// AGENT: keep shared path-file metadata in FileNode while storing file bytes
// in the QEMU block backend instead of duplicating contents in the node.
use super::*;

// AGENT: distinguish regular path files from directory nodes for exec checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
}

// AGENT: track unsynced changes so sync_data() and sync_all() can keep their
// different content-vs-metadata semantics.
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
            Arc::new(BlockCache::new(N_CHAINS)),
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

    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        self.cache
            .write_block_cached(ROOT_BLOCK_DEVICE, block, data)
    }

    fn flush(&self) -> Result<usize, &'static str> {
        self.cache.flush_dirty(self.device.as_ref())
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

// AGENT: FileNode owns only metadata, dirty state, directory entries, and the
// regular-file block map; actual bytes live in the shared RamBlockDevice.
pub struct FileNode {
    pub kind: FileKind,
    pub executable: AtomicBool,
    storage: Mutex<FileNodeBlocks>,
    dirty: Mutex<FileDirty>,
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
            dirty: Mutex::new(FileDirty::clean()),
            dir_entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // AGENT: create a directory node with a real entry list for read_entry().
    pub fn directory() -> Self {
        Self {
            kind: FileKind::Directory,
            executable: AtomicBool::new(false),
            storage: Mutex::new(FileNodeBlocks::empty()),
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

    // AGENT: expose the visible byte length without exposing storage blocks.
    pub fn len(&self) -> usize {
        self.storage.lock().unwrap().len
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

    fn ensure_block(
        storage: &mut FileNodeBlocks,
        backend: &FileStorage,
        file_block: usize,
    ) -> Result<usize, &'static str> {
        while storage.blocks.len() <= file_block {
            storage.blocks.push(None);
        }
        if let Some(block) = storage.blocks[file_block] {
            return Ok(block);
        }
        let block = backend.allocate_block()?;
        storage.blocks[file_block] = Some(block);
        Ok(block)
    }

    fn blocks_for_len(len: usize) -> Result<usize, &'static str> {
        if len == 0 {
            return Ok(0);
        }
        len.checked_add(BLOCK_CACHE_BLOCK_SIZE - 1)
            .map(|rounded| rounded / BLOCK_CACHE_BLOCK_SIZE)
            .ok_or("efbig")
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
        let mut storage = self.storage.lock().unwrap();
        let start = offset.unwrap_or(storage.len);
        let end = start.checked_add(buf.len()).ok_or("efbig")?;
        let grew = end > storage.len;

        let mut copied = 0usize;
        while copied < buf.len() {
            let abs = start.checked_add(copied).ok_or("efbig")?;
            let file_block = abs / BLOCK_CACHE_BLOCK_SIZE;
            let block_off = abs % BLOCK_CACHE_BLOCK_SIZE;
            let n = min(buf.len() - copied, BLOCK_CACHE_BLOCK_SIZE - block_off);
            let block = Self::ensure_block(&mut storage, backend, file_block)?;
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

        if grew || !buf.is_empty() {
            self.note_write(grew);
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
        *self.dirty.lock().unwrap() = FileDirty::clean();
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
            self.note_resize();
        }
        Ok(())
    }

    // AGENT: grow only the visible length; actual blocks are allocated lazily
    // when a later write stores non-hole bytes.
    pub(crate) fn ensure_data_len_at_least(&self, len: usize) {
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
            self.note_resize();
        }
    }

    // AGENT: data-only sync flushes dirty cached blocks and leaves metadata dirty.
    pub fn sync_data(&self, backend: &FileStorage) -> Result<(), &'static str> {
        backend.flush()?;
        self.dirty.lock().unwrap().data = false;
        Ok(())
    }

    // AGENT: full sync flushes cached blocks and clears all node dirty bits.
    pub fn sync_all(&self, backend: &FileStorage) -> Result<(), &'static str> {
        backend.flush()?;
        *self.dirty.lock().unwrap() = FileDirty::clean();
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
            .field("dirty", &self.dirty_state())
            .field("entries", &self.dir_entries.lock().unwrap().len())
            .finish()
    }
}
