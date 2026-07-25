#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v qemu-system-riscv64 >/dev/null 2>&1; then
    echo "qemu-system-riscv64 not found" >&2
    exit 127
fi

cd "$ROOT/kernel-qemu"
# AGENT: allow validation environments with a prebuilt target artifact to skip
# only the build step while still exercising both QEMU boots and host checks.
if [[ "${KERNEL_QEMU_SKIP_BUILD:-0}" != "1" ]]; then
    cargo build --release --features qemu-virtio-blk-smoke
fi

KERNEL="$ROOT/kernel-qemu/target/riscv64gc-unknown-none-elf/release/kernel-qemu"
TMP_DIR="$(mktemp -d)"
DISK_IMAGE="$TMP_DIR/virtio-blk.raw"
FIRST_LOG="$TMP_DIR/first-boot.log"
SECOND_LOG="$TMP_DIR/second-boot.log"
INPUT_MAGIC="CHAOS-VIRTIO-INPUT-v1"
OUTPUT_MAGIC="CHAOS-VIRTIO-PERSIST-v1"
INPUT_BLOCK=8
OUTPUT_BLOCK=9
trap 'rm -rf "$TMP_DIR"' EXIT

truncate -s 4M "$DISK_IMAGE"
printf '%s' "$INPUT_MAGIC" |
    dd of="$DISK_IMAGE" bs=512 seek="$INPUT_BLOCK" conv=notrunc status=none

# AGENT: boot the same raw image with a real virtio-mmio block device and keep
# the command identical across both persistence passes.
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
grep -F "[virtio-blk-smoke] input magic ok block=$INPUT_BLOCK" "$FIRST_LOG"
grep -F "[virtio-blk-smoke] write flushed block=$OUTPUT_BLOCK" "$FIRST_LOG"

PERSISTED="$(
    dd if="$DISK_IMAGE" bs=1 skip="$((OUTPUT_BLOCK * 512))" \
        count="${#OUTPUT_MAGIC}" status=none
)"
if [[ "$PERSISTED" != "$OUTPUT_MAGIC" ]]; then
    echo "host did not observe guest virtio-blk write" >&2
    exit 1
fi
echo "[host] persisted sector magic ok"

run_guest "$SECOND_LOG"
grep -F "[virtio-blk-smoke] input magic ok block=$INPUT_BLOCK" "$SECOND_LOG"
grep -F "[virtio-blk-smoke] persisted magic ok block=$OUTPUT_BLOCK" "$SECOND_LOG"
grep -F "[virtio-blk-smoke] write flushed block=$OUTPUT_BLOCK" "$SECOND_LOG"
