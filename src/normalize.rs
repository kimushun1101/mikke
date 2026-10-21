//! 検索用テキストの和欧間スペース正規化。

/// 両隣が非空白で、少なくとも片側が和文クラスの U+0020 を削除する。
pub fn normalize_search_text(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let is_japanese = |c: char| {
        matches!(
            c,
            '\u{3001}'..='\u{303f}'
                | '\u{3040}'..='\u{30ff}'
                | '\u{31f0}'..='\u{31ff}'
                | '\u{3400}'..='\u{4dbf}'
                | '\u{4e00}'..='\u{9fff}'
                | '\u{f900}'..='\u{faff}'
                | '\u{ff00}'..='\u{ffef}'
        )
    };
    let mut normalized = String::with_capacity(input.len());

    for (i, &c) in chars.iter().enumerate() {
        let remove = c == ' '
            && i > 0
            && i + 1 < chars.len()
            && !chars[i - 1].is_whitespace()
            && !chars[i + 1].is_whitespace()
            && (is_japanese(chars[i - 1]) || is_japanese(chars[i + 1]));
        if !remove {
            normalized.push(c);
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::normalize_search_text;

    #[test]
    fn removes_single_spaces_next_to_japanese_class() {
        for (input, expected) in [
            ("Typst は", "Typstは"),
            ("スペース あり", "スペースあり"),
            ("高速です。 LaTeX", "高速です。LaTeX"),
            ("ﾃｽﾄ abc", "ﾃｽﾄabc"),
            ("θ 制御", "θ制御"),
            ("0.13 です", "0.13です"),
        ] {
            assert_eq!(normalize_search_text(input), expected);
        }
    }

    #[test]
    fn preserves_spaces_outside_the_rule() {
        for input in [
            "hello world",
            "θ dot",
            "字　下げ",
            "字  下げ",
            "字\t下げ",
            "字\u{00a0}下げ",
            " 行頭",
            "行末 ",
            "行末 \n 行頭",
        ] {
            assert_eq!(normalize_search_text(input), input);
        }
    }

    #[test]
    fn normalization_is_idempotent() {
        for input in [
            "Typst は組版 システムです。",
            "字  下げ\n高速です。 LaTeX",
            "ﾃｽﾄ abc と θ 制御",
        ] {
            let once = normalize_search_text(input);
            assert_eq!(normalize_search_text(&once), once);
        }
    }
}
