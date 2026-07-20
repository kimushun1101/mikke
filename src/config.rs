//! 設定 (mikke.toml / .mikke.toml) 読み込みとルート解決。
//!
//! ルート解決の優先順: --root 引数 > 環境変数 MIKKE_ROOT > cwd から上方に
//! mikke.toml / .mikke.toml を探索 > git root。型不一致は silent に誤動作させず即エラー終了する
//! (特に「文字列配列指定に文字列を渡すと 1 文字ずつに分解される」事故を型検査で弾く)。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 設定ファイル名の探索順 (先頭が優先)。隠したい repo 向けに dotfile 変種も受ける。
pub const CONFIG_FILENAMES: &[&str] = &["mikke.toml", ".mikke.toml"];

/// dir 直下の設定ファイルを優先順で返す。両方あれば mikke.toml 優先 + stderr 警告。
pub fn find_config_file(dir: &Path) -> Option<PathBuf> {
    let existing: Vec<PathBuf> = CONFIG_FILENAMES
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .collect();
    if existing.len() > 1 {
        eprintln!(
            "Warning: {} に mikke.toml と .mikke.toml が両方あります。mikke.toml を使用します。",
            dir.display()
        );
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

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    // [scan]
    pub include: Vec<String>,
    pub exclude_dirs: HashSet<String>,
    pub exclude_files: HashSet<String>,
    // [index]
    pub index_rel: String,
    pub embeddings_rel: String,
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
    // 出力先 (index/embeddings) 配下を走査から除外する root 相対 prefix (自動算出)
    pub exclude_path_prefixes: Vec<String>,
}

impl Config {
    fn with_defaults(root: PathBuf) -> Self {
        Config {
            root,
            include: vec![".".to_string()],
            exclude_dirs: DEFAULT_EXCLUDE_DIRS.iter().map(|s| s.to_string()).collect(),
            exclude_files: DEFAULT_EXCLUDE_FILES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            index_rel: DEFAULT_INDEX_PATH.to_string(),
            embeddings_rel: DEFAULT_EMBEDDINGS_DIR.to_string(),
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
            exclude_path_prefixes: vec![],
        }
    }

    pub fn index_path(&self) -> PathBuf {
        self.root.join(&self.index_rel)
    }

    pub fn embeddings_dir(&self) -> PathBuf {
        self.root.join(&self.embeddings_rel)
    }
}

fn config_error(cfg_path: &Path, msg: &str) -> ! {
    eprintln!("Error: {}: {msg}", cfg_path.display());
    std::process::exit(1);
}

fn err_root(msg: &str) -> ! {
    eprintln!("Error: {msg}");
    std::process::exit(1);
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
    // git root フォールバック (encoding は UTF-8 前提)
    if let Ok(out) = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
    {
        if out.status.success() {
            let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !top.is_empty() {
                let p = PathBuf::from(top);
                return std::fs::canonicalize(&p).unwrap_or(p);
            }
        }
    }
    err_root(
        "ルートを特定できません。mikke.toml (または .mikke.toml) をノートフォルダのルートに置くか、--root / MIKKE_ROOT で指定してください。"
    );
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

/// <root>/mikke.toml (無ければ .mikke.toml) を読む。どちらも無ければデフォルト設定を返す。
pub fn load_config(root: PathBuf) -> Config {
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

        let scan = get_table(p, &data, "scan");
        cfg.include = str_list(p, &scan, "scan", "include", cfg.include);
        cfg.exclude_dirs = str_list(p, &scan, "scan", "exclude_dirs", sorted(&cfg.exclude_dirs))
            .into_iter()
            .collect();
        cfg.exclude_files = str_list(
            p,
            &scan,
            "scan",
            "exclude_files",
            sorted(&cfg.exclude_files),
        )
        .into_iter()
        .collect();

        let index = get_table(p, &data, "index");
        cfg.index_rel = scalar_str(p, &index, "index", "path", cfg.index_rel);
        cfg.embeddings_rel = scalar_str(p, &index, "index", "embeddings_dir", cfg.embeddings_rel);

        let semantic = get_table(p, &data, "semantic");
        cfg.semantic_enabled =
            scalar_bool(p, &semantic, "semantic", "enabled", cfg.semantic_enabled);
        cfg.model = scalar_str(p, &semantic, "semantic", "model", cfg.model);
        cfg.query_prefix = scalar_str(p, &semantic, "semantic", "query_prefix", cfg.query_prefix);
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
        cfg.vector_weight = scalar_num(p, &search, "search", "vector_weight", cfg.vector_weight);
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
