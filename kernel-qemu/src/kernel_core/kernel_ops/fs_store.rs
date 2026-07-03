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
    // its directory-entry list used by FHandle::read_entry().
    pub(crate) fn note_path_in_parent_dir(&self, resolved_path: &str) -> Result<(), &'static str> {
        let Some((parent, name)) = Self::parent_dir_entry(resolved_path) else {
            return Ok(());
        };
        let parent_node = self.file_nodes.read().unwrap().get(&parent).cloned();
        if let Some(node) = parent_node {
            if node.kind == FileKind::Directory {
                node.add_dir_entry(&name)?;
            }
        }
        Ok(())
    }

    // AGENT: install a regular path-backed file used by both file handles and exec.
    pub fn install_file(
        &self,
        path: &str,
        data: Vec<u8>,
        executable: bool,
    ) -> Result<(), &'static str> {
        let resolved = self.lookup_path(path)?;
        let node = Arc::new(FileNode::regular(data, executable));
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
        node.write_bytes(Some(offset), data)?;
        Ok(data.len())
    }
}
