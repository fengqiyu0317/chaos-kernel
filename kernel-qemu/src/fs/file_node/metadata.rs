// AGENT: isolate the FileNode metadata image format and its backend block
// ownership from live inode, directory, and regular-file mutation semantics.
use super::super::le_codec::LeReader;
use super::*;

const FILE_NODE_METADATA_MAGIC: &[u8; 4] = b"FNMD";
const FILE_NODE_METADATA_VERSION: u8 = 2;
const FILE_NODE_METADATA_HEADER_LEN: usize = 4 + 1 + 1 + 1 + 8 + 8 + 8;

// AGENT: borrow one lock-consistent view of live FileNode state while encoding
// it, without moving runtime ownership of EOF, data blocks, or directory data.
pub(super) struct MetadataSnapshot<'a> {
    kind: FileKind,
    executable: bool,
    byte_len: usize,
    data_blocks: &'a FileNodeBlocks,
    entries: &'a [DirEntry],
}

impl<'a> MetadataSnapshot<'a> {
    // AGENT: make every field represented by metadata encoding explicit at the
    // FileNode-to-metadata module boundary.
    pub(super) fn new(
        kind: FileKind,
        executable: bool,
        byte_len: usize,
        data_blocks: &'a FileNodeBlocks,
        entries: &'a [DirEntry],
    ) -> Self {
        Self {
            kind,
            executable,
            byte_len,
            data_blocks,
            entries,
        }
    }

    // AGENT: encode only logical inode metadata; metadata block ids remain
    // backend locators and must not become self-referential payload fields.
    pub(super) fn encode(&self) -> Result<Vec<u8>, &'static str> {
        let payload_len = metadata_payload_len(self.data_blocks.len(), self.entries, None)?;
        let mut payload = Vec::with_capacity(payload_len);

        put_metadata_bytes(&mut payload, FILE_NODE_METADATA_MAGIC);
        put_metadata_bytes(&mut payload, &[FILE_NODE_METADATA_VERSION]);
        put_metadata_bytes(
            &mut payload,
            &[match self.kind {
                FileKind::Regular => 1,
                FileKind::Directory => 2,
            }],
        );
        put_metadata_bytes(&mut payload, &[self.executable as u8]);
        put_metadata_u64(&mut payload, self.byte_len)?;
        put_metadata_u64(&mut payload, self.data_blocks.len())?;
        put_metadata_u64(&mut payload, self.entries.len())?;

        for block in self.data_blocks.ids() {
            let stored_id = block.checked_add(1).ok_or("efbig")?;
            put_metadata_u64(&mut payload, stored_id)?;
        }
        for entry in self.entries.iter() {
            put_metadata_bytes(&mut payload, &entry.inode.to_le_bytes());
            put_metadata_u64(&mut payload, entry.name.len())?;
            put_metadata_bytes(&mut payload, entry.name.as_bytes());
        }
        debug_assert_eq!(payload.len(), payload_len);

        Ok(payload)
    }
}

// AGENT: own the backend blocks that contain one FileNode's encoded metadata;
// the FileNode remains responsible for deciding when state changes need writes.
pub(super) struct FileMetadata {
    blocks: Mutex<FileNodeBlocks>,
}

impl FileMetadata {
    // AGENT: start without a backend locator so the first fallible mutation can
    // reserve metadata capacity before publishing live state.
    pub(super) fn empty() -> Self {
        Self {
            blocks: Mutex::new(FileNodeBlocks::empty()),
        }
    }

    // AGENT: adopt validated metadata locators from the inode table without
    // allocating new blocks or treating locator ids as FNMD payload fields.
    pub(super) fn from_ids(backend: &FileStorage, ids: &[usize]) -> Result<Self, &'static str> {
        if ids.is_empty() {
            return Err("eio");
        }
        Ok(Self {
            blocks: Mutex::new(FileNodeBlocks::from_ids(backend, ids)?),
        })
    }

    // AGENT: read and decode the complete zero-padded FNMD block sequence named
    // by one inode-table entry.
    pub(super) fn decode_from_blocks(
        backend: &FileStorage,
        ids: &[usize],
    ) -> Result<RecoveredNodeState, &'static str> {
        if ids.is_empty() {
            return Err("eio");
        }
        let capacity = ids.len().checked_mul(BLOCK_CACHE_BLOCK_SIZE).ok_or("eio")?;
        let mut payload = Vec::with_capacity(capacity);
        for &id in ids {
            payload.extend_from_slice(&backend.read_block(id)?);
        }
        decode_metadata_payload(&payload)
    }

    // AGENT: size a current or prospective metadata image without allocating;
    // callers can release live-state locks before reserving backend blocks.
    fn encoded_len_for_state(
        data_blocks: usize,
        entries: &[DirEntry],
        additional_entry_name: Option<&str>,
    ) -> Result<usize, &'static str> {
        metadata_payload_len(data_blocks, entries, additional_entry_name)
    }

    // AGENT: idempotently reserve enough metadata blocks before callers publish
    // a larger live state; the commit path computes its exact write count.
    fn ensure_capacity(
        &self,
        backend: &FileStorage,
        payload_len: usize,
    ) -> Result<(), &'static str> {
        let needed_blocks = blocks_for_len(payload_len.max(1))?;
        let mut blocks = self.blocks.lock().unwrap();
        if blocks.len() >= needed_blocks {
            return Ok(());
        }
        let mut allocated = Vec::new();
        while blocks.len() + allocated.len() < needed_blocks {
            allocated.push(backend.allocate_block()?);
        }
        blocks.blocks.append(&mut allocated);
        Ok(())
    }

    // AGENT: consume capacity reserved by the serialized mutation preflight,
    // write a complete image, then release blocks beyond the committed size.
    fn write_payload(&self, backend: &FileStorage, payload: &[u8]) -> Result<(), &'static str> {
        let needed_blocks = blocks_for_len(payload.len())?;
        let extra_blocks = {
            let mut blocks = self.blocks.lock().unwrap();
            if blocks.len() < needed_blocks {
                return Err("eio");
            }
            for idx in 0..needed_blocks {
                let start = idx * BLOCK_CACHE_BLOCK_SIZE;
                let end = min(start + BLOCK_CACHE_BLOCK_SIZE, payload.len());
                let mut block_payload = [0u8; BLOCK_CACHE_BLOCK_SIZE];
                if start < end {
                    block_payload[..end - start].copy_from_slice(&payload[start..end]);
                }
                backend.write_block(blocks.blocks[idx].id(), &block_payload)?;
            }
            if blocks.len() > needed_blocks {
                blocks.split_off(needed_blocks)
            } else {
                Vec::new()
            }
        };
        drop(extra_blocks);
        Ok(())
    }

    // AGENT: expose only the allocation count needed by FileNode diagnostics
    // and focused QEMU regressions, without leaking backend locator ids.
    pub(super) fn block_count(&self) -> usize {
        self.blocks.lock().unwrap().len()
    }

    // AGENT: snapshot metadata locators only for filesystem-wide inode-table
    // persistence; FNMD itself remains free of self-referential block ids.
    pub(super) fn block_ids(&self) -> Vec<usize> {
        self.blocks.lock().unwrap().ids().collect()
    }
}

// AGENT: keep all metadata preflight and commit orchestration beside the
// metadata store so FileNode's main implementation only expresses file logic.
impl FileNode {
    // AGENT: reject an existing child and reserve the prospective directory
    // image before insert_child publishes either of its live indexes.
    pub(super) fn prepare_child_insert(
        &self,
        backend: &FileStorage,
        name: ChildName<'_>,
    ) -> Result<(), &'static str> {
        let payload_len = {
            let directory = self.directory.lock().unwrap();
            if directory.by_name.contains_key(name.as_str()) {
                return Err("eexist");
            }
            let data_blocks = self.storage.lock().unwrap().blocks.len();
            FileMetadata::encoded_len_for_state(
                data_blocks,
                &directory.entries,
                Some(name.as_str()),
            )?
        };
        self.metadata.ensure_capacity(backend, payload_len)
    }

    // AGENT: skip zero-length writes and reserve metadata capacity only when a
    // write expands the data-block map; fixed-width EOF changes reuse capacity.
    pub(super) fn prepare_write(
        &self,
        backend: &FileStorage,
        offset: Option<usize>,
        len: usize,
    ) -> Result<(), &'static str> {
        if len == 0 {
            return Ok(());
        }
        let data_blocks = {
            let storage = self.storage.lock().unwrap();
            let start = offset.unwrap_or(storage.byte_len);
            let end = start.checked_add(len).ok_or("efbig")?;
            let data_blocks = blocks_for_len(end)?;
            if data_blocks <= storage.blocks.len() {
                return Ok(());
            }
            data_blocks
        };
        self.reserve_data_layout(backend, data_blocks)
    }

    // AGENT: reserve a target metadata layout only when set_len will change EOF
    // or the number of owned data blocks.
    pub(super) fn prepare_resize(
        &self,
        backend: &FileStorage,
        data_blocks: usize,
        byte_len: usize,
    ) -> Result<(), &'static str> {
        {
            let storage = self.storage.lock().unwrap();
            if storage.blocks.len() == data_blocks && storage.byte_len == byte_len {
                return Ok(());
            }
        }
        self.reserve_data_layout(backend, data_blocks)
    }

    // AGENT: short-circuit an already-satisfied fallocate request and otherwise
    // reserve its prospective data-block metadata before file growth begins.
    pub(super) fn prepare_growth(
        &self,
        backend: &FileStorage,
        data_blocks: usize,
        byte_len: usize,
    ) -> Result<bool, &'static str> {
        {
            let storage = self.storage.lock().unwrap();
            if storage.byte_len >= byte_len {
                return Ok(false);
            }
        }
        self.reserve_data_layout(backend, data_blocks)?;
        Ok(true)
    }

    // AGENT: encode a lock-consistent current state and release live-state
    // guards before writing its image through the block cache.
    pub(super) fn persist_state(&self, backend: &FileStorage) -> Result<(), &'static str> {
        let payload = {
            let directory = self.directory.lock().unwrap();
            let storage = self.storage.lock().unwrap();
            MetadataSnapshot::new(
                self.kind,
                self.executable.load(Ordering::Relaxed),
                storage.byte_len,
                &storage.blocks,
                &directory.entries,
            )
            .encode()?
        };
        self.metadata.write_payload(backend, &payload)
    }

    // AGENT: calculate a prospective regular-file image under the directory
    // lock, then release live state before fallible metadata-block allocation.
    pub(super) fn reserve_data_layout(
        &self,
        backend: &FileStorage,
        data_blocks: usize,
    ) -> Result<(), &'static str> {
        let payload_len = {
            let directory = self.directory.lock().unwrap();
            FileMetadata::encoded_len_for_state(data_blocks, &directory.entries, None)?
        };
        self.metadata.ensure_capacity(backend, payload_len)
    }
}

// AGENT: compute one encoded image length for current state or for a pending
// directory insertion that must be capacity-checked before it becomes visible.
fn metadata_payload_len(
    data_blocks: usize,
    entries: &[DirEntry],
    additional_entry_name: Option<&str>,
) -> Result<usize, &'static str> {
    let entry_count = entries
        .len()
        .checked_add(usize::from(additional_entry_name.is_some()))
        .ok_or("efbig")?;
    let mut entry_name_bytes = 0usize;
    for entry in entries.iter() {
        entry_name_bytes = checked_metadata_add(entry_name_bytes, entry.name.len())?;
    }
    if let Some(name) = additional_entry_name {
        entry_name_bytes = checked_metadata_add(entry_name_bytes, name.len())?;
    }

    let data_block_bytes = data_blocks.checked_mul(8).ok_or("efbig")?;
    let entry_metadata_bytes = entry_count.checked_mul(16).ok_or("efbig")?;
    let len = checked_metadata_add(FILE_NODE_METADATA_HEADER_LEN, data_block_bytes)?;
    let len = checked_metadata_add(len, entry_metadata_bytes)?;
    checked_metadata_add(len, entry_name_bytes)
}

// AGENT: centralize overflow-checked metadata length arithmetic.
fn checked_metadata_add(lhs: usize, rhs: usize) -> Result<usize, &'static str> {
    lhs.checked_add(rhs).ok_or("efbig")
}

// AGENT: append raw format fields without exposing encoding details to FileNode.
fn put_metadata_bytes(payload: &mut Vec<u8>, bytes: &[u8]) {
    payload.extend_from_slice(bytes);
}

// AGENT: keep host-width values out of the stable little-endian metadata image.
fn put_metadata_u64(payload: &mut Vec<u8>, value: usize) -> Result<(), &'static str> {
    let value = u64::try_from(value).map_err(|_| "efbig")?;
    put_metadata_bytes(payload, &value.to_le_bytes());
    Ok(())
}

// AGENT: decode the stable little-endian FNMD v2 image with strict bounds,
// flag, kind, UTF-8, block-count, and zero-padding validation.
fn decode_metadata_payload(payload: &[u8]) -> Result<RecoveredNodeState, &'static str> {
    let mut reader = LeReader::new(payload);
    if reader.take(FILE_NODE_METADATA_MAGIC.len())? != FILE_NODE_METADATA_MAGIC {
        return Err("eio");
    }
    if reader.u8()? != FILE_NODE_METADATA_VERSION {
        return Err("eio");
    }
    let kind = match reader.u8()? {
        1 => FileKind::Regular,
        2 => FileKind::Directory,
        _ => return Err("eio"),
    };
    let executable = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err("eio"),
    };
    let byte_len = usize::try_from(reader.u64()?).map_err(|_| "eio")?;
    let data_block_count = usize::try_from(reader.u64()?).map_err(|_| "eio")?;
    let entry_count = usize::try_from(reader.u64()?).map_err(|_| "eio")?;

    if data_block_count > reader.remaining() / 8 {
        return Err("eio");
    }
    let mut data_blocks = Vec::with_capacity(data_block_count);
    for _ in 0..data_block_count {
        let stored = reader.u64()?;
        let block = stored.checked_sub(1).ok_or("eio")?;
        data_blocks.push(usize::try_from(block).map_err(|_| "eio")?);
    }

    if entry_count > reader.remaining() / 16 {
        return Err("eio");
    }
    let mut entries = Vec::with_capacity(entry_count);
    let mut names = BTreeSet::new();
    for _ in 0..entry_count {
        let inode = reader.u64()?;
        if inode == 0 {
            return Err("eio");
        }
        let name_len = usize::try_from(reader.u64()?).map_err(|_| "eio")?;
        let name = core::str::from_utf8(reader.take(name_len)?)
            .map_err(|_| "eio")?
            .to_string();
        ChildName::new(&name).map_err(|_| "eio")?;
        if !names.insert(name.clone()) {
            return Err("eio");
        }
        entries.push(DirEntry { name, inode });
    }

    if reader.remaining_bytes().iter().any(|&byte| byte != 0) {
        return Err("eio");
    }
    match kind {
        FileKind::Regular => {
            if !entries.is_empty()
                || blocks_for_len(byte_len).map_err(|_| "eio")? != data_blocks.len()
            {
                return Err("eio");
            }
        }
        FileKind::Directory => {
            if executable || byte_len != 0 || !data_blocks.is_empty() {
                return Err("eio");
            }
        }
    }

    Ok(RecoveredNodeState {
        kind,
        executable,
        byte_len,
        data_blocks,
        entries,
    })
}
