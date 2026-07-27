use super::*;

// AGENT: keep Kernel pathname helpers as thin semantic adapters over Vfs so
// storage, node-table, and mount ownership cannot diverge again.
impl Kernel {
    // AGENT: resolve an existing pathname to mount-plus-node identity while
    // retaining only a display path for logging and exec process naming.
    pub(crate) fn lookup_file_node(&self, path: &str) -> Result<ResolvedPath, &'static str> {
        self.vfs.resolve(path)
    }

    // AGENT: translate openat's creation policy into one VFS operation whose
    // selected FsInstance owns both the node namespace and storage backend.
    pub(crate) fn open_regular_node(
        &self,
        path: &str,
        creation: CreateDisposition,
    ) -> Result<ResolvedPath, &'static str> {
        let (create, exclusive) = match creation {
            CreateDisposition::OpenExisting => (false, false),
            CreateDisposition::CreateIfMissing => (true, false),
            CreateDisposition::CreateNew => (true, true),
        };
        self.vfs.open_regular(path, create, exclusive)
    }

    // AGENT: install or replace a regular path through the visible mount's
    // filesystem instance, including backend-correct initial data writes.
    pub fn install_file(
        &self,
        path: &str,
        data: Vec<u8>,
        executable: bool,
    ) -> Result<(), &'static str> {
        self.vfs.install_regular(path, &data, executable)?;
        Ok(())
    }

    // AGENT: keep the existing exec fixture helper on top of object-VFS install.
    pub fn install_exec_file(&self, path: &str, data: Vec<u8>) -> Result<(), &'static str> {
        self.install_file(path, data, true)
    }

    // AGENT: create one user-requested directory in the FsInstance selected by
    // its resolved parent mount, preserving strict mkdirat EEXIST behavior.
    pub(crate) fn create_directory(&self, path: &str) -> Result<(), &'static str> {
        self.vfs.create_directory(path)?;
        Ok(())
    }

    // AGENT: establish boot fixture directories idempotently without exposing
    // filesystem-local path-table keys outside Vfs.
    pub fn install_directory(&self, path: &str) -> Result<(), &'static str> {
        self.vfs.install_directory(path)?;
        Ok(())
    }

    // AGENT: write a resolved regular node only through the storage owned by its
    // PathRef mount's filesystem instance.
    pub fn write_file_at(
        &self,
        path: &str,
        offset: usize,
        data: &[u8],
    ) -> Result<usize, &'static str> {
        let resolved = self.lookup_file_node(path)?;
        if resolved.path_ref.node.kind == FileKind::Directory {
            return Err("eisdir");
        }
        resolved.path_ref.node.write_bytes(
            resolved.path_ref.mount.fs().storage(),
            Some(offset),
            data,
        )?;
        Ok(data.len())
    }
}
