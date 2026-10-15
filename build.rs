use sha2::{Digest, Sha256};
use std::{env, fs, path::Path, process::Command};
fn main() {
    let package = Path::new("vendor/btls-sys/prebuilt/x86_64-pc-windows-msvc");
    let manifest = fs::read(package.join("manifest.json")).expect("native manifest");
    let value: serde_json::Value = serde_json::from_slice(&manifest).expect("manifest JSON");
    for (name, evidence) in value["files"].as_object().expect("manifest files") {
        assert!(
            matches!(
                name.as_str(),
                "lib/crypto.lib" | "lib/ssl.lib" | "bindings.rs" | "LICENSE-BORINGSSL"
            ),
            "unexpected native artifact"
        );
        println!("cargo:rerun-if-changed={}", package.join(name).display());
        let bytes = fs::read(package.join(name)).expect("native artifact");
        assert_eq!(
            Some(bytes.len() as u64),
            evidence["size"].as_u64(),
            "native artifact size"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            evidence["sha256"].as_str().unwrap(),
            "native artifact hash"
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        package.join("manifest.json").display()
    );
    println!("cargo:rerun-if-changed=build.rs");
    fn git(args: &[&str]) -> Option<String> {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().into())
    }
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) =
        git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r]))
    {
        println!("cargo:rerun-if-changed={reference}");
    }
    let revision = git(&["rev-parse", "HEAD"])
        .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let provenance = serde_json::json!({"version":env::var("CARGO_PKG_VERSION").unwrap(),"revision":revision,"target":env::var("TARGET").unwrap(),"scope":"build_time","verified":true,"manifest_sha256":format!("{:x}",Sha256::digest(&manifest)),"btls_sys_version":"0.5.6","files":value["files"]});
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("provenance.json"),
        serde_json::to_vec(&provenance).unwrap(),
    )
    .unwrap();
}
