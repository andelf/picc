#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DictationOptions {
    pub fullwidth_to_halfwidth: bool,
    pub space_around_punct: bool,
    pub space_between_cjk: bool,
    pub strip_trailing_punct: bool,
}

/// Apply user-configured text transforms before text goes on screen.
pub fn apply_dictation_transforms(text: &str, opts: DictationOptions) -> String {
    let mut result = text.to_string();

    if opts.fullwidth_to_halfwidth {
        result = fullwidth_to_halfwidth(&result);
    }
    if opts.space_around_punct || opts.space_between_cjk {
        result = auto_insert_spaces(&result, opts.space_around_punct, opts.space_between_cjk);
    }
    if opts.strip_trailing_punct {
        result = strip_trailing_punctuation(&result);
    }

    result
}

fn fullwidth_to_halfwidth(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            '。' => '.',
            '、' => ',',
            '【' => '[',
            '】' => ']',
            '「' => '"',
            '」' => '"',
            '《' => '<',
            '》' => '>',
            '\u{201C}' => '"',
            '\u{201D}' => '"',
            '\u{2018}' => '\'',
            '\u{2019}' => '\'',
            _ => c,
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharKind {
    Cjk,
    Latin,
    Digit,
    OpenBracket,
    CloseBracket,
    Delimiter,
    Space,
    Other,
}

fn classify(c: char) -> CharKind {
    match c {
        'A'..='Z' | 'a'..='z' => CharKind::Latin,
        '0'..='9' => CharKind::Digit,
        '(' | '[' | '<' => CharKind::OpenBracket,
        ')' | ']' | '>' => CharKind::CloseBracket,
        ',' | '.' | '!' | '?' | ':' | ';' => CharKind::Delimiter,
        ' ' => CharKind::Space,
        c if is_cjk(c) => CharKind::Cjk,
        _ => CharKind::Other,
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{2E80}'..='\u{2EFF}'
        | '\u{2F00}'..='\u{2FDF}'
        | '\u{3040}'..='\u{309F}'
        | '\u{30A0}'..='\u{30FF}'
        | '\u{3100}'..='\u{312F}'
        | '\u{3200}'..='\u{32FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{F900}'..='\u{FAFF}'
    )
}

fn is_cjk_boundary(left: CharKind, right: CharKind) -> bool {
    use CharKind::*;
    matches!(
        (left, right),
        (Cjk, Latin) | (Latin, Cjk) | (Cjk, Digit) | (Digit, Cjk)
    )
}

fn is_punct_space(left: CharKind, right: CharKind) -> bool {
    use CharKind::*;
    matches!(
        (left, right),
        (Delimiter | CloseBracket, Cjk | Latin | Digit | Other)
            | (Cjk | Latin | Digit | Other, OpenBracket)
    )
}

pub fn auto_insert_spaces(s: &str, punct: bool, cjk: bool) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 32);

    for (i, &c) in chars.iter().enumerate() {
        let kind = classify(c);

        if i > 0 {
            let prev = chars[i - 1];
            let prev_kind = classify(prev);

            if prev_kind != CharKind::Space && kind != CharKind::Space {
                let want_cjk = cjk && is_cjk_boundary(prev_kind, kind);
                let want_punct = punct && is_punct_space(prev_kind, kind);

                if want_cjk || want_punct {
                    let is_decimal_dot = prev == '.'
                        && prev_kind == CharKind::Delimiter
                        && classify(chars.get(i.wrapping_sub(2)).copied().unwrap_or(' '))
                            == CharKind::Digit
                        && kind == CharKind::Digit;
                    if !is_decimal_dot {
                        out.push(' ');
                    }
                }
            }
        }

        out.push(c);
    }

    out
}

fn strip_trailing_punctuation(s: &str) -> String {
    s.trim_end_matches(|c: char| {
        matches!(
            c,
            '.' | ','
                | '!'
                | '?'
                | ';'
                | ':'
                | '。'
                | '，'
                | '！'
                | '？'
                | '；'
                | '：'
                | '、'
                | '…'
        )
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{apply_dictation_transforms, auto_insert_spaces, DictationOptions};

    fn spaced(s: &str) -> String {
        auto_insert_spaces(s, true, true)
    }

    #[test]
    fn cjk_latin_spacing() {
        assert_eq!(auto_insert_spaces("中文abc", false, true), "中文 abc");
        assert_eq!(auto_insert_spaces("abc中文", false, true), "abc 中文");
        assert_eq!(
            auto_insert_spaces("中文abc中文", false, true),
            "中文 abc 中文"
        );
    }

    #[test]
    fn cjk_digit_spacing() {
        assert_eq!(auto_insert_spaces("第3章", false, true), "第 3 章");
        assert_eq!(auto_insert_spaces("100个", false, true), "100 个");
    }

    #[test]
    fn cjk_only_off_no_effect() {
        assert_eq!(auto_insert_spaces("中文abc", true, false), "中文abc");
    }

    #[test]
    fn delimiter_spacing() {
        assert_eq!(
            auto_insert_spaces("hello,world", true, false),
            "hello, world"
        );
        assert_eq!(auto_insert_spaces("a.b", true, false), "a. b");
        assert_eq!(auto_insert_spaces("ok!nice", true, false), "ok! nice");
    }

    #[test]
    fn decimal_point_no_space() {
        assert_eq!(spaced("3.14"), "3.14");
        assert_eq!(spaced("价格是9.99元"), "价格是 9.99 元");
    }

    #[test]
    fn bracket_spacing() {
        assert_eq!(
            auto_insert_spaces("hello(world)test", true, false),
            "hello (world) test"
        );
        assert_eq!(spaced("你好(世界)"), "你好 (世界)");
    }

    #[test]
    fn consecutive_punctuation() {
        assert_eq!(spaced("what?!ok"), "what?! ok");
        assert_eq!(spaced("a...b"), "a... b");
    }

    #[test]
    fn punct_only_off_no_effect() {
        assert_eq!(
            auto_insert_spaces("hello,world", false, true),
            "hello,world"
        );
    }

    #[test]
    fn already_spaced_idempotent() {
        assert_eq!(spaced("中文 abc 中文"), "中文 abc 中文");
        assert_eq!(spaced("hello, world"), "hello, world");
    }

    #[test]
    fn no_spurious_spaces() {
        assert_eq!(spaced("你好世界"), "你好世界");
        assert_eq!(spaced("hello world"), "hello world");
        assert_eq!(spaced("12345"), "12345");
    }

    #[test]
    fn mixed_complex() {
        assert_eq!(spaced("用Vue3和React开发"), "用 Vue3 和 React 开发");
        assert_eq!(spaced("这是v2.0版本"), "这是 v2.0 版本");
    }

    #[test]
    fn transforms_fullwidth_then_punct_spaces() {
        let opts = DictationOptions {
            fullwidth_to_halfwidth: true,
            space_around_punct: true,
            space_between_cjk: false,
            strip_trailing_punct: false,
        };
        assert_eq!(apply_dictation_transforms("你好，世界", opts), "你好, 世界");
    }

    #[test]
    fn transforms_cjk_spacing_independent() {
        let opts = DictationOptions {
            fullwidth_to_halfwidth: false,
            space_around_punct: false,
            space_between_cjk: true,
            strip_trailing_punct: false,
        };
        assert_eq!(
            apply_dictation_transforms("中文abc中文", opts),
            "中文 abc 中文"
        );
    }
}
