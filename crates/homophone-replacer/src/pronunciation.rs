use crate::Lexicon;

pub fn pronounce_phrase(lexicon: &Lexicon, phrase: &str) -> String {
    if let Some(pron) = lexicon.pronunciation(phrase) {
        return pron.to_string();
    }

    if phrase.chars().count() <= 1 {
        return phrase.to_string();
    }

    let mut result = String::new();
    for ch in phrase.chars() {
        let piece = ch.to_string();
        if let Some(pron) = lexicon.pronunciation(&piece) {
            result.push_str(pron);
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::pronounce_phrase;
    use crate::Lexicon;

    #[test]
    fn prefers_whole_phrase_pronunciation() {
        let lexicon = Lexicon::parse("湖南 hu2 nan2\n湖南人 hu2 nan2 ren2\n");
        assert_eq!(pronounce_phrase(&lexicon, "湖南人"), "hu2nan2ren2");
    }

    #[test]
    fn falls_back_to_character_level() {
        let lexicon = Lexicon::parse("悬 xuan2\n界 jie4\n");
        assert_eq!(pronounce_phrase(&lexicon, "悬界"), "xuan2jie4");
    }
}
