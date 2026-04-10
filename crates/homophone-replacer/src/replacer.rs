use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::phrase_matcher::match_phrases_with_limit;
use crate::pronunciation::pronounce_phrase;
use crate::Lexicon;

#[derive(Debug, Clone, Default)]
pub struct ReplaceRuleSet {
    rules: HashMap<String, String>,
    relaxed_rules: HashMap<String, String>,
}

impl ReplaceRuleSet {
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let input = fs::read_to_string(path)?;
        Ok(Self::parse(&input))
    }

    pub fn parse(input: &str) -> Self {
        let mut pairs = Vec::new();
        for line in input.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let mut parts = trimmed.split_whitespace();
            let Some(source) = parts.next() else {
                continue;
            };
            let Some(target) = parts.next() else {
                continue;
            };
            pairs.push((source.to_string(), target.to_string()));
        }

        let refs = pairs
            .iter()
            .map(|(source, target)| (source.as_str(), target.as_str()))
            .collect::<Vec<_>>();
        Self::from_pairs(&refs)
    }

    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        let mut rules = HashMap::new();
        let mut relaxed_rules = HashMap::new();
        for (source, target) in pairs {
            let source = (*source).to_string();
            let target = (*target).to_string();
            let relaxed = strip_tones(&source);

            rules.entry(source).or_insert_with(|| target.clone());
            relaxed_rules.entry(relaxed).or_insert(target);
        }
        Self {
            rules,
            relaxed_rules,
        }
    }

    pub fn get(&self, pronunciation: &str) -> Option<&str> {
        if let Some(target) = self.rules.get(pronunciation) {
            return Some(target.as_str());
        }

        let relaxed = strip_tones(pronunciation);
        self.relaxed_rules.get(&relaxed).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.rules
            .iter()
            .map(|(pronunciation, target)| (pronunciation.as_str(), target.as_str()))
    }
}

fn strip_tones(pronunciation: &str) -> String {
    pronunciation
        .chars()
        .filter(|ch| !ch.is_ascii_digit())
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ReplacerConfig {
    pub max_phrase_len: usize,
}

pub fn replace_text(
    lexicon: &Lexicon,
    rules: &ReplaceRuleSet,
    config: &ReplacerConfig,
    text: &str,
) -> String {
    let phrases = match_phrases_with_limit(lexicon, text, config.max_phrase_len.max(1));
    let mut result = String::new();
    let mut chinese_run: Vec<String> = Vec::new();

    let flush_run = |result: &mut String, run: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }

        let mut i = 0;
        while i < run.len() {
            let mut replaced = false;
            for end in (i + 1..=run.len()).rev() {
                let pronunciation = run[i..end]
                    .iter()
                    .map(|w| pronounce_phrase(lexicon, w))
                    .collect::<Vec<_>>()
                    .concat();
                if let Some(target) = rules.get(&pronunciation) {
                    result.push_str(target);
                    i = end;
                    replaced = true;
                    break;
                }
            }

            if !replaced {
                result.push_str(&run[i]);
                i += 1;
            }
        }

        run.clear();
    };

    for phrase in phrases {
        if is_replaceable_phrase(&phrase) {
            chinese_run.push(phrase);
        } else {
            flush_run(&mut result, &mut chinese_run);
            result.push_str(&phrase);
        }
    }
    flush_run(&mut result, &mut chinese_run);

    result.trim_end_matches(' ').to_string()
}

pub fn replace_text_from_files(
    lexicon_path: impl AsRef<Path>,
    rules_path: impl AsRef<Path>,
    config: &ReplacerConfig,
    text: &str,
) -> io::Result<String> {
    let lexicon = Lexicon::from_path(lexicon_path)?;
    let rules = ReplaceRuleSet::from_path(rules_path)?;
    Ok(replace_text(&lexicon, &rules, config, text))
}

pub fn build_rules_from_terms<'a>(
    lexicon: &Lexicon,
    terms: impl IntoIterator<Item = &'a str>,
) -> ReplaceRuleSet {
    let pairs = terms
        .into_iter()
        .filter_map(|term| {
            let trimmed = term.trim();
            if trimmed.is_empty() {
                return None;
            }
            let pronunciation = pronounce_phrase(lexicon, trimmed);
            Some((pronunciation, trimmed.to_string()))
        })
        .collect::<Vec<_>>();

    let refs = pairs
        .iter()
        .map(|(pronunciation, term)| (pronunciation.as_str(), term.as_str()))
        .collect::<Vec<_>>();
    ReplaceRuleSet::from_pairs(&refs)
}

fn is_replaceable_phrase(phrase: &str) -> bool {
    phrase
        .chars()
        .next()
        .map(|ch| !ch.is_ascii() && !ch.is_whitespace() && !is_punctuation(ch))
        .unwrap_or(false)
}

fn is_punctuation(ch: char) -> bool {
    matches!(
        ch,
        ',' | '.'
            | '!'
            | '?'
            | ':'
            | '"'
            | '\''
            | '，'
            | '。'
            | '！'
            | '？'
            | '“'
            | '”'
            | '‘'
            | '’'
    )
}

#[cfg(test)]
mod tests {
    use super::{build_rules_from_terms, replace_text, ReplaceRuleSet, ReplacerConfig};
    use crate::Lexicon;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn replaces_across_multiple_matched_phrases() {
        let lexicon = Lexicon::parse(
            "工 gong1\n投 tou2\n安装 an1 zhuang1\n机载 ji1 zai4\n传感器 chuan2 gan3 qi4\n",
        );
        let rules = ReplaceRuleSet::from_pairs(&[
            ("gong1tou2an1zhuang1", "弓头安装"),
            ("ji1zai4chuan2gan3qi4", "机载传感器"),
        ]);
        let config = ReplacerConfig { max_phrase_len: 10 };

        let actual = replace_text(&lexicon, &rules, &config, "工投安装机载传感器");
        assert_eq!(actual, "弓头安装机载传感器");
    }

    #[test]
    fn parses_rules_from_text() {
        let rules = ReplaceRuleSet::parse(
            "\
# comment
xuan2jie4xin1pian4 玄戒芯片
xuan2jie4xing1pian4 玄戒芯片
",
        );
        assert_eq!(rules.get("xuan2jie4xin1pian4"), Some("玄戒芯片"));
        assert_eq!(rules.get("xuan2jie4xing1pian4"), Some("玄戒芯片"));
    }

    #[test]
    fn replaces_text_from_files() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let lexicon_path = std::env::temp_dir().join(format!("lexicon-{unique}.txt"));
        let rules_path = std::env::temp_dir().join(format!("rules-{unique}.txt"));

        fs::write(&lexicon_path, "悬 xuan2\n界 jie4\n星 xing1\n片 pian4\n").unwrap();
        fs::write(&rules_path, "xuan2jie4xing1pian4 玄戒芯片\n").unwrap();

        let config = ReplacerConfig { max_phrase_len: 10 };
        let actual =
            super::replace_text_from_files(&lexicon_path, &rules_path, &config, "悬界星片")
                .unwrap();
        assert_eq!(actual, "玄戒芯片");

        let _ = fs::remove_file(lexicon_path);
        let _ = fs::remove_file(rules_path);
    }

    #[test]
    fn builds_rules_from_terms_using_character_fallback() {
        let lexicon = Lexicon::parse(
            "熊 xiong2\n欢 huan1\n周 zhou1\n王 wang2\n淑 shu1\n羽 yu3\n程 cheng2\n利 li4\n军 jun1\n",
        );
        let rules = build_rules_from_terms(&lexicon, ["熊欢周", "王淑羽", "程利军"]);

        assert_eq!(rules.get("xiong2huan1zhou1"), Some("熊欢周"));
        assert_eq!(rules.get("wang2shu1yu3"), Some("王淑羽"));
        assert_eq!(rules.get("cheng2li4jun1"), Some("程利军"));
    }

    #[test]
    fn matches_terms_when_only_tones_differ() {
        let lexicon = Lexicon::parse("熊 xiong2\n换 huan4\n欢 huan1\n周 zhou1\n");
        let rules = build_rules_from_terms(&lexicon, ["熊欢周"]);
        let config = ReplacerConfig { max_phrase_len: 10 };

        let actual = replace_text(&lexicon, &rules, &config, "熊换周");
        assert_eq!(actual, "熊欢周");
    }
}
