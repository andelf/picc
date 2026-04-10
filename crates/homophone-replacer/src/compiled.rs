use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use crate::{replace_text, Lexicon, ReplaceRuleSet, ReplacerConfig};

const MAGIC: &[u8; 4] = b"HMR1";
const VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct CompiledReplacer {
    lexicon: Lexicon,
    rules: ReplaceRuleSet,
}

#[derive(Debug)]
pub enum CompiledFormatError {
    InvalidMagic,
    UnsupportedVersion(u32),
    UnexpectedEof,
    InvalidUtf8,
    CountOverflow,
}

impl fmt::Display for CompiledFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => write!(f, "invalid compiled replacer header"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported compiled replacer version: {version}")
            }
            Self::UnexpectedEof => write!(f, "unexpected end of compiled replacer data"),
            Self::InvalidUtf8 => write!(f, "compiled replacer contains invalid utf-8"),
            Self::CountOverflow => write!(f, "compiled replacer count overflow"),
        }
    }
}

impl std::error::Error for CompiledFormatError {}

impl From<CompiledFormatError> for io::Error {
    fn from(value: CompiledFormatError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, value)
    }
}

impl CompiledReplacer {
    pub fn from_parts(lexicon: Lexicon, rules: ReplaceRuleSet) -> Self {
        Self { lexicon, rules }
    }

    pub fn compile(lexicon_text: &str, rules_text: &str) -> Self {
        Self {
            lexicon: Lexicon::parse(lexicon_text),
            rules: ReplaceRuleSet::parse(rules_text),
        }
    }

    pub fn compile_from_files(
        lexicon_path: impl AsRef<Path>,
        rules_path: impl AsRef<Path>,
    ) -> io::Result<Self> {
        Ok(Self {
            lexicon: Lexicon::from_path(lexicon_path)?,
            rules: ReplaceRuleSet::from_path(rules_path)?,
        })
    }

    pub fn lexicon(&self) -> &Lexicon {
        &self.lexicon
    }

    pub fn rules(&self) -> &ReplaceRuleSet {
        &self.rules
    }

    pub fn replace(&self, config: &ReplacerConfig, text: &str) -> String {
        replace_text(&self.lexicon, &self.rules, config, text)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());

        let mut lexicon_entries = self.lexicon.iter().collect::<Vec<_>>();
        lexicon_entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
        write_u32(&mut out, lexicon_entries.len() as u32);
        for (word, pronunciation) in lexicon_entries {
            write_string(&mut out, word);
            write_string(&mut out, pronunciation);
        }

        let mut rules = self.rules.iter().collect::<Vec<_>>();
        rules.sort_unstable_by(|a, b| a.0.cmp(b.0));
        write_u32(&mut out, rules.len() as u32);
        for (pronunciation, target) in rules {
            write_string(&mut out, pronunciation);
            write_string(&mut out, target);
        }

        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CompiledFormatError> {
        let mut cursor = Cursor::new(bytes);

        if cursor.take_exact(4)? != MAGIC {
            return Err(CompiledFormatError::InvalidMagic);
        }

        let version = cursor.read_u32()?;
        if version != VERSION {
            return Err(CompiledFormatError::UnsupportedVersion(version));
        }

        let lexicon_count = cursor.read_u32()? as usize;
        let mut lexicon_entries = Vec::with_capacity(lexicon_count);
        for _ in 0..lexicon_count {
            let word = cursor.read_string()?;
            let pronunciation = cursor.read_string()?;
            lexicon_entries.push(format!("{word} {pronunciation}"));
        }
        let lexicon = Lexicon::parse(&lexicon_entries.join("\n"));

        let rule_count = cursor.read_u32()? as usize;
        let mut rules = Vec::with_capacity(rule_count);
        for _ in 0..rule_count {
            let pronunciation = cursor.read_string()?;
            let target = cursor.read_string()?;
            rules.push((pronunciation, target));
        }
        let rule_refs = rules
            .iter()
            .map(|(pronunciation, target)| (pronunciation.as_str(), target.as_str()))
            .collect::<Vec<_>>();

        Ok(Self {
            lexicon,
            rules: ReplaceRuleSet::from_pairs(&rule_refs),
        })
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
        fs::write(path, self.to_bytes())
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        Self::from_bytes(&bytes).map_err(Into::into)
    }
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    write_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take_exact(&mut self, len: usize) -> Result<&'a [u8], CompiledFormatError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(CompiledFormatError::CountOverflow)?;
        if end > self.bytes.len() {
            return Err(CompiledFormatError::UnexpectedEof);
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn read_u32(&mut self) -> Result<u32, CompiledFormatError> {
        let bytes = self.take_exact(4)?;
        let mut array = [0_u8; 4];
        array.copy_from_slice(bytes);
        Ok(u32::from_le_bytes(array))
    }

    fn read_string(&mut self) -> Result<String, CompiledFormatError> {
        let len = self.read_u32()? as usize;
        let bytes = self.take_exact(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| CompiledFormatError::InvalidUtf8)
    }
}

#[cfg(test)]
mod tests {
    use super::{CompiledFormatError, CompiledReplacer};
    use crate::ReplacerConfig;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const LEXICON: &str = "\
悬 xuan2
界 jie4
星 xing1
片 pian4
";

    const RULES: &str = "\
xuan2jie4xing1pian4 玄戒芯片
";

    #[test]
    fn roundtrip_preserves_behavior() {
        let compiled = CompiledReplacer::compile(LEXICON, RULES);
        let bytes = compiled.to_bytes();
        let loaded = CompiledReplacer::from_bytes(&bytes).unwrap();

        let config = ReplacerConfig { max_phrase_len: 10 };
        assert_eq!(loaded.replace(&config, "悬界星片"), "玄戒芯片");
    }

    #[test]
    fn rejects_invalid_magic() {
        let err = CompiledReplacer::from_bytes(b"BAD!\x01\0\0\0").unwrap_err();
        assert!(matches!(err, CompiledFormatError::InvalidMagic));
    }

    #[test]
    fn rejects_truncated_data() {
        let compiled = CompiledReplacer::compile(LEXICON, RULES);
        let mut bytes = compiled.to_bytes();
        bytes.truncate(bytes.len() - 1);

        let err = CompiledReplacer::from_bytes(&bytes).unwrap_err();
        assert!(matches!(err, CompiledFormatError::UnexpectedEof));
    }

    #[test]
    fn can_save_and_load_from_file() {
        let compiled = CompiledReplacer::compile(LEXICON, RULES);
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("compiled-replacer-{unique}.bin"));

        compiled.save_to_path(&path).unwrap();
        let loaded = CompiledReplacer::load_from_path(&path).unwrap();

        let config = ReplacerConfig { max_phrase_len: 10 };
        assert_eq!(loaded.replace(&config, "悬界星片"), "玄戒芯片");

        let _ = fs::remove_file(path);
    }
}
