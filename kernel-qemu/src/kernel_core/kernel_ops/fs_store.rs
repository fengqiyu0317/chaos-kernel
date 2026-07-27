use super::*;

impl Kernel {
    // AGENT: normalize one pathname and translate its longest mount prefix into
    // the canonical key used by the path-backed FileNode table.
    fn resolve_path_key(&self, path: &str) -> Result<String, &'static str> {
        if path.is_empty() {
            return Err("enoent");
        }
        self.mnt.resolve(path)
    }

    // AGENT: resolve an existing pathname to the shared inode-like FileNode
    // while retaining its canonical namespace key for open and exec state.
    pub(crate) fn lookup_file_node(&self, path: &str) -> Result<ResolvedFileNode, &'static str> {
        let resolved = self.resolve_path_key(path)?;
        let node = self
            .file_nodes
            .read()
            .unwrap()
            .get(&resolved)
            .cloned()
            .ok_or("enoent")?;
        Ok(ResolvedFileNode {
            path: resolved,
            node,
        })
    }

    // AGENT: recognize both the ordinary root and mount-backed namespace roots,
    // then split every non-root resolved key into its parent and child name.
    fn is_namespace_root(path: &str) -> bool {
        path == "/" || path.ends_with(":/")
    }

    // AGENT: split one non-root resolved key while preserving the slash in a
    // mount-backed root such as `lookupdev:/`.
    fn parent_dir_entry(path: &str) -> Option<(String, String)> {
        if Self::is_namespace_root(path) {
            return None;
        }
        let path = path.trim_end_matches('/');
        if path.is_empty() || path == "/" {
            return None;
        }
        let slash = path.rfind('/')?;
        let name = &path[slash + 1..];
        if name.is_empty() {
            return None;
        }
        let parent = if slash == 0 {
            "/"
        } else if path[..slash].ends_with(':') {
            &path[..=slash]
        } else {
            &path[..slash]
        };
        Some((parent.to_string(), name.to_string()))
    }

    // AGENT: require one existing directory parent before a non-root namespace
    // insertion, returning Linux-style errors for missing and non-directory parents.
    fn require_parent_dir(
        nodes: &BTreeMap<String, Arc<FileNode>>,
        resolved_path: &str,
    ) -> Result<(Arc<FileNode>, String), &'static str> {
        let (parent, name) = Self::parent_dir_entry(resolved_path).ok_or("enoent")?;
        let parent_node = nodes.get(&parent).cloned().ok_or("enoent")?;
        if parent_node.kind != FileKind::Directory {
            return Err("enotdir");
        }
        Ok((parent_node, name))
    }

    // AGENT: register a new non-root node only after its parent directory entry
    // succeeds, keeping the path table unchanged on parent validation failures.
    fn insert_new_child_locked(
        &self,
        nodes: &mut BTreeMap<String, Arc<FileNode>>,
        resolved_path: String,
        node: Arc<FileNode>,
    ) -> Result<(), &'static str> {
        if nodes.contains_key(&resolved_path) {
            return Err("eexist");
        }
        let (parent_node, name) = Self::require_parent_dir(nodes, &resolved_path)?;
        parent_node.add_dir_entry(&self.file_storage(), &name)?;
        nodes.insert(resolved_path, node);
        Ok(())
    }

    // AGENT: perform lookup, O_EXCL validation, strict parent-directory
    // bookkeeping, and creation under one path-table write lock; callers pass
    // the original pathname so an unnormalized key cannot bypass resolution.
    pub(crate) fn open_regular_node(
        &self,
        path: &str,
        creation: CreateDisposition,
    ) -> Result<ResolvedFileNode, &'static str> {
        let resolved = self.resolve_path_key(path)?;
        let mut nodes = self.file_nodes.write().unwrap();
        if let Some(node) = nodes.get(&resolved).cloned() {
            if creation == CreateDisposition::CreateNew {
                return Err("eexist");
            }
            if node.kind != FileKind::Regular {
                return Err("eisdir");
            }
            return Ok(ResolvedFileNode {
                path: resolved,
                node,
            });
        }
        if creation == CreateDisposition::OpenExisting {
            return Err("enoent");
        }

        let node = Arc::new(FileNode::regular(false));
        self.insert_new_child_locked(&mut nodes, resolved.clone(), node.clone())?;
        Ok(ResolvedFileNode {
            path: resolved,
            node,
        })
    }

    // AGENT: install or replace a regular path-backed file only below an
    // existing directory, keeping directory nodes from being overwritten.
    pub fn install_file(
        &self,
        path: &str,
        data: Vec<u8>,
        executable: bool,
    ) -> Result<(), &'static str> {
        let resolved = self.resolve_path_key(path)?;
        let storage = self.file_storage();
        let node = Arc::new(FileNode::regular(executable));
        node.write_initial_bytes(&storage, &data)?;
        let mut nodes = self.file_nodes.write().unwrap();
        if let Some(existing) = nodes.get(&resolved) {
            if existing.kind != FileKind::Regular {
                return Err("eisdir");
            }
            nodes.insert(resolved, node);
            return Ok(());
        }
        self.insert_new_child_locked(&mut nodes, resolved, node)?;
        Ok(())
    }

    // AGENT: keep existing exec-test helper as an executable regular file install.
    pub fn install_exec_file(&self, path: &str, data: Vec<u8>) -> Result<(), &'static str> {
        self.install_file(path, data, true)
    }

    // AGENT: create one user-requested directory atomically and reject every
    // pre-existing node, keeping mkdirat distinct from idempotent boot install.
    pub(crate) fn create_directory(&self, path: &str) -> Result<(), &'static str> {
        let resolved = self.resolve_path_key(path)?;
        let mut nodes = self.file_nodes.write().unwrap();
        self.insert_new_child_locked(&mut nodes, resolved, Arc::new(FileNode::directory()))
    }

    // AGENT: install directories parent-first while allowing an internal caller
    // to establish an ordinary or mount-backed namespace root idempotently.
    pub fn install_directory(&self, path: &str) -> Result<(), &'static str> {
        let resolved = self.resolve_path_key(path)?;
        let mut nodes = self.file_nodes.write().unwrap();
        if let Some(existing) = nodes.get(&resolved) {
            return if existing.kind == FileKind::Directory {
                Ok(())
            } else {
                Err("eexist")
            };
        }
        let node = Arc::new(FileNode::directory());
        if Self::is_namespace_root(&resolved) {
            nodes.insert(resolved, node);
        } else {
            self.insert_new_child_locked(&mut nodes, resolved, node)?;
        }
        Ok(())
    }

    // AGENT: write into the shared path file contents visible to later exec.
    pub fn write_file_at(
        &self,
        path: &str,
        offset: usize,
        data: &[u8],
    ) -> Result<usize, &'static str> {
        let resolved = self.lookup_file_node(path)?;
        if resolved.node.kind == FileKind::Directory {
            return Err("eisdir");
        }
        resolved
            .node
            .write_bytes(&self.file_storage(), Some(offset), data)?;
        Ok(data.len())
    }
}
