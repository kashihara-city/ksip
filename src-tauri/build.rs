use std::{env, fs, path::PathBuf};
fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let lib = root.join("temp/build/native/lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    for name in [
        "ksip_entry",
        "ksip_stdio",
        "ksip_trust",
        "libbaresip",
        // Codec libraries the baresip modules call into.
        "opus",
        "g722_static",
        // TLS and the crypto that SRTP needs.
        "ssl",
        "crypto",
        "re-static",
        "ksip_webrtc_audio",
        "clang_rt.builtins-x86_64",
    ] {
        assert!(
            lib.join(format!("{name}.lib")).exists(),
            "Run scripts/build/native.ps1 first: {name}"
        );
        println!("cargo:rustc-link-lib=static={name}");
        println!(
            "cargo:rerun-if-changed={}",
            lib.join(format!("{name}.lib")).display()
        );
    }
    // LibreSSL takes its entropy from the Windows CNG library.
    println!("cargo:rustc-link-lib=bcrypt");
    // Only the sounds baresip plays for us. The rest of its set is for its own
    // console menu, which KSIP never uses.
    let played = ["busy.wav", "callwaiting.wav", "error.wav", "notfound.wav", "ring.wav", "ringback.wav"];
    let dir = root.join("temp/build/native/share/baresip");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut sounds = String::from("pub const SOUNDS: &[(&str, &[u8])] = &[\n");
    for name in played {
        let path = dir.join(name);
        assert!(path.is_file(), "missing sound: {}", path.display());
        sounds.push_str(&format!("({name:?}, include_bytes!({path:?})),\n"));
    }
    sounds.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("sounds.rs"),
        sounds,
    )
    .unwrap();
    for name in [
        "qwave", "iphlpapi", "wsock32", "ws2_32", "dbghelp", "winmm", "gdi32", "crypt32",
        "strmiids", "ole32", "oleaut32", "uuid", "advapi32",
    ] {
        println!("cargo:rustc-link-lib={name}");
    }
    tauri_build::build()
}
