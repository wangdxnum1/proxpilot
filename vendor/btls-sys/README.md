# ProxPilot prebuilt btls-sys

This is a local build adapter for **btls-sys 0.5.6**, upstream commit
`4edbf5d716ba014384569ac5c631cea83827abfc`. `src/lib.rs` and `LICENSE-MIT`
are copied unchanged from that crate. The replacement build script has no
build dependencies: it copies pregenerated bindings and links bundled static
BoringSSL libraries. It never runs CMake, NASM, Clang or bindgen.

The package supports `x86_64-pc-windows-msvc` with `crt-static` and the current
empty btls-sys feature set. Debug and Release Rust builds share the native
Release libraries. Unsupported targets, dynamic CRT and unbundled feature
sets fail explicitly instead of compiling native libraries unexpectedly.

## Provenance and compatibility

`prebuilt/x86_64-pc-windows-msvc/manifest.json` records source, compiler, CRT,
features, sizes and SHA256 digests. BoringSSL's bundled license is beside it.
The initial libraries were built with **MSVC toolset 14.51 (Visual Studio 2026)**.
Use that toolset or a compatible newer linker/runtime. Older MSVC toolsets
are not verified. Windows SDK headers/libraries are part of the normal MSVC
C++ installation. No LLVM installation is needed by consumers.

These files are included directly in Git, not Git LFS, and need no download
script. Keep Cargo.lock and use `cargo build --locked`. To verify package integrity,
run `powershell -File vendor/btls-sys/verify.ps1` from the repository root.
The prebuilt files have Git text conversion disabled so their hashes survive
cloning with Windows autocrlf enabled.

## Updating the package (maintainers only)

Native dependency updates require the producer's CMake/NASM/libclang tools.
Build the upstream **locked** btls-sys version and matching feature set with
`crt-static`. Copy `out/build/Release/crypto.lib`, `ssl.lib` and `out/bindings.rs`
from the SAME successful build; keep the upstream Rust source and licenses.
Refresh this adapter's version and manifest together. Do not mix headers,
bindings, features, CRT settings or architectures from different builds.

After regeneration, verify SHA256, run a fresh build/test with external tools
removed from PATH, then run the browser ClientHello and live proxy checks.
Consumers only need Rust and the compatible MSVC C++ toolchain.
