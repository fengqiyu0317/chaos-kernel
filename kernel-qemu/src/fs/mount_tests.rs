// AGENT: Keep mount-table regressions next to mount.rs after splitting the
// former mixed mount_io_disk module.
use super::MountTable;

// AGENT: Keep the QEMU boot selftest aggregator in the mount test module.
pub fn run_all() {
    bind_updates_existing_prefix_and_normalizes();
    bind_ignores_invalid_mount_points();
    mount_and_umount_report_invalid_requests();
    prefix_match_respects_directory_boundary();
    resolve_does_not_remap_the_matched_suffix();
}

// AGENT: repeated binds for the same mount point update the device instead of
// leaving ambiguous duplicate entries.
#[cfg_attr(test, test)]
fn bind_updates_existing_prefix_and_normalizes() {
    let mt = MountTable::new();

    mt.bind("/mnt/", "dev0");
    mt.bind("/mnt", "dev1");

    assert_eq!(mt.mount_count(), 1);
    assert!(mt.has_prefix("/mnt/"));
    assert_eq!(mt.resolve("/mnt").unwrap(), "dev1:/");
    assert_eq!(mt.resolve("/mnt/file").unwrap(), "dev1:/file");
    assert!(mt.unmount("/mnt/"));
    assert_eq!(mt.mount_count(), 0);
}

// AGENT: bind has no error channel, so malformed inputs are ignored instead
// of becoming unreachable mount-table entries.
#[cfg_attr(test, test)]
fn bind_ignores_invalid_mount_points() {
    let mt = MountTable::new();

    mt.bind("", "dev0");
    mt.bind("/", "dev0");
    mt.bind("mnt", "dev0");
    mt.bind("/valid", "");

    assert_eq!(mt.mount_count(), 0);
    assert!(!mt.has_prefix("/"));
    assert!(!mt.has_prefix("mnt"));
}

// AGENT: syscall-facing mount helpers must distinguish invalid or absent
// requests from successful table mutation.
#[cfg_attr(test, test)]
fn mount_and_umount_report_invalid_requests() {
    let mt = MountTable::new();

    assert_eq!(mt.mount("/", "dev0"), Err("einval"));
    assert_eq!(mt.mount("relative", "dev0"), Err("einval"));
    assert_eq!(mt.mount("/mnt", ""), Err("einval"));
    assert_eq!(mt.umount("/mnt"), Err("einval"));

    assert_eq!(mt.mount("/mnt/", "dev0"), Ok(()));
    assert_eq!(mt.umount("/mnt"), Ok(()));
    assert_eq!(mt.umount("/mnt"), Err("einval"));
}

// AGENT: mount resolution must only match complete path components.
#[cfg_attr(test, test)]
fn prefix_match_respects_directory_boundary() {
    let mt = MountTable::new();

    mt.bind("/mnt", "dev0");

    assert_eq!(mt.resolve("/mnt/file").unwrap(), "dev0:/file");
    assert_eq!(mt.resolve("/mnt2/file").unwrap(), "/mnt2/file");
    assert!(mt.find_mount("/mnt2/file").is_none());
}

// AGENT: after the longest mount prefix is chosen, its suffix stays inside
// that target instead of being resolved against unrelated mount entries.
#[cfg_attr(test, test)]
fn resolve_does_not_remap_the_matched_suffix() {
    let mt = MountTable::new();

    mt.bind("/mnt", "dev0");
    mt.bind("/x", "dev1");

    assert_eq!(mt.resolve("/mnt/x/file").unwrap(), "dev0:/x/file");

    mt.bind("/mnt/x", "dev2");

    assert_eq!(mt.resolve("/mnt/x/file").unwrap(), "dev2:/file");
}
