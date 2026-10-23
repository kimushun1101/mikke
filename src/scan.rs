//! ノート走査と frontmatter 解析の共通処理 (index / search / health / embed で共用)。

#![allow(dead_code)]

use crate::config::{
    display_path, load_scan_config, to_posix, warn_once, Config, CONFIG_FILENAMES,
};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::Match;
use regex::Regex;
use serde_yaml_ng::{Mapping, Value};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;

/// 1 ノートのパース結果。
#[derive(Debug, Clone)]
pub struct Note {
    pub path_rel: String, // root 相対 posix パス
    pub title: String,
    pub tags: Vec<String>,
    /// YAML の null / 空文字列だった tags 要素数。index/health で修正箇所を報告する。
    pub empty_tag_count: usize,
    pub date: String, // YYYY-MM-DD 正規化済 (無ければ "")
    pub updated: String,
    pub summary: String,
    pub content: String, // frontmatter を除いた本文
}

/// 最上位 root から委譲したネスト repo と、その配下のノート数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedRepo {
    pub root_rel: String,
    pub config_file: String,
    pub note_count: usize,
}

/// 重複除去後のノートと、走査中に委譲したネスト repo。
pub struct ScanResult {
    pub notes: Vec<(PathBuf, PathBuf)>,
    pub nested_repos: Vec<NestedRepo>,
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

/// tags の sequence を YAML の解釈どおりに文字列化する。
/// null / 空文字列は登録せず、有効な値は最初の出現順を保って重複除去する。
fn parse_tags(seq: &[Value]) -> (Vec<String>, usize) {
    let mut seen = HashSet::new();
    let mut tags = Vec::new();
    let mut empty_count = 0;
    for value in seq {
        if matches!(value, Value::Null) || matches!(value, Value::String(s) if s.is_empty()) {
            empty_count += 1;
            continue;
        }
        let tag = yaml_scalar_string(value);
        if seen.insert(tag.clone()) {
            tags.push(tag);
        }
    }
    (tags, empty_count)
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
    scan_notes(cfg).notes
}

/// 対象 .md とネスト repo の要約を列挙。
pub fn scan_notes(cfg: &Config) -> ScanResult {
    let top_root = cfg.root.clone();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut nested: BTreeSet<(String, String)> = BTreeSet::new();
    for inc in &cfg.include {
        let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
        walk_include(cfg, inc, &top_root, &mut files, &mut nested);
        files.sort(); // PathBuf の Ord (コンポーネント単位) で走査順を決定的にする
        for (abs, rel) in files {
            if seen.insert(rel.clone()) {
                out.push((abs, rel));
            }
        }
    }
    let nested_repos = nested
        .into_iter()
        .map(|(root_rel, config_file)| NestedRepo {
            note_count: out
                .iter()
                .filter(|(_, rel)| under_prefix(&to_posix(rel), &root_rel))
                .count(),
            root_rel,
            config_file,
        })
        .collect();
    ScanResult {
        notes: out,
        nested_repos,
    }
}

/// rel (repo root 相対の posix パス) が prefix そのものか、その配下か (要素単位。state は statement に当たらない)。
fn under_prefix(rel: &str, prefix: &str) -> bool {
    rel == prefix || (rel.starts_with(prefix) && rel.as_bytes().get(prefix.len()) == Some(&b'/'))
}

/// [scan] exclude_paths に当たるか。
fn excluded_by_path(cfg: &Config, rel_repo: &str) -> bool {
    cfg.exclude_paths.iter().any(|p| under_prefix(rel_repo, p))
}

/// index / embeddings の出力先の配下のファイルか (ディレクトリには当てない。出力先の下のネスト repo は歩く)。
fn in_output_dir(cfg: &Config, rel_repo: &str) -> bool {
    cfg.exclude_path_prefixes.iter().any(|p| {
        rel_repo.starts_with(p.as_str()) && rel_repo.as_bytes().get(p.len()) == Some(&b'/')
    })
}

/// [scan] respect_gitignore の判定。浅い順に (置き場所のディレクトリ, matcher) を持つ。
#[derive(Clone, Default)]
struct GitIgnores(Vec<(Rc<Path>, Rc<Gitignore>)>);

impl GitIgnores {
    /// root より上で最も近い `.git` を持つディレクトリから root までの規則を積む
    /// (`.git` が無ければ root の .gitignore だけ)。root 自身とその上は判定しない。
    fn for_root(root: &Path) -> Self {
        let mut dirs: Vec<&Path> = Vec::new();
        let mut found = false;
        for a in root.ancestors() {
            dirs.push(a);
            if a.join(".git").exists() {
                found = true;
                break;
            }
        }
        if !found {
            dirs = vec![root];
        }
        dirs.into_iter()
            .rev()
            .fold(Self::default(), |g, d| g.enter(d))
    }

    /// dir に入った後の規則。`.git` を持つ dir (git repo の境界) では外側の規則を捨てる。
    /// info/exclude を先に積むので、同じ repo のどの .gitignore よりも優先度が低い (git と同じ)。
    fn enter(&self, dir: &Path) -> Self {
        let git = dir.join(".git");
        let mut next = if git.exists() {
            let mut g = Self::default();
            let exclude = git.join("info").join("exclude");
            if git.is_dir() && exclude.is_file() {
                g.push(dir, &exclude);
            }
            g
        } else {
            self.clone()
        };
        let gitignore = dir.join(".gitignore");
        if gitignore.is_file() {
            next.push(dir, &gitignore);
        }
        next
    }

    fn push(&mut self, dir: &Path, file: &Path) {
        if let Some(gi) = build_ignore(dir, file) {
            self.0.push((Rc::from(dir), Rc::new(gi)));
        }
    }

    /// 深い層から照らし、最初に一致した結果 (無視 / 否定) を返す。
    /// 各層の置き場所からの相対パスで照らす (Windows の verbatim パスでも前方一致に頼らないため)。
    fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        for (dir, gi) in self.0.iter().rev() {
            let Ok(rel) = path.strip_prefix(dir) else {
                continue;
            };
            if rel.as_os_str().is_empty() {
                continue;
            }
            match gi.matched(rel, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }
}

/// file (.gitignore か info/exclude) の matcher を作る。大文字小文字は区別する。
/// 解釈できない行は警告して捨て、残りの行は使う。UTF-8 でない行も、その行だけを読み替えて残りを生かす
/// (GitignoreBuilder::add は UTF-8 でない行で読むのをやめるため、行ごとに渡す)。
fn build_ignore(dir: &Path, file: &Path) -> Option<Gitignore> {
    let mut b = GitignoreBuilder::new(dir);
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            warn_once(format!(
                "{}: 読めないため無視します ({e})",
                display_path(file)
            ));
            return None;
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    for (i, line) in text.lines().enumerate() {
        if let Err(e) = b.add_line(Some(file.to_path_buf()), line) {
            warn_once(format!(
                "{}: {} 行目を解釈できないため無視します ({e})",
                display_path(file),
                i + 1
            ));
        }
    }
    match b.build() {
        Ok(gi) if gi.is_empty() => None,
        Ok(gi) => Some(gi),
        Err(e) => {
            warn_once(format!(
                "{}: 一部を解釈できないため無視します ({e})",
                display_path(file)
            ));
            None
        }
    }
}

/// repo (cfg) の include 起点 1 つを走査する。起点自身がネスト repo ならそちらへ委譲。
fn walk_include(
    cfg: &Config,
    inc: &str,
    top_root: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
    nested: &mut BTreeSet<(String, String)>,
) {
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
    // 起点が exclude_paths の配下
    if excluded_by_path(cfg, &to_posix(base_rel)) {
        return;
    }
    let mut ig = cfg
        .respect_gitignore
        .then(|| GitIgnores::for_root(&cfg.root));
    if let Some(g) = ig.as_mut() {
        // root 自身は判定しない。root から起点まで 1 段ずつ判定しながら層を積む
        let mut d = cfg.root.clone();
        for c in base_rel.components() {
            d.push(c);
            if g.ignored(&d, true) {
                return;
            }
            *g = g.enter(&d);
        }
    }
    if base != cfg.root && has_config(&base) {
        walk_nested(cfg, &base, top_root, files, nested);
    } else {
        walk_dir(&base, cfg, ig.as_ref(), top_root, files, nested);
    }
}

/// ネスト repo のルート設定を読み、その include 起点群へ委譲走査する (孫以深も再帰)。
/// load_scan_config が子の [scan]/[index] と TOML の構文を検査し、誤りは path 付きで即エラー終了する
/// (silent に親規則で走査しない)。
///
/// 再帰は必ず停止する (サイクル不能): (1) ディレクトリ symlink は walk_dir が辿らない
/// (entry.file_type() は lstat 相当で、symlink は is_dir() false)、(2) repo 外を指す
/// include は walk_include の strip_prefix で弾かれる、(3) 委譲は「repo ルートの真部分
/// ディレクトリ」に対してのみ起きるため canonical パス深度が単調増加する。
fn walk_nested(
    parent: &Config,
    root: &Path,
    top_root: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
    nested: &mut BTreeSet<(String, String)>,
) {
    // 親の exclude_paths は境界の入口までしか効かない。中を指す値は知らせる
    if let (Ok(r), Some(cfg_path)) = (root.strip_prefix(&parent.root), &parent.config_path) {
        let rel = to_posix(r);
        let pfx = format!("{rel}/");
        for v in &parent.exclude_paths {
            if let Some(rest) = v.strip_prefix(&pfx) {
                warn_once(format!(
                    "{}: [scan] exclude_paths の \"{v}\" はネストしたノート repo ({rel}) の中を指すため効きません ({rel} の設定ファイルの exclude_paths に \"{rest}\" と書きます)",
                    display_path(cfg_path)
                ));
            }
        }
    }
    let child = load_scan_config(root.to_path_buf());
    if let (Ok(rel), Some(config_path)) = (root.strip_prefix(top_root), &child.config_path) {
        nested.insert((
            to_posix(rel),
            config_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ));
    }
    for inc in &child.include {
        walk_include(&child, inc, top_root, files, nested);
    }
}

/// repo (cfg) の走査規則で dir 直下を歩く。親の exclude_dirs / exclude_paths / .gitignore の判定は
/// ネスト境界の検知より先 (= 入口ごと塞げる)。
fn walk_dir(
    dir: &Path,
    cfg: &Config,
    ig: Option<&GitIgnores>,
    top_root: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
    nested: &mut BTreeSet<(String, String)>,
) {
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
        // exclude_paths と出力先の除外は、この repo のルート相対で判定する
        let Ok(rel_repo) = path.strip_prefix(&cfg.root) else {
            continue;
        };
        let rel_repo = to_posix(rel_repo);
        if ft.is_dir() {
            if cfg.exclude_dirs.contains(&name)
                || excluded_by_path(cfg, &rel_repo)
                || ig.is_some_and(|g| g.ignored(&path, true))
            {
                continue;
            }
            if has_config(&path) {
                walk_nested(cfg, &path, top_root, files, nested);
            } else {
                let next = ig.map(|g| g.enter(&path));
                walk_dir(&path, cfg, next.as_ref(), top_root, files, nested);
            }
        } else if name.ends_with(".md") && path.is_file() {
            if cfg.exclude_files.contains(&name)
                || excluded_by_path(cfg, &rel_repo)
                || in_output_dir(cfg, &rel_repo)
                || ig.is_some_and(|g| g.ignored(&path, false))
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

    let (tags, empty_tag_count) = match meta_get(&metadata, "tags") {
        Some(Value::Sequence(seq)) => parse_tags(seq),
        // tags を文字列で書くと 1 文字ずつ分解される (歴史的 quirk。変えるなら golden ごと意図的に)
        Some(Value::String(s)) => {
            let mut seen = HashSet::new();
            (
                s.chars()
                    .map(String::from)
                    .filter(|tag| seen.insert(tag.clone()))
                    .collect(),
                0,
            )
        }
        _ => (Vec::new(), 0),
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
        empty_tag_count,
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

#[cfg(test)]
mod tests {
    use super::parse_tags;
    use serde_yaml_ng::Value;

    #[test]
    fn tags_drop_empty_and_deduplicate_in_input_order() {
        let value: Value = serde_yaml_ng::from_str(
            "- #Dog\n- valid\n- #Cat\n- valid\n- \"#Dog\"\n- \"\"\n- other\n",
        )
        .unwrap();
        let Value::Sequence(seq) = value else {
            panic!("sequence ではない");
        };

        let (tags, empty_count) = parse_tags(&seq);
        assert_eq!(tags, ["valid", "#Dog", "other"]);
        assert_eq!(empty_count, 3);
    }
}
