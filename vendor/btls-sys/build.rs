use std::{env, fs, path::PathBuf};

fn main() {
    let target = env::var("TARGET").expect("Cargo TARGET is missing");
    assert_eq!(target, "x86_64-pc-windows-msvc",
        "ProxPilot's prebuilt BoringSSL package supports only Windows x64/MSVC; add a matching package before building another target");
    let features = env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    assert!(features.split(',').any(|f| f == "crt-static"),
        "Prebuilt BoringSSL uses the static CRT (/MT); build from the repository root with its .cargo/config.toml");
    for feature in ["FIPS", "MLKEM", "PREFIX_SYMBOLS", "UNDERSCORE_WILDCARDS"] {
        assert!(env::var_os(format!("CARGO_FEATURE_{feature}")).is_none(),
            "The bundled BoringSSL package was built without feature {feature}; regenerate matching libraries and bindings first");
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let package = root.join("prebuilt").join(&target);
    for relative in [
        "lib/crypto.lib",
        "lib/ssl.lib",
        "bindings.rs",
        "manifest.json",
    ] {
        let path = package.join(relative);
        println!("cargo:rerun-if-changed={}", path.display());
        assert!(path.is_file(), "Missing bundled BoringSSL artifact: {}; obtain the complete repository, including vendor/btls-sys/prebuilt", path.display());
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::copy(package.join("bindings.rs"), output.join("bindings.rs"))
        .expect("Unable to copy pregenerated BoringSSL bindings");
    println!(
        "cargo:rustc-link-search=native={}",
        package.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=crypto");
    println!("cargo:rustc-link-lib=static=ssl");
    println!("cargo:rustc-link-lib=advapi32");
}
