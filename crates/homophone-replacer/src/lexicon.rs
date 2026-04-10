use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexiconEntry {
    pub word: String,
    pub pronunciation: String,
}

#[derive(Debug, Clone, Default)]
pub struct Lexicon {
    entries: HashMap<String, String>,
}

impl Lexicon {
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let input = fs::read_to_string(path)?;
        Ok(Self::parse(&input))
    }

    pub fn parse(input: &str) -> Self {
        let mut entries = HashMap::new();

        for line in input.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let mut parts = trimmed.split_whitespace();
            let Some(word_raw) = parts.next() else {
                continue;
            };

            let word = word_raw.to_lowercase();
            if entries.contains_key(&word) {
                continue;
            }

            let mut pronunciation = String::new();
            for token in parts {
                let mut part = token.to_string();
                let needs_default_tone = part
                    .chars()
                    .last()
                    .map(|ch| !matches!(ch, '1' | '2' | '3' | '4'))
                    .unwrap_or(false);
                if needs_default_tone {
                    part.push('1');
                }
                pronunciation.push_str(&part);
            }

            if pronunciation.is_empty() {
                continue;
            }

            entries.insert(word, pronunciation);
        }

        Self { entries }
    }

    pub fn pronunciation(&self, word: &str) -> Option<&str> {
        self.entries.get(&word.to_lowercase()).map(String::as_str)
    }

    pub fn contains(&self, word: &str) -> bool {
        self.entries.contains_key(&word.to_lowercase())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(word, pronunciation)| (word.as_str(), pronunciation.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::Lexicon;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parse_skips_empty_lines_and_keeps_first_duplicate() {
        let lexicon = Lexicon::parse("\n湖南 hu2 nan2\n\n湖南 hu4 nan4\n玄戒 xuan2 jie4\n");

        assert_eq!(lexicon.pronunciation("湖南"), Some("hu2nan2"));
        assert_eq!(lexicon.pronunciation("玄戒"), Some("xuan2jie4"));
    }

    #[test]
    fn parse_appends_tone_one_for_missing_tone() {
        let lexicon = Lexicon::parse("吗 ma\n");
        assert_eq!(lexicon.pronunciation("吗"), Some("ma1"));
    }

    #[test]
    fn parse_ignores_line_without_pronunciation() {
        let lexicon = Lexicon::parse("湖南\n玄戒 xuan2 jie4\n");
        assert!(!lexicon.contains("湖南"));
        assert!(lexicon.contains("玄戒"));
    }

    #[test]
    fn can_load_from_file() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("lexicon-{unique}.txt"));
        fs::write(&path, "湖南 hu2 nan2\n").unwrap();

        let lexicon = Lexicon::from_path(&path).unwrap();
        assert_eq!(lexicon.pronunciation("湖南"), Some("hu2nan2"));

        let _ = fs::remove_file(path);
    }
}
