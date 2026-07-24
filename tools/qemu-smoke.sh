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
cargo build --release --features qemu-boot-smoke

KERNEL="$ROOT/kernel-qemu/target/riscv64gc-unknown-none-elf/release/kernel-qemu"
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

timeout 15s qemu-system-riscv64 \
    -machine virt \
    -m 128M \
    -nographic \
    -bios default \
    -kernel "$KERNEL" 2>&1 | tee "$LOG"

grep -F "[kernel-qemu] boot" "$LOG"
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
grep -F "[kernel-qemu] init process exited" "$LOG"
