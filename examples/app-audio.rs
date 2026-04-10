//! Capture audio output from a specific macOS application using ScreenCaptureKit.
//!
//! Usage: cargo run --example app-audio -- [app_name] [duration_secs]
//!   e.g. cargo run --example app-audio -- Safari 10

#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DeclaredClass, Message};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::*;

/// CoreAudioTypes AudioStreamBasicDescription
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    _reserved: u32,
}

const K_AUDIO_FORMAT_LINEAR_PCM: u32 = 0x6C70636D; // 'lpcm'
const K_AUDIO_FORMAT_FLAG_IS_FLOAT: u32 = 1 << 0;
const K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED: u32 = 1 << 5;

extern "C" {
    fn CMAudioFormatDescriptionGetStreamBasicDescription(
        desc: *const c_void,
    ) -> *const AudioStreamBasicDescription;
    fn CMSampleBufferGetFormatDescription(sbuf: *const c_void) -> *const c_void;
}

pub struct AudioHandlerIvars {
    samples_received: AtomicU64,
    total_bytes: AtomicU64,
    format_printed: AtomicBool,
    wav_writer: std::sync::Mutex<Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[name = "AudioHandler"]
    #[ivars = AudioHandlerIvars]
    pub struct AudioHandler;

    unsafe impl NSObjectProtocol for AudioHandler {}

    unsafe impl SCStreamOutput for AudioHandler {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            r#type: SCStreamOutputType,
        ) {
            if r#type != SCStreamOutputType::Audio {
                return;
            }

            // Print format info once
            if !self.ivars().format_printed.swap(true, Ordering::Relaxed) {
                let sbuf_ptr = sample_buffer as *const CMSampleBuffer as *const c_void;
                let fmt_desc = CMSampleBufferGetFormatDescription(sbuf_ptr);
                if !fmt_desc.is_null() {
                    let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(fmt_desc);
                    if !asbd.is_null() {
                        let asbd = &*asbd;
                        eprintln!("\n=== Audio Format ===");
                        eprintln!("  Sample rate:     {:.0} Hz", asbd.sample_rate);
                        eprintln!("  Channels:        {}", asbd.channels_per_frame);
                        eprintln!("  Bits/channel:    {}", asbd.bits_per_channel);
                        eprintln!("  Bytes/frame:     {}", asbd.bytes_per_frame);
                        eprintln!("  Bytes/packet:    {}", asbd.bytes_per_packet);
                        eprintln!("  Frames/packet:   {}", asbd.frames_per_packet);
                        eprintln!(
                            "  Format ID:       0x{:08x} ({})",
                            asbd.format_id,
                            std::str::from_utf8(&asbd.format_id.to_be_bytes()).unwrap_or("?")
                        );
                        eprintln!("  Format flags:    0x{:08x}", asbd.format_flags);
                        let is_float = asbd.format_flags & K_AUDIO_FORMAT_FLAG_IS_FLOAT != 0;
                        let is_non_interleaved =
                            asbd.format_flags & K_AUDIO_FORMAT_FLAG_IS_NON_INTERLEAVED != 0;
                        eprintln!("    Float:           {}", is_float);
                        eprintln!("    Non-interleaved: {}", is_non_interleaved);
                        eprintln!("====================\n");
                    }
                }
            }

            self.ivars()
                .samples_received
                .fetch_add(1, Ordering::Relaxed);

            let Some(block_buf) = sample_buffer.data_buffer() else {
                return;
            };

            let data_len = block_buf.data_length();
            if data_len == 0 {
                return;
            }

            let mut buf = vec![0u8; data_len];
            let status = block_buf.copy_data_bytes(
                0,
                data_len,
                core::ptr::NonNull::new(buf.as_mut_ptr() as *mut c_void).unwrap(),
            );
            if status != 0 {
                eprintln!("copy_data_bytes failed: {}", status);
                return;
            }

            self.ivars()
                .total_bytes
                .fetch_add(data_len as u64, Ordering::Relaxed);

            // Audio is non-interleaved: [L0,L1,...,Ln, R0,R1,...,Rn]
            // WAV needs interleaved: [L0,R0, L1,R1, ..., Ln,Rn]
            if let Ok(mut guard) = self.ivars().wav_writer.lock() {
                if let Some(writer) = guard.as_mut() {
                    let all_samples: &[f32] = unsafe {
                        std::slice::from_raw_parts(buf.as_ptr() as *const f32, buf.len() / 4)
                    };
                    let half = all_samples.len() / 2;
                    if half > 0 {
                        let left = &all_samples[..half];
                        let right = &all_samples[half..];
                        for i in 0..half {
                            let _ = writer.write_sample(left[i]);
                            let _ = writer.write_sample(right[i]);
                        }
                    }
                }
            }
        }
    }
);

impl AudioHandler {
    fn new(output_path: &str) -> Retained<Self> {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let writer =
            hound::WavWriter::create(output_path, spec).expect("failed to create wav writer");

        let this = Self::alloc().set_ivars(AudioHandlerIvars {
            samples_received: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            format_printed: AtomicBool::new(false),
            wav_writer: std::sync::Mutex::new(Some(writer)),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn finalize_wav(&self) {
        if let Ok(mut guard) = self.ivars().wav_writer.lock() {
            if let Some(writer) = guard.take() {
                let _ = writer.finalize();
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let app_name = args.get(1).map(|s| s.as_str()).unwrap_or("Safari");
    let duration_secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let output_file = format!("/tmp/app-audio-{}.wav", app_name.to_lowercase());

    println!(
        "Capturing audio from \"{}\" for {}s -> {}",
        app_name, duration_secs, output_file
    );

    let running = Arc::new(AtomicBool::new(true));
    setup_signal_handler(running.clone());

    // Get shareable content
    let content: Arc<std::sync::Mutex<Option<Retained<SCShareableContent>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let content_clone = content.clone();
    let done = Arc::new(AtomicBool::new(false));
    let done_clone = done.clone();

    let block = RcBlock::new(
        move |content_result: *mut SCShareableContent, error: *mut NSError| {
            if !error.is_null() {
                let err = unsafe { &*error };
                eprintln!("Error: {}", err.localizedDescription());
            } else if !content_result.is_null() {
                let c = unsafe { Retained::retain(content_result).unwrap() };
                *content_clone.lock().unwrap() = Some(c);
            }
            done_clone.store(true, Ordering::SeqCst);
        },
    );

    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&block);
    }

    while !done.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }

    let content = content
        .lock()
        .unwrap()
        .take()
        .expect("failed to get shareable content");

    // Find target app (match bundle ID first, then app name)
    let apps = unsafe { content.applications() };
    let mut target_app: Option<Retained<SCRunningApplication>> = None;
    let search = app_name.to_lowercase();
    for app in apps.iter() {
        let bundle = unsafe { app.bundleIdentifier() }.to_string().to_lowercase();
        if bundle.contains(&search) {
            let name = unsafe { app.applicationName() };
            println!("Found: {} ({})", name, bundle);
            target_app = Some(app.retain());
            break;
        }
    }
    if target_app.is_none() {
        for app in apps.iter() {
            let name = unsafe { app.applicationName() }.to_string();
            if name.to_lowercase().contains(&search) {
                let bundle = unsafe { app.bundleIdentifier() };
                println!("Found: {} ({})", name, bundle);
                target_app = Some(app.retain());
                break;
            }
        }
    }

    let target_app = target_app.unwrap_or_else(|| {
        println!("\nAvailable apps:");
        for app in apps.iter() {
            let name = unsafe { app.applicationName() };
            let bundle = unsafe { app.bundleIdentifier() };
            if !name.is_empty() {
                println!("  {} ({})", name, bundle);
            }
        }
        panic!("App \"{}\" not found", app_name);
    });

    let displays = unsafe { content.displays() };
    let display = displays.iter().next().expect("no display found");
    let app_array = NSArray::from_retained_slice(&[target_app]);
    let empty_windows: Retained<NSArray<SCWindow>> = NSArray::new();
    let filter = unsafe {
        SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
            SCContentFilter::alloc(),
            &display,
            &app_array,
            &empty_windows,
        )
    };

    let config = unsafe { SCStreamConfiguration::new() };
    unsafe {
        config.setCapturesAudio(true);
        config.setSampleRate(48000);
        config.setChannelCount(2);
        config.setWidth(2);
        config.setHeight(2);
    }

    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None)
    };

    let handler = AudioHandler::new(&output_file);
    let queue = DispatchQueue::new("audio-capture", None);
    unsafe {
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*handler),
                SCStreamOutputType::Audio,
                Some(&queue),
            )
            .expect("failed to add stream output");
    }

    let started = Arc::new(AtomicBool::new(false));
    let started_clone = started.clone();
    let start_block = RcBlock::new(move |error: *mut NSError| {
        if !error.is_null() {
            let err = unsafe { &*error };
            eprintln!("Start error: {}", err.localizedDescription());
        } else {
            println!("Capture started.");
        }
        started_clone.store(true, Ordering::SeqCst);
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&start_block)) };

    while !started.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }

    let start_time = std::time::Instant::now();
    while running.load(Ordering::Relaxed) && start_time.elapsed().as_secs() < duration_secs {
        std::thread::sleep(Duration::from_millis(200));
        let samples = handler.ivars().samples_received.load(Ordering::Relaxed);
        let bytes = handler.ivars().total_bytes.load(Ordering::Relaxed);
        eprint!(
            "\r  callbacks={}, bytes={:.1}KB, elapsed={:.1}s",
            samples,
            bytes as f64 / 1024.0,
            start_time.elapsed().as_secs_f64()
        );
    }
    eprintln!();

    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = stopped.clone();
    let stop_block = RcBlock::new(move |error: *mut NSError| {
        if !error.is_null() {
            let err = unsafe { &*error };
            eprintln!("Stop error: {}", err.localizedDescription());
        }
        stopped_clone.store(true, Ordering::SeqCst);
    });
    unsafe { stream.stopCaptureWithCompletionHandler(Some(&stop_block)) };

    while !stopped.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }

    handler.finalize_wav();

    let total_bytes = handler.ivars().total_bytes.load(Ordering::Relaxed);
    let audio_secs = total_bytes as f64 / (48000.0 * 2.0 * 4.0);
    println!(
        "Capture stopped. Saved {:.1}s audio to {}",
        audio_secs, output_file
    );
}

fn setup_signal_handler(running: Arc<AtomicBool>) {
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn handler(_: i32) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }

    unsafe {
        libc::signal(libc::SIGINT, handler as *const () as usize);
    }

    std::thread::spawn(move || loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            running.store(false, Ordering::SeqCst);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    });
}
