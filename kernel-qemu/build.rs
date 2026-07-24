use std::env;
use std::path::PathBuf;
use std::process::Command;

const USER_TARGET: &str = "riscv64gc-unknown-none-elf";

// AGENT: build the first-stage userspace init as a separate fixed-address
// RISC-V ELF before rustc embeds it into the no_std kernel image.
fn main() {
    println!("cargo:rerun-if-changed=user/init.rs");
    println!("cargo:rerun-if-changed=user/linker.ld");
    println!("cargo:rerun-if-env-changed=RUSTC");

    let target = env::var("TARGET").expect("Cargo should provide TARGET");
    assert_eq!(
        target, USER_TARGET,
        "kernel-qemu and its embedded init require the RISC-V bare-metal target"
    );

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("missing CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("missing OUT_DIR"));
    let source = manifest_dir.join("user/init.rs");
    let linker_script = manifest_dir.join("user/linker.ld");
    let output = out_dir.join("root-init");
    let rustc = env::var_os("RUSTC").expect("Cargo should provide RUSTC");

    let status = Command::new(rustc)
        .arg("--crate-name")
        .arg("kernel_qemu_init")
        .arg("--crate-type")
        .arg("bin")
        .arg("--edition=2021")
        .arg("--target")
        .arg(USER_TARGET)
        .arg("-C")
        .arg("panic=abort")
        .arg("-C")
        .arg("relocation-model=static")
        .arg("-C")
        .arg("code-model=medium")
        .arg("-C")
        .arg(format!("link-arg=-T{}", linker_script.display()))
        .arg("-C")
        .arg("link-arg=--build-id=none")
        .arg("-C")
        .arg("strip=symbols")
        .arg("-O")
        .arg("-o")
        .arg(&output)
        .arg(&source)
        .status()
        .expect("failed to invoke rustc for the embedded init");
    assert!(status.success(), "failed to build the embedded init ELF");
}
