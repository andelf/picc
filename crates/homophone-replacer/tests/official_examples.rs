use homophone_replacer::{
    official_non_streaming_sense_voice_hr_config, official_test_hr_assets,
    official_test_hr_expected_text,
};

#[test]
fn official_examples_share_the_same_hr_asset_bundle() {
    let assets = official_test_hr_assets();

    assert_eq!(
        assets.model,
        "./sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/model.int8.onnx"
    );
    assert_eq!(
        assets.tokens,
        "./sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2024-07-17/tokens.txt"
    );
    assert_eq!(assets.lexicon, "assets/lexicon.txt");
    assert_eq!(assets.rule_fst, "assets/common-mistakes.rules.txt");
    assert_eq!(assets.wave, "./test-hr.wav");
}

#[test]
fn official_non_streaming_config_uses_auto_language_and_itn() {
    let config = official_non_streaming_sense_voice_hr_config();

    assert_eq!(config.language, "auto");
    assert!(config.use_itn);
    assert_eq!(config.num_threads, 1);
    assert_eq!(config.provider, "cpu");
}

#[test]
fn official_non_streaming_config_points_at_the_shared_assets() {
    let config = official_non_streaming_sense_voice_hr_config();
    let assets = official_test_hr_assets();

    assert_eq!(config.assets, assets);
}

#[test]
fn official_test_hr_expected_text_matches_the_documented_phrase() {
    assert_eq!(
        official_test_hr_expected_text(),
        "下面是一个测试玄戒芯片湖南人弓头安装机载传感器"
    );
}
