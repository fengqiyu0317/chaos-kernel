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
        let _cache = rehash_mount_cache(&self.mnt.entries.read().unwrap());
        Ok(resolved)
    }

    // AGENT: install a regular path-backed file used by both file handles and exec.
    pub fn install_file(
        &self,
        path: &str,
        data: Vec<u8>,
        executable: bool,
    ) -> Result<(), &'static str> {
        let resolved = self.lookup_path(path)?;
        self.file_nodes
            .write()
            .unwrap()
            .insert(resolved, Arc::new(FileNode::regular(data, executable)));
        Ok(())
    }

    // AGENT: keep existing exec-test helper as an executable regular file install.
    pub fn install_exec_file(&self, path: &str, data: Vec<u8>) -> Result<(), &'static str> {
        self.install_file(path, data, true)
    }

    // AGENT: read a byte range from consecutive cached blocks on the installed
    // QEMU block backend; callers pass the exact byte length to trim padding.
    pub fn read_cached_block_bytes(
        &self,
        dev: usize,
        first_block: usize,
        byte_len: usize,
    ) -> Result<Vec<u8>, &'static str> {
        if byte_len == 0 {
            return Ok(Vec::new());
        }
        let blocks = (byte_len + BLOCK_CACHE_BLOCK_SIZE - 1) / BLOCK_CACHE_BLOCK_SIZE;
        let mut out = Vec::with_capacity(blocks * BLOCK_CACHE_BLOCK_SIZE);
        for offset in 0..blocks {
            let block = first_block.checked_add(offset).ok_or("eio")?;
            let bytes = self
                .cache
                .read_block_cached(self.block_device.as_ref(), dev, block)?;
            out.extend_from_slice(&bytes);
        }
        out.truncate(byte_len);
        Ok(out)
    }

    // AGENT: install one executable file from the QEMU block backend through
    // BlockCache, preserving the existing path-backed exec data source.
    pub fn install_exec_file_from_cached_blocks(
        &self,
        path: &str,
        dev: usize,
        first_block: usize,
        byte_len: usize,
    ) -> Result<(), &'static str> {
        let data = self.read_cached_block_bytes(dev, first_block, byte_len)?;
        self.install_exec_file(path, data)
    }

    // AGENT: seed /bin/init from block 0 of the root block device when boot code
    // links a non-empty image; an empty image keeps today's carrier-only boot.
    pub fn install_root_init_from_block_device(&self) -> Result<bool, &'static str> {
        let byte_len = self.block_device.byte_len();
        if byte_len == 0 {
            return Ok(false);
        }
        self.install_exec_file_from_cached_blocks("/bin/init", ROOT_BLOCK_DEVICE, 0, byte_len)?;
        Ok(true)
    }

    // AGENT: install a directory node so exec can distinguish directories.
    pub fn install_directory(&self, path: &str) -> Result<(), &'static str> {
        let resolved = self.lookup_path(path)?;
        self.file_nodes
            .write()
            .unwrap()
            .insert(resolved, Arc::new(FileNode::directory()));
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
        let mut contents = node.data.lock().unwrap();
        let end = offset.checked_add(data.len()).ok_or("efbig")?;
        if end > contents.len() {
            contents.resize(end, 0);
        }
        contents[offset..end].copy_from_slice(data);
        Ok(data.len())
    }
}
