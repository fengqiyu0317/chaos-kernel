// AGENT: connect filesystem instances, mount topology, and resolved path
// references while retaining flat filesystem-local path tables in stage one.
use super::*;

// AGENT: retain a canonical external path only for display and exec naming;
// object identity is exclusively the embedded FInstance.
#[derive(Clone)]
pub struct ResolvedPath {
    pub path_ref: FInstance,
    pub display_path: String,
}

// AGENT: keep the temporary filesystem-local key private to VFS resolution so
// callers cannot mistake it for stable file identity.
struct InternalResolution {
    resolved: ResolvedPath,
    fs_path: String,
}

// AGENT: own the process-wide mount topology and allocate runtime filesystem
// identities for first-stage syscall-created ChaosFs instances.
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

    // AGENT: canonicalize external absolute paths consistently before lookup,
    // creation, attachment, or detach operations.
    fn canonicalize(path: &str) -> Result<String, &'static str> {
        FsInstance::normalize_path(path)
    }

    // AGENT: append one component to a filesystem-internal canonical path.
    fn push_component(path: &mut String, component: &str) {
        if path.len() > 1 {
            path.push('/');
        }
        path.push_str(component);
    }

    // AGENT: walk external components using each FsInstance's temporary flat
    // lookup table and cross the visible top mount after matching an inode.
    fn resolve_internal(
        &self,
        path: &str,
        follow_final_mount: bool,
    ) -> Result<InternalResolution, &'static str> {
        let display_path = Self::canonicalize(path)?;
        let components: Vec<&str> = display_path
            .split('/')
            .filter(|component| !component.is_empty())
            .collect();
        let mut mount = self.mounts.root();
        let mut node = mount.fs().root();
        let mut fs_path = String::from("/");

        for (index, component) in components.iter().enumerate() {
            Self::push_component(&mut fs_path, component);
            node = mount.fs().lookup(&fs_path)?;
            let final_component = index + 1 == components.len();
            if !final_component || follow_final_mount {
                if let Some(child) = self.mounts.mounted_on(&mount, &node) {
                    mount = child;
                    node = mount.fs().root();
                    fs_path = String::from("/");
                }
            }
        }

        Ok(InternalResolution {
            resolved: ResolvedPath {
                path_ref: FInstance::new(mount, node),
                display_path,
            },
            fs_path,
        })
    }

    // AGENT: resolve one visible absolute path to stable mount-plus-node identity.
    pub fn resolve(&self, path: &str) -> Result<ResolvedPath, &'static str> {
        Ok(self.resolve_internal(path, true)?.resolved)
    }

    // AGENT: resolve a missing child's visible parent and derive the temporary
    // filesystem-local key used only inside the selected FsInstance.
    fn child_location(&self, path: &str) -> Result<(String, Arc<Mount>, String), &'static str> {
        let display_path = Self::canonicalize(path)?;
        if display_path == "/" {
            return Err("eexist");
        }
        let slash = display_path.rfind('/').ok_or("enoent")?;
        let name = &display_path[slash + 1..];
        if name.is_empty() {
            return Err("enoent");
        }
        let parent_path = if slash == 0 {
            String::from("/")
        } else {
            display_path[..slash].to_string()
        };
        let parent = self.resolve_internal(&parent_path, true)?;
        if parent.resolved.path_ref.node.kind != FileKind::Directory {
            return Err("enotdir");
        }
        let mut fs_path = parent.fs_path;
        Self::push_component(&mut fs_path, name);
        Ok((display_path, parent.resolved.path_ref.mount, fs_path))
    }

    // AGENT: perform regular-file lookup and optional creation inside the
    // filesystem selected by the resolved parent mount.
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
                if existing.resolved.path_ref.node.kind != FileKind::Regular {
                    return Err("eisdir");
                }
                Ok(existing.resolved)
            }
            Err("enoent") if create => {
                let (display_path, mount, fs_path) = self.child_location(path)?;
                let node = mount.fs().open_regular(&fs_path, true, exclusive)?;
                Ok(ResolvedPath {
                    path_ref: FInstance::new(mount, node),
                    display_path,
                })
            }
            Err(error) => Err(error),
        }
    }

    // AGENT: install an in-kernel regular file into the filesystem reached by
    // the visible path, using that mount's storage for initial contents.
    pub fn install_regular(
        &self,
        path: &str,
        data: &[u8],
        executable: bool,
    ) -> Result<ResolvedPath, &'static str> {
        let (display_path, mount, fs_path) = match self.resolve_internal(path, true) {
            Ok(existing) => (
                existing.resolved.display_path,
                existing.resolved.path_ref.mount,
                existing.fs_path,
            ),
            Err("enoent") => self.child_location(path)?,
            Err(error) => return Err(error),
        };
        let node = mount.fs().install_regular(&fs_path, data, executable)?;
        Ok(ResolvedPath {
            path_ref: FInstance::new(mount, node),
            display_path,
        })
    }

    // AGENT: create one user-requested directory in the filesystem selected by
    // its resolved parent, rejecting any visible pre-existing object.
    pub fn create_directory(&self, path: &str) -> Result<ResolvedPath, &'static str> {
        match self.resolve_internal(path, true) {
            Ok(_) => Err("eexist"),
            Err("enoent") => {
                let (display_path, mount, fs_path) = self.child_location(path)?;
                let node = mount.fs().create_directory(&fs_path)?;
                Ok(ResolvedPath {
                    path_ref: FInstance::new(mount, node),
                    display_path,
                })
            }
            Err(error) => Err(error),
        }
    }

    // AGENT: establish a kernel fixture directory idempotently in the selected
    // filesystem without creating synthetic mount-backed path keys.
    pub fn install_directory(&self, path: &str) -> Result<ResolvedPath, &'static str> {
        match self.resolve_internal(path, true) {
            Ok(existing) => {
                if existing.resolved.path_ref.node.kind == FileKind::Directory {
                    Ok(existing.resolved)
                } else {
                    Err("eexist")
                }
            }
            Err("enoent") => {
                let (display_path, mount, fs_path) = self.child_location(path)?;
                let node = mount.fs().install_directory(&fs_path)?;
                Ok(ResolvedPath {
                    path_ref: FInstance::new(mount, node),
                    display_path,
                })
            }
            Err(error) => Err(error),
        }
    }

    // AGENT: resolve a mountpoint without crossing its final visible attachment
    // so repeated attaches stack on the same parent inode.
    fn mountpoint(&self, path: &str) -> Result<FInstance, &'static str> {
        let canonical = Self::canonicalize(path)?;
        if canonical == "/" {
            return Err("einval");
        }
        Ok(self.resolve_internal(&canonical, false)?.resolved.path_ref)
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
