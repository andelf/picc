use homophone_replacer::{
    match_phrases, pronounce_phrase, replace_text, Lexicon, ReplaceRuleSet, ReplacerConfig,
};

const SAMPLE_LEXICON: &str = "\
玄戒 xuan2 jie4
芯片 xin1 pian4
玄戒芯片 xuan2 jie4 xin1 pian4
湖南 hu2 nan2
湖南人 hu2 nan2 ren2
人 ren2
弓头 gong1 tou2
安装 an1 zhuang1
弓头安装 gong1 tou2 an1 zhuang1
机载 ji1 zai4
传感器 chuan2 gan3 qi4
机载传感器 ji1 zai4 chuan2 gan3 qi4
悬 xuan2
界 jie4
星 xing1
片 pian4
工 gong1
投 tou2
基 ji1
载 zai3
器 qi4
吗 ma
";

fn sample_lexicon() -> Lexicon {
    Lexicon::parse(SAMPLE_LEXICON)
}

fn sample_rules() -> ReplaceRuleSet {
    ReplaceRuleSet::from_pairs(&[
        ("xuan2jie4xin1pian4", "玄戒芯片"),
        ("xuan2jie4xing1pian4", "玄戒芯片"),
        ("fu2nan2ren2", "湖南人"),
        ("gong1tou2an1zhuang1", "弓头安装"),
        ("ji1zai3chuan2gan3qi4", "机载传感器"),
        ("ji1zai4chuan2gan3qi4", "机载传感器"),
    ])
}

fn sample_config() -> ReplacerConfig {
    ReplacerConfig { max_phrase_len: 10 }
}

#[test]
fn replaces_the_main_document_example() {
    let actual = replace_text(
        &sample_lexicon(),
        &sample_rules(),
        &sample_config(),
        "下面是一个测试悬界芯片湖南人工投安装基载传感器",
    );
    assert_eq!(actual, "下面是一个测试玄戒芯片湖南人弓头安装机载传感器");
}

#[test]
fn does_not_replace_when_full_pronunciation_does_not_match() {
    let rules = ReplaceRuleSet::from_pairs(&[("xuan2jie4xin1pian4", "玄戒芯片")]);
    let actual = replace_text(&sample_lexicon(), &rules, &sample_config(), "悬界星");
    assert_eq!(actual, "悬界星");
}

#[test]
fn supports_multiple_misrecognitions_for_the_same_target() {
    let rules = sample_rules();
    let config = sample_config();
    let lexicon = sample_lexicon();

    let actual_a = replace_text(&lexicon, &rules, &config, "悬界芯片");
    let actual_b = replace_text(&lexicon, &rules, &config, "悬界星片");

    assert_eq!(actual_a, "玄戒芯片");
    assert_eq!(actual_b, "玄戒芯片");
}

#[test]
fn keeps_non_chinese_segments_unchanged() {
    let actual = replace_text(
        &sample_lexicon(),
        &sample_rules(),
        &sample_config(),
        "OpenAI 悬界星片 v2",
    );
    assert_eq!(actual, "OpenAI 玄戒芯片 v2");
}

#[test]
fn prefers_longest_phrase_match() {
    let actual = match_phrases(&sample_lexicon(), "湖南人");
    assert_eq!(actual, vec!["湖南人"]);
}

#[test]
fn falls_back_to_character_level_pronunciation() {
    let actual = pronounce_phrase(&sample_lexicon(), "悬界");
    assert_eq!(actual, "xuan2jie4");
}

#[test]
fn keeps_first_duplicate_lexicon_entry() {
    let lexicon = Lexicon::parse(
        "\
湖南 hu2 nan2
湖南 hu4 nan4
",
    );
    assert_eq!(lexicon.pronunciation("湖南"), Some("hu2nan2"));
}

#[test]
fn appends_tone_one_for_neutral_tone_like_entries() {
    let lexicon = Lexicon::parse("吗 ma");
    assert_eq!(lexicon.pronunciation("吗"), Some("ma1"));
}

#[test]
fn trims_trailing_space_after_mixed_language_output() {
    let rules = ReplaceRuleSet::from_pairs(&[("xuan2jie4xing1pian4", "玄戒芯片")]);
    let actual = replace_text(
        &sample_lexicon(),
        &rules,
        &sample_config(),
        "hello 悬界星片",
    );
    assert_eq!(actual, "hello 玄戒芯片");
}

#[test]
fn keeps_first_target_when_duplicate_pronunciation_rules_are_given() {
    let rules = ReplaceRuleSet::from_pairs(&[
        ("xuan2jie4xin1pian4", "玄戒芯片"),
        ("xuan2jie4xin1pian4", "别的结果"),
    ]);
    assert_eq!(rules.get("xuan2jie4xin1pian4"), Some("玄戒芯片"));
}

#[test]
fn can_replace_when_only_tones_are_different() {
    let lexicon = Lexicon::parse(
        "\
熊 xiong2
欢 huan1
换 huan4
周 zhou1
",
    );
    let rules = ReplaceRuleSet::from_pairs(&[("xiong2huan1zhou1", "熊欢周")]);
    let actual = replace_text(&lexicon, &rules, &sample_config(), "熊换周");
    assert_eq!(actual, "熊欢周");
}
