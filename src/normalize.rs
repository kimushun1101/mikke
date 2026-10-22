//! 検索用テキストの正規化 (全角・半角の幅の畳み込みと、和欧間スペースの削除)。

use unicode_normalization::UnicodeNormalization;

/// 幅の畳み込みの対象: 全角形・半角形 (U+FF01–U+FFEF)。U+FFE3 (￣) は NFKC で
/// 「U+0020 + 結合文字」になり、クエリに空白を作るので外す。
fn is_width_form(c: char) -> bool {
    matches!(c, '\u{ff01}'..='\u{ffef}') && c != '\u{ffe3}'
}

/// 全角形・半角形の連続部分だけに NFKC をかける (`ＧＰＳ` → `GPS`、`ｶﾞｲﾄﾞ` → `ガイド`)。
/// 連続部分ごとにかけるので、半角カナの濁点・半濁点は前の字と合成される。
/// 幅以外の NFKC (`①` → `1`、`㎏` → `kg`、U+3000 → U+0020 など) はかけない。空白は作りも消しもしない。
pub fn fold_width(input: &str) -> String {
    if !input.chars().any(is_width_form) {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut run = String::new();
    for c in input.chars() {
        if is_width_form(c) {
            run.push(c);
            continue;
        }
        if !run.is_empty() {
            out.extend(run.nfkc());
            run.clear();
        }
        out.push(c);
    }
    out.extend(run.nfkc());
    out
}

/// 索引 (notes_fts / tags.tag_search) と tag / title のキーワードに使う検索用の形。
/// 幅を畳んでから和欧間スペースを削除する (逆順だと `ＡＢＣ ＤＥＦ` の空白だけが消え、
/// `ABC DEF` と食い違う)。find のクエリには空白の削除をかけず、fold_width だけを使う。
pub fn search_text(input: &str) -> String {
    normalize_search_text(&fold_width(input))
}

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
    use super::{fold_width, normalize_search_text, search_text};

    #[test]
    fn folds_fullwidth_and_halfwidth_forms() {
        for (input, expected) in [
            ("ＧＰＳ首輪", "GPS首輪"),
            ("ＡＢＣ１２３", "ABC123"),
            ("ｷﾂﾈ", "キツネ"),
            ("ｶﾞｲﾄﾞ", "ガイド"),
            ("ｶﾞﾝ", "ガン"),
            ("ﾊﾟﾝﾀﾞ", "パンダ"),
            ("ｇｐｓ", "gps"),
            ("！？", "!?"),
        ] {
            assert_eq!(fold_width(input), expected, "{input}");
        }
    }

    #[test]
    fn does_not_fold_outside_width_forms() {
        for input in [
            "①",
            "²",
            "㎏",
            "Ⅳ",
            "ﬁ",
            "㈱",
            "字　下げ",
            "字\u{00a0}下げ",
            "゛",
            "￣",
            "Äpfel",
        ] {
            assert_eq!(fold_width(input), input, "{input}");
        }
    }

    /// 対象の文字はどれも、畳んでも空白を作らない (語分割と AND を変えない)。
    #[test]
    fn folding_never_creates_whitespace() {
        for cp in 0xff01..=0xffef_u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let folded = fold_width(&c.to_string());
            assert!(
                !folded.chars().any(char::is_whitespace),
                "U+{cp:04X} -> {folded:?}"
            );
        }
    }

    #[test]
    fn folds_width_before_removing_spaces() {
        assert_eq!(search_text("ＡＢＣ ＤＥＦ"), search_text("ABC DEF"));
        assert_eq!(search_text("ＡＢＣ ＤＥＦ"), "ABC DEF");
        assert_eq!(search_text("ﾃｽﾄ abc"), "テストabc");
        assert_eq!(search_text("字　下げ"), "字　下げ");
    }

    #[test]
    fn search_text_is_idempotent() {
        for input in [
            "ＧＰＳ 首輪 ｶﾞｲﾄﾞ",
            "ＡＢＣ ＤＥＦ",
            "ﾃｽﾄ abc と θ 制御",
            "ﾞ単独",
        ] {
            let once = search_text(input);
            assert_eq!(search_text(&once), once, "{input}");
        }
    }

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
