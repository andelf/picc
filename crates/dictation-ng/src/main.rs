//! dictation-ng: Push-to-Talk Dictation with Fun-ASR-Nano
//!
//! Hold right Command key to dictate speech, release to type recognized text.
//! Uses Fun-ASR-Nano (offline LLM-based ASR) via sherpa-onnx for recognition.
//! Performs periodic recognition during recording for real-time preview.

mod sherpa;

use std::cell::Cell;
use std::io::{Read as _, Write as _};
use std::path::Path;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use clap::Parser;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSImage, NSMenu, NSMenuItem, NSStatusBar,
    NSStatusItem,
};
use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};
use objc2_core_foundation::{CFMachPort, CFRunLoop, kCFRunLoopCommonModes};
use objc2_core_graphics::{
    CGEvent, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType,
};
use objc2_foundation::{NSString, NSTimer};

use picc::input::type_text;

const NX_DEVICERCMDKEYMASK: u64 = 0x10;

static SHOULD_START: AtomicBool = AtomicBool::new(false);
static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

#[derive(Parser)]
#[command(about = "Push-to-Talk Dictation (Fun-ASR-Nano) — hold right Command to dictate")]
struct Args {
    /// Model directory (auto-downloads if not specified)
    #[arg(long)]
    model_dir: Option<String>,
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    _user_info: *mut std::ffi::c_void,
) -> *mut CGEvent {
    if event_type == CGEventType::FlagsChanged {
        let flags = CGEvent::flags(Some(event.as_ref()));
        let device_flags = flags.0 & 0xFFFF;
        let right_cmd_pressed = (device_flags & NX_DEVICERCMDKEYMASK) != 0;

        static WAS_DOWN: AtomicBool = AtomicBool::new(false);
        let was_down = WAS_DOWN.load(Ordering::Relaxed);

        if right_cmd_pressed && !was_down {
            WAS_DOWN.store(true, Ordering::Relaxed);
            SHOULD_START.store(true, Ordering::Relaxed);
        } else if !right_cmd_pressed && was_down {
            WAS_DOWN.store(false, Ordering::Relaxed);
            SHOULD_STOP.store(true, Ordering::Relaxed);
        }
    }
    event.as_ptr()
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DictationNGMenuDelegate"]
    #[derive(Debug, PartialEq)]
    struct MenuDelegate;

    #[allow(non_snake_case)]
    impl MenuDelegate {
        #[unsafe(method(quit:))]
        fn quit(&self, _sender: &AnyObject) {
            std::process::exit(0);
        }
    }
);

fn set_status_icon(item: &NSStatusItem, recording: bool, mtm: MainThreadMarker) {
    let name = if recording { "mic.fill" } else { "mic" };
    if let Some(button) = item.button(mtm) {
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            Some(&NSString::from_str("Dictation NG")),
        ) {
            image.setTemplate(true);
            button.setImage(Some(&image));
        }
    }
}

const MODEL_DIR_NAME: &str = "sherpa-onnx-funasr-nano-int8-2025-12-30";
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-funasr-nano-int8-2025-12-30.tar.bz2";

fn ensure_model(base_dir: &Path) -> String {
    let model_dir = base_dir.join(MODEL_DIR_NAME);
    let marker = model_dir.join("encoder_adaptor.int8.onnx");
    if marker.exists() {
        return model_dir.to_string_lossy().into_owned();
    }

    eprintln!(
        "[dictation-ng] Fun-ASR-Nano model not found at {}",
        model_dir.display()
    );
    eprintln!("[dictation-ng] first run — downloading model, this may take a few minutes...");
    std::fs::create_dir_all(base_dir).expect("failed to create model directory");

    let archive = base_dir.join("funasr-nano.tar.bz2");

    let resp = reqwest::blocking::Client::new()
        .get(MODEL_URL)
        .send()
        .expect("failed to download model — check your network connection");
    let total = resp.content_length().unwrap_or(0);
    let total_mb = total as f64 / 1_048_576.0;
    let mut downloaded: u64 = 0;
    let mut file = std::fs::File::create(&archive).expect("failed to create archive file");
    let mut reader = resp;
    let mut buf = [0u8; 65536];
    let start = std::time::Instant::now();
    loop {
        let n = reader
            .read(&mut buf)
            .expect("download error — check your network connection");
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).expect("write error");
        downloaded += n as u64;
        if total > 0 {
            let pct = (downloaded as f64 / total as f64 * 100.0) as u32;
            let mb = downloaded as f64 / 1_048_576.0;
            let elapsed = start.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 { mb / elapsed } else { 0.0 };
            let bar_len = 30;
            let filled = (bar_len as f64 * downloaded as f64 / total as f64) as usize;
            let bar: String = "\u{2588}".repeat(filled) + &"\u{2591}".repeat(bar_len - filled);
            eprint!("\r[dictation-ng] {bar} {mb:.1}/{total_mb:.1} MB ({pct}%) {speed:.1} MB/s");
        }
    }
    eprintln!();
    drop(file);

    eprint!("[dictation-ng] extracting...");
    let status = std::process::Command::new("tar")
        .args([
            "xjf",
            &archive.to_string_lossy(),
            "-C",
            &base_dir.to_string_lossy(),
        ])
        .status()
        .expect("failed to run tar");
    assert!(status.success(), "tar extraction failed");
    std::fs::remove_file(&archive).ok();

    eprintln!(" done");
    model_dir.to_string_lossy().into_owned()
}

fn downsample_to_16k(samples: &[f32], native_rate: u32) -> Vec<f32> {
    if native_rate == 16000 {
        return samples.to_vec();
    }
    let ratio = native_rate as f64 / 16000.0;
    let out_len = (samples.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let src = i as f64 * ratio;
            let idx = src as usize;
            let frac = src - idx as f64;
            let a = samples[idx];
            let b = samples.get(idx + 1).copied().unwrap_or(a);
            a + (b - a) * frac as f32
        })
        .collect()
}

fn main() {
    let args = Args::parse();

    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    // --- Model setup ---
    let model_dir = args.model_dir.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap();
        let base = Path::new(&home).join(".local/share/picc");
        ensure_model(&base)
    });

    eprintln!("[dictation-ng] loading Fun-ASR-Nano from {model_dir}...");
    let recognizer = sherpa::Recognizer::new_funasr_nano(&model_dir)
        .expect("failed to init Fun-ASR-Nano — check --model-dir");
    let recognizer = Arc::new(Mutex::new(recognizer));
    eprintln!("[dictation-ng] model loaded");

    // --- Audio engine ---
    let audio_engine = unsafe { AVAudioEngine::new() };

    // --- CGEventTap ---
    let event_mask: CGEventMask = 1 << CGEventType::FlagsChanged.0;
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::HIDEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            event_mask,
            Some(event_tap_callback),
            ptr::null_mut(),
        )
    }
    .expect("failed to create event tap — grant Accessibility permission");

    let run_loop_source = CFMachPort::new_run_loop_source(None, Some(&tap), 0)
        .expect("failed to create run loop source");
    unsafe {
        let run_loop = CFRunLoop::current().expect("no current run loop");
        run_loop.add_source(Some(&run_loop_source), kCFRunLoopCommonModes);
    }

    // --- Menubar ---
    let status_bar = NSStatusBar::systemStatusBar();
    let status_item = status_bar.statusItemWithLength(-1.0);
    set_status_icon(&status_item, false, mtm);

    let delegate: Retained<MenuDelegate> =
        unsafe { objc2::msg_send![MenuDelegate::alloc(mtm), init] };
    let menu = NSMenu::new(mtm);
    let quit_item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Quit"),
            Some(sel!(quit:)),
            &NSString::from_str("q"),
        )
    };
    unsafe { quit_item.setTarget(Some(&delegate)) };
    menu.addItem(&quit_item);
    status_item.setMenu(Some(&menu));

    eprintln!("[dictation-ng] ready — hold right Command to dictate, release to type");

    // --- State ---
    let is_recording = Cell::new(false);
    let accumulated_samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let native_sample_rate: Cell<u32> = Cell::new(16000);
    // Preview: last recognized text during recording
    let preview_text: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    // Track how many 16kHz samples the last preview covered
    let last_recognized_len: Cell<usize> = Cell::new(0);
    // Track how many raw samples the last preview covered (for tail slicing)
    let last_recognized_raw_len: Cell<usize> = Cell::new(0);

    let _tap = tap;
    let _run_loop_source = run_loop_source;

    // --- Timer ---
    let _timer = unsafe {
        let samples_ref = accumulated_samples.clone();
        let preview_ref = preview_text.clone();
        let recognizer_ref = recognizer.clone();

        let block = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            // --- START recording ---
            if SHOULD_START.swap(false, Ordering::Relaxed) && !is_recording.get() {
                eprintln!("[dictation-ng] recording...");
                is_recording.set(true);
                last_recognized_len.set(0);
                set_status_icon(&status_item, true, mtm);

                if let Ok(mut s) = samples_ref.lock() {
                    s.clear();
                }
                if let Ok(mut t) = preview_ref.lock() {
                    t.clear();
                }

                let microphone = audio_engine.inputNode();
                let format = microphone.outputFormatForBus(0);
                let sr = format.sampleRate() as u32;
                native_sample_rate.set(sr);

                let samples_tap = samples_ref.clone();
                let tap_block = RcBlock::new(
                    move |buffer: NonNull<AVAudioPCMBuffer>, _time: NonNull<AVAudioTime>| {
                        let buf = buffer.as_ref();
                        let float_data = buf.floatChannelData();
                        let frame_length = buf.frameLength();
                        if !float_data.is_null() && frame_length > 0 {
                            let channel0 = (*float_data).as_ptr();
                            let slice = std::slice::from_raw_parts(channel0, frame_length as usize);
                            if let Ok(mut samples) = samples_tap.lock() {
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

                audio_engine.prepare();
                if let Err(e) = audio_engine.startAndReturnError() {
                    eprintln!("[dictation-ng] audio engine error: {:?}", e);
                    is_recording.set(false);
                    set_status_icon(&status_item, false, mtm);
                    return;
                }
            }

            // --- Periodic preview during recording ---
            if is_recording.get() {
                let raw_len = samples_ref.lock().map(|s| s.len()).unwrap_or(0);
                let sr = native_sample_rate.get();
                let samples_16k_len = if sr == 16000 {
                    raw_len
                } else {
                    (raw_len as f64 / (sr as f64 / 16000.0)) as usize
                };

                // Recognize every ~1.5s of new audio (24000 samples at 16kHz)
                if samples_16k_len > last_recognized_len.get() + 24000 {
                    let raw_samples = samples_ref.lock().unwrap().clone();
                    let current_raw_len = raw_samples.len();
                    let samples_16k = downsample_to_16k(&raw_samples, sr);
                    drop(raw_samples);

                    if let Ok(mut rec) = recognizer_ref.try_lock() {
                        let t0 = std::time::Instant::now();
                        let text = rec.transcribe(16000, &samples_16k);
                        let elapsed = t0.elapsed();
                        if !text.is_empty() {
                            eprintln!(
                                "[dictation-ng] preview ({:.0}ms): {}",
                                elapsed.as_secs_f64() * 1000.0,
                                text
                            );
                            if let Ok(mut t) = preview_ref.lock() {
                                *t = text;
                            }
                        }
                        last_recognized_len.set(samples_16k_len);
                        last_recognized_raw_len.set(current_raw_len);
                    }
                }
            }

            // --- STOP recording ---
            if SHOULD_STOP.swap(false, Ordering::Relaxed) && is_recording.get() {
                let stop_t0 = std::time::Instant::now();
                eprintln!("[dictation-ng] stopped");
                is_recording.set(false);
                set_status_icon(&status_item, false, mtm);

                let microphone = audio_engine.inputNode();
                microphone.removeTapOnBus(0);
                audio_engine.stop();

                let raw_samples = samples_ref.lock().unwrap().clone();
                if !raw_samples.is_empty() {
                    let sr = native_sample_rate.get();
                    let prev_raw_len = last_recognized_raw_len.get();
                    let prev_text = preview_ref.lock().unwrap().clone();

                    // If preview covered most of the audio, only recognize the tail
                    let has_preview = !prev_text.is_empty() && prev_raw_len > 0;
                    let tail_raw_len = raw_samples.len() - prev_raw_len;
                    // Tail threshold: < 3s of raw audio at native rate
                    let use_tail = has_preview && tail_raw_len < (sr as usize * 3);

                    if use_tail {
                        let tail_secs = tail_raw_len as f64 / sr as f64;
                        if tail_raw_len == 0 {
                            // No new audio since last preview — use preview directly
                            eprintln!("[dictation-ng] using preview (no new audio)");
                            type_text(&prev_text);
                            let total_ms = stop_t0.elapsed().as_secs_f64() * 1000.0;
                            eprintln!(
                                "[dictation-ng] result: {} (inference=0ms, total={:.0}ms)",
                                prev_text, total_ms
                            );
                        } else {
                            // Recognize only the tail, prepend preview text
                            let tail_samples = &raw_samples[prev_raw_len..];
                            let tail_16k = downsample_to_16k(tail_samples, sr);
                            eprintln!(
                                "[dictation-ng] tail transcription ({:.1}s, preview covered {:.1}s)...",
                                tail_secs,
                                prev_raw_len as f64 / sr as f64
                            );
                            if let Ok(mut rec) = recognizer_ref.lock() {
                                let t0 = std::time::Instant::now();
                                let tail_text = rec.transcribe(16000, &tail_16k);
                                let inference_ms = t0.elapsed().as_secs_f64() * 1000.0;
                                let final_text = format!("{}{}", prev_text, tail_text);
                                type_text(&final_text);
                                let total_ms = stop_t0.elapsed().as_secs_f64() * 1000.0;
                                eprintln!(
                                    "[dictation-ng] result: {} (inference={:.0}ms, total={:.0}ms)",
                                    final_text, inference_ms, total_ms
                                );
                            }
                        }
                    } else {
                        // No usable preview or too much new audio — full recognition
                        let samples_16k = downsample_to_16k(&raw_samples, sr);
                        let audio_secs = samples_16k.len() as f64 / 16000.0;
                        eprintln!("[dictation-ng] full transcription ({:.1}s)...", audio_secs);

                        if let Ok(mut rec) = recognizer_ref.lock() {
                            let t0 = std::time::Instant::now();
                            let text = rec.transcribe(16000, &samples_16k);
                            let inference_ms = t0.elapsed().as_secs_f64() * 1000.0;
                            if !text.is_empty() {
                                type_text(&text);
                                let total_ms = stop_t0.elapsed().as_secs_f64() * 1000.0;
                                eprintln!(
                                    "[dictation-ng] result: {} (inference={:.0}ms, total={:.0}ms)",
                                    text, inference_ms, total_ms
                                );
                            }
                        }
                    }
                }
            }
        });

        NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &block)
    };

    app.run();
}
