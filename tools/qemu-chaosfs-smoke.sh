#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v qemu-system-riscv64 >/dev/null 2>&1; then
    echo "qemu-system-riscv64 not found" >&2
    exit 127
fi

cd "$ROOT/kernel-qemu"
# AGENT: build a dedicated filesystem-recovery image without conflating it with
# the raw-sector transport smoke or ordinary boot policy.
if [[ "${KERNEL_QEMU_SKIP_BUILD:-0}" != "1" ]]; then
    cargo build --release --features qemu-chaosfs-smoke
fi

KERNEL="$ROOT/kernel-qemu/target/riscv64gc-unknown-none-elf/release/kernel-qemu"
TMP_DIR="$(mktemp -d)"
DISK_IMAGE="$TMP_DIR/chaosfs.raw"
FIRST_LOG="$TMP_DIR/first-boot.log"
SECOND_LOG="$TMP_DIR/second-boot.log"
trap 'rm -rf "$TMP_DIR"' EXIT

truncate -s 4M "$DISK_IMAGE"

# AGENT: use the exact same block image for both boots so only the first may
# format and the second must recover the committed superblock and inode graph.
run_guest() {
    local log="$1"
    timeout 15s qemu-system-riscv64 \
        -machine virt \
        -m 128M \
        -nographic \
        -bios default \
        -kernel "$KERNEL" \
        -drive "file=$DISK_IMAGE,format=raw,if=none,id=rootdisk" \
        -device virtio-blk-device,drive=rootdisk 2>&1 | tee "$log"
}

run_guest "$FIRST_LOG"
grep -F "[kernel-qemu] virtio-blk ready" "$FIRST_LOG"
grep -F "[chaosfs-smoke] formatted source=virtio0" "$FIRST_LOG"
grep -F "[chaosfs-smoke] persisted file written bytes=" "$FIRST_LOG"

SUPER_MAGIC="$(dd if="$DISK_IMAGE" bs=1 count=7 status=none)"
if [[ "$SUPER_MAGIC" != "CHAOSFS" ]]; then
    echo "host did not observe the ChaosFs superblock" >&2
    exit 1
fi
echo "[host] ChaosFs superblock magic ok"

run_guest "$SECOND_LOG"
grep -F "[kernel-qemu] virtio-blk ready" "$SECOND_LOG"
grep -F "[chaosfs-smoke] recovered file bytes=" "$SECOND_LOG"
grep -F "[chaosfs-smoke] allocator preserved recovered file" "$SECOND_LOG"
if grep -Fq "[chaosfs-smoke] formatted source=virtio0" "$SECOND_LOG"; then
    echo "second boot formatted instead of mounting ChaosFs" >&2
    exit 1
fi
