// AGENT: model filesystem attachments by mount and inode identity instead of
// rewriting absolute path strings into synthetic device-prefixed keys.
use super::*;

pub type MountId = usize;
const ROOT_MOUNT_ID: MountId = 1;
pub const MNT_DETACH: usize = 0x2;

// AGENT: distinguish a ref-counted, flush-before-commit unmount from a lazy
// subtree detach that preserves already-pinned objects after namespace removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnmountMode {
    Normal,
    Lazy,
}

// AGENT: block new topology crossings while an unmount flushes without making
// a failed flush publish a partially detached namespace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
pub enum MountState {
    Attached = 0,
    Unmounting = 1,
    Detached = 2,
}

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
    active_refs: AtomicUsize,
    state: AtomicUsize,
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
            active_refs: AtomicUsize::new(0),
            state: AtomicUsize::new(MountState::Attached as usize),
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
            active_refs: AtomicUsize::new(0),
            state: AtomicUsize::new(MountState::Attached as usize),
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

    // AGENT: expose the lifecycle state without allowing path or fd code to
    // mutate topology state outside MountTable's write-side transaction.
    pub fn state(&self) -> MountState {
        match self.state.load(Ordering::SeqCst) {
            value if value == MountState::Attached as usize => MountState::Attached,
            value if value == MountState::Unmounting as usize => MountState::Unmounting,
            value if value == MountState::Detached as usize => MountState::Detached,
            _ => unreachable!("invalid mount lifecycle state"),
        }
    }

    // AGENT: count only explicit path/fd pins; topology Arcs and diagnostic
    // clones deliberately do not participate in ordinary-unmount busy checks.
    pub fn active_refs(&self) -> usize {
        self.active_refs.load(Ordering::SeqCst)
    }

    // AGENT: close the race between an unlocked path-object constructor and an
    // unmount transition by validating state on both sides of the increment.
    fn try_pin(mount: Arc<Self>) -> Result<MountPin, &'static str> {
        match mount.state() {
            MountState::Attached => {}
            MountState::Unmounting => return Err("ebusy"),
            MountState::Detached => return Err("einval"),
        }
        mount.active_refs.fetch_add(1, Ordering::SeqCst);
        match mount.state() {
            MountState::Attached => Ok(MountPin { mount }),
            MountState::Unmounting => {
                mount.active_refs.fetch_sub(1, Ordering::SeqCst);
                Err("ebusy")
            }
            MountState::Detached => {
                mount.active_refs.fetch_sub(1, Ordering::SeqCst);
                Err("einval")
            }
        }
    }

    // AGENT: establish a visible-mount pin while the caller still holds the
    // topology read lock, so unmount cannot pass its busy check in between.
    fn pin_attached_locked(mount: Arc<Self>) -> Result<MountPin, &'static str> {
        match mount.state() {
            MountState::Attached => {
                mount.active_refs.fetch_add(1, Ordering::SeqCst);
                Ok(MountPin { mount })
            }
            MountState::Unmounting => Err("ebusy"),
            MountState::Detached => Err("einval"),
        }
    }

    // AGENT: begin one topology-serialized unmount and report competing or
    // stale callers without overwriting their lifecycle transition.
    fn begin_unmount_locked(&self) -> Result<(), &'static str> {
        self.state
            .compare_exchange(
                MountState::Attached as usize,
                MountState::Unmounting as usize,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map(|_| ())
            .map_err(|state| {
                if state == MountState::Unmounting as usize {
                    "ebusy"
                } else {
                    "einval"
                }
            })
    }

    // AGENT: roll a failed or rejected unmount back to a path-visible state.
    fn restore_attached_locked(&self) {
        let previous = self
            .state
            .swap(MountState::Attached as usize, Ordering::SeqCst);
        debug_assert_eq!(previous, MountState::Unmounting as usize);
    }

    // AGENT: publish final detachment only after flush and topology removal
    // have both succeeded.
    fn finish_detached_locked(&self) {
        let previous = self
            .state
            .swap(MountState::Detached as usize, Ordering::SeqCst);
        debug_assert_eq!(previous, MountState::Unmounting as usize);
    }
}

// AGENT: make every durable path identity contribute exactly one explicit busy
// reference independently from Arc topology ownership and temporary raw clones.
pub struct MountPin {
    mount: Arc<Mount>,
}

impl MountPin {
    // AGENT: pin an attached mount outside the topology lock using the mount
    // state handshake that rejects an overlapping unmount transition.
    pub(crate) fn try_new(mount: Arc<Mount>) -> Result<Self, &'static str> {
        Mount::try_pin(mount)
    }

    // AGENT: lend the stable Arc identity when a topology API needs the parent
    // mount without converting topology ownership into another active pin.
    pub fn mount(&self) -> &Arc<Mount> {
        &self.mount
    }
}

impl Clone for MountPin {
    // AGENT: cloning an already-valid path reference remains valid after lazy
    // detach and therefore increments without rechecking attachment state.
    fn clone(&self) -> Self {
        self.mount.active_refs.fetch_add(1, Ordering::SeqCst);
        Self {
            mount: self.mount.clone(),
        }
    }
}

impl Drop for MountPin {
    // AGENT: release the explicit busy reference independently from when the
    // underlying Arc allocation is finally reclaimed.
    fn drop(&mut self) {
        let previous = self.mount.active_refs.fetch_sub(1, Ordering::SeqCst);
        debug_assert!(previous > 0, "mount pin count underflow");
    }
}

impl Deref for MountPin {
    type Target = Mount;

    fn deref(&self) -> &Self::Target {
        self.mount.as_ref()
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

    // AGENT: detect any direct child-mount stack owned by the target mount;
    // stacked mounts below the target at its parent mountpoint are unrelated.
    fn has_children_locked(
        children: &BTreeMap<MountPointKey, Vec<Arc<Mount>>>,
        mount_id: MountId,
    ) -> bool {
        children.keys().any(|key| key.parent_mount == mount_id)
    }

    // AGENT: collect every attachment whose parent-mount identity descends from
    // one visible root without recursively reacquiring the topology lock.
    fn collect_subtree_locked(
        children: &BTreeMap<MountPointKey, Vec<Arc<Mount>>>,
        root: Arc<Mount>,
    ) -> Vec<Arc<Mount>> {
        let mut mounts = vec![root.clone()];
        let mut ids = BTreeSet::new();
        let mut pending = VecDeque::new();
        ids.insert(root.id());
        pending.push_back(root.id());
        while let Some(parent_id) = pending.pop_front() {
            for (key, stack) in children {
                if key.parent_mount != parent_id {
                    continue;
                }
                for mount in stack {
                    if ids.insert(mount.id()) {
                        mounts.push(mount.clone());
                        pending.push_back(mount.id());
                    }
                }
            }
        }
        mounts
    }

    // AGENT: derive the exact stack key once so prepare and commit validate the
    // same parent-view/inode identity rather than re-resolving pathname text.
    fn mountpoint_key(parent: &Arc<Mount>, node: &Arc<FileNode>) -> MountPointKey {
        MountPointKey {
            parent_mount: parent.id(),
            inode: node.id(),
        }
    }

    // AGENT: restore every mount reserved by a failed flush while holding the
    // topology write lock, preserving the pre-unmount namespace atomically.
    fn restore_unmounting(&self, mounts: &[Arc<Mount>]) {
        let _children = self.children.write().unwrap();
        for mount in mounts {
            if mount.state() == MountState::Unmounting {
                mount.restore_attached_locked();
            }
        }
    }

    // AGENT: flush outside the mount-table lock; lazy detach intentionally uses
    // the same synchronous policy for every filesystem in its detached subtree.
    fn flush_mounts(mounts: &[Arc<Mount>]) -> Result<(), &'static str> {
        for mount in mounts {
            mount.fs().flush()?;
        }
        Ok(())
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
        match parent.state() {
            MountState::Attached => {}
            MountState::Unmounting => return Err("ebusy"),
            MountState::Detached => return Err("einval"),
        }
        let key = Self::mountpoint_key(parent, &mountpoint);
        if children
            .get(&key)
            .and_then(|stack| stack.last())
            .is_some_and(|mount| mount.state() == MountState::Unmounting)
        {
            return Err("ebusy");
        }
        let mount = Mount::attached(
            self.allocate_mount_id(),
            fs,
            parent,
            mountpoint.clone(),
            flags,
        );
        children.entry(key).or_default().push(mount.clone());
        Ok(mount)
    }

    // AGENT: find the visible top attachment and establish its active pin in one
    // read-side critical section, closing the path-walk/unmount race window.
    pub fn mounted_on(
        &self,
        parent: &Arc<Mount>,
        node: &Arc<FileNode>,
    ) -> Result<Option<MountPin>, &'static str> {
        if !parent.fs().owns_node(node) {
            return Err("exdev");
        }
        let children = self.children.read().unwrap();
        match parent.state() {
            MountState::Attached => {}
            MountState::Unmounting => return Err("ebusy"),
            MountState::Detached => return Err("einval"),
        }
        children
            .get(&Self::mountpoint_key(parent, node))
            .and_then(|stack| stack.last().cloned())
            .map(Mount::pin_attached_locked)
            .transpose()
    }

    // AGENT: perform either a busy-checked single-mount unmount or a lazy
    // subtree detach, with flush failure rolling back every lifecycle state.
    pub fn unmount_top(
        &self,
        parent: &Arc<Mount>,
        node: &Arc<FileNode>,
        mode: UnmountMode,
    ) -> Result<Arc<Mount>, &'static str> {
        if !parent.fs().owns_node(node) {
            return Err("exdev");
        }
        let key = Self::mountpoint_key(parent, node);
        let (mount, subtree) = {
            let children = self.children.write().unwrap();
            if !self.contains_mount_locked(&children, parent) {
                return Err("einval");
            }
            let mount = children
                .get(&key)
                .and_then(|stack| stack.last())
                .cloned()
                .ok_or("einval")?;
            let subtree = match mode {
                UnmountMode::Normal => vec![mount.clone()],
                UnmountMode::Lazy => Self::collect_subtree_locked(&children, mount.clone()),
            };
            if subtree
                .iter()
                .any(|candidate| candidate.state() != MountState::Attached)
            {
                return Err("ebusy");
            }
            for (index, candidate) in subtree.iter().enumerate() {
                if let Err(error) = candidate.begin_unmount_locked() {
                    for started in &subtree[..index] {
                        started.restore_attached_locked();
                    }
                    return Err(error);
                }
            }
            if mode == UnmountMode::Normal
                && (mount.active_refs() != 0 || Self::has_children_locked(&children, mount.id()))
            {
                mount.restore_attached_locked();
                return Err("ebusy");
            }
            (mount, subtree)
        };

        if let Err(error) = Self::flush_mounts(&subtree) {
            self.restore_unmounting(&subtree);
            return Err(error);
        }

        let mut children = self.children.write().unwrap();
        let topology_unchanged = children
            .get(&key)
            .and_then(|stack| stack.last())
            .is_some_and(|top| Arc::ptr_eq(top, &mount))
            && subtree.iter().all(|candidate| {
                candidate.state() == MountState::Unmounting
                    && self.contains_mount_locked(&children, candidate)
            });
        if !topology_unchanged {
            for candidate in &subtree {
                if candidate.state() == MountState::Unmounting {
                    candidate.restore_attached_locked();
                }
            }
            return Err("eio");
        }

        if mode == UnmountMode::Lazy {
            let subtree_ids: BTreeSet<MountId> =
                subtree.iter().map(|candidate| candidate.id()).collect();
            debug_assert!(!subtree_ids.contains(&key.parent_mount));
            children.retain(|child_key, _| !subtree_ids.contains(&child_key.parent_mount));
        }
        let remove_key = {
            let stack = children
                .get_mut(&key)
                .expect("validated unmount target stack disappeared under write lock");
            let removed = stack
                .pop()
                .expect("validated unmount target stack became empty under write lock");
            debug_assert!(Arc::ptr_eq(&removed, &mount));
            stack.is_empty()
        };
        if remove_key {
            children.remove(&key);
        }
        for candidate in &subtree {
            candidate.finish_detached_locked();
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
