//! ノート repo の健全性チェック (`mikke health`)。
//!
//! チェック項目 (セクション順もこの通り、各々件数付き):
//!   1. frontmatter 破損 — index 非依存で filesystem 直接スキャン (scan_skip_prefixes 適用)。
//!   2. 実行bit欠落 — exec_bit_prefixes 配下の tracked *.sh の git index mode != 100755。
//!   3. タグなし / 4. 要約なし / 5. 低ボリューム / 6. updated未設定 — index ベース。
//!      index 鮮度は stdout のみ、md レポートには含めない (レポートの決定性を守る)。
//!
//! md レポート (--md-report): 揮発情報を含めず決定的に生成、改行 LF 固定。
//! パスはレポート置き場からの相対 md リンク (# は %23、空白/括弧は <> wrap、[] はエスケープ)。

#![allow(dead_code)]

use crate::config::{to_posix, Config};
use crate::scan::{iter_notes, scan_frontmatter_issue};
use crate::search;
use rusqlite::{params, Connection};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

type Section = (String, Vec<(String, String)>);

fn skip(path: &str, prefixes: &[String]) -> bool {
    prefixes.iter().any(|p| path.starts_with(p.as_str()))
}

/// 絶対化 + `.`/`..` の字句正規化 (symlink は解決しない)。
fn absolutize(p: &Path) -> PathBuf {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// os.path.relpath 相当 (base から target への相対パス)。
fn relpath(target: &Path, base: &Path) -> PathBuf {
    let t: Vec<Component> = target.components().collect();
    let b: Vec<Component> = base.components().collect();
    let common = t.iter().zip(b.iter()).take_while(|(a, c)| a == c).count();
    let mut out = PathBuf::new();
    for _ in common..b.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// root 相対の rel_path を、レポート置き場から辿れる相対 md リンクにする。
/// 表示テキストは root 相対のまま。wikilink でなくパスリンクなのは basename 衝突回避のため。
fn md_link(rel_path: &str, report_dir: &Path, root: &Path) -> String {
    let target = absolutize(&root.join(rel_path));
    let mut href = to_posix(&relpath(&target, report_dir));
    // '#' は fragment 解釈されリンク先が壊れる。<> wrap では救えないので percent-encode
    href = href.replace('%', "%25").replace('#', "%23");
    if href.contains(' ') || href.contains('(') || href.contains(')') {
        href = format!("<{href}>");
    }
    // 非バランスな [ ] はリンクラベルを早期に閉じて構文を壊すためエスケープ
    let label = rel_path.replace('[', "\\[").replace(']', "\\]");
    format!("[{label}]({href})")
}

fn rows_filtered(conn: &Connection, sql: &str, prefixes: &[String]) -> Vec<(String, String)> {
    let mut stmt = conn.prepare(sql).expect("health SELECT の準備に失敗");
    stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .expect("health SELECT に失敗")
        .flatten()
        .filter(|(p, _)| !skip(p, prefixes))
        .collect()
}

pub fn cmd_health(cfg: &Config, report_path: Option<&Path>) {
    let conn = search::connect(cfg);

    // --- filesystem 直接スキャン: frontmatter 破損 + index 鮮度 ---
    let gen_ts: Option<f64> = conn
        .query_row("SELECT value FROM meta WHERE key = 'generated'", [], |r| {
            r.get::<_, String>(0)
        })
        .ok()
        .and_then(|v| v.parse::<f64>().ok());
    let mut broken: Vec<(String, String, String)> = Vec::new();
    let mut stale_files = 0usize;
    for (md_file, rel) in iter_notes(cfg) {
        let rel_posix = to_posix(&rel);
        if skip(&rel_posix, &cfg.scan_skip_prefixes) {
            continue;
        }
        if let Some((kind, detail)) = scan_frontmatter_issue(&md_file) {
            broken.push((rel_posix, kind, detail));
        }
        if let Some(gen) = gen_ts {
            let mtime = md_file
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            if mtime > gen {
                stale_files += 1;
            }
        }
    }

    // 実行bit欠落チェック: index (= tree 内容) のみに依存しホスト非依存。
    let mut mode_issues: Vec<(String, String)> = Vec::new();
    if !cfg.exec_bit_prefixes.is_empty() {
        let mut args: Vec<String> = vec![
            "-C".to_string(),
            cfg.root.to_string_lossy().into_owned(),
            "-c".to_string(),
            "core.quotepath=off".to_string(),
            "ls-files".to_string(),
            "-s".to_string(),
            "--".to_string(),
        ];
        args.extend(cfg.exec_bit_prefixes.iter().cloned());
        if let Ok(out) = Command::new("git").args(&args).output() {
            if out.status.success() {
                let ls_out = String::from_utf8_lossy(&out.stdout);
                for line in ls_out.lines() {
                    let (meta_part, path_part) = line.split_once('\t').unwrap_or((line, ""));
                    let mode = meta_part.split_whitespace().next().unwrap_or("");
                    if path_part.ends_with(".sh") && mode != "100755" {
                        mode_issues.push((
                            path_part.to_string(),
                            format!("  -- index mode {mode} (実行bit欠落)"),
                        ));
                    }
                }
            }
        }
    }

    // --- index ベースの品質チェック (quality_skip_prefixes を適用) ---
    let no_tags = rows_filtered(
        &conn,
        "SELECT n.path, n.title FROM notes n
         WHERE NOT EXISTS (SELECT 1 FROM tags t WHERE t.path = n.path)
         ORDER BY n.path",
        &cfg.quality_skip_prefixes,
    );
    let no_summary = rows_filtered(
        &conn,
        "SELECT path, title FROM notes WHERE (summary IS NULL OR summary = '') ORDER BY path",
        &cfg.quality_skip_prefixes,
    );
    let low_word: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT path, title, word_count FROM notes WHERE word_count < ?1 ORDER BY word_count")
            .expect("health SELECT の準備に失敗");
        stmt.query_map(params![cfg.min_words], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .expect("health SELECT に失敗")
        .flatten()
        .filter(|(p, _, _)| !skip(p, &cfg.quality_skip_prefixes))
        .collect()
    };
    let no_updated = rows_filtered(
        &conn,
        "SELECT path, title FROM notes WHERE (updated IS NULL OR updated = '') ORDER BY path",
        &cfg.quality_skip_prefixes,
    );
    let total: usize = {
        let mut stmt = conn
            .prepare("SELECT path FROM notes")
            .expect("health SELECT の準備に失敗");
        stmt.query_map([], |r| r.get::<_, String>(0))
            .expect("health SELECT に失敗")
            .flatten()
            .filter(|p| !skip(p, &cfg.quality_skip_prefixes))
            .count()
    };

    // --- セクション組み立て (stdout と md レポートで共用。決定的な内容のみ) ---
    let mut sections: Vec<Section> = Vec::new();
    if !broken.is_empty() {
        sections.push((
            format!("frontmatter 破損 ({}件)", broken.len()),
            broken
                .iter()
                .map(|(p, kind, detail)| (p.clone(), format!("  -- {kind}: {detail}")))
                .collect(),
        ));
    }
    if !mode_issues.is_empty() {
        sections.push((
            format!("実行bit欠落 ({}件)", mode_issues.len()),
            mode_issues,
        ));
    }
    if !no_tags.is_empty() {
        sections.push((
            format!("タグなし ({}件)", no_tags.len()),
            no_tags
                .iter()
                .map(|(p, t)| (p.clone(), format!("  -- {t}")))
                .collect(),
        ));
    }
    if !no_summary.is_empty() {
        sections.push((
            format!("要約なし ({}件)", no_summary.len()),
            no_summary
                .iter()
                .map(|(p, t)| (p.clone(), format!("  -- {t}")))
                .collect(),
        ));
    }
    if !low_word.is_empty() {
        sections.push((
            format!(
                "低ボリューム: < {} words ({}件)",
                cfg.min_words,
                low_word.len()
            ),
            low_word
                .iter()
                .map(|(p, t, wc)| (p.clone(), format!("  -- {t} ({wc} words)")))
                .collect(),
        ));
    }
    if !no_updated.is_empty() {
        sections.push((
            format!("updated未設定 ({}件)", no_updated.len()),
            no_updated
                .iter()
                .map(|(p, t)| (p.clone(), format!("  -- {t}")))
                .collect(),
        ));
    }
    let issues: usize = sections.iter().map(|(_, items)| items.len()).sum();

    for (heading, items) in &sections {
        println!("[{heading}]");
        for (path, desc) in items {
            println!("  {path}{desc}");
        }
        println!();
    }

    // index 鮮度は実行時依存 (stdout のみ、issues にも report にも含めない)
    if gen_ts.is_none() {
        println!("[index鮮度] 生成時刻が index に無い (旧形式)。mikke index で再構築を推奨\n");
    } else if stale_files > 0 {
        println!("[index鮮度] index 生成後に更新された md が {stale_files}件。mikke index で再構築を推奨\n");
    }

    if issues == 0 {
        println!("問題のあるノートはありません。");
    } else {
        println!("--- 合計: {total}ノート中 {issues}件の問題（重複あり）");
    }

    if let Some(report_path) = report_path {
        let mut lines: Vec<String> = [
            "---",
            "title: KB health レポート (自動生成)",
            "tags: [meta, health, auto-generated]",
            "summary: >-",
            "  mikke health --md-report が生成する KB 健全性の最新スナップショット。",
            "  内容が変わった時だけ commit する運用を想定 (生成時刻は git log を参照)。",
            "---",
            "",
            "# KB health レポート",
            "",
            "> 自動生成 (`mikke health --md-report`)。手編集しない。",
            "> 実行時刻・index 鮮度など揮発情報は含めない (このファイルの差分 = 実質的な状態変化)。",
            "",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let report_dir = absolutize(report_path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        if !sections.is_empty() {
            for (heading, items) in &sections {
                lines.push(format!("## {heading}"));
                lines.push(String::new());
                for (path, desc) in items {
                    lines.push(format!("- {}{desc}", md_link(path, &report_dir, &cfg.root)));
                }
                lines.push(String::new());
            }
        } else {
            lines.push("問題のあるノートはありません。".to_string());
            lines.push(String::new());
        }
        lines.push(format!("合計: {total}ノート中 {issues}件の問題 (重複あり)"));
        // newline は LF 固定 (CRLF が混ざると「内容が変わった時だけ commit」の決定性が壊れる)
        if let Err(e) = std::fs::write(report_path, lines.join("\n") + "\n") {
            eprintln!("Error: レポートを書き出せません: {e}");
            std::process::exit(1);
        }
        eprintln!("レポート書き出し: {}", report_path.display());
    }
}
