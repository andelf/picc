use std::io::{Read as _, Write as _};
use std::path::Path;

const SENSEVOICE_MODEL_DIR: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17";
const SENSEVOICE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17.tar.bz2";
const PARAKEET_DE_MODEL_DIR: &str = "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8";
const PARAKEET_DE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPaths {
    pub model: String,
    pub tokens: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransducerModelPaths {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelArchiveSpec<'a> {
    pub model_dir_name: &'a str,
    pub marker_filename: &'a str,
    pub archive_url: &'a str,
    pub log_prefix: &'a str,
    pub display_name: &'a str,
}

pub fn ensure_tar_bz2_model(base_dir: &Path, spec: ModelArchiveSpec<'_>) -> Result<String, String> {
    let model_dir = base_dir.join(spec.model_dir_name);
    let marker = model_dir.join(spec.marker_filename);
    if marker.exists() {
        return Ok(model_dir.to_string_lossy().into_owned());
    }

    eprintln!(
        "[{}] {} not found at {}",
        spec.log_prefix,
        spec.display_name,
        model_dir.display()
    );
    eprintln!(
        "[{}] first run — downloading model, this may take a few minutes...",
        spec.log_prefix
    );
    std::fs::create_dir_all(base_dir).map_err(|e| format!("create model directory: {e}"))?;

    let archive = base_dir.join(format!("{}.tar.bz2", spec.log_prefix));
    let resp = reqwest::blocking::Client::new()
        .get(spec.archive_url)
        .send()
        .map_err(|e| format!("download request failed: {e}"))?;
    let total = resp.content_length().unwrap_or(0);
    let total_mb = total as f64 / 1_048_576.0;
    let mut downloaded: u64 = 0;
    let mut file = std::fs::File::create(&archive).map_err(|e| format!("create archive: {e}"))?;
    let mut reader = resp;
    let mut buf = [0u8; 65_536];
    let start = std::time::Instant::now();
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("download read: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("archive write: {e}"))?;
        downloaded += n as u64;
        if total > 0 {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
            let mb = downloaded as f64 / 1_048_576.0;
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { mb / elapsed } else { 0.0 };
            let bar_len = 30;
            let filled = (bar_len as f64 * downloaded as f64 / total as f64) as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(bar_len - filled);
            eprint!(
                "\r[{}] {bar} {mb:.1}/{total_mb:.1} MB ({pct}%) {speed:.1} MB/s",
                spec.log_prefix
            );
        }
    }
    eprintln!();
    drop(file);

    eprint!("[{}] extracting...", spec.log_prefix);
    let status = std::process::Command::new("tar")
        .args([
            "xjf",
            &archive.to_string_lossy(),
            "-C",
            &base_dir.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("tar execute: {e}"))?;
    if !status.success() {
        return Err("tar extraction failed".into());
    }
    std::fs::remove_file(&archive).ok();
    eprintln!(" done");

    Ok(model_dir.to_string_lossy().into_owned())
}

pub fn resolve_repo_sensevoice_paths(
    base_dir: &Path,
    requested_model_dir: Option<&str>,
    model_file_name: &str,
) -> Result<ModelPaths, String> {
    let model_dir = requested_model_dir
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            let preferred = base_dir.join(SENSEVOICE_MODEL_DIR);
            if preferred.join(model_file_name).exists() {
                preferred.to_string_lossy().into_owned()
            } else {
                ensure_tar_bz2_model(
                    base_dir,
                    ModelArchiveSpec {
                        model_dir_name: SENSEVOICE_MODEL_DIR,
                        marker_filename: "model.int8.onnx",
                        archive_url: SENSEVOICE_URL,
                        log_prefix: "voice-correct",
                        display_name: "SenseVoice model",
                    },
                )
                .expect("failed to download SenseVoice model")
            }
        });

    let model_path = Path::new(&model_dir).join(model_file_name);
    if !model_path.exists() {
        return Err(format!(
            "SenseVoice model file not found: {}",
            model_path.display()
        ));
    }

    let tokens_path = Path::new(&model_dir).join("tokens.txt");
    if !tokens_path.exists() {
        return Err(format!(
            "SenseVoice tokens not found: {}",
            tokens_path.display()
        ));
    }

    Ok(ModelPaths {
        model: model_path.to_string_lossy().into_owned(),
        tokens: Some(tokens_path.to_string_lossy().into_owned()),
    })
}

pub fn resolve_repo_parakeet_de_paths(
    base_dir: &Path,
    requested_model_dir: Option<&str>,
) -> Result<TransducerModelPaths, String> {
    let model_dir = requested_model_dir
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            let preferred = base_dir.join(PARAKEET_DE_MODEL_DIR);
            if preferred.join("encoder.int8.onnx").exists() {
                preferred.to_string_lossy().into_owned()
            } else {
                ensure_tar_bz2_model(
                    base_dir,
                    ModelArchiveSpec {
                        model_dir_name: PARAKEET_DE_MODEL_DIR,
                        marker_filename: "encoder.int8.onnx",
                        archive_url: PARAKEET_DE_URL,
                        log_prefix: "voice-correct-de",
                        display_name: "Parakeet German-English model",
                    },
                )
                .expect("failed to download Parakeet model")
            }
        });

    let model_dir = Path::new(&model_dir);
    let encoder = pick_existing_file(model_dir, &["encoder.int8.onnx", "encoder.onnx"])?;
    let decoder = pick_existing_file(model_dir, &["decoder.int8.onnx", "decoder.onnx"])?;
    let joiner = pick_existing_file(model_dir, &["joiner.int8.onnx", "joiner.onnx"])?;
    let tokens = pick_existing_file(model_dir, &["tokens.txt"])?;

    Ok(TransducerModelPaths {
        encoder,
        decoder,
        joiner,
        tokens,
    })
}

const SILERO_VAD_FILENAME: &str = "silero_vad.onnx";
const SILERO_VAD_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx";

/// Resolve (and auto-download) the Silero VAD model path.
/// Returns the full path to `silero_vad.onnx` under `base_dir`.
pub fn resolve_silero_vad_model_path(base_dir: &Path) -> Result<String, String> {
    let model_path = base_dir.join(SILERO_VAD_FILENAME);
    if model_path.exists() {
        return Ok(model_path.to_string_lossy().into_owned());
    }

    eprintln!("[vad] silero_vad.onnx not found at {}", model_path.display());
    eprintln!("[vad] downloading Silero VAD model...");
    std::fs::create_dir_all(base_dir).map_err(|e| format!("create model directory: {e}"))?;

    let resp = reqwest::blocking::Client::new()
        .get(SILERO_VAD_URL)
        .send()
        .map_err(|e| format!("download request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| format!("download read: {e}"))?;
    let size_mb = bytes.len() as f64 / 1_048_576.0;
    std::fs::write(&model_path, &bytes).map_err(|e| format!("write model: {e}"))?;
    eprintln!("[vad] downloaded {size_mb:.1} MB → {}", model_path.display());

    Ok(model_path.to_string_lossy().into_owned())
}

fn pick_existing_file(model_dir: &Path, names: &[&str]) -> Result<String, String> {
    for name in names {
        let path = model_dir.join(name);
        if path.exists() {
            return Ok(path.to_string_lossy().into_owned());
        }
    }

    Err(format!(
        "Missing required model file in {} (expected one of: {})",
        model_dir.display(),
        names.join(", ")
    ))
}
