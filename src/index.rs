//! インデックス生成 (`mikke index`)。
//!
//! SQLite スキーマ (drop→再作成の全再構築):
//!   notes(path PK, title NOT NULL, date, updated, summary, word_count)
//!     INDEX idx_notes_date ON notes(date DESC)
//!   tags(path, tag, PK(path,tag))  INDEX idx_tags_tag ON tags(tag)
//!   links(path, target, PK(path,target))
//!   notes_fts USING fts5(path UNINDEXED, title, content, tokenize='trigram')
//!   meta(key PK, value)   -- meta['generated'] に生成時刻 (health 鮮度判定 +
//!                            auto_rebuild の stale 判定)、meta['scanned_paths_sha256'] に
//!                            走査 path 集合のスナップショット (auto_rebuild の stale 判定)
//!
//! meta['generated'] は epoch 秒 (ナノ秒精度の小数文字列) で保存する。
//! 秒精度だと mtime (小数秒) との比較で秒未満の更新を取りこぼすため
//! (index フォーマットは内部表現で互換保証しない — docs/SPEC.md)。

#![allow(dead_code)]

use crate::config::{to_posix, Config};
use crate::scan;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "
    DROP TABLE IF EXISTS notes_fts;
    DROP TABLE IF EXISTS links;
    DROP TABLE IF EXISTS tags;
    DROP TABLE IF EXISTS notes;
    DROP TABLE IF EXISTS meta;

    CREATE TABLE notes (
        path TEXT PRIMARY KEY,
        title TEXT NOT NULL,
        date TEXT,
        updated TEXT,
        summary TEXT,
        word_count INTEGER
    );
    CREATE INDEX idx_notes_date ON notes(date DESC);

    CREATE TABLE tags (
        path TEXT NOT NULL,
        tag TEXT NOT NULL,
        PRIMARY KEY (path, tag)
    );
    CREATE INDEX idx_tags_tag ON tags(tag);

    CREATE TABLE links (
        path TEXT NOT NULL,
        target TEXT NOT NULL,
        PRIMARY KEY (path, target)
    );

    CREATE VIRTUAL TABLE notes_fts USING fts5(
        path UNINDEXED,
        title,
        content,
        tokenize='trigram'
    );

    CREATE TABLE meta (
        key TEXT PRIMARY KEY,
        value TEXT
    );
";

/// 走査 path 集合のスナップショットを保存する meta キー。
const META_SCANNED_PATHS: &str = "scanned_paths_sha256";

/// 走査 path 集合 (root 相対 posix) のスナップショット: ソート済リストの SHA-256。
/// mtime が保存される移動・リネームや追加・削除を auto_rebuild の stale 判定で検知する。
fn scanned_paths_hash(rel_paths: &[String]) -> String {
    let mut sorted: Vec<&str> = rel_paths.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    Sha256::digest(sorted.join("\n").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// ファイルの mtime を epoch 秒 (f64) で返す。取得不能は 0.0 (= stale 扱いしない)。
pub fn mtime_epoch_secs(path: &Path) -> f64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn emit(use_stderr: bool, line: &str) {
    if use_stderr {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
}

/// index を全再構築し、frontmatter 破損リスト (path, 種別, 詳細) を返す。
/// use_stderr=true で進捗出力を stderr へ (auto-build 時に stdout を汚さない)。
pub fn build_to(cfg: &Config, use_stderr: bool) -> Vec<(String, String, String)> {
    let index_path = cfg.index_path();
    if let Some(parent) = index_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::remove_file(&index_path);
    let conn = Connection::open(&index_path).unwrap_or_else(|e| {
        eprintln!("Error: index を作成できません: {e}");
        std::process::exit(2);
    });
    conn.execute_batch(SCHEMA).expect("schema 作成に失敗");

    let mut note_count = 0i64;
    let mut tag_set: HashSet<String> = HashSet::new();
    let mut issues: Vec<(String, String, String)> = Vec::new();
    // スナップショットは走査した全 path (frontmatter 破損で notes から除外される分も含む)。
    // notes 行数と違い破損ファイルの有無で恒常 stale 化しない。
    let mut scanned_paths: Vec<String> = Vec::new();

    for (md_file, rel) in scan::iter_notes(cfg) {
        let rel_posix = to_posix(&rel);
        scanned_paths.push(rel_posix.clone());
        if let Some((kind, detail)) = scan::scan_frontmatter_issue(&md_file) {
            eprintln!("Warning: {rel_posix}: frontmatter {kind} — {detail}");
            issues.push((rel_posix.clone(), kind, detail));
        }
        let note = match scan::load_note(&md_file, &rel) {
            Some(n) => n,
            None => {
                eprintln!("Warning: {rel_posix} の読み込みに失敗 (index から除外)");
                continue;
            }
        };

        conn.execute(
            "INSERT INTO notes (path, title, date, updated, summary, word_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                note.path_rel,
                note.title,
                note.date,
                note.updated,
                note.summary,
                scan::count_words(&note.content)
            ],
        )
        .expect("notes への INSERT に失敗");
        for tag in &note.tags {
            conn.execute(
                "INSERT INTO tags (path, tag) VALUES (?1, ?2)",
                params![note.path_rel, tag],
            )
            .expect("tags への INSERT に失敗");
        }
        for target in scan::extract_wikilinks(&note.content) {
            conn.execute(
                "INSERT INTO links (path, target) VALUES (?1, ?2)",
                params![note.path_rel, target],
            )
            .expect("links への INSERT に失敗");
        }
        conn.execute(
            "INSERT INTO notes_fts (path, title, content) VALUES (?1, ?2, ?3)",
            params![note.path_rel, note.title, note.content],
        )
        .expect("notes_fts への INSERT に失敗");

        tag_set.extend(note.tags.iter().cloned());
        note_count += 1;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let generated = format!("{}.{:09}", now.as_secs(), now.subsec_nanos());
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('generated', ?1)",
        params![generated],
    )
    .expect("meta への INSERT に失敗");
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
        params![META_SCANNED_PATHS, scanned_paths_hash(&scanned_paths)],
    )
    .expect("meta への INSERT に失敗");

    emit(
        use_stderr,
        &format!("インデックスを生成しました: {}", index_path.display()),
    );
    emit(use_stderr, &format!("  ノート数: {note_count}"));
    emit(use_stderr, &format!("  タグ数: {}", tag_set.len()));
    // silent skip の可視化: 破損は握り潰さず必ず件数を出す (0 件でも明示)
    if issues.is_empty() {
        emit(use_stderr, "  frontmatter 破損: 0件");
    } else {
        emit(
            use_stderr,
            &format!(
                "  ⚠ frontmatter 破損: {}件 (詳細は上の Warning / mikke health)",
                issues.len()
            ),
        );
    }
    issues
}

/// index を全再構築し、frontmatter 破損リスト (path, 種別, 詳細) を返す。
pub fn build(cfg: &Config) -> Vec<(String, String, String)> {
    build_to(cfg, false)
}

pub fn cmd_index(cfg: &Config, check: bool) {
    let issues = build(cfg);
    if check && !issues.is_empty() {
        eprintln!(
            "Error: frontmatter 破損 {}件 (--check 指定のため非 0 で終了)",
            issues.len()
        );
        // 「検出 = 結果」なので 1 (構築自体の失敗 = 2 と区別 — docs/SPEC.md「exit code」)
        std::process::exit(1);
    }
}

/// 既存 index が stale かを短命 Connection で判定する (auto_rebuild 用)。
/// stale 条件: meta['generated'] / スナップショットが無い旧形式、いずれかの md の
/// mtime > generated、走査 path 集合のハッシュがスナップショットと不一致。
fn index_is_stale(cfg: &Config) -> bool {
    let conn = match Connection::open(cfg.index_path()) {
        Ok(c) => c,
        Err(_) => return true, // 開けない index は stale 扱いで作り直す
    };
    let meta = |key: &str| -> Option<String> {
        conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
            r.get::<_, String>(0)
        })
        .ok()
    };
    // meta['generated'] が無い旧形式 index は stale 扱い
    let Some(generated) = meta("generated").and_then(|v| v.parse::<f64>().ok()) else {
        return true;
    };
    let Some(saved_hash) = meta(META_SCANNED_PATHS) else {
        return true;
    };
    let mut rel_paths: Vec<String> = Vec::new();
    for (md_file, rel) in scan::iter_notes(cfg) {
        if mtime_epoch_secs(&md_file) > generated {
            return true;
        }
        rel_paths.push(to_posix(&rel));
    }
    scanned_paths_hash(&rel_paths) != saved_hash
}

/// index が無ければ build する (clone 直後フォールバック)。stderr に告知。
/// 存在時の鮮度判定はしない — health はこちらを使い、auto_rebuild が
/// 鮮度診断をマスクしないようにする (docs/SPEC.md「index スキーマ」)。
pub fn ensure_index_exists(cfg: &Config) {
    if !cfg.index_path().exists() {
        eprintln!("インデックスが無いため生成しています...");
        build_to(cfg, true);
    }
}

/// 検索系コマンドの index 準備。無ければ build し、[index] auto_rebuild = true なら
/// 鮮度を判定して stale なら全再構築する (告知はいずれも stderr — stdout は安定出力)。
pub fn ensure_index(cfg: &Config) {
    if !cfg.index_path().exists() {
        eprintln!("インデックスが無いため生成しています...");
        build_to(cfg, true);
        return;
    }
    if cfg.auto_rebuild && index_is_stale(cfg) {
        eprintln!("ソースの更新を検知したため index を再構築しています...");
        build_to(cfg, true);
    }
}
