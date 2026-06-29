#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v qemu-system-riscv64 >/dev/null 2>&1; then
    echo "qemu-system-riscv64 not found" >&2
    exit 127
fi

cd "$ROOT/kernel-qemu"
cargo build --release

KERNEL="$ROOT/kernel-qemu/target/riscv64gc-unknown-none-elf/release/kernel-qemu"
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

timeout 15s qemu-system-riscv64 \
    -machine virt \
    -nographic \
    -bios default \
    -kernel "$KERNEL" 2>&1 | tee "$LOG"

grep -F "[kernel-qemu] boot" "$LOG"
grep -F "[kernel-qemu] heap alloc smoke" "$LOG"
grep -F "[kernel-qemu] timer tick observed" "$LOG"
grep -F "[kernel-qemu] shutdown" "$LOG"
