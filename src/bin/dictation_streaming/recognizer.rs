use std::ffi::CString;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnlineModelKind {
    Transducer,
    Paraformer,
}

/// Safe wrapper around sherpa-onnx Online Recognizer (streaming).
pub struct OnlineRecognizer {
    recognizer: *const sherpa_rs::sherpa_rs_sys::SherpaOnnxOnlineRecognizer,
    stream: *const sherpa_rs::sherpa_rs_sys::SherpaOnnxOnlineStream,
    sample_rate: i32,
}

#[derive(Debug, Clone)]
pub struct OnlineConfig {
    pub model_kind: OnlineModelKind,
    pub encoder: String,
    pub decoder: String,
    pub joiner: Option<String>,
    pub tokens: String,
    pub num_threads: i32,
    pub provider: String,
    pub sample_rate: i32,
    pub feature_dim: i32,
    pub enable_endpoint: bool,
    pub rule1_min_trailing_silence: f32,
    pub rule2_min_trailing_silence: f32,
    pub rule3_min_utterance_length: f32,
}

impl Default for OnlineConfig {
    fn default() -> Self {
        Self {
            model_kind: OnlineModelKind::Transducer,
            encoder: String::new(),
            decoder: String::new(),
            joiner: None,
            tokens: String::new(),
            num_threads: 4,
            provider: "cpu".into(),
            sample_rate: 16000,
            feature_dim: 80,
            enable_endpoint: true,
            rule1_min_trailing_silence: 2.4,
            rule2_min_trailing_silence: 1.2,
            rule3_min_utterance_length: 20.0,
        }
    }
}

impl OnlineRecognizer {
    pub fn new(config: &OnlineConfig) -> Result<Self, String> {
        use sherpa_rs::sherpa_rs_sys::*;

        let encoder = CString::new(config.encoder.as_str()).unwrap();
        let decoder = CString::new(config.decoder.as_str()).unwrap();
        let joiner = config
            .joiner
            .as_ref()
            .map(|s| CString::new(s.as_str()).unwrap());
        let tokens = CString::new(config.tokens.as_str()).unwrap();
        let provider = CString::new(config.provider.as_str()).unwrap();
        let decoding_method = CString::new("greedy_search").unwrap();

        unsafe {
            let mut c_config: SherpaOnnxOnlineRecognizerConfig = std::mem::zeroed();

            c_config.feat_config.sample_rate = config.sample_rate;
            c_config.feat_config.feature_dim = config.feature_dim;

            match config.model_kind {
                OnlineModelKind::Transducer => {
                    c_config.model_config.transducer.encoder = encoder.as_ptr();
                    c_config.model_config.transducer.decoder = decoder.as_ptr();
                    c_config.model_config.transducer.joiner =
                        joiner.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
                }
                OnlineModelKind::Paraformer => {
                    c_config.model_config.paraformer.encoder = encoder.as_ptr();
                    c_config.model_config.paraformer.decoder = decoder.as_ptr();
                }
            }
            c_config.model_config.tokens = tokens.as_ptr();
            c_config.model_config.num_threads = config.num_threads;
            c_config.model_config.provider = provider.as_ptr();

            c_config.decoding_method = decoding_method.as_ptr();
            c_config.enable_endpoint = config.enable_endpoint as i32;
            c_config.rule1_min_trailing_silence = config.rule1_min_trailing_silence;
            c_config.rule2_min_trailing_silence = config.rule2_min_trailing_silence;
            c_config.rule3_min_utterance_length = config.rule3_min_utterance_length;

            let recognizer = SherpaOnnxCreateOnlineRecognizer(&c_config);
            if recognizer.is_null() {
                return Err("SherpaOnnxCreateOnlineRecognizer returned null".into());
            }

            let stream = SherpaOnnxCreateOnlineStream(recognizer);
            if stream.is_null() {
                SherpaOnnxDestroyOnlineRecognizer(recognizer);
                return Err("SherpaOnnxCreateOnlineStream returned null".into());
            }

            Ok(Self {
                recognizer,
                stream,
                sample_rate: config.sample_rate,
            })
        }
    }

    /// Feed audio samples (f32, 16kHz) to the recognizer.
    pub fn accept_waveform(&self, samples: &[f32]) {
        unsafe {
            sherpa_rs::sherpa_rs_sys::SherpaOnnxOnlineStreamAcceptWaveform(
                self.stream,
                self.sample_rate,
                samples.as_ptr(),
                samples.len() as i32,
            );
        }
    }

    /// Decode all available frames and return current partial text.
    pub fn decode_and_get_text(&self) -> String {
        unsafe {
            use sherpa_rs::sherpa_rs_sys::*;

            while SherpaOnnxIsOnlineStreamReady(self.recognizer, self.stream) != 0 {
                SherpaOnnxDecodeOnlineStream(self.recognizer, self.stream);
            }

            let result = SherpaOnnxGetOnlineStreamResult(self.recognizer, self.stream);
            if result.is_null() {
                return String::new();
            }
            let text = if (*result).text.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr((*result).text)
                    .to_string_lossy()
                    .into_owned()
            };
            SherpaOnnxDestroyOnlineRecognizerResult(result);
            text
        }
    }

    /// Check if an endpoint (silence after speech) was detected.
    pub fn is_endpoint(&self) -> bool {
        unsafe {
            sherpa_rs::sherpa_rs_sys::SherpaOnnxOnlineStreamIsEndpoint(self.recognizer, self.stream)
                != 0
        }
    }

    /// Reset the stream for the next utterance (call after endpoint).
    pub fn reset(&self) {
        unsafe {
            sherpa_rs::sherpa_rs_sys::SherpaOnnxOnlineStreamReset(self.recognizer, self.stream);
        }
    }

    /// Signal that no more audio will be sent.
    pub fn input_finished(&self) {
        unsafe {
            sherpa_rs::sherpa_rs_sys::SherpaOnnxOnlineStreamInputFinished(self.stream);
        }
    }
}

unsafe impl Send for OnlineRecognizer {}
unsafe impl Sync for OnlineRecognizer {}

impl Drop for OnlineRecognizer {
    fn drop(&mut self) {
        unsafe {
            use sherpa_rs::sherpa_rs_sys::*;
            SherpaOnnxDestroyOnlineStream(self.stream);
            SherpaOnnxDestroyOnlineRecognizer(self.recognizer);
        }
    }
}
