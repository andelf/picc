#[cfg(not(dictation_ng_stub))]
mod real;
#[cfg(not(dictation_ng_stub))]
mod sherpa;

#[cfg(dictation_ng_stub)]
fn main() {
    eprintln!("dictation-ng is available in this workspace, but the native sherpa bundle is not installed.");
    eprintln!("Set DICTATION_NG_SHERPA_DIR to a valid sherpa-onnx v1.12.28 bundle to enable the real binary.");
    eprintln!("The real implementation remains in crates/dictation-ng/src/real.rs.");
    std::process::exit(1);
}

#[cfg(not(dictation_ng_stub))]
fn main() {
    real::main();
}
