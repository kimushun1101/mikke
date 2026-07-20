//! インデックス生成 (`mikke index`)。
//!
//! SQLite スキーマ (drop→再作成の全再構築):
//!   notes(path PK, title NOT NULL, date, updated, summary, word_count)
//!     INDEX idx_notes_date ON notes(date DESC)
//!   tags(path, tag, PK(path,tag))  INDEX idx_tags_tag ON tags(tag)
//!   links(path, target, PK(path,target))
//!   notes_fts USING fts5(path UNINDEXED, title, content, tokenize='trigram')
//!   meta(key PK, value)   -- meta['generated'] に生成時刻 (health 鮮度判定用)
//!
//! meta['generated'] は epoch 秒 (ナノ秒精度の小数文字列) で保存する。
//! 秒精度だと mtime (小数秒) との比較で秒未満の更新を取りこぼすため
//! (index フォーマットは内部表現で互換保証しない — docs/SPEC.md)。

#![allow(dead_code)]

use crate::config::{to_posix, Config};
use crate::scan;
use rusqlite::{params, Connection};
use std::collections::HashSet;
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
        std::process::exit(1);
    });
    conn.execute_batch(SCHEMA).expect("schema 作成に失敗");

    let mut note_count = 0i64;
    let mut tag_set: HashSet<String> = HashSet::new();
    let mut issues: Vec<(String, String, String)> = Vec::new();

    for (md_file, rel) in scan::iter_notes(cfg) {
        let rel_posix = to_posix(&rel);
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
        std::process::exit(1);
    }
}

/// index が無ければ build する (clone 直後フォールバック)。stderr に告知。
pub fn ensure_index(cfg: &Config) {
    if !cfg.index_path().exists() {
        eprintln!("インデックスが無いため生成しています...");
        build_to(cfg, true);
    }
}
