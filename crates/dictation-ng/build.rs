use std::env;
use std::path::PathBuf;

fn main() {
    let home = env::var("HOME").unwrap();
    let sherpa_dir = format!("{home}/.local/share/picc/sherpa-onnx-v1.12.28");
    let lib_dir = format!("{sherpa_dir}/lib");
    let include_dir = format!("{sherpa_dir}/include");

    // Link sherpa-onnx static libraries
    println!("cargo:rustc-link-search=native={lib_dir}");
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

    // macOS frameworks
    for fw in &["Foundation", "CoreML", "Accelerate", "CoreFoundation"] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }

    // C++ standard library
    println!("cargo:rustc-link-lib=c++");

    // Generate bindings
    let header = format!("{include_dir}/sherpa-onnx/c-api/c-api.h");
    let bindings = bindgen::Builder::default()
        .header(&header)
        .allowlist_function("SherpaOnnx.*")
        .allowlist_type("SherpaOnnx.*")
        .clang_arg(format!("-I{include_dir}"))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}
