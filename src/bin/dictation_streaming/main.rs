mod recognizer;
mod resample;

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Parser)]
#[command(name = "dictation-streaming")]
struct Args {
    /// Path to model directory
    #[arg(long, default_value_t = default_model_dir())]
    model_dir: String,

    /// Model type: auto, transducer, paraformer
    #[arg(long, default_value = "auto")]
    model_type: String,

    /// Number of threads for inference
    #[arg(long, default_value_t = 4)]
    num_threads: i32,
}

fn default_model_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.local/share/picc/sherpa-onnx-streaming-zipformer-zh-xlarge-int8-2025-06-30")
}

fn pick_existing_file(model_dir: &Path, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .map(|name| model_dir.join(name))
        .find(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
}

fn detect_model_kind(
    model_dir: &Path,
    requested: &str,
) -> Result<recognizer::OnlineModelKind, String> {
    match requested {
        "transducer" => Ok(recognizer::OnlineModelKind::Transducer),
        "paraformer" => Ok(recognizer::OnlineModelKind::Paraformer),
        "auto" => {
            if pick_existing_file(model_dir, &["joiner.int8.onnx", "joiner.onnx"]).is_some() {
                Ok(recognizer::OnlineModelKind::Transducer)
            } else if pick_existing_file(model_dir, &["encoder.int8.onnx", "encoder.onnx"])
                .is_some()
                && pick_existing_file(model_dir, &["decoder.int8.onnx", "decoder.onnx"]).is_some()
            {
                Ok(recognizer::OnlineModelKind::Paraformer)
            } else {
                Err(format!(
                    "could not detect model type from {}",
                    model_dir.display()
                ))
            }
        }
        other => Err(format!(
            "unsupported --model-type: {other} (expected auto, transducer, paraformer)"
        )),
    }
}

fn build_online_config(model_dir: &Path, args: &Args) -> Result<recognizer::OnlineConfig, String> {
    let tokens = model_dir.join("tokens.txt");
    if !tokens.exists() {
        return Err(format!("missing tokens.txt in {}", model_dir.display()));
    }

    let model_kind = detect_model_kind(model_dir, &args.model_type)?;
    let encoder = pick_existing_file(model_dir, &["encoder.int8.onnx", "encoder.onnx"])
        .ok_or_else(|| format!("missing encoder model in {}", model_dir.display()))?;
    let decoder = pick_existing_file(model_dir, &["decoder.int8.onnx", "decoder.onnx"])
        .ok_or_else(|| format!("missing decoder model in {}", model_dir.display()))?;

    let joiner = match model_kind {
        recognizer::OnlineModelKind::Transducer => Some(
            pick_existing_file(model_dir, &["joiner.int8.onnx", "joiner.onnx"])
                .ok_or_else(|| format!("missing joiner model in {}", model_dir.display()))?,
        ),
        recognizer::OnlineModelKind::Paraformer => None,
    };

    Ok(recognizer::OnlineConfig {
        model_kind,
        encoder,
        decoder,
        joiner,
        tokens: tokens.to_string_lossy().into_owned(),
        num_threads: args.num_threads,
        ..Default::default()
    })
}

type TapBlock = RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>;

fn setup_audio_engine(
    sample_buf: Arc<Mutex<Vec<f32>>>,
) -> (Retained<AVAudioEngine>, u32, TapBlock) {
    unsafe {
        let engine = AVAudioEngine::new();
        let microphone = engine.inputNode();
        let format = microphone.outputFormatForBus(0);
        let native_rate = format.sampleRate() as u32;

        let tap_block: TapBlock = RcBlock::new(
            move |buffer: NonNull<AVAudioPCMBuffer>, _time: NonNull<AVAudioTime>| {
                let buf = buffer.as_ref();
                let float_data = buf.floatChannelData();
                let frame_length = buf.frameLength();
                if !float_data.is_null() && frame_length > 0 {
                    let channel0 = (*float_data).as_ptr();
                    let slice = std::slice::from_raw_parts(channel0, frame_length as usize);
                    if let Ok(mut samples) = sample_buf.lock() {
                        samples.extend_from_slice(slice);
                    }
                }
            },
        );

        microphone.installTapOnBus_bufferSize_format_block(
            0,
            4096,
            Some(&format),
            &*tap_block as *const _ as *mut _,
        );

        engine.prepare();
        engine
            .startAndReturnError()
            .expect("failed to start audio engine");

        (engine, native_rate, tap_block)
    }
}

fn main() {
    let args = Args::parse();
    let model_dir = PathBuf::from(&args.model_dir);

    if !model_dir.exists() {
        eprintln!("Model not found at: {}", model_dir.display());
        eprintln!("Download from: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models");
        std::process::exit(1);
    }

    let config = build_online_config(&model_dir, &args).unwrap_or_else(|err| {
        eprintln!("Invalid model directory: {err}");
        eprintln!("Download from: https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models");
        std::process::exit(1);
    });
    eprintln!(
        "Using {} model from {}",
        match config.model_kind {
            recognizer::OnlineModelKind::Transducer => "transducer",
            recognizer::OnlineModelKind::Paraformer => "paraformer",
        },
        model_dir.display()
    );
    let rec = recognizer::OnlineRecognizer::new(&config).expect("failed to create recognizer");

    // Audio buffer
    let sample_buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let running = Arc::new(AtomicBool::new(true));

    // Setup audio
    let (engine, native_rate, _tap_block) = setup_audio_engine(sample_buf.clone());
    eprintln!("Listening... (native rate: {native_rate}Hz, Ctrl+C to stop)");

    // Ctrl+C handler
    let running_clone = running.clone();
    ctrlc::set_handler(move || {
        running_clone.store(false, Ordering::SeqCst);
    })
    .expect("failed to set Ctrl+C handler");

    // Recognition loop (main thread)
    let mut last_text = String::new();
    while running.load(Ordering::SeqCst) {
        // Drain samples from audio buffer
        let samples = {
            let mut buf = sample_buf.lock().unwrap();
            if buf.is_empty() {
                drop(buf);
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            std::mem::take(&mut *buf)
        };

        // Resample to 16kHz if needed
        let samples_16k = if native_rate != 16000 {
            resample::resample(&samples, native_rate, 16000)
        } else {
            samples
        };

        // Feed to recognizer
        rec.accept_waveform(&samples_16k);

        // Decode and get partial result
        let text = rec.decode_and_get_text();
        if text != last_text && !text.is_empty() {
            eprint!("\r\x1b[2K{}", text);
            last_text = text.clone();
        }

        // Check endpoint
        if rec.is_endpoint() {
            if !last_text.is_empty() {
                println!("\r\x1b[2K{}", last_text);
                last_text.clear();
            }
            rec.reset();
        }
    }

    // Graceful shutdown: flush pending utterance if any
    if !last_text.is_empty() {
        rec.input_finished();
        let text = rec.decode_and_get_text();
        if !text.trim().is_empty() {
            println!("\r\x1b[2K{}", text);
        }
    }

    unsafe {
        let microphone = engine.inputNode();
        microphone.removeTapOnBus(0);
        engine.stop();
    }

    eprintln!("\nStopped.");
}
