use std::{env, fs, path::PathBuf};
fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    let lib = root.join("temp/build/native/lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    for name in [
        // baresip's main under another name (patch-baresip.py).
        "ksip_entry",
        // The KSIP modules, built apart from baresip (src-native/CMakeLists.txt);
        // baresip's static module table names them. The one function the app
        // calls before baresip's main (ksip_stdio_init) is in there too.
        "ksip_modules",
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
    let played = ["busy.wav", "error.wav", "notfound.wav", "ring.wav", "ringback.wav"];
    let dir = root.join("temp/build/native/share/baresip");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut sounds = String::from("pub const SOUNDS: &[(&str, &[u8])] = &[\n");
    for name in played {
        // The ringback tone goes through the call's player, which takes 48 kHz
        // mono only, so it ships converted. The rest go through the alert
        // device, which takes baresip's own 8 kHz files as they are.
        let path = if name == "ringback.wav" {
            root.join("src-tauri/sounds/ringback.wav")
        } else {
            dir.join(name)
        };
        println!("cargo:rerun-if-changed={}", path.display());
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
    // Where the build came from, for the first line of the app log (app.rs):
    // the tag the version was cut at, the commits since it and the commit
    // (git describe, "v0.0.14-2-gbab294f"), with "-dirty" when the tree had
    // changes that were not committed. A checkout without git, or without
    // the tags, gives what it can (the commit alone, or "no git"). Looked at
    // again whenever the sources or the checked-out commit change, so that
    // it is never stale.
    let source = std::process::Command::new("git")
        .args(["describe", "--tags", "--long", "--dirty", "--always"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "no git".to_string());
    println!("cargo:rustc-env=KSIP_SOURCE={source}");
    for name in ["src-tauri/src", "src-web", "src-native", ".git/HEAD", ".git/index"] {
        println!("cargo:rerun-if-changed={}", root.join(name).display());
    }
    if let Ok(head) = fs::read_to_string(root.join(".git/HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed={}", root.join(".git").join(reference).display());
        }
    }
    tauri_build::build()
}
