<!-- AGENT: M9 MM source-first migration notes for the copied kernel-sim baseline. -->

# kernel-sim MM migration baseline

This directory currently contains a direct source-first copy of:

- `kernel-sim/src/kernel/mm/mod.rs`
- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/mm/alloc.rs`
- `kernel-sim/src/kernel/mm/bits.rs`
- `kernel-sim/src/kernel/mm/memory.rs`

The files are now registered through the migrated `kernel-qemu/src/mod.rs` tree
and compile in the `#![no_std]` QEMU crate. The current MM work is no longer a
plain source copy: anonymous resident pages are backed by QEMU `PgFrame`s and
Sv39 leaf PTEs, while higher-level VMA and syscall-facing semantics still
follow the `kernel-sim` source-first shape. The second eager stage now also
backs regular-file `MAP_PRIVATE` / `MAP_SHARED` VMAs with positioned I/O,
resident file metadata, first-write dirty tracking, and transactional unmap.

Remaining replacement and hardening work:

- `std::collections` imports with `alloc::collections` where applicable.
- `Vec`, `Box`, and related heap allocations behind a QEMU global allocator.
- `std::sync::Mutex` with a QEMU-side lock or irq-safe critical section.
- `std::mem` and `std::slice` paths with `core::mem` and `core::slice`.
- `CLK` timer references with the QEMU timer tick source.
- Host `FramePool` slot accounting with QEMU physical-memory initialization
  from linker symbols and the QEMU `virt` RAM range.
- Replace eager file population with lazy page-fault loading and report `SIGBUS`
  for accesses beyond the valid file object instead of retaining zero pages.
- Add one global file-page cache so independently created `MAP_SHARED` aliases
  observe writes immediately; define synchronization with external
  truncate/grow while mappings remain live.
- Add `msync`, the `mprotect` syscall surface, `MAP_FIXED_NOREPLACE`, and any
  additional mmap flags only with their complete Linux validation/lifecycle
  semantics. The current internal `protect()` helper is not an ABI endpoint.
- Extend checkpoint images with explicit file backing and stable reopen rules;
  until then any file-backed VMA deliberately returns `ENOTSUP` rather than
  restoring silently as anonymous memory.
- Implement fork-advice semantics as one complete feature: add `madvise`
  syscall dispatch, validate `MADV_DONTFORK` / `MADV_DOFORK`, split VMAs at
  advice-range boundaries, update the VMA policy, and cover fork inheritance
  with QEMU regressions. Do not restore a standalone `VM_DONTCOPY` bit before
  those producer and verification paths exist.

Semantic entries to preserve while replacing internals:

- `AddrSpace`, `VmRegion`, `VmMap`, `SharedPage`, and `FramePool`.
- `map_region()`, `unmap_range()`, `protect()`, `release_all_pages()`.
- `read_user_bytes()` and `write_user_bytes()` until the standard QEMU
  usercopy/read-write path replaces them.
- Current anonymous `mmap`, `munmap`, `brk`, COW, and frame-release observable
  behavior as the `kernel-sim` compatibility baseline.

Added and expected support files in this module:

- `sv39.rs` for QEMU/RISC-V page-table walk, map, unmap, translate, and
  permission bits.
- `usercopy.rs` for copy-in/copy-out over translated user pages.
