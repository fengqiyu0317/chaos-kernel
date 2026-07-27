// AGENT: connect filesystem instances, mount topology, and FInstance identity
// through real direct-child component traversal.
use super::*;

// AGENT: retain a normalized external path only for display and exec naming;
// object identity is exclusively the embedded FInstance.
#[derive(Clone)]
pub struct ResolvedPath {
    pub path_ref: FInstance,
    pub display_path: String,
}

// AGENT: carry the current mount-plus-inode view and the simplified absolute
// ancestor stack needed for ordered dot-dot traversal in this VFS stage.
struct WalkState {
    current: FInstance,
    ancestors: Vec<FInstance>,
    names: Vec<String>,
}

// AGENT: distinguish parent traversal from an already-validated ordinary child
// so lower namespace layers never receive a raw pathname component.
#[derive(Clone, Copy)]
enum PathComponent<'a> {
    Parent,
    Child(ChildName<'a>),
}

// AGENT: separate creation-parent resolution from full-path lookup so a missing
// final name is never confused with a missing or non-directory intermediate.
struct ParentResolution<'a> {
    parent: FInstance,
    name: ChildName<'a>,
    display_path: String,
}

// AGENT: own the process-wide mount topology and allocate runtime filesystem
// identities for syscall-created ChaosFs instances.
pub struct Vfs {
    pub mounts: MountTable,
    next_fs_id: AtomicUsize,
}

// AGENT: resolve absolute paths across identity-keyed mounts, then delegate all
// node creation and storage access to the selected FsInstance.
impl Vfs {
    // AGENT: construct a VFS around the already-created root filesystem and its
    // unique root mount attachment.
    pub fn new(root_fs: Arc<FsInstance>) -> Self {
        let next_fs_id = root_fs.id().checked_add(1).expect("root fs id overflow");
        Self {
            mounts: MountTable::new(root_fs),
            next_fs_id: AtomicUsize::new(next_fs_id),
        }
    }

    // AGENT: return the root filesystem through the root mount ownership chain.
    pub fn root_fs(&self) -> Arc<FsInstance> {
        self.mounts.root().fs().clone()
    }

    // AGENT: allocate one runtime filesystem instance for a caller-selected
    // storage backend; source/device discovery remains a later VFS stage.
    pub fn new_filesystem(&self, storage: FileStorage) -> Arc<FsInstance> {
        let id = self
            .next_fs_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("runtime filesystem id space exhausted");
        FsInstance::new(id, storage)
    }

    // AGENT: parse one absolute path into parent markers and validated child
    // names while discarding separators and dot components.
    fn components(path: &str) -> Result<Vec<PathComponent<'_>>, &'static str> {
        if path.is_empty() {
            return Err("enoent");
        }
        if !path.starts_with('/') {
            return Err("einval");
        }
        let mut components = Vec::new();
        for component in path.split('/') {
            match component {
                "" | "." => {}
                ".." => components.push(PathComponent::Parent),
                name => components.push(PathComponent::Child(ChildName::new(name)?)),
            }
        }
        Ok(components)
    }

    // AGENT: render the successfully traversed external component stack without
    // using it as filesystem object identity or a lookup key.
    fn display_path(names: &[String]) -> String {
        if names.is_empty() {
            String::from("/")
        } else {
            let mut path = String::new();
            for name in names {
                path.push('/');
                path.push_str(name);
            }
            path
        }
    }

    // AGENT: look up exactly one direct child in the current filesystem and
    // optionally cross the visible mount attached to that inode.
    fn step_down(
        &self,
        current: &FInstance,
        name: ChildName<'_>,
        follow_mount: bool,
    ) -> Result<FInstance, &'static str> {
        let node = current.mount.fs().lookup_child(&current.node, name)?;
        let mut next = FInstance::new(current.mount.clone(), node);
        if follow_mount {
            if let Some(child_mount) = self.mounts.mounted_on(&next.mount, &next.node) {
                next = FInstance::new(child_mount.clone(), child_mount.fs().root());
            }
        }
        Ok(next)
    }

    // AGENT: apply dot-dot only after proving the current object is a directory;
    // absolute traversal remains pinned at the VFS root in this stage.
    fn step_up(state: &mut WalkState) -> Result<(), &'static str> {
        if state.current.node.kind != FileKind::Directory {
            return Err("enotdir");
        }
        if state.ancestors.len() > 1 {
            state.ancestors.pop();
            state.names.pop();
        }
        state.current = state
            .ancestors
            .last()
            .expect("absolute VFS walk always retains its root")
            .clone();
        Ok(())
    }

    // AGENT: walk components in source order so missing/.. and regular/.. cannot
    // be collapsed before the required lookup and directory checks occur.
    fn walk_components(
        &self,
        components: &[PathComponent<'_>],
        follow_final_mount: bool,
    ) -> Result<WalkState, &'static str> {
        let root_mount = self.mounts.root();
        let root = FInstance::new(root_mount.clone(), root_mount.fs().root());
        let mut state = WalkState {
            current: root.clone(),
            ancestors: vec![root],
            names: Vec::new(),
        };

        for (index, component) in components.iter().copied().enumerate() {
            let name = match component {
                PathComponent::Parent => {
                    Self::step_up(&mut state)?;
                    continue;
                }
                PathComponent::Child(name) => name,
            };
            let final_component = index + 1 == components.len();
            state.current =
                self.step_down(&state.current, name, !final_component || follow_final_mount)?;
            state.ancestors.push(state.current.clone());
            state.names.push(name.as_str().to_string());
        }
        Ok(state)
    }

    // AGENT: resolve an absolute path to a stable object while deciding whether
    // an attachment on its final inode should be crossed.
    fn resolve_internal(
        &self,
        path: &str,
        follow_final_mount: bool,
    ) -> Result<ResolvedPath, &'static str> {
        let components = Self::components(path)?;
        let state = self.walk_components(&components, follow_final_mount)?;
        Ok(ResolvedPath {
            display_path: Self::display_path(&state.names),
            path_ref: state.current,
        })
    }

    // AGENT: resolve a creation target's parent by walking every preceding
    // component and leaving exactly one validated ordinary child name.
    fn resolve_parent<'a>(&self, path: &'a str) -> Result<ParentResolution<'a>, &'static str> {
        let mut components = Self::components(path)?;
        let name = match components.pop().ok_or("eexist")? {
            PathComponent::Parent => {
                components.push(PathComponent::Parent);
                let _ = self.walk_components(&components, true)?;
                return Err("eexist");
            }
            PathComponent::Child(name) => name,
        };
        let state = self.walk_components(&components, true)?;
        let mut names = state.names.clone();
        names.push(name.as_str().to_string());
        Ok(ParentResolution {
            parent: state.current,
            name,
            display_path: Self::display_path(&names),
        })
    }

    // AGENT: resolve one visible absolute path to stable mount-plus-inode identity.
    pub fn resolve(&self, path: &str) -> Result<ResolvedPath, &'static str> {
        self.resolve_internal(path, true)
    }

    // AGENT: perform regular-file lookup and optional atomic creation inside the
    // filesystem selected by the resolved parent FInstance.
    pub(crate) fn open_regular(
        &self,
        path: &str,
        create: bool,
        exclusive: bool,
    ) -> Result<ResolvedPath, &'static str> {
        match self.resolve_internal(path, true) {
            Ok(existing) => {
                if exclusive {
                    return Err("eexist");
                }
                if existing.path_ref.node.kind != FileKind::Regular {
                    return Err("eisdir");
                }
                Ok(existing)
            }
            Err("enoent") if create => {
                let parent = self.resolve_parent(path)?;
                let node = parent.parent.mount.fs().open_regular_at(
                    &parent.parent.node,
                    parent.name,
                    true,
                    exclusive,
                )?;
                Ok(ResolvedPath {
                    path_ref: FInstance::new(parent.parent.mount, node),
                    display_path: parent.display_path,
                })
            }
            Err(error) => Err(error),
        }
    }

    // AGENT: install an in-kernel regular file at a resolved parent using that
    // mount's storage for initial contents and direct-child metadata.
    pub fn install_regular(
        &self,
        path: &str,
        data: &[u8],
        executable: bool,
    ) -> Result<ResolvedPath, &'static str> {
        match self.resolve_internal(path, true) {
            Ok(existing) if existing.display_path == "/" => return Err("eisdir"),
            Ok(_) | Err("enoent") => {}
            Err(error) => return Err(error),
        }
        let parent = self.resolve_parent(path)?;
        let node = parent.parent.mount.fs().install_regular_at(
            &parent.parent.node,
            parent.name,
            data,
            executable,
        )?;
        Ok(ResolvedPath {
            path_ref: FInstance::new(parent.parent.mount, node),
            display_path: parent.display_path,
        })
    }

    // AGENT: create one user-requested directory through an atomic parent/name
    // operation, rejecting any visible pre-existing object.
    pub fn create_directory(&self, path: &str) -> Result<ResolvedPath, &'static str> {
        match self.resolve_internal(path, true) {
            Ok(_) => return Err("eexist"),
            Err("enoent") => {}
            Err(error) => return Err(error),
        }
        let parent = self.resolve_parent(path)?;
        let node = parent
            .parent
            .mount
            .fs()
            .create_directory_at(&parent.parent.node, parent.name)?;
        Ok(ResolvedPath {
            path_ref: FInstance::new(parent.parent.mount, node),
            display_path: parent.display_path,
        })
    }

    // AGENT: establish a kernel fixture directory idempotently in the selected
    // filesystem without creating synthetic mount-backed path keys.
    pub fn install_directory(&self, path: &str) -> Result<ResolvedPath, &'static str> {
        match self.resolve_internal(path, true) {
            Ok(existing) => {
                return if existing.path_ref.node.kind == FileKind::Directory {
                    Ok(existing)
                } else {
                    Err("eexist")
                };
            }
            Err("enoent") => {}
            Err(error) => return Err(error),
        }
        let parent = self.resolve_parent(path)?;
        let node = parent
            .parent
            .mount
            .fs()
            .install_directory_at(&parent.parent.node, parent.name)?;
        Ok(ResolvedPath {
            path_ref: FInstance::new(parent.parent.mount, node),
            display_path: parent.display_path,
        })
    }

    // AGENT: resolve a mountpoint without crossing its final visible attachment
    // so repeated attaches stack on the same parent inode.
    fn mountpoint(&self, path: &str) -> Result<FInstance, &'static str> {
        let resolved = self.resolve_internal(path, false)?;
        if resolved.display_path == "/" {
            return Err("einval");
        }
        Ok(resolved.path_ref)
    }

    // AGENT: attach a filesystem at an existing visible directory mountpoint.
    pub fn attach(
        &self,
        target: &str,
        fs: Arc<FsInstance>,
        flags: MountFlags,
    ) -> Result<Arc<Mount>, &'static str> {
        let mountpoint = self.mountpoint(target)?;
        self.mounts
            .attach(&mountpoint.mount, mountpoint.node, fs, flags)
    }

    // AGENT: detach and return the visible top mount at an exact external path.
    pub fn detach_top(&self, target: &str) -> Result<Arc<Mount>, &'static str> {
        let mountpoint = self.mountpoint(target)?;
        self.mounts.detach_top(&mountpoint.mount, &mountpoint.node)
    }
}
