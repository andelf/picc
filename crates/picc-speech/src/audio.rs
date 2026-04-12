use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc, Mutex,
};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_avf_audio::{
    AVAudioEngine, AVAudioEngineConfigurationChangeNotification, AVAudioPCMBuffer, AVAudioTime,
};
use objc2_foundation::{NSNotification, NSNotificationCenter};
use objc2_speech::SFSpeechAudioBufferRecognitionRequest;

type TapBlock = RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>;

pub struct AudioCaptureConfig {
    pub buffer_size: u32,
    pub use_none_format: bool,
    pub collect_gate: Option<&'static AtomicBool>,
    pub rms_out: Option<&'static AtomicU32>,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        Self {
            buffer_size: 4096,
            use_none_format: false,
            collect_gate: None,
            rms_out: None,
        }
    }
}

pub struct AudioEngineManager {
    engine: RefCell<Retained<AVAudioEngine>>,
    config_changed: Arc<AtomicBool>,
    _observer: Retained<AnyObject>,
    tap_block: RefCell<Option<TapBlock>>,
}

impl AudioEngineManager {
    pub fn new() -> Self {
        let config_changed = Arc::new(AtomicBool::new(false));
        let config_changed_for_block = config_changed.clone();
        let observer = unsafe {
            let notification_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
                config_changed_for_block.store(true, Ordering::Relaxed);
            });
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(AVAudioEngineConfigurationChangeNotification),
                None,
                None,
                &*notification_block,
            )
        };

        Self {
            engine: RefCell::new(unsafe { AVAudioEngine::new() }),
            config_changed,
            _observer: observer.into(),
            tap_block: RefCell::new(None),
        }
    }

    pub fn take_config_changed(&self) -> bool {
        self.config_changed.swap(false, Ordering::Relaxed)
    }

    pub fn recreate_engine(&self) {
        {
            let engine = self.engine.borrow();
            unsafe {
                let microphone = engine.inputNode();
                microphone.removeTapOnBus(0);
                engine.stop();
            }
        }
        self.tap_block.borrow_mut().take();
        self.engine.replace(unsafe { AVAudioEngine::new() });
    }

    pub fn start_sample_capture(
        &self,
        samples: Arc<Mutex<Vec<f32>>>,
        native_sample_rate: &Cell<u32>,
        config: AudioCaptureConfig,
    ) -> Result<(), String> {
        let engine = self.engine.borrow();
        let (microphone, format) = unsafe {
            let microphone = engine.inputNode();
            microphone.removeTapOnBus(0);
            let format = microphone.outputFormatForBus(0);
            (microphone, format)
        };
        native_sample_rate.set(unsafe { format.sampleRate() as u32 });

        let collect_gate = config.collect_gate;
        let rms_out = config.rms_out;
        let tap_block: TapBlock = RcBlock::new(
            move |buffer: NonNull<AVAudioPCMBuffer>, _time: NonNull<AVAudioTime>| {
                if let Some(gate) = collect_gate {
                    if !gate.load(Ordering::Relaxed) {
                        return;
                    }
                }

                let buf = unsafe { buffer.as_ref() };
                let float_data = unsafe { buf.floatChannelData() };
                let frame_length = unsafe { buf.frameLength() };
                if !float_data.is_null() && frame_length > 0 {
                    let channel0 = unsafe { (*float_data).as_ptr() };
                    let slice =
                        unsafe { std::slice::from_raw_parts(channel0, frame_length as usize) };
                    if let Some(rms_bits) = rms_out {
                        let sum_sq: f32 = slice.iter().map(|s| s * s).sum();
                        let rms = (sum_sq / slice.len() as f32).sqrt();
                        rms_bits.store(rms.to_bits(), Ordering::Relaxed);
                    }
                    if let Ok(mut locked) = samples.lock() {
                        locked.extend_from_slice(slice);
                    }
                }
            },
        );

        unsafe {
            microphone.installTapOnBus_bufferSize_format_block(
                0,
                config.buffer_size,
                if config.use_none_format {
                    None
                } else {
                    Some(&format)
                },
                &*tap_block as *const _ as *mut _,
            );
            engine.prepare();
        }
        if let Err(err) = unsafe { engine.startAndReturnError() } {
            unsafe { microphone.removeTapOnBus(0) };
            return Err(format!("{err:?}"));
        }
        self.tap_block.replace(Some(tap_block));
        Ok(())
    }

    pub fn start_request_capture(
        &self,
        request: Retained<SFSpeechAudioBufferRecognitionRequest>,
        config: AudioCaptureConfig,
    ) -> Result<(), String> {
        let engine = self.engine.borrow();
        let (microphone, format) = unsafe {
            let microphone = engine.inputNode();
            microphone.removeTapOnBus(0);
            let format = microphone.outputFormatForBus(0);
            (microphone, format)
        };
        let rms_out = config.rms_out;
        let tap_block: TapBlock = RcBlock::new(
            move |buffer: NonNull<AVAudioPCMBuffer>, _time: NonNull<AVAudioTime>| {
                let buf = unsafe { buffer.as_ref() };
                let float_data = unsafe { buf.floatChannelData() };
                let frame_length = unsafe { buf.frameLength() };
                if !float_data.is_null() && frame_length > 0 {
                    let channel0 = unsafe { (*float_data).as_ptr() };
                    let slice =
                        unsafe { std::slice::from_raw_parts(channel0, frame_length as usize) };
                    if let Some(rms_bits) = rms_out {
                        let sum_sq: f32 = slice.iter().map(|s| s * s).sum();
                        let rms = (sum_sq / slice.len() as f32).sqrt();
                        rms_bits.store(rms.to_bits(), Ordering::Relaxed);
                    }
                }
                unsafe { request.appendAudioPCMBuffer(buf) };
            },
        );

        unsafe {
            microphone.installTapOnBus_bufferSize_format_block(
                0,
                config.buffer_size,
                Some(&format),
                &*tap_block as *const _ as *mut _,
            );
            engine.prepare();
        }
        if let Err(err) = unsafe { engine.startAndReturnError() } {
            unsafe { microphone.removeTapOnBus(0) };
            return Err(format!("{err:?}"));
        }
        self.tap_block.replace(Some(tap_block));
        Ok(())
    }

    pub fn stop(&self) {
        let engine = self.engine.borrow();
        unsafe {
            let microphone = engine.inputNode();
            microphone.removeTapOnBus(0);
            engine.stop();
        }
        self.tap_block.borrow_mut().take();
    }

    pub fn stop_and_reset(&self) {
        let engine = self.engine.borrow();
        unsafe {
            let microphone = engine.inputNode();
            microphone.removeTapOnBus(0);
            engine.stop();
            engine.reset();
        }
        self.tap_block.borrow_mut().take();
    }
}

pub fn resample_linear(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if samples.is_empty() || source_rate == target_rate || target_rate == 0 {
        return samples.to_vec();
    }

    let ratio = source_rate as f64 / target_rate as f64;
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

#[cfg(test)]
mod tests {
    use super::resample_linear;

    #[test]
    fn identity_when_rates_match() {
        let input = vec![0.0, 1.0, 2.0, 3.0];
        assert_eq!(resample_linear(&input, 16_000, 16_000), input);
    }

    #[test]
    fn downsample_reduces_length() {
        let input = vec![0.0; 48_000];
        let out = resample_linear(&input, 48_000, 16_000);
        assert!((15_999..=16_001).contains(&out.len()));
    }
}
