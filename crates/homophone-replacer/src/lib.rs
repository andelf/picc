//! Rust implementation skeleton for sherpa-onnx style homophone replacement.

pub mod compiled;
pub mod lexicon;
pub mod official_examples;
pub mod phrase_matcher;
pub mod pronunciation;
pub mod replacer;

pub use compiled::{CompiledFormatError, CompiledReplacer};
pub use lexicon::{Lexicon, LexiconEntry};
pub use official_examples::{
    official_non_streaming_sense_voice_hr_config, official_test_hr_assets,
    official_test_hr_expected_text, OfficialFixturePaths, OfficialSenseVoiceHrConfig,
};
pub use phrase_matcher::match_phrases;
pub use pronunciation::pronounce_phrase;
pub use replacer::{
    build_rules_from_terms, replace_text, replace_text_from_files, ReplaceRuleSet, ReplacerConfig,
};
