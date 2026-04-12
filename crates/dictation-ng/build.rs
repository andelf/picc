use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(dictation_ng_stub)");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let default_sherpa_dir = env::var("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/picc/sherpa-onnx-v1.12.28"))
        .unwrap_or_else(|_| PathBuf::from(".local/share/picc/sherpa-onnx-v1.12.28"));

    let sherpa_dir = env::var("DICTATION_NG_SHERPA_DIR")
        .map(PathBuf::from)
        .unwrap_or(default_sherpa_dir);
    let lib_dir = sherpa_dir.join("lib");
    let include_dir = sherpa_dir.join("include");
    let native_header = include_dir.join("sherpa-onnx/c-api/c-api.h");

    if native_header.exists() && lib_dir.is_dir() {
        configure_native_build(&include_dir, &lib_dir, &native_header);
        return;
    }

    let vendored_header = manifest_dir
        .join("../../vendor/sherpa-onnx/sherpa-onnx/c-api/c-api.h")
        .canonicalize()
        .ok();

    if let Some(vendored_header) = vendored_header {
        println!("cargo:rustc-cfg=dictation_ng_stub");
        println!(
            "cargo:warning=dictation-ng external sherpa bundle not found at {}; building stub binary instead",
            sherpa_dir.display()
        );
        println!(
            "cargo:warning=set DICTATION_NG_SHERPA_DIR to a valid sherpa-onnx v1.12.28 bundle to enable the real binary"
        );
        println!("cargo:rerun-if-env-changed=DICTATION_NG_SHERPA_DIR");
        println!("cargo:rerun-if-changed={}", vendored_header.display());
        return;
    }

    panic!(
        "dictation-ng could not find a native sherpa bundle or vendored c-api header; expected {} or vendor/sherpa-onnx",
        native_header.display()
    );
}

fn configure_native_build(include_dir: &Path, lib_dir: &Path, header: &Path) {
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for lib in &[
        "sherpa-onnx-c-api",
        "sherpa-onnx-core",
        "sherpa-onnx-cxx-api",
        "sherpa-onnx-fst",
        "sherpa-onnx-fstfar",
        "sherpa-onnx-kaldifst-core",
        "kaldi-decoder-core",
        "kaldi-native-fbank-core",
        "kissfft-float",
        "onnxruntime",
        "ssentencepiece_core",
    ] {
        println!("cargo:rustc-link-lib=static={lib}");
    }

    for fw in &["Foundation", "CoreML", "Accelerate", "CoreFoundation"] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }
    println!("cargo:rustc-link-lib=c++");

    let bindings = bindgen::Builder::default()
        .header(header.to_string_lossy())
        .allowlist_function("SherpaOnnx.*")
        .allowlist_type("SherpaOnnx.*")
        .clang_arg(format!("-I{}", include_dir.display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    println!("cargo:rerun-if-env-changed=DICTATION_NG_SHERPA_DIR");
    println!("cargo:rerun-if-changed={}", header.display());
}
