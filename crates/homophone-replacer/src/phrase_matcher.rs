use crate::Lexicon;

const DEFAULT_MAX_PHRASE_LEN: usize = 10;

pub fn match_phrases(lexicon: &Lexicon, text: &str) -> Vec<String> {
    match_phrases_with_limit(lexicon, text, DEFAULT_MAX_PHRASE_LEN)
}

pub(crate) fn match_phrases_with_limit(
    lexicon: &Lexicon,
    text: &str,
    max_phrase_len: usize,
) -> Vec<String> {
    let tokens = split_utf8_like_sherpa(text);
    let mut phrases = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        let current = &tokens[i];
        if !is_candidate_start(current) {
            phrases.push(current.clone());
            i += 1;
            continue;
        }

        let mut matched = None;
        let mut end = usize::min(tokens.len(), i + max_phrase_len.max(1));
        while end > i + 1 {
            let candidate = tokens[i..end].concat();
            if is_candidate_end(&candidate) && lexicon.contains(&candidate) {
                matched = Some(candidate);
                break;
            }
            end -= 1;
        }

        if let Some(candidate) = matched {
            phrases.push(candidate);
            i = end;
        } else {
            phrases.push(current.clone());
            i += 1;
        }
    }

    phrases
}

fn split_utf8_like_sherpa(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i].is_ascii_alphabetic() {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
        } else {
            out.push(chars[i].to_string());
            i += 1;
        }
    }

    out
}

fn is_candidate_start(token: &str) -> bool {
    token
        .chars()
        .next()
        .map(|ch| !ch.is_ascii() && !ch.is_whitespace() && !is_punctuation(ch))
        .unwrap_or(false)
}

fn is_candidate_end(candidate: &str) -> bool {
    candidate
        .chars()
        .last()
        .map(|ch| !ch.is_ascii_alphabetic() && !ch.is_ascii_punctuation())
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
    use super::{match_phrases, match_phrases_with_limit};
    use crate::Lexicon;

    #[test]
    fn groups_ascii_words_and_prefers_longest_match() {
        let lexicon = Lexicon::parse("湖南 hu2 nan2\n湖南人 hu2 nan2 ren2\n");
        assert_eq!(
            match_phrases(&lexicon, "hello 湖南人"),
            vec!["hello", " ", "湖南人"]
        );
    }

    #[test]
    fn respects_phrase_length_limit() {
        let lexicon = Lexicon::parse("弓头安装 gong1 tou2 an1 zhuang1\n");
        assert_eq!(
            match_phrases_with_limit(&lexicon, "弓头安装", 1),
            vec!["弓", "头", "安", "装"]
        );
    }
}
