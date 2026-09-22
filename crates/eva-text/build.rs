//! Compiles `swift/eva_formatter.swift` (docs/PLAN.md §3: "el único Swift
//! del proyecto") into a dylib and links it into whatever binary ends up
//! depending on `eva-text` (`eva-worker`, `eva-cli`, `eva-eval`, and their
//! test binaries).
//!
//! Built as a **dynamic** library on purpose, not a static archive: Swift's
//! runtime (`libswiftCore`, `libswift_Concurrency`, ...) lives in the OS's
//! dyld shared cache on modern macOS, not as discrete `.a`/`.dylib` files a
//! generic linker can resolve `-lswiftCore`-style — only `swiftc` itself
//! knows how to link against it. Emitting a dylib lets `swiftc` do that
//! linking once, up front; from Cargo's `cc`-driven link step onward,
//! `libeva_formatter.dylib` is just an ordinary opaque shared library, no
//! different from linking against `libsqlite3` — no Swift-specific linker
//! knowledge required downstream.
//!
//! Requires the macOS 26 SDK (`FoundationModels` does not exist in older
//! SDKs) and a `swiftc` on `PATH` — both already required by this project's
//! `objc2`/AppKit bindings elsewhere, so this is not a new constraint.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "macos" {
        panic!(
            "eva-text's Apple Intelligence bridge only builds for macOS (target_os was '{target_os}') — \
             this workspace is macOS-only, see docs/PLAN.md §3"
        );
    }

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by Cargo"));
    let repo_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/eva-text is always two levels below the repo root");
    let swift_source = repo_root.join("swift").join("eva_formatter.swift");
    println!("cargo:rerun-if-changed={}", swift_source.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("set by Cargo"));
    let dylib_path = out_dir.join("libeva_formatter.dylib");

    let cargo_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let arch = match cargo_arch.as_str() {
        "aarch64" => "arm64",
        // "x86_64" already matches Swift's own spelling of the triple.
        other => other,
    };
    let target_triple = format!("{arch}-apple-macosx26.0");

    // An absolute `-install_name` pointing straight at OUT_DIR is not the
    // most portable choice in the abstract, but it is the correct one here:
    // `cargo build`/`test`/`run` always rebuild this script before relying
    // on a stale dylib (Cargo reruns it and relinks dependents whenever
    // `cargo:rustc-link-search` — emitted below — changes), and the shipped
    // `.app` bundle gets its own, separate `@executable_path`-relative
    // dylib via `packaging/build-app.sh`, which does not use this path at
    // all. Chasing full relocatability here would add rpath machinery this
    // single-target build has no use for.
    let status = Command::new("swiftc")
        .arg("-emit-library")
        .arg("-module-name")
        .arg("eva_formatter")
        .arg("-target")
        .arg(&target_triple)
        .arg("-Xlinker")
        .arg("-install_name")
        .arg("-Xlinker")
        .arg(&dylib_path)
        .arg("-o")
        .arg(&dylib_path)
        .arg(&swift_source)
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "no se pudo ejecutar swiftc ({e}) — instala las Command Line Tools de Xcode \
                 (`xcode-select --install`) con el SDK de macOS 26 o más reciente"
            )
        });

    if !status.success() {
        panic!("swiftc falló compilando {} (ver el error arriba)", swift_source.display());
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=dylib=eva_formatter");
}
