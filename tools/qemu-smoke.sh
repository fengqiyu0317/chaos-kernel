#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v qemu-system-riscv64 >/dev/null 2>&1; then
    echo "qemu-system-riscv64 not found" >&2
    exit 127
fi

cd "$ROOT/kernel-qemu"
# AGENT: keep boot diagnostics out of ordinary images and opt into them only
# for this smoke-validation build.
if [[ "${KERNEL_QEMU_SKIP_BUILD:-0}" != "1" ]]; then
    cargo build --release --features qemu-boot-smoke
fi

KERNEL="$ROOT/kernel-qemu/target/riscv64gc-unknown-none-elf/release/kernel-qemu"
LOG="$(mktemp)"
DISK_IMAGE="$(mktemp)"
trap 'rm -f "$LOG" "$DISK_IMAGE"' EXIT
truncate -s 4M "$DISK_IMAGE"

timeout 15s qemu-system-riscv64 \
    -machine virt \
    -m 128M \
    -nographic \
    -bios default \
    -kernel "$KERNEL" \
    -drive "file=$DISK_IMAGE,format=raw,if=none,id=rootdisk" \
    -device virtio-blk-device,drive=rootdisk 2>&1 | tee "$LOG"

grep -F "[kernel-qemu] boot" "$LOG"
grep -F "[kernel-qemu] virtio-blk ready" "$LOG"
# AGENT: keep the ordinary boot gate sensitive to fixed-arena regressions and
# page leaks in the post-bootstrap global allocator.
grep -F "[kernel-qemu] dynamic heap ready" "$LOG"
grep -F "[kernel-qemu] heap alloc smoke" "$LOG"
grep -F "[kernel-qemu] heap reclaim smoke passed" "$LOG"
grep -F "[kernel-qemu] timer tick observed" "$LOG"
grep -F "[kernel-qemu] timer wheel target observed" "$LOG"
# AGENT: ordinary boot must now install and run the embedded RISC-V init, cross
# the user write ecall, and terminate through the migrated init-exit policy.
grep -F "[kernel-qemu] installed embedded /bin/init" "$LOG"
grep -F "[kernel-qemu] CPU0 scheduler start" "$LOG"
grep -F "[init] userspace /bin/init reached" "$LOG"
# AGENT: require a real U-mode mkdirat ecall before openat creates its child.
grep -F "[init] mkdirat round-trip passed" "$LOG"
# AGENT: require the real U-mode openat -> regular-file write round trip.
grep -F "[init] openat round-trip passed" "$LOG"
# AGENT: require real U-mode dup plus source-close descriptor survival.
grep -F "[init] dup round-trip passed" "$LOG"
# AGENT: require real U-mode fstat/newfstatat copyout before descriptor teardown.
grep -F "[init] stat round-trip passed" "$LOG"
# AGENT: require real U-mode pipe2 fd copyout plus write/read/close traversal.
grep -F "[init] pipe2 round-trip passed" "$LOG"
# AGENT: require the real U-mode close ecall to release the surviving dup fd.
grep -F "[init] close round-trip passed" "$LOG"
grep -F "[kernel-qemu] init process exited" "$LOG"
