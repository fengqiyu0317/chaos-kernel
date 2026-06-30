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
plain source copy: anonymous/file-backed resident pages are backed by QEMU
`PgFrame`s and Sv39 leaf PTEs, while higher-level VMA and syscall-facing
semantics still follow the `kernel-sim` source-first shape.

Remaining replacement and hardening work:

- `std::collections` imports with `alloc::collections` where applicable.
- `Vec`, `Box`, and related heap allocations behind a QEMU global allocator.
- Keep `Arc<Mutex<Vec<u8>>>` out of resident page storage; file-backed mmap
  still uses it only as file-node backing metadata until the file layer is fully
  migrated.
- `std::sync::Mutex` with a QEMU-side lock or irq-safe critical section.
- `std::mem` and `std::slice` paths with `core::mem` and `core::slice`.
- `CLK` timer references with the QEMU timer tick source.
- Host `FramePool` slot accounting with QEMU physical-memory initialization
  from linker symbols and the QEMU `virt` RAM range.

Semantic entries to preserve while replacing internals:

- `AddrSpace`, `VmRegion`, `VmMap`, `PageTableEntry`, and `FramePool`.
- `map_region()`, `unmap_range()`, `protect()`, `release_all_pages()`.
- `read_user_bytes()`, `write_user_bytes()`, and prefix-access checks.
- Current `mmap`, `munmap`, `brk`, COW, and frame-release observable behavior
  as the `kernel-sim` compatibility baseline.

Added and expected support files in this module:

- `sv39.rs` for QEMU/RISC-V page-table walk, map, unmap, translate, and
  permission bits.
- `usercopy.rs` for copy-in/copy-out over translated user pages.
