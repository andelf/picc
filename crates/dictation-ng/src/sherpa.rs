//! Minimal safe wrapper around sherpa-onnx C API for Fun-ASR-Nano.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(dead_code)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use std::ffi::{CStr, CString};

pub struct Recognizer {
    ptr: *const SherpaOnnxOfflineRecognizer,
}

unsafe impl Send for Recognizer {}
unsafe impl Sync for Recognizer {}

impl Recognizer {
    pub fn new_funasr_nano(model_dir: &str) -> Result<Self, String> {
        let encoder_adaptor =
            CString::new(format!("{model_dir}/encoder_adaptor.int8.onnx")).unwrap();
        let embedding = CString::new(format!("{model_dir}/embedding.int8.onnx")).unwrap();
        let llm = CString::new(format!("{model_dir}/llm.int8.onnx")).unwrap();
        let tokenizer = CString::new(format!("{model_dir}/Qwen3-0.6B")).unwrap();
        let provider = CString::new("cpu").unwrap();
        let decoding_method = CString::new("greedy_search").unwrap();

        unsafe {
            let mut config: SherpaOnnxOfflineRecognizerConfig = std::mem::zeroed();

            config.model_config.funasr_nano.encoder_adaptor = encoder_adaptor.as_ptr();
            config.model_config.funasr_nano.embedding = embedding.as_ptr();
            config.model_config.funasr_nano.llm = llm.as_ptr();
            config.model_config.funasr_nano.tokenizer = tokenizer.as_ptr();
            config.model_config.num_threads = 2;
            config.model_config.debug = 0;
            config.model_config.provider = provider.as_ptr();
            config.feat_config.sample_rate = 16000;
            config.feat_config.feature_dim = 80;
            config.decoding_method = decoding_method.as_ptr();

            let recognizer = SherpaOnnxCreateOfflineRecognizer(&config);
            if recognizer.is_null() {
                return Err("Failed to create Fun-ASR-Nano recognizer".into());
            }
            Ok(Recognizer { ptr: recognizer })
        }
    }

    pub fn transcribe(&mut self, sample_rate: u32, samples: &[f32]) -> String {
        unsafe {
            let stream = SherpaOnnxCreateOfflineStream(self.ptr);
            SherpaOnnxAcceptWaveformOffline(
                stream,
                sample_rate as i32,
                samples.as_ptr(),
                samples.len() as i32,
            );
            SherpaOnnxDecodeOfflineStream(self.ptr, stream);
            let result_ptr = SherpaOnnxGetOfflineStreamResult(stream);
            let text = if !result_ptr.is_null() {
                let raw = (*result_ptr).text;
                if !raw.is_null() {
                    CStr::from_ptr(raw).to_string_lossy().into_owned()
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            SherpaOnnxDestroyOfflineRecognizerResult(result_ptr);
            SherpaOnnxDestroyOfflineStream(stream);
            text
        }
    }
}

impl Drop for Recognizer {
    fn drop(&mut self) {
        unsafe {
            SherpaOnnxDestroyOfflineRecognizer(self.ptr);
        }
    }
}
