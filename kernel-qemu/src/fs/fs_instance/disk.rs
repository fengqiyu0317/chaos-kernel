// AGENT: define the first recoverable ChaosFs disk format and keep format,
// mount-time validation, inode reconstruction, bitmap recovery, and sync beside
// the FsInstance ownership boundary they rebuild.
use super::super::le_codec::{LeReader, LeWriter};
use super::*;

const CHAOSFS_MAGIC: &[u8; 8] = b"CHAOSFS\0";
const CHAOSFS_VERSION: u32 = 1;
const CHAOSFS_SUPERBLOCK: usize = 0;
const CHAOSFS_SUPERBLOCK_LEN: usize = 8 + 4 + 4 + 8 * 6;
const CHAOSFS_BITMAP_START: usize = 1;
const BITS_PER_BITMAP_BLOCK: usize = BLOCK_CACHE_BLOCK_SIZE * 8;
const MIN_DEVICE_BLOCKS: usize = 16;
const MIN_INODE_TABLE_BLOCKS: usize = 8;
const MAX_INODE_TABLE_BLOCKS: usize = 256;

const INODE_TABLE_MAGIC: &[u8; 8] = b"CHINODE\0";
const INODE_TABLE_VERSION: u32 = 1;

// AGENT: retain the validated fixed-region layout on each persistent FsInstance
// so later flushes cannot silently choose a different on-disk geometry.
#[derive(Clone, Copy)]
pub(super) struct ChaosFsLayout {
    device_blocks: usize,
    bitmap_start: usize,
    bitmap_blocks: usize,
    inode_table_start: usize,
    inode_table_blocks: usize,
    data_start: usize,
}

// AGENT: derive and retain the one layout accepted by format, mount, and flush.
impl ChaosFsLayout {
    // AGENT: derive deterministic metadata regions from device capacity; this
    // format version deliberately keeps both bitmap and inode table fixed-size.
    fn for_device(device_blocks: usize) -> Result<Self, &'static str> {
        if device_blocks < MIN_DEVICE_BLOCKS {
            return Err("enospc");
        }
        let bitmap_blocks = device_blocks
            .checked_add(BITS_PER_BITMAP_BLOCK - 1)
            .ok_or("enospc")?
            / BITS_PER_BITMAP_BLOCK;
        let inode_table_start = CHAOSFS_BITMAP_START
            .checked_add(bitmap_blocks)
            .ok_or("enospc")?;
        let proportional = device_blocks / 64;
        let inode_table_blocks = max(
            MIN_INODE_TABLE_BLOCKS,
            min(MAX_INODE_TABLE_BLOCKS, proportional),
        );
        let data_start = inode_table_start
            .checked_add(inode_table_blocks)
            .ok_or("enospc")?;
        if data_start >= device_blocks {
            return Err("enospc");
        }
        Ok(Self {
            device_blocks,
            bitmap_start: CHAOSFS_BITMAP_START,
            bitmap_blocks,
            inode_table_start,
            inode_table_blocks,
            data_start,
        })
    }

    // AGENT: reserve every fixed metadata block before any inode can allocate
    // ordinary data or FNMD storage.
    fn reserved_blocks(self) -> Vec<usize> {
        (0..self.data_start).collect()
    }
}

// AGENT: name the explicit disk lifecycle operations without hiding formatting
// inside mount; callers may choose an open-or-format policy outside this type.
pub struct ChaosFs;

// AGENT: expose explicit format and recovery operations without embedding boot
// policy or source naming in the disk-format implementation.
impl ChaosFs {
    // AGENT: let boot policy distinguish a zeroed new image from an unrelated
    // non-ChaosFs device before choosing the explicit format operation.
    pub fn superblock_is_blank(device: &dyn BlockDevice) -> Result<bool, &'static str> {
        Ok(device
            .read_block(CHAOSFS_SUPERBLOCK)?
            .iter()
            .all(|&byte| byte == 0))
    }

    // AGENT: initialize a new empty filesystem, write a recoverable root inode,
    // and commit all fixed metadata before returning the sole live instance.
    pub fn format(
        fs_id: FsId,
        device: Arc<dyn BlockDevice>,
        cache_chains: usize,
    ) -> Result<Arc<FsInstance>, &'static str> {
        let layout = ChaosFsLayout::for_device(device.block_count())?;
        let storage = storage_from_allocated(
            device,
            cache_chains,
            layout.device_blocks,
            &layout.reserved_blocks(),
        )?;
        let root = Arc::new(FileNode::directory(ROOT_INODE_ID));
        let mut inodes = BTreeMap::new();
        inodes.insert(ROOT_INODE_ID, root.clone());
        let fs = Arc::new(FsInstance {
            id: fs_id,
            kind: FsKind::ChaosFs,
            storage,
            root,
            inodes: RwLock::new(inodes),
            next_inode: AtomicU64::new(ROOT_INODE_ID + 1),
            disk: Some(layout),
        });
        fs.flush()?;
        Ok(fs)
    }

    // AGENT: recover only a valid existing ChaosFs image; absence returns
    // enodev and every malformed known-format structure returns eio.
    pub fn mount(
        fs_id: FsId,
        device: Arc<dyn BlockDevice>,
        cache_chains: usize,
    ) -> Result<Arc<FsInstance>, &'static str> {
        let superblock = device.read_block(CHAOSFS_SUPERBLOCK)?;
        let layout = decode_superblock(&superblock, device.block_count())?;
        let bitmap =
            read_device_region(device.as_ref(), layout.bitmap_start, layout.bitmap_blocks)?;
        let allocated = decode_bitmap(&bitmap, layout)?;
        let storage =
            storage_from_allocated(device, cache_chains, layout.device_blocks, &allocated)?;
        let table = read_storage_region(
            &storage,
            layout.inode_table_start,
            layout.inode_table_blocks,
        )?;
        let records = decode_inode_table(&table, layout)?;
        let decoded = decode_and_validate_nodes(&storage, layout, &allocated, records)?;
        construct_filesystem(fs_id, storage, layout, decoded)
    }
}

// AGENT: pair one cache and one recovered allocator with one device so every
// mounted inode observes coherent data and unique block ownership.
fn storage_from_allocated(
    device: Arc<dyn BlockDevice>,
    cache_chains: usize,
    device_blocks: usize,
    allocated: &[usize],
) -> Result<FileStorage, &'static str> {
    if cache_chains == 0 {
        return Err("einval");
    }
    let allocator = Arc::new(FileBlockAllocator::from_allocated(
        device_blocks,
        allocated,
    )?);
    Ok(FileStorage::new(
        Arc::new(BlockCache::new(cache_chains)),
        device,
        allocator,
    ))
}

// AGENT: serialize all inode images first, then publish their locators, exact
// allocator ownership, and fixed layout before one cache/device flush.
pub(super) fn flush_filesystem(
    fs: &FsInstance,
    layout: ChaosFsLayout,
) -> Result<usize, &'static str> {
    if fs.kind != FsKind::ChaosFs || fs.storage.block_count() != layout.device_blocks {
        return Err("eio");
    }
    let records = {
        let inodes = fs.inodes.read().unwrap();
        let mut records = Vec::with_capacity(inodes.len());
        for (&inode, node) in inodes.iter() {
            let metadata_blocks = node.sync_metadata(&fs.storage)?;
            records.push(InodeTableRecord {
                inode,
                metadata_blocks,
            });
        }
        records
    };
    let inode_table = encode_inode_table(&records, layout)?;
    write_storage_region(
        &fs.storage,
        layout.inode_table_start,
        layout.inode_table_blocks,
        &inode_table,
    )?;

    let allocated = fs.storage.allocated_block_ids();
    let bitmap = encode_bitmap(&allocated, layout)?;
    write_storage_region(
        &fs.storage,
        layout.bitmap_start,
        layout.bitmap_blocks,
        &bitmap,
    )?;
    fs.storage
        .write_block(CHAOSFS_SUPERBLOCK, &encode_superblock(layout)?)?;
    fs.storage.flush()
}

// AGENT: keep inode-table records independent from decoded FNMD contents; each
// record is only the stable inode id plus that inode's metadata locators.
struct InodeTableRecord {
    inode: InodeId,
    metadata_blocks: Vec<usize>,
}

// AGENT: hold decoded state with its locator record until global block and tree
// integrity validation has completed.
struct DecodedInode {
    inode: InodeId,
    metadata_blocks: Vec<usize>,
    state: RecoveredNodeState,
}

// AGENT: encode the versioned fixed geometry in host-width-independent little
// endian fields and zero the remainder of the superblock sector.
fn encode_superblock(layout: ChaosFsLayout) -> Result<[u8; BLOCK_CACHE_BLOCK_SIZE], &'static str> {
    let mut block = [0u8; BLOCK_CACHE_BLOCK_SIZE];
    let mut writer = LeWriter::new(&mut block);
    writer.bytes(CHAOSFS_MAGIC)?;
    writer.u32(CHAOSFS_VERSION)?;
    writer.u32(u32::try_from(BLOCK_CACHE_BLOCK_SIZE).map_err(|_| "eio")?)?;
    writer.u64(ROOT_INODE_ID)?;
    writer.usize(layout.inode_table_start)?;
    writer.usize(layout.inode_table_blocks)?;
    writer.usize(layout.bitmap_start)?;
    writer.usize(layout.bitmap_blocks)?;
    writer.usize(layout.device_blocks)?;
    debug_assert_eq!(writer.position(), CHAOSFS_SUPERBLOCK_LEN);
    Ok(block)
}

// AGENT: recognize ChaosFs magic separately from rejecting a malformed known
// version, then require geometry to match the attached device exactly.
fn decode_superblock(block: &[u8], device_blocks: usize) -> Result<ChaosFsLayout, &'static str> {
    let mut reader = LeReader::new(block);
    if reader.take(CHAOSFS_MAGIC.len())? != CHAOSFS_MAGIC {
        return Err("enodev");
    }
    if reader.u32()? != CHAOSFS_VERSION
        || usize::try_from(reader.u32()?).map_err(|_| "eio")? != BLOCK_CACHE_BLOCK_SIZE
        || reader.u64()? != ROOT_INODE_ID
    {
        return Err("eio");
    }
    let stored = ChaosFsLayout {
        inode_table_start: reader.usize()?,
        inode_table_blocks: reader.usize()?,
        bitmap_start: reader.usize()?,
        bitmap_blocks: reader.usize()?,
        device_blocks: reader.usize()?,
        data_start: 0,
    };
    if reader.remaining_bytes().iter().any(|&byte| byte != 0) {
        return Err("eio");
    }
    let expected = ChaosFsLayout::for_device(device_blocks).map_err(|_| "eio")?;
    if stored.device_blocks != expected.device_blocks
        || stored.bitmap_start != expected.bitmap_start
        || stored.bitmap_blocks != expected.bitmap_blocks
        || stored.inode_table_start != expected.inode_table_start
        || stored.inode_table_blocks != expected.inode_table_blocks
    {
        return Err("eio");
    }
    Ok(expected)
}

// AGENT: serialize stable inode identities and external FNMD locators into the
// bounded fixed inode-table region.
fn encode_inode_table(
    records: &[InodeTableRecord],
    layout: ChaosFsLayout,
) -> Result<Vec<u8>, &'static str> {
    let capacity = region_capacity(layout.inode_table_blocks)?;
    let mut table = Vec::new();
    table.extend_from_slice(INODE_TABLE_MAGIC);
    table.extend_from_slice(&INODE_TABLE_VERSION.to_le_bytes());
    table.extend_from_slice(&0u32.to_le_bytes());
    table.extend_from_slice(
        &u64::try_from(records.len())
            .map_err(|_| "enospc")?
            .to_le_bytes(),
    );
    for record in records {
        if record.inode == 0 || record.metadata_blocks.is_empty() {
            return Err("eio");
        }
        table.extend_from_slice(&record.inode.to_le_bytes());
        table.extend_from_slice(
            &u64::try_from(record.metadata_blocks.len())
                .map_err(|_| "enospc")?
                .to_le_bytes(),
        );
        for &block in record.metadata_blocks.iter() {
            table.extend_from_slice(&u64::try_from(block).map_err(|_| "eio")?.to_le_bytes());
        }
        if table.len() > capacity {
            return Err("enospc");
        }
    }
    Ok(table)
}

// AGENT: decode a zero-padded inode table while rejecting duplicate identities,
// empty locator lists, and locators outside the allocatable data region.
fn decode_inode_table(
    table: &[u8],
    layout: ChaosFsLayout,
) -> Result<Vec<InodeTableRecord>, &'static str> {
    let mut reader = LeReader::new(table);
    if reader.take(INODE_TABLE_MAGIC.len())? != INODE_TABLE_MAGIC
        || reader.u32()? != INODE_TABLE_VERSION
        || reader.u32()? != 0
    {
        return Err("eio");
    }
    let count = reader.usize()?;
    if count == 0 || count > reader.remaining() / 16 {
        return Err("eio");
    }
    let mut records = Vec::with_capacity(count);
    let mut inodes = BTreeSet::new();
    for _ in 0..count {
        let inode = reader.u64()?;
        if inode == 0 || !inodes.insert(inode) {
            return Err("eio");
        }
        let block_count = reader.usize()?;
        if block_count == 0 || block_count > reader.remaining() / 8 {
            return Err("eio");
        }
        let mut metadata_blocks = Vec::with_capacity(block_count);
        for _ in 0..block_count {
            let block = reader.usize()?;
            if block < layout.data_start || block >= layout.device_blocks {
                return Err("eio");
            }
            metadata_blocks.push(block);
        }
        records.push(InodeTableRecord {
            inode,
            metadata_blocks,
        });
    }
    if reader.remaining_bytes().iter().any(|&byte| byte != 0) {
        return Err("eio");
    }
    Ok(records)
}

// AGENT: publish exact allocator ownership and require every fixed metadata
// block to remain reserved in the committed image.
fn encode_bitmap(allocated: &[usize], layout: ChaosFsLayout) -> Result<Vec<u8>, &'static str> {
    let mut bitmap = vec![0u8; region_capacity(layout.bitmap_blocks)?];
    let mut seen = BTreeSet::new();
    for &block in allocated {
        if block >= layout.device_blocks || !seen.insert(block) {
            return Err("eio");
        }
        bitmap[block / 8] |= 1 << (block % 8);
    }
    for reserved in 0..layout.data_start {
        if !seen.contains(&reserved) {
            return Err("eio");
        }
    }
    Ok(bitmap)
}

// AGENT: recover allocated block ids while rejecting missing reservations and
// set bits beyond the physical device capacity.
fn decode_bitmap(bitmap: &[u8], layout: ChaosFsLayout) -> Result<Vec<usize>, &'static str> {
    if bitmap.len() != region_capacity(layout.bitmap_blocks)? {
        return Err("eio");
    }
    let mut allocated = Vec::new();
    for block in 0..layout.device_blocks {
        if bitmap[block / 8] & (1 << (block % 8)) != 0 {
            allocated.push(block);
        }
    }
    for bit in layout.device_blocks..bitmap.len() * 8 {
        if bitmap[bit / 8] & (1 << (bit % 8)) != 0 {
            return Err("eio");
        }
    }
    if (0..layout.data_start).any(|reserved| bitmap[reserved / 8] & (1 << (reserved % 8)) == 0) {
        return Err("eio");
    }
    Ok(allocated)
}

// AGENT: decode every FNMD only after its locators are bitmap-owned and unique,
// then validate data ownership and the complete inode tree before construction.
fn decode_and_validate_nodes(
    storage: &FileStorage,
    layout: ChaosFsLayout,
    allocated: &[usize],
    records: Vec<InodeTableRecord>,
) -> Result<Vec<DecodedInode>, &'static str> {
    let allocated: BTreeSet<usize> = allocated.iter().copied().collect();
    let mut referenced: BTreeSet<usize> = (0..layout.data_start).collect();
    for record in records.iter() {
        for &block in record.metadata_blocks.iter() {
            if !allocated.contains(&block) || !referenced.insert(block) {
                return Err("eio");
            }
        }
    }

    let mut decoded = Vec::with_capacity(records.len());
    for record in records {
        let state = FileNode::decode_persisted(storage, &record.metadata_blocks)?;
        for &block in state.data_blocks.iter() {
            if block < layout.data_start
                || block >= layout.device_blocks
                || !allocated.contains(&block)
                || !referenced.insert(block)
            {
                return Err("eio");
            }
        }
        decoded.push(DecodedInode {
            inode: record.inode,
            metadata_blocks: record.metadata_blocks,
            state,
        });
    }
    // Bitmap-only allocations can represent a cleanly flushed open-but-unlinked
    // inode or an interrupted older transaction. Preserve them as unavailable
    // leaks rather than reusing possibly live data; every reachable block was
    // already required to be present and unique above.
    validate_inode_tree(&decoded)?;
    Ok(decoded)
}

// AGENT: require one directory root, one parent for every other inode, no
// cycles or hard links, and complete reachability from the root.
fn validate_inode_tree(decoded: &[DecodedInode]) -> Result<(), &'static str> {
    let mut by_inode = BTreeMap::new();
    for (index, inode) in decoded.iter().enumerate() {
        if by_inode.insert(inode.inode, index).is_some() {
            return Err("eio");
        }
    }
    let root = *by_inode.get(&ROOT_INODE_ID).ok_or("eio")?;
    if decoded[root].state.kind != FileKind::Directory {
        return Err("eio");
    }

    let mut parents = BTreeMap::<InodeId, usize>::new();
    for inode in decoded.iter() {
        for entry in inode.state.entries.iter() {
            if entry.inode == ROOT_INODE_ID || !by_inode.contains_key(&entry.inode) {
                return Err("eio");
            }
            let count = parents.entry(entry.inode).or_default();
            *count = count.checked_add(1).ok_or("eio")?;
            if *count != 1 {
                return Err("eio");
            }
        }
    }
    for inode in decoded.iter() {
        if inode.inode != ROOT_INODE_ID && parents.get(&inode.inode).copied() != Some(1) {
            return Err("eio");
        }
    }

    let mut visited = BTreeSet::new();
    let mut pending = vec![ROOT_INODE_ID];
    while let Some(inode) = pending.pop() {
        if !visited.insert(inode) {
            return Err("eio");
        }
        let index = *by_inode.get(&inode).ok_or("eio")?;
        pending.extend(decoded[index].state.entries.iter().map(|entry| entry.inode));
    }
    if visited.len() != decoded.len() {
        return Err("eio");
    }
    Ok(())
}

// AGENT: adopt all validated block wrappers into one FileStorage-backed inode
// map and resume inode allocation strictly above every recovered identity.
fn construct_filesystem(
    fs_id: FsId,
    storage: FileStorage,
    layout: ChaosFsLayout,
    decoded: Vec<DecodedInode>,
) -> Result<Arc<FsInstance>, &'static str> {
    let mut inodes = BTreeMap::new();
    let mut max_inode = 0u64;
    for inode in decoded {
        max_inode = max(max_inode, inode.inode);
        let node = FileNode::recover(inode.inode, &storage, &inode.metadata_blocks, inode.state)?;
        if inodes.insert(inode.inode, node).is_some() {
            return Err("eio");
        }
    }
    let root = inodes.get(&ROOT_INODE_ID).cloned().ok_or("eio")?;
    let next_inode = max_inode.checked_add(1).ok_or("eio")?;
    Ok(Arc::new(FsInstance {
        id: fs_id,
        kind: FsKind::ChaosFs,
        storage,
        root,
        inodes: RwLock::new(inodes),
        next_inode: AtomicU64::new(next_inode),
        disk: Some(layout),
    }))
}

// AGENT: read fixed metadata before FileStorage exists, with checked region
// capacity and end arithmetic.
fn read_device_region(
    device: &dyn BlockDevice,
    start: usize,
    blocks: usize,
) -> Result<Vec<u8>, &'static str> {
    let mut payload = Vec::with_capacity(region_capacity(blocks)?);
    for block in start..start.checked_add(blocks).ok_or("eio")? {
        payload.extend_from_slice(&device.read_block(block)?);
    }
    Ok(payload)
}

// AGENT: read fixed metadata through the unique recovered cache once allocator
// ownership has been reconstructed.
fn read_storage_region(
    storage: &FileStorage,
    start: usize,
    blocks: usize,
) -> Result<Vec<u8>, &'static str> {
    let mut payload = Vec::with_capacity(region_capacity(blocks)?);
    for block in start..start.checked_add(blocks).ok_or("eio")? {
        payload.extend_from_slice(&storage.read_block(block)?);
    }
    Ok(payload)
}

// AGENT: zero-pad and overwrite every block in a fixed metadata region so stale
// records cannot survive a shorter later commit.
fn write_storage_region(
    storage: &FileStorage,
    start: usize,
    blocks: usize,
    payload: &[u8],
) -> Result<(), &'static str> {
    let capacity = region_capacity(blocks)?;
    if payload.len() > capacity {
        return Err("enospc");
    }
    for index in 0..blocks {
        let begin = index * BLOCK_CACHE_BLOCK_SIZE;
        let end = min(begin + BLOCK_CACHE_BLOCK_SIZE, payload.len());
        let mut block = [0u8; BLOCK_CACHE_BLOCK_SIZE];
        if begin < end {
            block[..end - begin].copy_from_slice(&payload[begin..end]);
        }
        storage.write_block(start + index, &block)?;
    }
    Ok(())
}

// AGENT: centralize overflow-checked conversion from blocks to serialized bytes.
fn region_capacity(blocks: usize) -> Result<usize, &'static str> {
    blocks.checked_mul(BLOCK_CACHE_BLOCK_SIZE).ok_or("eio")
}
