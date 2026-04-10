#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficialFixturePaths {
    pub model: String,
    pub tokens: String,
    pub lexicon: String,
    pub rule_fst: String,
    pub wave: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficialSenseVoiceHrConfig {
    pub language: String,
    pub use_itn: bool,
    pub num_threads: usize,
    pub provider: String,
    pub assets: OfficialFixturePaths,
}

pub fn official_test_hr_assets() -> OfficialFixturePaths {
    OfficialFixturePaths {
        model: "./sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/model.int8.onnx".to_string(),
        tokens: "./sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/tokens.txt".to_string(),
        lexicon: "assets/lexicon.txt".to_string(),
        rule_fst: "assets/common-mistakes.rules.txt".to_string(),
        wave: "./test-hr.wav".to_string(),
    }
}

pub fn official_non_streaming_sense_voice_hr_config() -> OfficialSenseVoiceHrConfig {
    OfficialSenseVoiceHrConfig {
        language: "auto".to_string(),
        use_itn: true,
        num_threads: 1,
        provider: "cpu".to_string(),
        assets: official_test_hr_assets(),
    }
}

pub fn official_test_hr_expected_text() -> &'static str {
    "下面是一个测试玄戒芯片湖南人弓头安装机载传感器"
}
