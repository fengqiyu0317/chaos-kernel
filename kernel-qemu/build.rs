use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

const USER_TARGET: &str = "riscv64gc-unknown-none-elf";

// AGENT: build every first-stage userspace source as a separate fixed-address
// RISC-V ELF before rustc embeds the images into the no_std kernel.
fn main() {
    println!("cargo:rerun-if-changed=user/init.rs");
    println!("cargo:rerun-if-changed=user/exec_smoke.rs");
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
    let linker_script = manifest_dir.join("user/linker.ld");
    let rustc = env::var_os("RUSTC").expect("Cargo should provide RUSTC");

    build_user_binary(
        &rustc,
        &manifest_dir.join("user/init.rs"),
        &linker_script,
        &out_dir.join("root-init"),
        "kernel_qemu_init",
    );
    build_user_binary(
        &rustc,
        &manifest_dir.join("user/exec_smoke.rs"),
        &linker_script,
        &out_dir.join("exec-smoke"),
        "kernel_qemu_exec_smoke",
    );
}

// AGENT: keep the rustc/linker contract identical for init and its exec target
// while giving Cargo independently named ELF outputs to embed.
fn build_user_binary(
    rustc: &OsStr,
    source: &Path,
    linker_script: &Path,
    output: &Path,
    crate_name: &str,
) {
    let status = Command::new(rustc)
        .arg("--crate-name")
        .arg(crate_name)
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
        .expect("failed to invoke rustc for an embedded userspace ELF");
    assert!(status.success(), "failed to build embedded userspace ELF");
}
