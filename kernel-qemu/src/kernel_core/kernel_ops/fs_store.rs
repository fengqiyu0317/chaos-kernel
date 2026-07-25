use super::*;

impl Kernel {
    pub fn lookup_path(&self, path: &str) -> Result<String, &'static str> {
        if path.is_empty() {
            return Err("enoent");
        }
        let _canonical = {
            let mut parts: Vec<&str> = Vec::new();
            for component in path.split('/') {
                match component {
                    "" | "." => {}
                    ".." => {
                        parts.pop();
                    }
                    c => {
                        parts.push(c);
                    }
                }
            }
            let mut canonical = String::from("/");
            for (idx, part) in parts.iter().enumerate() {
                if idx > 0 {
                    canonical.push('/');
                }
                canonical.push_str(part);
            }
            canonical
        };
        let resolved = self.mnt.resolve(path)?;
        Ok(resolved)
    }

    // AGENT: split a resolved path into its parent directory path and child name.
    fn parent_dir_entry(path: &str) -> Option<(String, String)> {
        let path = path.trim_end_matches('/');
        if path.is_empty() || path == "/" {
            return None;
        }
        let slash = path.rfind('/')?;
        let name = &path[slash + 1..];
        if name.is_empty() {
            return None;
        }
        let parent = if slash == 0 { "/" } else { &path[..slash] };
        Some((parent.to_string(), name.to_string()))
    }

    // AGENT: if a real parent directory node exists, expose this path through
    // its directory-entry list used by FInstance::read_entry().
    pub(crate) fn note_path_in_parent_dir(&self, resolved_path: &str) -> Result<(), &'static str> {
        let Some((parent, name)) = Self::parent_dir_entry(resolved_path) else {
            return Ok(());
        };
        let parent_node = self.file_nodes.read().unwrap().get(&parent).cloned();
        if let Some(node) = parent_node {
            if node.kind == FileKind::Directory {
                node.add_dir_entry(&self.file_storage(), &name)?;
            }
        }
        Ok(())
    }

    // AGENT: perform lookup, O_EXCL validation, optional parent-directory
    // bookkeeping, and creation under one path-table write lock.
    pub(crate) fn open_regular_node(
        &self,
        resolved_path: &str,
        create: bool,
        exclusive: bool,
    ) -> Result<Arc<FileNode>, &'static str> {
        let mut nodes = self.file_nodes.write().unwrap();
        if let Some(node) = nodes.get(resolved_path).cloned() {
            if create && exclusive {
                return Err("eexist");
            }
            if node.kind != FileKind::Regular {
                return Err("eisdir");
            }
            return Ok(node);
        }
        if !create {
            return Err("enoent");
        }

        if let Some((parent, name)) = Self::parent_dir_entry(resolved_path) {
            if let Some(parent_node) = nodes.get(&parent).cloned() {
                if parent_node.kind != FileKind::Directory {
                    return Err("enotdir");
                }
                parent_node.add_dir_entry(&self.file_storage(), &name)?;
            }
        }

        let node = Arc::new(FileNode::regular(false));
        nodes.insert(resolved_path.to_string(), node.clone());
        Ok(node)
    }

    // AGENT: install a regular path-backed file used by both file instances and exec.
    pub fn install_file(
        &self,
        path: &str,
        data: Vec<u8>,
        executable: bool,
    ) -> Result<(), &'static str> {
        let resolved = self.lookup_path(path)?;
        let storage = self.file_storage();
        let node = Arc::new(FileNode::regular(executable));
        node.write_initial_bytes(&storage, &data)?;
        self.file_nodes
            .write()
            .unwrap()
            .insert(resolved.clone(), node);
        self.note_path_in_parent_dir(&resolved)?;
        Ok(())
    }

    // AGENT: keep existing exec-test helper as an executable regular file install.
    pub fn install_exec_file(&self, path: &str, data: Vec<u8>) -> Result<(), &'static str> {
        self.install_file(path, data, true)
    }

    // AGENT: install a directory node so exec can distinguish directories.
    pub fn install_directory(&self, path: &str) -> Result<(), &'static str> {
        let resolved = self.lookup_path(path)?;
        self.file_nodes
            .write()
            .unwrap()
            .insert(resolved.clone(), Arc::new(FileNode::directory()));
        self.note_path_in_parent_dir(&resolved)?;
        Ok(())
    }

    // AGENT: write into the shared path file contents visible to later exec.
    pub fn write_file_at(
        &self,
        path: &str,
        offset: usize,
        data: &[u8],
    ) -> Result<usize, &'static str> {
        let resolved = self.lookup_path(path)?;
        let node = self
            .file_nodes
            .read()
            .unwrap()
            .get(&resolved)
            .cloned()
            .ok_or("enoent")?;
        if node.kind == FileKind::Directory {
            return Err("eisdir");
        }
        node.write_bytes(&self.file_storage(), Some(offset), data)?;
        Ok(data.len())
    }
}
