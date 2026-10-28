//! 設定 (mikke.toml / .mikke.toml) 読み込みとルート解決。
//!
//! ルート解決の優先順: --root 引数 > 環境変数 MIKKE_ROOT > cwd から上方に
//! mikke.toml / .mikke.toml を探索。型不一致は silent に誤動作させず即エラー終了する
//! (特に「文字列配列指定に文字列を渡すと 1 文字ずつに分解される」事故を型検査で弾く)。
//! 未知の節・キーと効かない値は警告にとどめる (docs/SPEC.md「設定読み込みの厳格さ」)。

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 設定ファイル名の探索順 (先頭が優先)。隠したい repo 向けに dotfile 変種も受ける。
pub const CONFIG_FILENAMES: &[&str] = &["mikke.toml", ".mikke.toml"];

/// 設定スキーマの全キー。docs/SPEC.md「設定スキーマ」と揃える (unit test で照合する)。
/// 未知の節・キーの警告と候補の提示に使う。
const KNOWN_KEYS: &[(&str, &[&str])] = &[
    (
        "scan",
        &[
            "include",
            "exclude_dirs",
            "exclude_files",
            "exclude_paths",
            "respect_gitignore",
        ],
    ),
    ("index", &["path", "embeddings_dir", "auto_rebuild"]),
    (
        "semantic",
        &["enabled", "model", "query_prefix", "passage_prefix"],
    ),
    (
        "search",
        &[
            "bm25_limit",
            "rrf_k",
            "bm25_weight",
            "vector_weight",
            "candidate_factor",
        ],
    ),
    (
        "health",
        &[
            "scan_skip_prefixes",
            "quality_skip_prefixes",
            "min_words",
            "exec_bit_prefixes",
            "disable",
        ],
    ),
];
/// ネストした repo の設定で読む節 (docs/SPEC.md「ネストしたノート repo」)。
const NESTED_SECTIONS: &[&str] = &["scan", "index"];

static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// 設定の警告を stderr に出す。同じ文面は 1 回の実行で 1 度だけ出す
/// (ネスト repo の設定は走査のたびに読み直し、auto_rebuild の検索では最大 3 回読むため)。
pub fn warn_once(msg: String) {
    let mut w = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    if w.insert(msg.clone()) {
        eprintln!("Warning: {msg}");
    }
}

/// dir 直下の設定ファイルを優先順で返す。両方あれば mikke.toml 優先 + stderr 警告。
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    let existing: Vec<PathBuf> = CONFIG_FILENAMES
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .collect();
    if existing.len() > 1 {
        warn_once(format!(
            "{} に mikke.toml と .mikke.toml が両方あります。mikke.toml を使用します。",
            display_path(dir)
        ));
    }
    existing.into_iter().next()
}

const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    ".obsidian",
    ".claude",
    ".agents",
    ".codex",
    ".cursor",
    ".gemini",
    ".git",
    ".venv",
    "__pycache__",
    "node_modules",
    "templates",
    "dist",
    "build",
];
const DEFAULT_EXCLUDE_FILES: &[&str] = &["README.md", "CLAUDE.md", "AGENTS.md", "GEMINI.md"];
const DEFAULT_INDEX_PATH: &str = ".mikke/index.sqlite";
const DEFAULT_EMBEDDINGS_DIR: &str = ".mikke/embeddings";
const DEFAULT_MODEL: &str = "intfloat/multilingual-e5-small";

/// [health] disable で指定できるチェック名 (docs/SPEC.md「health 判定」)。
/// 呼び出し側 (health.rs) もこの定数を参照し、綴りミスをコンパイル時に検知する。
pub const HEALTH_CHECK_FRONTMATTER: &str = "frontmatter";
pub const HEALTH_CHECK_TAGS: &str = "tags";
pub const HEALTH_CHECK_SUMMARY: &str = "summary";
pub const HEALTH_CHECK_LOW_WORDS: &str = "low_words";
pub const HEALTH_CHECK_UPDATED: &str = "updated";
const HEALTH_CHECK_NAMES: &[&str] = &[
    HEALTH_CHECK_FRONTMATTER,
    HEALTH_CHECK_TAGS,
    HEALTH_CHECK_SUMMARY,
    HEALTH_CHECK_LOW_WORDS,
    HEALTH_CHECK_UPDATED,
];

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    /// 読んだ設定ファイル (無ければ None)。警告の文面に使う。
    pub config_path: Option<PathBuf>,
    // [scan]
    pub include: Vec<String>,
    pub exclude_dirs: HashSet<String>,
    pub exclude_files: HashSet<String>,
    /// [scan] exclude_paths を正規化した値 (root 相対 posix、重複除去)。
    pub exclude_paths: Vec<String>,
    pub respect_gitignore: bool,
    // [index]
    pub index_rel: String,
    pub embeddings_rel: String,
    pub auto_rebuild: bool,
    // [semantic]
    pub semantic_enabled: bool,
    pub model: String,
    pub query_prefix: String,
    pub passage_prefix: String,
    // [search]
    pub bm25_limit: i64,
    pub rrf_k: i64,
    pub bm25_weight: f64,
    pub vector_weight: f64,
    pub candidate_factor: i64,
    // [health]
    pub scan_skip_prefixes: Vec<String>,
    pub quality_skip_prefixes: Vec<String>,
    pub min_words: i64,
    pub exec_bit_prefixes: Vec<String>,
    pub disable: Vec<String>,
    // index / embeddings の出力先から自動で算出した root 相対の prefix (配下の .md ファイルだけを外す)。
    pub exclude_path_prefixes: Vec<String>,
}

impl Config {
    fn with_defaults(root: PathBuf) -> Self {
        Config {
            root,
            config_path: None,
            include: vec![".".to_string()],
            exclude_dirs: DEFAULT_EXCLUDE_DIRS.iter().map(|s| s.to_string()).collect(),
            exclude_files: DEFAULT_EXCLUDE_FILES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            exclude_paths: vec![],
            respect_gitignore: false,
            index_rel: DEFAULT_INDEX_PATH.to_string(),
            embeddings_rel: DEFAULT_EMBEDDINGS_DIR.to_string(),
            auto_rebuild: false,
            semantic_enabled: false,
            model: DEFAULT_MODEL.to_string(),
            query_prefix: "query: ".to_string(),
            passage_prefix: "passage: ".to_string(),
            bm25_limit: 50,
            rrf_k: 60,
            bm25_weight: 0.4,
            vector_weight: 0.6,
            candidate_factor: 4,
            scan_skip_prefixes: vec![],
            quality_skip_prefixes: vec![],
            min_words: 50,
            exec_bit_prefixes: vec![],
            disable: vec![],
            exclude_path_prefixes: vec![],
        }
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join(&self.index_rel)
    }

    pub fn embeddings_dir(&self) -> PathBuf {
        self.root.join(&self.embeddings_rel)
    }

    /// [health] disable に挙げられたチェックか (許容名の検査は load_config 側)。
    pub fn health_disabled(&self, check: &str) -> bool {
        self.disable.iter().any(|d| d == check)
    }
}

fn config_error(cfg_path: &Path, msg: &str) -> ! {
    eprintln!("Error: {}: {msg}", display_path(cfg_path));
    std::process::exit(2);
}

fn err_root(msg: &str) -> ! {
    eprintln!("Error: {msg}");
    std::process::exit(2);
}

/// 検索対象 repo のルートを決定する。
pub fn resolve_root(cli_root: Option<&str>) -> PathBuf {
    if let Some(cli) = cli_root {
        let p = PathBuf::from(cli);
        if !p.is_dir() {
            err_root(&format!("--root が存在しません: {cli}"));
        }
        return std::fs::canonicalize(&p).unwrap_or(p);
    }
    if let Ok(env_root) = std::env::var("MIKKE_ROOT") {
        if !env_root.is_empty() {
            let p = PathBuf::from(&env_root);
            if !p.is_dir() {
                err_root(&format!("MIKKE_ROOT が存在しません: {env_root}"));
            }
            return std::fs::canonicalize(&p).unwrap_or(p);
        }
    }
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| std::fs::canonicalize(&c).ok().or(Some(c)))
        .unwrap_or_else(|| PathBuf::from("."));
    for d in cwd.ancestors() {
        // 発見のみ (警告は load_config 側の find_config_file が一度だけ出す)
        if CONFIG_FILENAMES.iter().any(|n| d.join(n).is_file()) {
            return d.to_path_buf();
        }
    }
    let mut msg = format!(
        "ルートを特定できません (cwd: {})。次のどれかで root を指定してください。\n  - ノートフォルダのルートに空の mikke.toml (または .mikke.toml) を置く\n  - --root <PATH> を付ける\n  - 環境変数 MIKKE_ROOT を設定する",
        display_path(&cwd)
    );
    if let Some(d) = cwd.ancestors().find(|d| d.join(".git").exists()) {
        msg.push_str(&format!(
            "\n  {} に .git があります。0.6 までは、git repo の root を root にしていました。",
            display_path(d)
        ));
    }
    err_root(&msg);
}

// --- TOML 型検査ヘルパ (型不一致は即エラー終了 — docs/SPEC.md「設定読み込みの厳格さ」) ---

fn get_table(cfg_path: &Path, data: &toml::Value, key: &str) -> toml::value::Table {
    match data.get(key) {
        None => toml::value::Table::new(),
        Some(toml::Value::Table(t)) => t.clone(),
        Some(_) => config_error(cfg_path, &format!("[{key}] はテーブルで指定してください")),
    }
}

fn str_list(
    cfg_path: &Path,
    section: &toml::value::Table,
    sec: &str,
    key: &str,
    default: Vec<String>,
) -> Vec<String> {
    match section.get(key) {
        None => default,
        Some(toml::Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                match v {
                    toml::Value::String(s) => out.push(s.clone()),
                    // 文字列を渡すと 1 文字ずつに分解される事故を型検査で弾く
                    _ => config_error(cfg_path, &format!(
                        "[{sec}] {key} は文字列の配列で指定してください (例: {key} = [\"a\", \"b\"])"
                    )),
                }
            }
            out
        }
        Some(_) => config_error(
            cfg_path,
            &format!("[{sec}] {key} は文字列の配列で指定してください (例: {key} = [\"a\", \"b\"])"),
        ),
    }
}

fn scalar_str(
    cfg_path: &Path,
    section: &toml::value::Table,
    sec: &str,
    key: &str,
    default: String,
) -> String {
    match section.get(key) {
        None => default,
        Some(toml::Value::String(s)) => s.clone(),
        Some(_) => config_error(
            cfg_path,
            &format!("[{sec}] {key} は文字列で指定してください"),
        ),
    }
}

fn scalar_bool(
    cfg_path: &Path,
    section: &toml::value::Table,
    sec: &str,
    key: &str,
    default: bool,
) -> bool {
    match section.get(key) {
        None => default,
        Some(toml::Value::Boolean(b)) => *b,
        Some(_) => config_error(
            cfg_path,
            &format!("[{sec}] {key} は真偽値で指定してください"),
        ),
    }
}

fn scalar_int(
    cfg_path: &Path,
    section: &toml::value::Table,
    sec: &str,
    key: &str,
    default: i64,
) -> i64 {
    match section.get(key) {
        None => default,
        // toml::Value は bool と Integer が別型なので真偽値の混入はここで弾かれる
        Some(toml::Value::Integer(n)) => *n,
        Some(_) => config_error(cfg_path, &format!("[{sec}] {key} は整数で指定してください")),
    }
}

fn scalar_num(
    cfg_path: &Path,
    section: &toml::value::Table,
    sec: &str,
    key: &str,
    default: f64,
) -> f64 {
    match section.get(key) {
        None => default,
        Some(toml::Value::Integer(n)) => *n as f64,
        Some(toml::Value::Float(f)) => *f,
        Some(_) => config_error(cfg_path, &format!("[{sec}] {key} は数値で指定してください")),
    }
}

// --- 未知のキーと効かない値の検査 (警告のみ — docs/SPEC.md「設定読み込みの厳格さ」) ---

/// char 単位の Levenshtein 距離。
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// name に近い候補を 1 つ返す。距離 2 以下かつ名前の長さ未満のうち最小 (同点は先に出たもの)。
fn near<'a>(name: &str, cands: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let len = name.chars().count();
    let mut best: Option<(usize, &'a str)> = None;
    for c in cands {
        let d = edit_distance(name, c);
        if d <= 2 && d < len && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c)
}

/// key を持つ節を表の順で探す (except の節は除く)。
fn section_having(key: &str, except: Option<&str>) -> Option<&'static str> {
    KNOWN_KEYS
        .iter()
        .find(|(s, keys)| Some(*s) != except && keys.contains(&key))
        .map(|(s, _)| *s)
}

fn hint(h: Option<String>) -> String {
    h.map(|h| format!(" ({h} ?)")).unwrap_or_default()
}

/// 未知の節・キーの警告文を返す。sections は中のキーを検査する節
/// (最上位では全節、ネストでは NESTED_SECTIONS)。表に無い名前の節は常に報告する。
fn unknown_key_messages(p: &Path, data: &toml::Value, sections: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(top) = data.as_table() else {
        return out;
    };
    for (name, value) in top {
        if let Some((_, keys)) = KNOWN_KEYS.iter().find(|(s, _)| s == name) {
            // 読まない節と、テーブルでない値 (型エラーは get_table が出す) は見ない
            if !sections.contains(&name.as_str()) {
                continue;
            }
            let Some(t) = value.as_table() else {
                continue;
            };
            for k in t.keys() {
                if keys.contains(&k.as_str()) {
                    continue;
                }
                let h = section_having(k, Some(name))
                    .map(|s| format!("[{s}] {k}"))
                    .or_else(|| near(k, keys.iter().copied()).map(str::to_string));
                out.push(format!(
                    "{}: 未知のキー [{name}] {k} を無視します{}",
                    display_path(p),
                    hint(h)
                ));
            }
        } else if value.is_table() {
            let h = near(name, KNOWN_KEYS.iter().map(|(s, _)| *s)).map(|s| format!("[{s}]"));
            out.push(format!(
                "{}: 未知の節 [{name}] を無視します{}",
                display_path(p),
                hint(h)
            ));
        } else {
            let h = section_having(name, None).map(|s| format!("[{s}] {name}"));
            out.push(format!(
                "{}: 節の外の未知のキー {name} を無視します{}",
                display_path(p),
                hint(h)
            ));
        }
    }
    out
}

/// [scan] exclude_paths の値を root 相対の posix パスにそろえる。Err は理由 (文の述部)。
fn normalize_exclude_path(v: &str) -> Result<String, &'static str> {
    if v.is_empty() {
        return Err("空文字です");
    }
    if v.chars().any(char::is_control) {
        return Err("制御文字を含みます (TOML の \"...\" の中では \\ がエスケープになります)");
    }
    if v.contains('\\') {
        return Err("`\\` を含みます");
    }
    let b = v.as_bytes();
    if v.starts_with('/') || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') {
        return Err("絶対パスです");
    }
    let parts: Vec<&str> = v
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect();
    if parts.contains(&"..") {
        return Err("`..` を含みます");
    }
    if parts.is_empty() {
        return Err("root そのものを指します");
    }
    Ok(parts.join("/"))
}

/// exclude_dirs / exclude_files の値の警告 (名前の完全一致で比べるため効かない値)。
fn warn_name_values(p: &Path, key: &str, what: &str, values: &[String]) {
    for v in values {
        if v.contains('/') || (cfg!(windows) && v.contains('\\')) {
            warn_once(format!(
                "{}: [scan] {key} の \"{v}\" はどの{what}とも一致しません (名前の完全一致で比べます。root 相対のパスで外すには [scan] exclude_paths を使います)",
                display_path(p)
            ));
        } else if v.contains('*') || v.contains('?') {
            warn_once(format!(
                "{}: [scan] {key} の \"{v}\" は glob として扱いません (名前の完全一致で比べます。パターンで外すには [scan] respect_gitignore = true と .gitignore を使います)",
                display_path(p)
            ));
        }
    }
}

/// <root>/mikke.toml (無ければ .mikke.toml) を全節読む。どちらも無ければデフォルト設定を返す。
pub fn load_config(root: PathBuf) -> Config {
    load(root, true)
}

/// ネスト repo 用。[scan] と [index] だけを読み、検査する
/// (子の [health]/[search]/[semantic] は使わないので読まない — docs/SPEC.md「ネストしたノート repo」)。
pub fn load_scan_config(root: PathBuf) -> Config {
    load(root, false)
}

fn load(root: PathBuf, full: bool) -> Config {
    let mut cfg = Config::with_defaults(root.clone());
    if let Some(toml_path) = find_config_file(&root) {
        let raw = match std::fs::read(&toml_path) {
            Ok(b) => b,
            Err(e) => config_error(&toml_path, &format!("読み込みに失敗: {e}")),
        };
        // utf-8-sig: Windows エディタが付ける BOM を許容する
        let text = strip_bom(&String::from_utf8_lossy(&raw));
        let data: toml::Value = match toml::from_str(&text) {
            Ok(v) => v,
            Err(e) => config_error(&toml_path, &format!("読み込みに失敗: {e}")),
        };
        let p = toml_path.as_path();
        cfg.config_path = Some(toml_path.clone());
        let all_sections: Vec<&str> = KNOWN_KEYS.iter().map(|(s, _)| *s).collect();
        let sections = if full {
            all_sections.as_slice()
        } else {
            NESTED_SECTIONS
        };
        for msg in unknown_key_messages(p, &data, sections) {
            warn_once(msg);
        }

        let scan = get_table(p, &data, "scan");
        cfg.include = str_list(p, &scan, "scan", "include", cfg.include);
        let exclude_dirs = str_list(p, &scan, "scan", "exclude_dirs", sorted(&cfg.exclude_dirs));
        let exclude_files = str_list(
            p,
            &scan,
            "scan",
            "exclude_files",
            sorted(&cfg.exclude_files),
        );
        cfg.respect_gitignore =
            scalar_bool(p, &scan, "scan", "respect_gitignore", cfg.respect_gitignore);
        for v in str_list(p, &scan, "scan", "exclude_paths", vec![]) {
            match normalize_exclude_path(&v) {
                Ok(n) => {
                    if !cfg.exclude_paths.contains(&n) {
                        cfg.exclude_paths.push(n);
                    }
                }
                Err(reason) => config_error(
                    p,
                    &format!(
                        "[scan] exclude_paths の {} は {reason}。root 相対のパスを / 区切りで書いてください (例: exclude_paths = [\"state\"])",
                        // 制御文字は見えるように Debug で出す
                        if v.chars().any(char::is_control) {
                            format!("{v:?}")
                        } else {
                            format!("\"{v}\"")
                        }
                    ),
                ),
            }
        }
        // 黙って効かない値の警告 (既定値には当たる値が無い)
        warn_name_values(p, "exclude_dirs", "ディレクトリ名", &exclude_dirs);
        warn_name_values(p, "exclude_files", "ファイル名", &exclude_files);
        for v in &cfg.exclude_paths {
            if v.contains('*') || v.contains('?') {
                warn_once(format!(
                    "{}: [scan] exclude_paths の \"{v}\" は glob として扱いません (root 相対のパスの完全一致で比べます)",
                    display_path(p)
                ));
            }
        }
        for v in &cfg.include {
            if cfg.root.join(v).is_file() {
                warn_once(format!(
                    "{}: [scan] include の \"{v}\" はファイルのため走査しません (include にはディレクトリを書きます)",
                    display_path(p)
                ));
            }
        }
        cfg.exclude_dirs = exclude_dirs.into_iter().collect();
        cfg.exclude_files = exclude_files.into_iter().collect();

        let index = get_table(p, &data, "index");
        cfg.index_rel = scalar_str(p, &index, "index", "path", cfg.index_rel);
        // 走査は名前が .md で終わるファイルを拾うため、index がノートとして走査され、既存のノートも上書きされ得る
        // (大文字小文字を区別しない FS では .MD も既存のノートを指す)
        if Path::new(&cfg.index_rel)
            .file_name()
            .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().ends_with(".md"))
        {
            config_error(
                p,
                &format!(
                    "[index] path に .md で終わる path は指定できません (ノートとして走査され、既存のノートを上書きするおそれがあるため): {}",
                    cfg.index_rel
                ),
            );
        }
        cfg.embeddings_rel = scalar_str(p, &index, "index", "embeddings_dir", cfg.embeddings_rel);
        cfg.auto_rebuild = scalar_bool(p, &index, "index", "auto_rebuild", cfg.auto_rebuild);

        if full {
            let semantic = get_table(p, &data, "semantic");
            cfg.semantic_enabled =
                scalar_bool(p, &semantic, "semantic", "enabled", cfg.semantic_enabled);
            cfg.model = scalar_str(p, &semantic, "semantic", "model", cfg.model);
            cfg.query_prefix =
                scalar_str(p, &semantic, "semantic", "query_prefix", cfg.query_prefix);
            cfg.passage_prefix = scalar_str(
                p,
                &semantic,
                "semantic",
                "passage_prefix",
                cfg.passage_prefix,
            );

            let search = get_table(p, &data, "search");
            cfg.bm25_limit = scalar_int(p, &search, "search", "bm25_limit", cfg.bm25_limit);
            cfg.rrf_k = scalar_int(p, &search, "search", "rrf_k", cfg.rrf_k);
            cfg.bm25_weight = scalar_num(p, &search, "search", "bm25_weight", cfg.bm25_weight);
            cfg.vector_weight =
                scalar_num(p, &search, "search", "vector_weight", cfg.vector_weight);
            cfg.candidate_factor = scalar_int(
                p,
                &search,
                "search",
                "candidate_factor",
                cfg.candidate_factor,
            );

            let health = get_table(p, &data, "health");
            cfg.scan_skip_prefixes = str_list(
                p,
                &health,
                "health",
                "scan_skip_prefixes",
                cfg.scan_skip_prefixes,
            );
            cfg.quality_skip_prefixes = str_list(
                p,
                &health,
                "health",
                "quality_skip_prefixes",
                cfg.quality_skip_prefixes,
            );
            cfg.min_words = scalar_int(p, &health, "health", "min_words", cfg.min_words);
            cfg.exec_bit_prefixes = str_list(
                p,
                &health,
                "health",
                "exec_bit_prefixes",
                cfg.exec_bit_prefixes,
            );
            cfg.disable = str_list(p, &health, "health", "disable", cfg.disable);
            // 未知名は typo を silent no-op にせず即エラー終了する (「設定読み込みの厳格さ」)
            for name in &cfg.disable {
                if !HEALTH_CHECK_NAMES.contains(&name.as_str()) {
                    config_error(
                        p,
                        &format!(
                            "[health] disable に未知のチェック名 \"{name}\" (指定可能: {})",
                            HEALTH_CHECK_NAMES.join(", ")
                        ),
                    );
                }
            }
        }
    }

    // git 内部と mikke 既定出力ディレクトリは設定によらず常に走査対象外
    cfg.exclude_dirs.insert(".git".to_string());
    cfg.exclude_dirs.insert(".mikke".to_string());

    // index / embeddings の出力先は「そのパス配下」を root 相対 prefix で除外する。
    // 名前を exclude_dirs に足すと同名ディレクトリが任意の深さで全消えする事故になる。
    let mut prefixes: Vec<String> = Vec::new();
    for (rel_str, is_file) in [
        (cfg.index_rel.clone(), true),
        (cfg.embeddings_rel.clone(), false),
    ] {
        let p = PathBuf::from(&rel_str);
        let p = if p.is_absolute() {
            match p.canonicalize().ok().and_then(|abs| {
                root.canonicalize()
                    .ok()
                    .and_then(|r| abs.strip_prefix(&r).ok().map(|x| x.to_path_buf()))
            }) {
                Some(rel) => rel,
                None => continue, // ルート外への出力は走査に影響しない
            }
        } else {
            p
        };
        let d = if is_file {
            p.parent().map(|x| x.to_path_buf()).unwrap_or_default()
        } else {
            p
        };
        let d_posix = to_posix(&d);
        if d_posix != "." && !d_posix.is_empty() {
            prefixes.push(d_posix);
        }
    }
    cfg.exclude_path_prefixes = prefixes;
    cfg
}

fn strip_bom(s: &str) -> String {
    s.strip_prefix('\u{feff}').unwrap_or(s).to_string()
}

fn sorted(set: &HashSet<String>) -> Vec<String> {
    let mut v: Vec<String> = set.iter().cloned().collect();
    v.sort();
    v
}

/// パスを posix (`/`) 表現の文字列にする。
pub fn to_posix(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// パスを表示用の文字列にする。root は canonicalize 済みで、Windows では `\\?\C:\...` の形になるため、
/// 表示する時だけ `\\?\` を外す (#67)。内部保持とファイル操作のパスは canonicalize したままにする。
/// `\\?\C:\...` → `C:\...`、`\\?\UNC\server\share\...` → `\\server\share\...`。
/// 他の形 (`\\?\GLOBALROOT\...` など) と、外すと MAX_PATH (260) を超える時はそのまま出す
/// (外した形では開けないため)。Windows 以外は Path::display() と同じ。
pub fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let stripped = match p.components().next() {
            Some(Component::Prefix(c)) => match c.kind() {
                Prefix::VerbatimDisk(_) => s.strip_prefix(r"\\?\").map(str::to_string),
                Prefix::VerbatimUNC(..) => s.strip_prefix(r"\\?\UNC\").map(|r| format!(r"\\{r}")),
                _ => None,
            },
            _ => None,
        };
        if let Some(t) = stripped.filter(|t| t.encode_utf16().count() <= 260) {
            return t;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_path_keeps_plain_paths() {
        for p in ["notes/a.md", ".mikke/index.db", ""] {
            assert_eq!(
                display_path(Path::new(p)),
                Path::new(p).display().to_string()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn display_path_strips_verbatim_prefix() {
        for (p, want) in [
            (r"\\?\C:\notes\.mikke\index.db", r"C:\notes\.mikke\index.db"),
            (r"\\?\UNC\server\share\notes", r"\\server\share\notes"),
            // 他の verbatim の形はそのまま
            (
                r"\\?\GLOBALROOT\Device\HarddiskVolume1\x",
                r"\\?\GLOBALROOT\Device\HarddiskVolume1\x",
            ),
            (r"C:\notes\a.md", r"C:\notes\a.md"),
            (r"\\server\share\notes", r"\\server\share\notes"),
        ] {
            assert_eq!(display_path(Path::new(p)), want, "{p}");
        }
        // 外すと 260 文字を超える時は外さない
        let long = format!(r"\\?\C:\{}", "a".repeat(258));
        assert_eq!(display_path(Path::new(&long)), long);
        let fit = format!(r"\\?\C:\{}", "a".repeat(257));
        assert_eq!(display_path(Path::new(&fit)), &fit[4..]);
    }

    /// md 本文から ```toml ブロックの中身を順に抜き出す。
    fn toml_blocks(md: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur: Option<String> = None;
        for line in md.lines() {
            match cur.as_mut() {
                None if line.trim_start().starts_with("```toml") => cur = Some(String::new()),
                None => {}
                Some(_) if line.trim_start().starts_with("```") => out.push(cur.take().unwrap()),
                Some(b) => {
                    b.push_str(line);
                    b.push('\n');
                }
            }
        }
        out
    }

    fn all_sections() -> Vec<&'static str> {
        KNOWN_KEYS.iter().map(|(s, _)| *s).collect()
    }

    fn msgs(src: &str, sections: &[&str]) -> Vec<String> {
        let v: toml::Value = toml::from_str(src).unwrap();
        unknown_key_messages(Path::new("x"), &v, sections)
    }

    #[test]
    fn known_keys_match_spec_schema() {
        let spec = include_str!("../docs/SPEC.md");
        let after = &spec[spec.find("## 設定スキーマ").unwrap()..];
        let block = toml_blocks(after).into_iter().next().unwrap();
        let v: toml::Value = toml::from_str(&block).unwrap();
        let mut from_spec = BTreeSet::new();
        for (sec, t) in v.as_table().unwrap() {
            for k in t.as_table().unwrap().keys() {
                from_spec.insert((sec.clone(), k.clone()));
            }
        }
        let from_table: BTreeSet<(String, String)> = KNOWN_KEYS
            .iter()
            .flat_map(|(s, keys)| keys.iter().map(|k| (s.to_string(), k.to_string())))
            .collect();
        assert_eq!(from_spec, from_table);
    }

    #[test]
    fn docs_toml_blocks_have_no_unknown_keys() {
        let all = all_sections();
        for (name, md) in [
            ("README.md", include_str!("../README.md")),
            ("docs/SPEC.md", include_str!("../docs/SPEC.md")),
        ] {
            let blocks = toml_blocks(md);
            assert!(!blocks.is_empty(), "{name} に toml ブロックが無い");
            for b in blocks {
                let v: toml::Value = toml::from_str(&b)
                    .unwrap_or_else(|e| panic!("{name} の toml ブロックが読めない: {e}\n{b}"));
                let m = unknown_key_messages(Path::new("x"), &v, &all);
                assert!(m.is_empty(), "{name}: {m:?}");
            }
        }
    }

    #[test]
    fn unknown_key_messages_cases() {
        let all = all_sections();
        let m = msgs("[scan]\nexclude_dir = []\n", &all);
        assert_eq!(m.len(), 1);
        assert!(
            m[0].contains("未知のキー [scan] exclude_dir") && m[0].contains("(exclude_dirs ?)")
        );

        let m = msgs("[scna]\ninclude = []\nwalrus = 1\n", &all);
        assert_eq!(m.len(), 1, "{m:?}");
        assert!(m[0].contains("未知の節 [scna]") && m[0].contains("([scan] ?)"));

        let m = msgs("[index]\nauto_rebuld = true\n", &all);
        assert!(m[0].contains("(auto_rebuild ?)"), "{m:?}");

        let m = msgs("[search]\nauto_rebuild = true\n", &all);
        assert!(m[0].contains("([index] auto_rebuild ?)"), "{m:?}");

        let m = msgs("auto_rebuild = true\n", &all);
        assert!(
            m[0].contains("節の外の未知のキー auto_rebuild")
                && m[0].contains("([index] auto_rebuild ?)"),
            "{m:?}"
        );

        let m = msgs("[scan]\nzebra = 1\n", &all);
        assert!(m[0].ends_with("を無視します"), "{m:?}");

        let nested = msgs(
            "health = 1\n[search]\nbm25_limit = \"x\"\n[scna]\n",
            NESTED_SECTIONS,
        );
        assert_eq!(nested.len(), 1, "{nested:?}");
        assert!(nested[0].contains("[scna]"));
        assert!(msgs("[health]\nwalrus = 1\n", NESTED_SECTIONS).is_empty());

        assert!(msgs(
            "[scan]\ninclude = []\nexclude_paths = []\n[index]\nauto_rebuild = true\n",
            &all
        )
        .is_empty());
    }

    #[test]
    fn near_cases() {
        assert_eq!(
            near("exclude_dir", ["include", "exclude_dirs"]),
            Some("exclude_dirs")
        );
        assert_eq!(near("scna", ["scan", "index"]), Some("scan"));
        assert_eq!(near("abcdef", ["abcdxy"]), Some("abcdxy"));
        assert_eq!(near("abcdef", ["abcxyz"]), None);
        assert_eq!(near("ab", ["xy"]), None);
    }

    #[test]
    fn normalize_exclude_path_cases() {
        for (v, want) in [
            ("state", "state"),
            ("state/", "state"),
            ("./state", "state"),
            ("a//b", "a/b"),
            ("./a/./b/", "a/b"),
            ("a..b", "a..b"),
        ] {
            assert_eq!(normalize_exclude_path(v).as_deref(), Ok(want), "{v:?}");
        }
        for v in [
            "",
            ".",
            "./",
            "//",
            "/state",
            "C:/state",
            "c:state",
            "..",
            "../x",
            "a/../b",
            "state\\cache",
            "a\tb",
        ] {
            assert!(normalize_exclude_path(v).is_err(), "{v:?}");
        }
    }
}
