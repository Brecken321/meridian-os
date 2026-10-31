//! Kernel build script: passes the linker script, generates the 256 interrupt
//! entry stubs and records the git revision for the version banner.

use std::env;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Exception vectors for which the CPU pushes an error code.
fn has_error_code(vector: u32) -> bool {
    matches!(vector, 8 | 10 | 11 | 12 | 13 | 14 | 17 | 21 | 29 | 30)
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    println!("cargo:rustc-link-arg-bins=-T{}", manifest.join("linker.ld").display());
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=build.rs");

    // Every stub pushes a dummy error code when the CPU does not, then the
    // vector number, so the common handler always sees the same frame.
    let mut asm = String::new();
    asm.push_str(".section .text.isr_stubs,\"ax\"\n");
    for v in 0..256u32 {
        writeln!(asm, ".p2align 4\nisr_stub_{v}:").unwrap();
        if !has_error_code(v) {
            asm.push_str("    push 0\n");
        }
        writeln!(asm, "    push {v}\n    jmp isr_common").unwrap();
    }
    asm.push_str(".section .rodata.isr_stub_table,\"a\"\n.p2align 3\n.global isr_stub_table\nisr_stub_table:\n");
    for v in 0..256u32 {
        writeln!(asm, "    .quad isr_stub_{v}").unwrap();
    }
    asm.push_str(".text\n");
    fs::write(out.join("isr_stubs.s"), asm).unwrap();

    let hash = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .current_dir(&manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=MERIDIAN_GIT_HASH={hash}");
    let git_dir = manifest.join("../.git");
    if git_dir.join("HEAD").exists() {
        println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
        println!("cargo:rerun-if-changed={}", git_dir.join("index").display());
    }
}
