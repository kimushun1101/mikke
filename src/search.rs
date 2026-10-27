//! 検索コマンド群 (find / tag / title / semantic / hybrid / list-tags / recent / links / backlinks)。
//!
//! FTS 変換 (fts_query): 空白で語分割 → 各語を個別に `"..."` quote (内部 " は "") → AND/OR 連結。
//! find のクエリは MATCH/LIKE 経路を保つため空白を正規化せず、幅の畳み込み (fold_width) だけをかける。
//! trigram 制約: 各語 3 文字以上なら `ORDER BY rank` (FTS5 BM25)。1 語でも <3 文字を含めば
//!   LIKE フォールバック (各語の条件を AND/OR で結び、簡易スコア順)。
//! find の見出し: 順序を正直に表示。bm25_limit 到達時は打ち切りを明示。
//! tag/title: 検索列と keyword を正規化した mikke_lower(...) LIKE %kw% 部分一致、SORT_KEY 降順。
//! LIKE の小文字化は列・keyword とも Rust の to_lowercase (SQLite の LOWER は ASCII だけ)。
//! keyword の `\` `%` `_` は ESCAPE '\' で逃がし、ワイルドカードにしない。
//! recent: SORT_KEY != '' を SORT_KEY 降順 LIMIT。
//! SORT_KEY は「date、空なら updated」。同じキーは path 昇順。
//! list-tags: GROUP BY tag ORDER BY COUNT(*) DESC, tag。
//! hybrid RRF: 各ストリーム top_n*candidate_factor → rank 1 始まり → score += w * 1/(rrf_k+rank)。
//!   semantic 未構築なら vec 重み 0 で再正規化し BM25 のみへ degrade。via に bm25/vec/bm25+vec。
//! links/backlinks: links テーブル (wikilink の生 target) を読むリンクグラフ。target →
//!   ノートの解決規則・件数・出力は docs/SPEC.md「リンクグラフ」(テキスト出力のみ)。
//! --json: stdout を JSON Lines (1 行目メタ行 + 1 件 1 行) に切り替える。スキーマは docs/SPEC.md。

#![allow(dead_code)]

use crate::config::Config;
use crate::index;
use crate::normalize::{fold_width, search_text};
use crate::scan;
use rusqlite::functions::FunctionFlags;
use rusqlite::{params, params_from_iter, Connection, OpenFlags};
use std::collections::HashMap;

/// 検索結果 1 件。--json ではこのまま 1 行の JSON へ serialize する (field 順 = キー順)。
#[derive(serde::Serialize)]
pub struct Hit {
    pub path: String,
    pub title: String,
    pub date: String,
    /// 並びのキーの補い (date が空なら updated で並べる)。空なら ""。
    pub updated: String,
    pub tags: Vec<String>,
    pub summary: String,
    /// semantic/hybrid のみ。JSON は f64 全精度 (テキスト出力の 4 桁丸めと異なる)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// hybrid のみ (bm25 / vec / bm25+vec)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// find --snippet のみ。生ファイルで検索語が当たった行。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<LineMatch>>,
}

/// find --snippet で表示する当たり行 1 行。
#[derive(serde::Serialize)]
pub struct LineMatch {
    pub line: usize,
    pub text: String,
}

/// --json の 1 行目メタ行。テキスト見出しが持つ情報 (件数・順序種別・打ち切り・degrade) を
/// JSON でも失わないための行。hit 行と区別できるよう type: "meta" を付ける。
#[derive(serde::Serialize)]
struct Meta {
    r#type: &'static str,
    command: &'static str,
    count: usize,
    /// find / hybrid のみ: "all" (全語 AND) | "any" (いずれか OR)。
    /// hybrid では BM25 ストリームの結合方法。
    #[serde(skip_serializing_if = "Option::is_none")]
    r#match: Option<&'static str>,
    /// find / hybrid のみ: "relevance" (BM25 順) | "approx" (短語 fallback の簡易スコア順)。
    /// hybrid では BM25 ストリームの順序。
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<&'static str>,
    /// find / tag / title のみ: 上限 (find は --top か bm25_limit、tag / title は --top) 到達による
    /// 打ち切り (true なら全ヒット数は不明)。tag / title は --top を付けた時だけ出す。
    #[serde(skip_serializing_if = "Option::is_none")]
    capped: Option<bool>,
    /// hybrid のみ: semantic ストリームが使えず BM25 のみへ degrade したか (未構築・実行時失敗とも)。
    #[serde(skip_serializing_if = "Option::is_none")]
    degraded: Option<bool>,
}

impl Meta {
    fn new(command: &'static str, count: usize) -> Self {
        Meta {
            r#type: "meta",
            command,
            count,
            r#match: None,
            order: None,
            capped: None,
            degraded: None,
        }
    }
}

/// 1 行分の JSON を stdout へ出す (JSON モードの唯一の stdout 出口)。
fn print_json<T: serde::Serialize>(value: &T) {
    // 通常経路の値 (String/bool/usize/f64) では to_string は失敗しない (非有限 f64 も
    // serde_json は Err でなく null として出力する)。それでも Err が返る異常値では
    // panic (exit 101) で落とさず、エラー規約 (docs/SPEC.md「exit code」) の exit 2 で終了する。
    match serde_json::to_string(value) {
        Ok(line) => println!("{line}"),
        Err(e) => {
            eprintln!("Error: JSON への変換に失敗しました ({e})。");
            std::process::exit(2);
        }
    }
}

fn open_index(cfg: &Config) -> Connection {
    // CREATE を付けない: 消えた index を 0 バイトで作り、最初のクエリで panic するのを防ぐ
    match Connection::open_with_flags(
        cfg.index_path(),
        OpenFlags::default() - OpenFlags::SQLITE_OPEN_CREATE,
    ) {
        Ok(c) => {
            register_lower(&c);
            c
        }
        Err(e) => {
            eprintln!("Error: index を開けません: {e}");
            std::process::exit(2);
        }
    }
}

/// LIKE の両辺を同じ規則で小文字にする `mikke_lower(text)` を登録する。
/// SQLite の LOWER は ASCII しか変えず、keyword 側の to_lowercase と食い違うため。
fn register_lower(conn: &Connection) {
    conn.create_scalar_function(
        "mikke_lower",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|s| s.to_lowercase())),
    )
    .expect("mikke_lower の登録に失敗");
}

/// LIKE pattern の中で `\` `%` `_` を文字そのものとして扱わせる (SQL 側は ESCAPE '\')。
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 並びのキー: date、空なら updated (どちらも無ければ '')。notes を `n` として参照する。
const SORT_KEY: &str = "COALESCE(NULLIF(n.date, ''), NULLIF(n.updated, ''), '')";

/// Hit 側の並びのキー (SORT_KEY と同じ規則)。
fn sort_key(hit: &Hit) -> &str {
    if hit.date.is_empty() {
        &hit.updated
    } else {
        &hit.date
    }
}

/// 検索系コマンド用の接続。auto_rebuild 有効なら stale 時に全再構築してから開く。
pub fn connect(cfg: &Config) -> Connection {
    index::ensure_index(cfg);
    open_index(cfg)
}

/// health 用の接続。auto_rebuild の鮮度判定を通さない
/// (再構築すると health の index 鮮度診断がマスクされるため)。
pub fn connect_no_rebuild(cfg: &Config) -> Connection {
    index::ensure_index_exists(cfg);
    open_index(cfg)
}

/// path のリストに対応する notes 行を取得 (path 順序を保持)。
fn fetch_notes(conn: &Connection, paths: &[String]) -> Vec<Hit> {
    if paths.is_empty() {
        return Vec::new();
    }
    let placeholders = vec!["?"; paths.len()].join(",");

    let sql = format!(
        "SELECT path, title, date, updated, summary FROM notes WHERE path IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql).expect("notes SELECT の準備に失敗");
    let mut row_map: HashMap<String, (String, String, String, String)> = HashMap::new();
    let rows = stmt
        .query_map(params_from_iter(paths.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .expect("notes SELECT に失敗");
    for row in rows.flatten() {
        let (path, title, date, updated, summary) = row;
        row_map.insert(
            path,
            (
                title,
                date.unwrap_or_default(),
                updated.unwrap_or_default(),
                summary.unwrap_or_default(),
            ),
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
        let Some((title, date, updated, summary)) = row_map.get(p) else {
            continue;
        };
        out.push(Hit {
            path: p.clone(),
            title: title.clone(),
            date: date.clone(),
            updated: updated.clone(),
            summary: summary.clone(),
            tags: tag_map.get(p).cloned().unwrap_or_default(),
            score: None,
            via: None,
            matches: None,
        });
    }
    out
}

fn print_notes(notes: &[Hit], json: bool) {
    if json {
        // JSON モードの stdout は JSON Lines のみ (0 件文言・sentinel 文は出さない)。
        for note in notes {
            print_json(note);
        }
        return;
    }
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
        if let Some(matches) = &note.matches {
            for matched in matches {
                println!("    {}: {}", matched.line, matched.text);
            }
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

/// query_paths に件数の上限を足したもの。`?2` に上限 (None なら -1 = 無制限) を渡す。
fn query_paths_top(conn: &Connection, sql: &str, param: &str, top: Option<u64>) -> Vec<String> {
    let limit = top.map_or(-1, |n| n as i64);
    let mut stmt = conn.prepare(sql).expect("SELECT の準備に失敗");
    let rows = stmt
        .query_map(params![param, limit], |r| r.get::<_, String>(0))
        .expect("SELECT に失敗");
    rows.flatten().collect()
}

/// 前後を trim して検索用 LIKE pattern を作る。空白だけなら元の keyword を使う。
fn like_pattern(keyword: &str) -> String {
    let trimmed = keyword.trim();
    let keyword = if trimmed.is_empty() { keyword } else { trimmed };
    format!("%{}%", escape_like(&search_text(keyword).to_lowercase()))
}

/// tag / title の --top による打ち切り (find と同じ count >= N の保守的判定)。
fn top_capped(top: Option<u64>, count: usize) -> Option<bool> {
    top.map(|n| count as u64 >= n)
}

/// 打ち切りの見出しの注記 (find と同じ文言)。
fn capped_note(capped: bool, limit: impl std::fmt::Display) -> String {
    if capped {
        format!(" ※上限到達: 上位{limit}件で打ち切り (全ヒット数は不明)")
    } else {
        String::new()
    }
}

pub fn cmd_tag(cfg: &Config, keyword: &str, top: Option<u64>, json: bool) -> usize {
    let conn = connect(cfg);
    let sql = format!(
        "SELECT n.path
         FROM notes n
         WHERE n.path IN (SELECT t.path FROM tags t WHERE mikke_lower(t.tag_search) LIKE ?1 ESCAPE '\\')
         ORDER BY {SORT_KEY} DESC, n.path
         LIMIT ?2"
    );
    let paths = query_paths_top(&conn, &sql, &like_pattern(keyword), top);
    if paths.is_empty() {
        // 0 件 early-return 経路。JSON でもメタ行は必ず出す (exit code は main 側で 1)。
        if json {
            let mut meta = Meta::new("tag", 0);
            meta.capped = top_capped(top, 0);
            print_json(&meta);
        } else {
            println!("タグ '{keyword}' に一致するノートが見つかりませんでした。");
        }
        return 0;
    }
    let notes = fetch_notes(&conn, &paths);
    let capped = top_capped(top, notes.len());
    if json {
        let mut meta = Meta::new("tag", notes.len());
        meta.capped = capped;
        print_json(&meta);
    } else {
        let note = capped_note(capped == Some(true), top.unwrap_or(0));
        println!("タグ '{keyword}' の検索結果 ({}件){note}:\n", notes.len());
    }
    print_notes(&notes, json);
    notes.len()
}

pub fn cmd_title(cfg: &Config, keyword: &str, top: Option<u64>, json: bool) -> usize {
    let conn = connect(cfg);
    let sql = format!(
        "SELECT n.path
         FROM notes_fts f JOIN notes n ON f.path = n.path
         WHERE mikke_lower(f.title) LIKE ?1 ESCAPE '\\'
         ORDER BY {SORT_KEY} DESC, n.path
         LIMIT ?2"
    );
    let paths = query_paths_top(&conn, &sql, &like_pattern(keyword), top);
    let notes = fetch_notes(&conn, &paths);
    let capped = top_capped(top, notes.len());
    if json {
        let mut meta = Meta::new("title", notes.len());
        meta.capped = capped;
        print_json(&meta);
    } else {
        let note = capped_note(capped == Some(true), top.unwrap_or(0));
        println!(
            "タイトル '{keyword}' の検索結果 ({}件){note}:\n",
            notes.len()
        );
    }
    print_notes(&notes, json);
    notes.len()
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

/// 複数語をすべて含む (AND) か、いずれかを含む (OR) か。
#[derive(Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    All,
    Any,
}

impl MatchMode {
    fn from_any(any: bool) -> Self {
        if any {
            Self::Any
        } else {
            Self::All
        }
    }

    fn sql_joiner(self) -> &'static str {
        match self {
            Self::All => " AND ",
            Self::Any => " OR ",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Any => "any",
        }
    }
}

/// FTS5 trigram 用にキーワードを MATCH 式へ変換する。
/// 入力全体を 1 つの quote で囲むと連続一致要求で 0 件化するため語ごと quote が必須。
fn fts_query(keyword: &str, match_mode: MatchMode) -> String {
    split_terms(keyword)
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(match_mode.sql_joiner())
}

/// FTS5 全文検索の path を BM25 relevance 順 (best-first) で返す。
/// 短語 (<3 文字) を 1 語でも含めば LIKE フォールバック。判定は語ごと。
fn bm25_ranked_paths(
    conn: &Connection,
    keyword: &str,
    limit: i64,
    match_mode: MatchMode,
) -> Vec<String> {
    let terms = split_terms(keyword);
    if is_match_path(keyword) {
        // FTS5 の特殊カラム `rank` = BM25 (重みデフォルト)。昇順で関連度が高い順。
        let mut stmt = conn
            .prepare("SELECT path FROM notes_fts WHERE notes_fts MATCH ?1 ORDER BY rank LIMIT ?2")
            .expect("FTS SELECT の準備に失敗");
        let rows = stmt
            .query_map(params![fts_query(keyword, match_mode), limit], |r| {
                r.get::<_, String>(0)
            })
            .expect("FTS SELECT に失敗");
        rows.flatten().collect()
    } else {
        // trigram で拾えない短語 (<3 文字) を含む。AND/OR を保った LIKE フォールバック。
        let mut clauses: Vec<&str> = Vec::new();
        let mut like_params: Vec<String> = Vec::new();
        for t in &terms {
            clauses.push(
                "(mikke_lower(f.content) LIKE ? ESCAPE '\\' OR mikke_lower(f.title) LIKE ? ESCAPE '\\')",
            );
            let like = format!("%{}%", escape_like(&t.to_lowercase()));
            like_params.push(like.clone());
            like_params.push(like);
        }
        // 全候補を読み、簡易スコアで並べてから limit で切る (date で並べてから切ると古い本命が落ちる)
        let sql = format!(
            "SELECT n.path, mikke_lower(f.title), mikke_lower(f.content), {SORT_KEY}
             FROM notes_fts f JOIN notes n ON f.path = n.path
             WHERE {}",
            clauses.join(match_mode.sql_joiner())
        );
        let mut stmt = conn.prepare(&sql).expect("LIKE SELECT の準備に失敗");
        let rows = stmt
            .query_map(params_from_iter(like_params.iter()), |r| {
                Ok(LikeCandidate {
                    path: r.get(0)?,
                    title: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    content: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    key: r.get(3)?,
                })
            })
            .expect("LIKE SELECT に失敗");
        let lowered: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
        rank_like_candidates(&lowered, rows.flatten().collect(), limit)
    }
}

/// LIKE フォールバックの候補 1 件 (title / content は mikke_lower 済み、key は SORT_KEY)。
struct LikeCandidate {
    path: String,
    title: String,
    content: String,
    key: String,
}

/// 簡易スコアの定数。BM25 の tf の飽和と文書長の正規化だけを取り、AND/OR とも idf は使わない。
const LIKE_K1: f64 = 1.2;
const LIKE_B: f64 = 0.75;
/// title に語を含む時の加点 (語ごと)。
const LIKE_TITLE_BONUS: f64 = 1.0;

/// 候補を簡易スコア降順 → SORT_KEY 降順 → path 昇順に並べ、上位 limit 件の path を返す。
/// terms は小文字化済み。本文の長さは候補の平均で正規化する。
fn rank_like_candidates(terms: &[String], cands: Vec<LikeCandidate>, limit: i64) -> Vec<String> {
    let lens: Vec<f64> = cands
        .iter()
        .map(|c| c.content.chars().count() as f64)
        .collect();
    let avg = if lens.is_empty() {
        1.0
    } else {
        (lens.iter().sum::<f64>() / lens.len() as f64).max(1.0)
    };
    let mut scored: Vec<(f64, LikeCandidate)> = cands
        .into_iter()
        .zip(lens)
        .map(|(c, len)| {
            let norm = LIKE_K1 * (1.0 - LIKE_B + LIKE_B * len / avg);
            let mut score = 0.0;
            for t in terms.iter().filter(|t| !t.is_empty()) {
                let tf = c.content.matches(t.as_str()).count() as f64;
                if tf > 0.0 {
                    score += tf * (LIKE_K1 + 1.0) / (tf + norm);
                }
                if c.title.contains(t.as_str()) {
                    score += LIKE_TITLE_BONUS;
                }
            }
            (score, c)
        })
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.partial_cmp(sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.key.cmp(&a.key))
            .then_with(|| a.path.cmp(&b.path))
    });
    scored
        .into_iter()
        .take(limit.max(0) as usize)
        .map(|(_, c)| c.path)
        .collect()
}

/// bm25_ranked_paths の順序の種別 (JSON の order)。
fn bm25_order(relevance: bool) -> &'static str {
    if relevance {
        "relevance"
    } else {
        "approx"
    }
}

/// keyword が MATCH 経路 (全語 3 文字以上) に入るか。
fn is_match_path(keyword: &str) -> bool {
    split_terms(keyword).iter().all(|t| t.chars().count() >= 3)
}

const SNIPPET_MAX_MATCHES: usize = 3;
const SNIPPET_MAX_CHARS: usize = 200;

/// 長い当たり行を、正規化後の一致位置を中心に約 SNIPPET_MAX_CHARS 字へ切り出す。
/// 正規化前後の文字位置は対応付けず、そのまま原文側の位置として使う。
fn truncate_snippet(line: &str, matched: Option<(usize, usize)>) -> String {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() <= SNIPPET_MAX_CHARS {
        return line.to_string();
    }

    let start = matched
        .map(|(pos, len)| {
            let center = pos.saturating_add(len / 2).min(chars.len());
            center
                .saturating_sub(SNIPPET_MAX_CHARS / 2)
                .min(chars.len() - SNIPPET_MAX_CHARS)
        })
        .unwrap_or(0);
    let end = start + SNIPPET_MAX_CHARS;
    let mut text = String::new();
    if start > 0 {
        text.push('…');
    }
    text.extend(chars[start..end].iter());
    if end < chars.len() {
        text.push('…');
    }
    text
}

/// frontmatter が占める先頭の行数 (無い・閉じ `---` が無い時は 0)。
/// 判定は load_note と同じく、先頭の空行を飛ばした最初の行が 3 個以上の `-` であること。
fn frontmatter_line_count(lines: &[&str]) -> usize {
    let Some(open) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return 0;
    };
    let first = lines[open].trim_end();
    if first.len() < 3 || !first.chars().all(|c| c == '-') {
        return 0;
    }
    lines[open + 1..]
        .iter()
        .position(|l| l.trim_end() == "---")
        .map_or(0, |close| open + 1 + close + 1)
}

/// find のヒットを生ファイルで再確認し、検索語を含む本文の行を最大 3 行付ける。
/// frontmatter の行は見出しの title / tags / summary と重なるので外す。含む語の種類が多い行を
/// 優先して選び (AND では全語を含む行が答えの行になりやすい)、表示はファイル内の順にする。
fn add_snippets(cfg: &Config, notes: &mut [Hit], keyword: &str) {
    let terms: Vec<String> = split_terms(keyword)
        .into_iter()
        .map(|term| fold_width(term).to_lowercase())
        .collect();

    for note in notes {
        let mut candidates = Vec::new();
        let raw = scan::read_utf8_sig(&cfg.root.join(&note.path)).unwrap_or_default();
        let lines: Vec<&str> = raw.lines().collect();
        for (index, line) in lines
            .iter()
            .enumerate()
            .skip(frontmatter_line_count(&lines))
        {
            // 索引と同じ正規化 (幅の畳み込み + 和欧間スペースの削除) で判定する。
            // 削った空白の分だけ切り出しの中心がずれるのは許容する
            let normalized = search_text(line).to_lowercase();
            let hits: Vec<(usize, usize)> = terms
                .iter()
                .filter_map(|term| {
                    normalized.find(term).map(|byte_pos| {
                        (normalized[..byte_pos].chars().count(), term.chars().count())
                    })
                })
                .collect();
            if !hits.is_empty() {
                let first = hits.iter().min_by_key(|(pos, _)| *pos).copied();
                candidates.push((hits.len(), index, first));
            }
        }
        // 含む語の種類が多い順 → 早い行の順で選び、ファイル内の順に戻す
        candidates.sort_by_key(|&(count, index, _)| (std::cmp::Reverse(count), index));
        candidates.truncate(SNIPPET_MAX_MATCHES);
        candidates.sort_by_key(|&(_, index, _)| index);
        note.matches = Some(
            candidates
                .into_iter()
                .map(|(_, index, matched)| LineMatch {
                    line: index + 1,
                    text: truncate_snippet(lines[index], matched),
                })
                .collect(),
        );
    }
}

pub fn cmd_find(
    cfg: &Config,
    words: &[String],
    top: Option<u64>,
    json: bool,
    any: bool,
    snippet: bool,
) -> usize {
    // 無クォートの複数語でも語落ちさせず、既定は全語、--any はいずれかで結合する。
    let keyword = words.join(" ");
    let match_mode = MatchMode::from_any(any);
    // 検索には幅を畳んだ語を使い (語長の判定も畳んだ後)、見出しには打った語を出す
    let folded = fold_width(&keyword);
    let conn = connect(cfg);
    // --top は bm25_limit の代わり (bm25_limit は既定値であって天井ではない)
    let limit = top.map_or(cfg.bm25_limit, |n| n as i64);
    let paths = bm25_ranked_paths(&conn, &folded, limit, match_mode);
    let mut notes = fetch_notes(&conn, &paths);
    if snippet {
        add_snippets(cfg, &mut notes, &keyword);
    }
    // 短語 (<3字) を含むと trigram 不可で LIKE フォールバック (簡易スコア順)。順序の正体を正直に出す。
    let relevance = is_match_path(&folded);
    // 上限に達した場合は「全体がこの件数」と誤読されないよう打ち切りを明示する。
    let capped = notes.len() as i64 >= limit;
    if json {
        let mut meta = Meta::new("find", notes.len());
        meta.r#match = Some(match_mode.name());
        meta.order = Some(bm25_order(relevance));
        meta.capped = Some(capped);
        print_json(&meta);
    } else {
        let order = if relevance {
            "BM25 relevance 順"
        } else {
            "簡易スコア順 (短語を含み BM25 算出不可)"
        };
        let match_note = if match_mode == MatchMode::Any {
            ", いずれかを含む (OR)"
        } else {
            ""
        };
        let capped_str = capped_note(capped, limit);
        println!(
            "全文検索 '{keyword}' の結果 ({}件, {order}{match_note}){capped_str}:\n",
            notes.len()
        );
    }
    print_notes(&notes, json);
    notes.len()
}

/// list-tags --json の 1 行 ({"tag":"...","count":N})。
#[derive(serde::Serialize)]
struct TagCount<'a> {
    tag: &'a str,
    count: i64,
}

pub fn cmd_list_tags(cfg: &Config, json: bool) {
    let conn = connect(cfg);
    let mut stmt = conn
        .prepare("SELECT tag, COUNT(*) AS n FROM tags GROUP BY tag ORDER BY n DESC, tag")
        .expect("tags 集計の準備に失敗");
    let rows: Vec<(String, i64)> = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .expect("tags 集計に失敗")
        .flatten()
        .collect();
    if json {
        print_json(&Meta::new("list-tags", rows.len()));
        for (tag, n) in &rows {
            print_json(&TagCount { tag, count: *n });
        }
        return;
    }
    println!("タグ一覧 ({}件):\n", rows.len());
    for (tag, n) in rows {
        println!("  {tag} ({n}件)");
    }
}

pub fn cmd_recent(cfg: &Config, count: usize, json: bool) {
    let conn = connect(cfg);
    let sql = format!(
        "SELECT n.path FROM notes n WHERE {SORT_KEY} != '' ORDER BY {SORT_KEY} DESC, n.path LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql).expect("recent SELECT の準備に失敗");
    let paths: Vec<String> = stmt
        .query_map(params![count as i64], |r| r.get::<_, String>(0))
        .expect("recent SELECT に失敗")
        .flatten()
        .collect();
    let notes = fetch_notes(&conn, &paths);
    if json {
        print_json(&Meta::new("recent", notes.len()));
    } else {
        println!("最近のノート (上位{count}件):\n");
    }
    print_notes(&notes, json);
}

/// notes.path の全件 (path 昇順)。links テーブルは小さく、target の解決は全走査で足りる。
fn all_note_paths(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT path FROM notes ORDER BY path")
        .expect("notes SELECT の準備に失敗");
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .expect("notes SELECT に失敗");
    rows.flatten().collect()
}

/// wikilink target からノート部を取り出す (`#fragment` と末尾の `.md` は照合前に除去)。
/// ノート部が空 (`[[#anchor]]`) は自ノート内アンカーで対象外 → None。
fn target_note_part(target: &str) -> Option<&str> {
    let note = target.split('#').next().unwrap_or(target);
    let note = note.strip_suffix(".md").unwrap_or(note);
    if note.is_empty() {
        None
    } else {
        Some(note)
    }
}

/// target 表現 (fragment 除去済) が path のノートを指しうるか。
/// 規則: path から `.md` を除いた文字列に対する、パス成分単位の末尾一致
/// (`[[note-name]]` の stem 完全一致は 1 成分の末尾一致として包含)。大文字小文字は区別する。
fn target_matches(path: &str, target: &str) -> bool {
    let path_no_md = path.strip_suffix(".md").unwrap_or(path);
    path_no_md == target || path_no_md.ends_with(&format!("/{target}"))
}

/// links / backlinks の `<note>` 引数を root 相対 path へ解決する。
/// 完全一致 → 引数の `.md` を除いた文字列で target と同じ成分末尾一致のフォールバック。
/// 該当なし・複数一致は「結果 0 件」と区別して入力エラーの exit 2
/// (複数一致は silent に 1 件を選ばず、候補 path を stderr へ列挙する)。
fn resolve_note_arg(conn: &Connection, arg: &str) -> String {
    let paths = all_note_paths(conn);
    if paths.iter().any(|p| p == arg) {
        return arg.to_string();
    }
    let q = arg.strip_suffix(".md").unwrap_or(arg);
    let matches: Vec<&String> = paths.iter().filter(|p| target_matches(p, q)).collect();
    match matches.len() {
        0 => {
            eprintln!("Error: ノート '{arg}' が見つかりません (root 相対 path か stem で指定)。");
            std::process::exit(2);
        }
        1 => matches[0].clone(),
        _ => {
            eprintln!("Error: ノート '{arg}' が複数に一致します。path で一意に指定してください:");
            for p in matches {
                eprintln!("  {p}");
            }
            std::process::exit(2);
        }
    }
}

/// 発リンク一覧: 対象ノートの wikilink target を列挙し、ノートへ解決して表示する。
/// 返り値は結果件数 (= 対象 target 数。未解決の列挙も結果に数える。0 件は main 側で exit 1)。
pub fn cmd_links(cfg: &Config, note: &str) -> usize {
    let conn = connect(cfg);
    let path = resolve_note_arg(&conn, note);
    let targets = query_paths(
        &conn,
        "SELECT target FROM links WHERE path = ?1 ORDER BY target",
        &path,
    );
    let all_paths = all_note_paths(&conn);

    let mut resolved_paths: Vec<String> = Vec::new();
    let mut unresolved: Vec<&String> = Vec::new();
    let mut counted = 0usize;
    let mut resolved_count = 0usize;
    for target in &targets {
        let Some(note_part) = target_note_part(target) else {
            continue; // `[[#anchor]]` は自ノート内アンカーで対象外
        };
        counted += 1;
        let hits: Vec<&String> = all_paths
            .iter()
            .filter(|p| target_matches(p, note_part))
            .collect();
        if hits.is_empty() {
            unresolved.push(target);
        } else {
            resolved_count += 1;
            // 複数一致は全件表示 (曖昧さを隠さない)。同一ノートへの重複 target は 1 回だけ
            for h in hits {
                if !resolved_paths.contains(h) {
                    resolved_paths.push(h.clone());
                }
            }
        }
    }

    println!(
        "'{path}' の発リンク (target {counted}件, 解決 {resolved_count}件 / 未解決 {}件):\n",
        unresolved.len()
    );
    if counted == 0 {
        println!("wikilink がありません。");
        return 0;
    }
    let notes = fetch_notes(&conn, &resolved_paths);
    if !notes.is_empty() {
        print_notes(&notes, false);
    }
    // 解決できない target は黙って落とさず明示列挙 (未解決 = 壊れたリンクとは限らない — docs/SPEC.md)
    for target in unresolved {
        println!("  (未解決) [[{target}]]");
    }
    counted
}

/// 被リンク一覧: 対象ノートを指しうる target を持つノートを表示する。返り値は結果件数。
pub fn cmd_backlinks(cfg: &Config, note: &str) -> usize {
    let conn = connect(cfg);
    let path = resolve_note_arg(&conn, note);
    let mut stmt = conn
        .prepare("SELECT path, target FROM links ORDER BY path, target")
        .expect("links SELECT の準備に失敗");
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .expect("links SELECT に失敗");
    let mut sources: Vec<String> = Vec::new();
    for (src, target) in rows.flatten() {
        let Some(note_part) = target_note_part(&target) else {
            continue;
        };
        if target_matches(&path, note_part) && !sources.contains(&src) {
            sources.push(src);
        }
    }
    let mut notes = fetch_notes(&conn, &sources);
    // tag/title と同じ SORT_KEY 降順 (キーが空なら末尾)、同じキーは path 昇順で決定的に
    notes.sort_by(|a, b| sort_key(b).cmp(sort_key(a)).then(a.path.cmp(&b.path)));
    println!("'{path}' の被リンク ({}件):\n", notes.len());
    print_notes(&notes, false);
    notes.len()
}

fn embeddings_available(cfg: &Config) -> bool {
    cfg.embeddings_dir().join("embeddings.safetensors").exists()
        && cfg.embeddings_dir().join("metadata.json").exists()
}

pub fn cmd_semantic(cfg: &Config, query: &str, top: usize, json: bool) -> usize {
    #[cfg(feature = "semantic")]
    {
        let ranked = match crate::embed::semantic_ranked(cfg, query, top) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Error: {e}。");
                eprintln!("  mikke embed を実行してください (semantic feature が必要)。");
                std::process::exit(2);
            }
        };
        let conn = connect(cfg);
        let paths: Vec<String> = ranked.iter().map(|(p, _)| p.clone()).collect();
        let scores: HashMap<&str, f64> = ranked.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let mut notes = fetch_notes(&conn, &paths);
        for n in &mut notes {
            n.score = Some(*scores.get(n.path.as_str()).unwrap_or(&0.0));
        }
        if json {
            print_json(&Meta::new("semantic", notes.len()));
        } else {
            println!("セマンティック検索 '{query}' の結果 (上位{top}件):\n");
        }
        print_notes(&notes, json);
        notes.len()
    }
    #[cfg(not(feature = "semantic"))]
    {
        let _ = (cfg, query, top, json);
        // feature 無効ビルドでは silent に劣化させず明示エラーで exit する。
        eprintln!("Error: semantic 検索はこのビルドで無効です (cargo install --git https://github.com/kimushun1101/mikke --locked --features semantic で有効化)。");
        eprintln!("  semantic 付きビルドを使うか、find/hybrid を使ってください。");
        std::process::exit(2);
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
pub fn cmd_hybrid(cfg: &Config, query: &str, top: usize, json: bool, any: bool) -> usize {
    let candidate_depth = top as i64 * cfg.candidate_factor; // 各ストリームから多めに取って融合する
    let conn = connect(cfg);
    let match_mode = MatchMode::from_any(any);

    // --- ストリーム 1: BM25 (幅を畳んだ語で引く。semantic には原文を渡す) ---
    let folded = fold_width(query);
    let bm25_paths = bm25_ranked_paths(&conn, &folded, candidate_depth, match_mode);
    // semantic のモデル読み込みと推論の間は閉じておく (Windows では開いている間 index を差し替えられない)
    drop(conn);
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
    let conn = open_index(cfg);
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

    let relevance = is_match_path(&folded);
    if json {
        let mut meta = Meta::new("hybrid", notes.len());
        meta.r#match = Some(match_mode.name());
        meta.order = Some(bm25_order(relevance));
        meta.degraded = Some(!has_vector);
        print_json(&meta);
    } else {
        let mode = if has_vector {
            "BM25 + semantic"
        } else {
            "BM25 のみ (semantic 未構築)"
        };
        // BM25 ストリームが簡易スコア順の時はそれを示す (find の見出しと同じく順序の正体を出す)
        let approx = if relevance {
            ""
        } else {
            ", BM25 は簡易スコア順 (短語を含む)"
        };
        let match_note = if match_mode == MatchMode::Any {
            ", BM25 はいずれかを含む (OR)"
        } else {
            ""
        };
        println!("ハイブリッド検索 '{query}' の結果 (上位{top}件, {mode}{approx}{match_note}):\n");
    }
    print_notes(&notes, json);
    notes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(path: &str, title: &str, content: &str, key: &str) -> LikeCandidate {
        LikeCandidate {
            path: path.into(),
            title: title.into(),
            content: content.into(),
            key: key.into(),
        }
    }

    /// 簡易スコア: 本文が短く全語を繰り返し含むノートが、長く散発的に含むノートより上に来る。
    /// 同点は SORT_KEY 降順 → path 昇順。
    #[test]
    fn like_score_prefers_dense_short_notes_and_breaks_ties() {
        let long = format!("{}巣箱 野鳥 巣箱 野鳥", "池のカモを数えた。".repeat(200));
        let dense = "巣箱に野鳥が来た。巣箱の野鳥を数えた。";
        let terms = vec!["巣箱".to_string(), "野鳥".to_string()];
        let ranked = rank_like_candidates(
            &terms,
            vec![
                cand("long.md", "記録", &long, "2026-05-01"),
                cand("dense.md", "記録", dense, "2020-01-01"),
                cand("b.md", "同点", "巣箱 野鳥", "2024-01-01"),
                cand("a.md", "同点", "巣箱 野鳥", "2024-01-01"),
                cand("c.md", "同点", "巣箱 野鳥", "2025-01-01"),
            ],
            10,
        );
        let pos = |p: &str| ranked.iter().position(|x| x == p).unwrap();
        assert!(pos("dense.md") < pos("long.md"), "{ranked:?}");
        assert!(
            pos("c.md") < pos("a.md") && pos("a.md") < pos("b.md"),
            "{ranked:?}"
        );
        assert_eq!(
            rank_like_candidates(&terms, vec![cand("x.md", "", "巣箱", "")], 0).len(),
            0
        );
    }

    /// 成分末尾一致は `/` 境界で切る — 文字列 suffix 一致 (`[[note]]` が
    /// `notes/my-note.md` に当たる類の偽陽性) を否定する。
    #[test]
    fn target_matches_component_boundary_not_string_suffix() {
        assert!(!target_matches("notes/my-note.md", "note"));
        assert!(!target_matches("notes/my-note.md", "tes/my-note"));
        assert!(target_matches("notes/my-note.md", "my-note"));
        assert!(target_matches("notes/my-note.md", "notes/my-note"));
    }

    /// target 末尾の `.md` は fragment と同じ前処理で除く —
    /// `[[notes/short.md]]` 形式が path 側の `.md` 除去と対で解決される。
    #[test]
    fn target_note_part_strips_trailing_md() {
        assert_eq!(target_note_part("notes/short.md"), Some("notes/short"));
        assert_eq!(target_note_part("short.md"), Some("short"));
        // fragment → `.md` の順で除く
        assert_eq!(target_note_part("notes/short.md#sec"), Some("notes/short"));
    }

    /// `#fragment` は照合前に除去され、fragment 付き target が解決できる。
    /// ノート部が空 (`[[#anchor]]`) の対象外化は fixture link-hub.md の
    /// `[[#規則]]` + golden links_hub.txt (target 4件) が e2e で固定済み。
    #[test]
    fn target_note_part_strips_fragment() {
        assert_eq!(target_note_part("x#sec"), Some("x"));
        assert_eq!(
            target_note_part("filename-title#由来"),
            Some("filename-title")
        );
    }
}
