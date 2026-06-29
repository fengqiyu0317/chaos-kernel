use super::*;

impl Kernel {
    // AGENT: route System V semaphore lookup through the kernel-owned IPC store.
    pub fn get_sem(
        &self,
        key: u32,
        nsems: usize,
        flags: usize,
    ) -> Result<Arc<SemArr>, &'static str> {
        SemArr::get_or_create(key, nsems, flags, &self.sem_store)
    }

    // AGENT: route shared-memory lookup through the kernel-owned IPC store.
    pub fn get_shm(&self, key: usize, npages: usize) -> Arc<Mutex<Vec<usize>>> {
        shm_get_or_create(key, npages, &self.shm_store)
    }
}
