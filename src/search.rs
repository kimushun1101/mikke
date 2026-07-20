//! 検索コマンド群 (find / tag / title / semantic / hybrid / list-tags / recent)。
//!
//! FTS 変換 (fts_query): 空白で語分割 → 各語を個別に `"..."` quote (内部 " は "") → AND 連結。
//! trigram 制約: 各語 3 文字以上なら `ORDER BY rank` (FTS5 BM25)。1 語でも <3 文字を含めば
//!   LIKE フォールバック (全語 AND で LOWER(content/title) LIKE %term%、date 降順、relevance 無し)。
//! find の見出し: 順序を正直に表示。bm25_limit 到達時は打ち切りを明示。
//! tag/title: LOWER(...) LIKE %kw% 部分一致、date 降順。recent: date!='' を date 降順 LIMIT。
//! list-tags: GROUP BY tag ORDER BY COUNT(*) DESC, tag。
//! hybrid RRF: 各ストリーム top_n*candidate_factor → rank 1 始まり → score += w * 1/(rrf_k+rank)。
//!   semantic 未構築なら vec 重み 0 で再正規化し BM25 のみへ degrade。via に bm25/vec/bm25+vec。

#![allow(dead_code)]

use crate::config::Config;
use crate::index;
use rusqlite::{params, params_from_iter, Connection};
use std::collections::HashMap;

/// 検索結果 1 件。
pub struct Hit {
    pub path: String,
    pub title: String,
    pub date: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub score: Option<f64>,
    pub via: Option<String>,
}

pub fn connect(cfg: &Config) -> Connection {
    index::ensure_index(cfg);
    match Connection::open(cfg.index_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: index を開けません: {e}");
            std::process::exit(1);
        }
    }
}

/// path のリストに対応する notes 行を取得 (path 順序を保持)。
fn fetch_notes(conn: &Connection, paths: &[String]) -> Vec<Hit> {
    if paths.is_empty() {
        return Vec::new();
    }
    let placeholders = vec!["?"; paths.len()].join(",");

    let sql =
        format!("SELECT path, title, date, summary FROM notes WHERE path IN ({placeholders})");
    let mut stmt = conn.prepare(&sql).expect("notes SELECT の準備に失敗");
    let mut row_map: HashMap<String, (String, String, String)> = HashMap::new();
    let rows = stmt
        .query_map(params_from_iter(paths.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .expect("notes SELECT に失敗");
    for row in rows.flatten() {
        let (path, title, date, summary) = row;
        row_map.insert(
            path,
            (title, date.unwrap_or_default(), summary.unwrap_or_default()),
        );
    }

    let tag_sql = format!("SELECT path, tag FROM tags WHERE path IN ({placeholders}) ORDER BY tag");
    let mut tag_stmt = conn.prepare(&tag_sql).expect("tags SELECT の準備に失敗");
    let mut tag_map: HashMap<String, Vec<String>> = HashMap::new();
    let tag_rows = tag_stmt
        .query_map(params_from_iter(paths.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .expect("tags SELECT に失敗");
    for (path, tag) in tag_rows.flatten() {
        tag_map.entry(path).or_default().push(tag);
    }

    let mut out = Vec::new();
    for p in paths {
        let Some((title, date, summary)) = row_map.get(p) else {
            continue;
        };
        out.push(Hit {
            path: p.clone(),
            title: title.clone(),
            date: date.clone(),
            summary: summary.clone(),
            tags: tag_map.get(p).cloned().unwrap_or_default(),
            score: None,
            via: None,
        });
    }
    out
}

fn print_notes(notes: &[Hit]) {
    if notes.is_empty() {
        println!("該当するノートが見つかりませんでした。");
        return;
    }
    for note in notes {
        let date_str = if note.date.is_empty() {
            String::new()
        } else {
            format!(" ({})", note.date)
        };
        let tags_str = note.tags.join(", ");
        let score_str = match note.score {
            Some(score) => {
                let via_str = note
                    .via
                    .as_ref()
                    .map(|v| format!(" via {v}"))
                    .unwrap_or_default();
                format!("  [score: {score:.4}{via_str}]")
            }
            None => String::new(),
        };
        println!("  {}{}{}", note.title, date_str, score_str);
        println!("    path: {}", note.path);
        if !tags_str.is_empty() {
            println!("    tags: {tags_str}");
        }
        // 欠落を「未表示」と区別できるよう明示。空のまま黙ると本文未読での内容推測=作話を誘発する。
        if note.summary.is_empty() {
            println!("    summary: (なし — 要約未設定。内容は path を開いて確認)");
        } else {
            println!("    summary: {}", note.summary);
        }
        println!();
    }
}

fn query_paths(conn: &Connection, sql: &str, param: &str) -> Vec<String> {
    let mut stmt = conn.prepare(sql).expect("SELECT の準備に失敗");
    let rows = stmt
        .query_map(params![param], |r| r.get::<_, String>(0))
        .expect("SELECT に失敗");
    rows.flatten().collect()
}

pub fn cmd_tag(cfg: &Config, keyword: &str) {
    let conn = connect(cfg);
    let paths = query_paths(
        &conn,
        "SELECT DISTINCT n.path
         FROM tags t JOIN notes n ON t.path = n.path
         WHERE LOWER(t.tag) LIKE ?1
         ORDER BY n.date DESC",
        &format!("%{}%", keyword.to_lowercase()),
    );
    if paths.is_empty() {
        println!("タグ '{keyword}' に一致するノートが見つかりませんでした。");
        return;
    }
    let notes = fetch_notes(&conn, &paths);
    println!("タグ '{keyword}' の検索結果 ({}件):\n", notes.len());
    print_notes(&notes);
}

pub fn cmd_title(cfg: &Config, keyword: &str) {
    let conn = connect(cfg);
    let paths = query_paths(
        &conn,
        "SELECT path FROM notes WHERE LOWER(title) LIKE ?1 ORDER BY date DESC",
        &format!("%{}%", keyword.to_lowercase()),
    );
    let notes = fetch_notes(&conn, &paths);
    println!("タイトル '{keyword}' の検索結果 ({}件):\n", notes.len());
    print_notes(&notes);
}

/// keyword を空白で語分割 (空白のみなら keyword 全体を 1 語扱い)。
fn split_terms(keyword: &str) -> Vec<&str> {
    let terms: Vec<&str> = keyword.split_whitespace().collect();
    if terms.is_empty() {
        vec![keyword]
    } else {
        terms
    }
}

/// FTS5 trigram 用にキーワードを MATCH 式へ変換する。
/// 入力全体を 1 つの quote で囲むと連続一致要求で 0 件化するため語ごと quote が必須。
fn fts_query(keyword: &str) -> String {
    split_terms(keyword)
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// FTS5 全文検索の path を BM25 relevance 順 (best-first) で返す。
/// 短語 (<3 文字) を 1 語でも含めば LIKE フォールバック。判定は語ごと。
fn bm25_ranked_paths(conn: &Connection, keyword: &str, limit: i64) -> Vec<String> {
    let terms = split_terms(keyword);
    if terms.iter().all(|t| t.chars().count() >= 3) {
        // FTS5 の特殊カラム `rank` = BM25 (重みデフォルト)。昇順で関連度が高い順。
        let mut stmt = conn
            .prepare("SELECT path FROM notes_fts WHERE notes_fts MATCH ?1 ORDER BY rank LIMIT ?2")
            .expect("FTS SELECT の準備に失敗");
        let rows = stmt
            .query_map(params![fts_query(keyword), limit], |r| {
                r.get::<_, String>(0)
            })
            .expect("FTS SELECT に失敗");
        rows.flatten().collect()
    } else {
        // trigram で拾えない短語 (<3 文字) を含む。全語 AND の LIKE フォールバック。
        let mut clauses: Vec<&str> = Vec::new();
        let mut like_params: Vec<String> = Vec::new();
        for t in &terms {
            clauses.push("(LOWER(f.content) LIKE ? OR LOWER(f.title) LIKE ?)");
            let like = format!("%{}%", t.to_lowercase());
            like_params.push(like.clone());
            like_params.push(like);
        }
        let sql = format!(
            "SELECT n.path
             FROM notes_fts f JOIN notes n ON f.path = n.path
             WHERE {}
             ORDER BY n.date DESC
             LIMIT {limit}",
            clauses.join(" AND ")
        );
        let mut stmt = conn.prepare(&sql).expect("LIKE SELECT の準備に失敗");
        let rows = stmt
            .query_map(params_from_iter(like_params.iter()), |r| {
                r.get::<_, String>(0)
            })
            .expect("LIKE SELECT に失敗");
        rows.flatten().collect()
    }
}

pub fn cmd_find(cfg: &Config, words: &[String]) {
    // find はフラグを持たないため全語を結合 (無クォートの複数語でも語落ちしない)。
    let keyword = words.join(" ");
    let conn = connect(cfg);
    let limit = cfg.bm25_limit;
    let paths = bm25_ranked_paths(&conn, &keyword, limit);
    let notes = fetch_notes(&conn, &paths);
    // 短語 (<3字) を含むと trigram 不可で LIKE フォールバック (date 降順)。順序の正体を正直に出す。
    let order = if split_terms(&keyword).iter().all(|t| t.chars().count() >= 3) {
        "BM25 relevance 順"
    } else {
        "date 降順 (短語を含み relevance 算出不可)"
    };
    // 上限に達した場合は「全体がこの件数」と誤読されないよう打ち切りを明示する。
    let capped = if notes.len() as i64 >= limit {
        format!(" ※上限到達: 上位{limit}件で打ち切り (全ヒット数は不明)")
    } else {
        String::new()
    };
    println!(
        "全文検索 '{keyword}' の結果 ({}件, {order}){capped}:\n",
        notes.len()
    );
    print_notes(&notes);
}

pub fn cmd_list_tags(cfg: &Config) {
    let conn = connect(cfg);
    let mut stmt = conn
        .prepare("SELECT tag, COUNT(*) AS n FROM tags GROUP BY tag ORDER BY n DESC, tag")
        .expect("tags 集計の準備に失敗");
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .expect("tags 集計に失敗")
        .flatten()
        .collect();
    println!("タグ一覧 ({}件):\n", rows.len());
    for (tag, n) in rows {
        println!("  {tag} ({n}件)");
    }
}

pub fn cmd_recent(cfg: &Config, count: usize) {
    let conn = connect(cfg);
    let mut stmt = conn
        .prepare("SELECT path FROM notes WHERE date != '' ORDER BY date DESC LIMIT ?1")
        .expect("recent SELECT の準備に失敗");
    let paths: Vec<String> = stmt
        .query_map(params![count as i64], |r| r.get::<_, String>(0))
        .expect("recent SELECT に失敗")
        .flatten()
        .collect();
    let notes = fetch_notes(&conn, &paths);
    println!("最近のノート (上位{count}件):\n");
    print_notes(&notes);
}

fn embeddings_available(cfg: &Config) -> bool {
    cfg.embeddings_dir().join("embeddings.safetensors").exists()
        && cfg.embeddings_dir().join("metadata.json").exists()
}

pub fn cmd_semantic(cfg: &Config, query: &str, top: usize) {
    #[cfg(feature = "semantic")]
    {
        let ranked = match crate::embed::semantic_ranked(cfg, query, top) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: {e}。");
                eprintln!("  mikke embed を実行してください (semantic feature が必要)。");
                std::process::exit(1);
            }
        };
        let conn = connect(cfg);
        let paths: Vec<String> = ranked.iter().map(|(p, _)| p.clone()).collect();
        let scores: HashMap<&str, f64> = ranked.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let mut notes = fetch_notes(&conn, &paths);
        for n in &mut notes {
            n.score = Some(*scores.get(n.path.as_str()).unwrap_or(&0.0));
        }
        println!("セマンティック検索 '{query}' の結果 (上位{top}件):\n");
        print_notes(&notes);
    }
    #[cfg(not(feature = "semantic"))]
    {
        let _ = (cfg, query, top);
        // feature 無効ビルドでは silent に劣化させず明示エラーで exit する。
        eprintln!("Error: semantic 検索はこのビルドで無効です (cargo build --features semantic で有効化)。");
        eprintln!("  semantic 付きビルドを使うか、find/hybrid を使ってください。");
        std::process::exit(1);
    }
}

/// hybrid の semantic ストリーム。使えない場合は None (BM25 のみへ degrade)。
fn semantic_stream(cfg: &Config, query: &str, depth: usize) -> Option<Vec<String>> {
    if !embeddings_available(cfg) {
        if cfg.semantic_enabled {
            eprintln!("Note: 埋め込み未構築のため BM25 のみで実行します (mikke embed で有効化)。");
        }
        return None;
    }
    #[cfg(feature = "semantic")]
    return match crate::embed::semantic_ranked(cfg, query, depth) {
        Ok(sem) => Some(sem.into_iter().map(|(p, _)| p).collect()),
        Err(e) => {
            eprintln!("Warning: semantic 検索に失敗 ({e})、BM25 のみで継続します。");
            None
        }
    };
    #[cfg(not(feature = "semantic"))]
    {
        let _ = (query, depth);
        eprintln!(
            "Warning: semantic 検索に失敗 (このビルドは semantic 無効)、BM25 のみで継続します。"
        );
        None
    }
}

/// BM25 (キーワード) と semantic (意味) を RRF 融合するハイブリッド検索。
pub fn cmd_hybrid(cfg: &Config, query: &str, top: usize) {
    let candidate_depth = top as i64 * cfg.candidate_factor; // 各ストリームから多めに取って融合する
    let conn = connect(cfg);

    // --- ストリーム 1: BM25 ---
    let bm25_paths = bm25_ranked_paths(&conn, query, candidate_depth);
    let bm25_rank: HashMap<&str, i64> = bm25_paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i as i64 + 1))
        .collect();

    // --- ストリーム 2: semantic (未構築なら BM25 のみへ degrade) ---
    let vector_paths = semantic_stream(cfg, query, candidate_depth as usize);
    let has_vector = vector_paths.is_some();
    let vector_paths = vector_paths.unwrap_or_default();
    let vector_rank: HashMap<&str, i64> = vector_paths
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i as i64 + 1))
        .collect();

    // --- 重み正規化 (欠けたストリームは 0 にして再正規化) ---
    let mut w_bm25 = cfg.bm25_weight;
    let mut w_vec = if has_vector { cfg.vector_weight } else { 0.0 };
    let total_w = w_bm25 + w_vec;
    if total_w > 0.0 {
        w_bm25 /= total_w;
        w_vec /= total_w;
    }

    // --- RRF 融合 (順序は bm25 順 → 未出現の vec 順で決定的に) ---
    let mut order: Vec<String> = bm25_paths.clone();
    for p in &vector_paths {
        if !bm25_rank.contains_key(p.as_str()) {
            order.push(p.clone());
        }
    }
    let mut fused: Vec<(String, f64, String)> = order
        .into_iter()
        .map(|p| {
            let mut score = 0.0;
            let mut streams: Vec<&str> = Vec::new();
            if let Some(r) = bm25_rank.get(p.as_str()) {
                score += w_bm25 * (1.0 / (cfg.rrf_k + r) as f64);
                streams.push("bm25");
            }
            if let Some(r) = vector_rank.get(p.as_str()) {
                score += w_vec * (1.0 / (cfg.rrf_k + r) as f64);
                streams.push("vec");
            }
            (p, score, streams.join("+"))
        })
        .collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused.truncate(top);

    let paths: Vec<String> = fused.iter().map(|(p, _, _)| p.clone()).collect();
    let meta: HashMap<&str, (f64, &str)> = fused
        .iter()
        .map(|(p, s, v)| (p.as_str(), (*s, v.as_str())))
        .collect();
    let mut notes = fetch_notes(&conn, &paths);
    for n in &mut notes {
        if let Some((score, via)) = meta.get(n.path.as_str()) {
            n.score = Some(*score);
            n.via = Some((*via).to_string());
        }
    }
    notes.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mode = if has_vector {
        "BM25 + semantic"
    } else {
        "BM25 のみ (semantic 未構築)"
    };
    println!("ハイブリッド検索 '{query}' の結果 (上位{top}件, {mode}):\n");
    print_notes(&notes);
}
