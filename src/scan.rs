//! ノート走査と frontmatter 解析の共通処理 (index / search / health / embed で共用)。

#![allow(dead_code)]

use crate::config::{load_config, to_posix, Config, CONFIG_FILENAMES};
use regex::Regex;
use serde_yaml_ng::{Mapping, Value};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 1 ノートのパース結果。
#[derive(Debug, Clone)]
pub struct Note {
    pub path_rel: String, // root 相対 posix パス
    pub title: String,
    pub tags: Vec<String>,
    pub date: String, // YYYY-MM-DD 正規化済 (無ければ "")
    pub updated: String,
    pub summary: String,
    pub content: String, // frontmatter を除いた本文
}

/// CJK 文字数 + スペース区切り語数。
/// 文字クラスは正確に: CJK = `[\u{3000}-\u{9fff}\u{f900}-\u{faff}\u{ff66}-\u{ff9f}]` の文字数、
/// ascii = `[a-zA-Z0-9]+` の語数。範囲はエスケープで書く (生リテラルは NFC 化で壊れる)。
pub fn count_words(text: &str) -> i64 {
    let cjk = text
        .chars()
        .filter(|c| {
            matches!(c,
                '\u{3000}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{ff66}'..='\u{ff9f}')
        })
        .count();
    let mut ascii_words = 0usize;
    let mut in_word = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if !in_word {
                ascii_words += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    (cjk + ascii_words) as i64
}

/// `[[...]]` / `[[...|alias]]` からリンク先を抽出。set 化して sorted。
pub fn extract_wikilinks(text: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\[\[([^\]|]+?)(?:\|[^\]]+?)?\]\]").unwrap());
    let set: BTreeSet<String> = re.captures_iter(text).map(|c| c[1].to_string()).collect();
    set.into_iter().collect()
}

/// タイトル: frontmatter `title` → 最初の `# ` 見出し → ファイル名 stem の優先順。
/// 見出しは各行に `^#\s+(.+)$` を適用し、一致部分を trim する。
pub fn extract_title(fm_title: Option<&str>, content: &str, path: &Path) -> String {
    if let Some(t) = fm_title {
        return t.to_string();
    }
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            let mut chars = rest.chars();
            if let Some(first) = chars.next() {
                // `#\s+(.+)` — 先頭 1 文字が空白で、その後に最低 1 文字残っていれば match
                if first.is_whitespace() && chars.next().is_some() {
                    return rest.trim().to_string();
                }
            }
        }
    }
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// YAML 値の truthiness 判定 (null/false/0/空文字/空コレクションは falsy)。
fn yaml_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Sequence(s) => !s.is_empty(),
        Value::Mapping(m) => !m.is_empty(),
        Value::Tagged(t) => yaml_truthy(&t.value),
    }
}

/// スカラー値の文字列化 (bool の表記 "True"/"False" 含め出力は golden で固定)。
fn yaml_scalar_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Number(n) => n.to_string(),
        other => serde_yaml_ng::to_string(other)
            .map(|s| s.trim_end().to_string())
            .unwrap_or_default(),
    }
}

/// date/updated を `YYYY-MM-DD` 文字列へ正規化。空は ""。
/// YAML が date 型で解釈した場合も文字列化して揃える
/// (serde_yaml_ng は日付を文字列として読むため文字列化で十分)。
pub fn normalize_date(value: Option<&Value>) -> String {
    match value {
        Some(v) if yaml_truthy(v) => yaml_scalar_string(v),
        _ => String::new(),
    }
}

/// frontmatter の境界行 (`^-{3,}\s*$`, MULTILINE)。
fn fm_split_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^-{3,}\s*$").unwrap())
}

/// 厳密な frontmatter 区切り行 (`^---\s*$`, MULTILINE)。破損スキャン (閉じ `---` 検知) 用。
fn fm_delim_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)^---\s*$").unwrap())
}

/// frontmatter 開始の検知: 先頭行が 3 個以上の `-` + 空白のみ。
fn detect_frontmatter(text: &str) -> bool {
    let first = text.lines().next().unwrap_or("");
    let dashes = first.chars().take_while(|&c| c == '-').count();
    dashes >= 3 && first[dashes..].trim().is_empty()
}

/// frontmatter の分離。YAML パース失敗のみ None (呼び出し側で warning)。
/// 閉じ `---` 欠落はエラーにせず metadata 空 + 全文 content として扱う (破損検知は health が担う)。
fn split_frontmatter(text: &str) -> Option<(Mapping, String)> {
    if !detect_frontmatter(text) {
        return Some((Mapping::new(), text.to_string()));
    }
    let parts: Vec<&str> = fm_split_re().splitn(text, 3).collect();
    if parts.len() < 3 {
        // 閉じ boundary 無し → ValueError → metadata 空・content は全文のまま
        return Some((Mapping::new(), text.to_string()));
    }
    let fm = parts[1];
    let content = parts[2].trim().to_string();
    if fm.trim().is_empty() {
        return Some((Mapping::new(), content));
    }
    match serde_yaml_ng::from_str::<Value>(fm) {
        Err(_) => None,
        Ok(Value::Mapping(m)) => Some((m, content)),
        Ok(_) => Some((Mapping::new(), content)), // dict 以外は metadata 扱いしない
    }
}

fn meta_get<'a>(m: &'a Mapping, key: &str) -> Option<&'a Value> {
    m.get(Value::String(key.to_string()))
}

/// dir がノート repo のルートか (mikke.toml / .mikke.toml を持つか)。
fn has_config(dir: &Path) -> bool {
    CONFIG_FILENAMES.iter().any(|n| dir.join(n).is_file())
}

/// 対象 .md を (絶対パス, root 相対パス) で列挙。
/// 設定ファイルを持つサブディレクトリは「ネストしたノート repo」として、その配下の走査を
/// 子の [scan]/[index] 設定へ委譲する (rel は常に最上位 root 相対。docs/SPEC.md 参照)。
pub fn iter_notes(cfg: &Config) -> Vec<(PathBuf, PathBuf)> {
    let top_root = cfg.root.clone();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    for inc in &cfg.include {
        let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
        walk_include(cfg, inc, &top_root, &mut files);
        files.sort(); // PathBuf の Ord (コンポーネント単位) で走査順を決定的にする
        for (abs, rel) in files {
            if seen.insert(rel.clone()) {
                out.push((abs, rel));
            }
        }
    }
    out
}

/// repo (cfg) の include 起点 1 つを走査する。起点自身がネスト repo ならそちらへ委譲。
fn walk_include(cfg: &Config, inc: &str, top_root: &Path, files: &mut Vec<(PathBuf, PathBuf)>) {
    let base = match cfg.root.join(inc).canonicalize() {
        Ok(b) if b.is_dir() => b,
        _ => return,
    };
    // include がこの repo のルート外を指した場合は対象外
    let Ok(base_rel) = base.strip_prefix(&cfg.root) else {
        return;
    };
    // 起点自身が exclude_dirs 名の配下にある場合も対象外 (旧ファイル単位フィルタと同値)
    if base_rel.components().any(|c| {
        cfg.exclude_dirs
            .contains(&c.as_os_str().to_string_lossy().into_owned())
    }) {
        return;
    }
    if base != cfg.root && has_config(&base) {
        walk_nested(&base, top_root, files);
    } else {
        walk_dir(&base, cfg, top_root, files);
    }
}

/// ネスト repo のルート設定を読み、その include 起点群へ委譲走査する (孫以深も再帰)。
/// 子設定が壊れている場合は load_config が path 付きで即エラー終了する (silent に親規則で走査しない)。
///
/// 再帰は必ず停止する (サイクル不能): (1) ディレクトリ symlink は walk_dir が辿らない
/// (entry.file_type() は lstat 相当で、symlink は is_dir() false)、(2) repo 外を指す
/// include は walk_include の strip_prefix で弾かれる、(3) 委譲は「repo ルートの真部分
/// ディレクトリ」に対してのみ起きるため canonical パス深度が単調増加する。
fn walk_nested(root: &Path, top_root: &Path, files: &mut Vec<(PathBuf, PathBuf)>) {
    let child = load_config(root.to_path_buf());
    for inc in &child.include {
        walk_include(&child, inc, top_root, files);
    }
}

/// repo (cfg) の走査規則で dir 直下を歩く。ネスト repo 境界の検知は
/// 親の exclude_dirs 判定より後 (= 親はディレクトリ名で入口ごと塞げる)。
fn walk_dir(dir: &Path, cfg: &Config, top_root: &Path, files: &mut Vec<(PathBuf, PathBuf)>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            if cfg.exclude_dirs.contains(&name) {
                continue;
            }
            if has_config(&path) {
                walk_nested(&path, top_root, files);
            } else {
                walk_dir(&path, cfg, top_root, files);
            }
        } else if name.ends_with(".md") && path.is_file() {
            if cfg.exclude_files.contains(&name) {
                continue;
            }
            // index/embeddings の出力先除外はこの repo のルート相対 prefix で判定する
            let Ok(rel_repo) = path.strip_prefix(&cfg.root) else {
                continue;
            };
            let rel_repo_posix = to_posix(rel_repo);
            if cfg
                .exclude_path_prefixes
                .iter()
                .any(|pfx| rel_repo_posix.starts_with(&format!("{pfx}/")))
            {
                continue;
            }
            let Ok(rel_top) = path.strip_prefix(top_root) else {
                continue;
            };
            files.push((path.clone(), rel_top.to_path_buf()));
        }
    }
}

/// frontmatter を読んで Note を作る。読込/パース失敗は None (呼び出し側で warning)。
pub fn load_note(abs: &Path, rel: &Path) -> Option<Note> {
    let raw = read_utf8_sig(abs).ok()?;
    let text = raw.trim(); // 前後の空白/空行を除いてから frontmatter を判定する
    let (metadata, content) = split_frontmatter(text)?;

    let fm_title = meta_get(&metadata, "title")
        .filter(|v| yaml_truthy(v))
        .map(yaml_scalar_string);
    let title = extract_title(fm_title.as_deref(), &content, abs);

    let tags: Vec<String> = match meta_get(&metadata, "tags") {
        Some(Value::Sequence(seq)) => seq.iter().map(yaml_scalar_string).collect(),
        // tags を文字列で書くと 1 文字ずつ分解される (歴史的 quirk。変えるなら golden ごと意図的に)
        Some(Value::String(s)) => s.chars().map(String::from).collect(),
        _ => Vec::new(),
    };
    let date = normalize_date(meta_get(&metadata, "date"));
    let updated = normalize_date(meta_get(&metadata, "updated"));
    let summary = meta_get(&metadata, "summary")
        .filter(|v| yaml_truthy(v))
        .map(yaml_scalar_string)
        .unwrap_or_default();

    Some(Note {
        path_rel: to_posix(rel),
        title,
        tags,
        date,
        updated,
        summary,
        content,
    })
}

/// frontmatter の silent 破損検知。問題なければ None、あれば (種別, 詳細)。
pub fn scan_frontmatter_issue(path: &Path) -> Option<(String, String)> {
    let text = match read_utf8_sig(path) {
        Ok(t) => t,
        Err(e) => {
            let msg = e.to_string();
            let first = msg.lines().next().unwrap_or("").to_string();
            return Some(("読込不可".to_string(), first));
        }
    };
    if !text.starts_with("---") {
        return None; // frontmatter 無しは破損ではない (タグ/要約欠落として health が拾う)
    }
    let first_nl = match text.find('\n') {
        Some(i) => i,
        None => {
            return Some((
                "閉じ---欠落".to_string(),
                "先頭 '---' のみで本文が無い".to_string(),
            ))
        }
    };
    let m = match fm_delim_re().find_at(&text, first_nl + 1) {
        Some(m) => m,
        None => {
            return Some((
                "閉じ---欠落".to_string(),
                "終端 '---' が無く metadata が silent に喪失する".to_string(),
            ))
        }
    };
    let fm = &text[first_nl + 1..m.start()];
    if !fm.trim().is_empty() {
        if let Err(e) = serde_yaml_ng::from_str::<Value>(fm) {
            let msg = e.to_string();
            let first = msg.lines().next().unwrap_or("").to_string();
            return Some(("YAMLエラー".to_string(), first));
        }
    }
    None
}

/// utf-8-sig 相当読み込み: UTF-8 として読み、先頭 BOM があれば除去。
pub fn read_utf8_sig(path: &Path) -> std::io::Result<String> {
    let raw = std::fs::read(path)?;
    let s = String::from_utf8_lossy(&raw).into_owned();
    Ok(s.strip_prefix('\u{feff}')
        .map(|x| x.to_string())
        .unwrap_or(s))
}
