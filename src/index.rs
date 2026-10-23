//! インデックス生成 (`mikke index`)。
//!
//! SQLite スキーマ (一時ファイルに 1 transaction で作り、rename で差し替える全再構築):
//!   notes(path PK, title NOT NULL, date, updated, summary, word_count)
//!     INDEX idx_notes_date ON notes(date DESC)
//!   tags(path, tag, tag_search, PK(path,tag))  INDEX idx_tags_tag ON tags(tag)
//!   links(path, target, PK(path,target))
//!   notes_fts USING fts5(path UNINDEXED, title, content, tokenize='trigram')
//!   meta(key PK, value)   -- meta['generated'] に生成時刻 (health 鮮度判定 +
//!                            auto_rebuild の stale 判定)、meta['scanned_paths_sha256'] に
//!                            走査 path 集合のスナップショット、meta['index_format'] に索引形式
//!
//! meta['generated'] は epoch 秒 (ナノ秒精度の小数文字列) で保存する。
//! 秒精度だと mtime (小数秒) との比較で秒未満の更新を取りこぼすため
//! (index フォーマットは内部表現で互換保証しない — docs/SPEC.md)。

#![allow(dead_code)]

use crate::config::{display_path, to_posix, Config};
use crate::normalize::search_text;
use crate::scan;
use rusqlite::{params, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct BuildIssues {
    frontmatter: Vec<(String, String, String)>,
    empty_tags: Vec<(String, usize)>,
}

impl BuildIssues {
    fn count(&self) -> usize {
        self.frontmatter.len() + self.empty_tags.len()
    }
}

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
        tag_search TEXT NOT NULL,
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
const META_INDEX_FORMAT: &str = "index_format";
// 正規化規則 (src/normalize.rs) や検索用の列を変えた場合は上げる。
const INDEX_FORMAT: &str = "4";

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

/// index を全再構築し、frontmatter 破損と tags 空要素の検出結果を返す。
/// use_stderr=true で進捗出力を stderr へ (auto-build 時に stdout を汚さない)。
/// 既存のファイルが mikke の index でない・読めない時は、消さずに exit 2 にする。
pub fn build_to(cfg: &Config, use_stderr: bool) -> BuildIssues {
    let index_path = cfg.index_path();
    prepare(cfg, |s| decide_index(&index_path, s), use_stderr)
        .expect("decide_index は Skip を返さない")
}

// --- 既存の index の判定 ---

/// mikke の index が持ちうる表 (fts5 の shadow table を含む)。
const MIKKE_TABLES: &[&str] = &[
    "notes",
    "tags",
    "links",
    "notes_fts",
    "notes_fts_data",
    "notes_fts_idx",
    "notes_fts_content",
    "notes_fts_docsize",
    "notes_fts_config",
    "meta",
];
/// 検索と health が読む表。
const REQUIRED_TABLES: &[&str] = &["notes", "tags", "links", "notes_fts", "meta"];
/// notes の列 (cid 順)。列を変える時は、過去の列も受け入れるようにする。
const NOTES_COLUMNS: &[&str] = &["path", "title", "date", "updated", "summary", "word_count"];
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// [index] path の既存ファイルの状態。
pub(crate) enum IndexState {
    Missing,
    Index {
        format: Option<String>,
        generated: Option<f64>,
        scanned_hash: Option<String>,
        tables_ok: bool,
    },
    /// 作りかけ (0 バイト・表が無い・mikke の表だけで meta テーブルが無い)
    Residue,
    /// mikke の index でない (理由)
    Foreign(&'static str),
    /// 開けない・読めない
    Unreadable(String),
}

/// 既存の index を短命の接続で調べる。md の走査はしない。
/// 開いた File と Connection は返る前に閉じる (Windows では開いたままだと書き手の rename を塞ぐ)。
pub(crate) fn inspect_index(path: &Path) -> IndexState {
    let md = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return IndexState::Missing,
        Err(e) => return IndexState::Unreadable(e.to_string()),
    };
    if md.is_dir() {
        return IndexState::Foreign("ディレクトリです");
    }
    if !md.is_file() {
        return IndexState::Foreign("通常のファイルではありません");
    }
    if md.len() == 0 {
        return IndexState::Residue;
    }
    // byte 18/19 (読み書きの形式) まで読む。2 は WAL
    let mut header = Vec::with_capacity(20);
    let read = File::open(path).and_then(|f| f.take(20).read_to_end(&mut header).map(|_| ()));
    if let Err(e) = read {
        return IndexState::Unreadable(e.to_string());
    }
    if !header.starts_with(SQLITE_HEADER) {
        return IndexState::Foreign("SQLite のファイルではありません");
    }
    // mikke は WAL を使わない。開くと SQLite が -wal を書き戻して消すので、開かない
    if header.len() >= 20 && (header[18] == 2 || header[19] == 2) {
        return IndexState::Foreign("WAL モードの SQLite です");
    }
    // 先に読み取り専用で形を見る。読み取り専用の接続はファイルに書かない
    let conn = match open_probe(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(e) => return IndexState::Unreadable(e.to_string()),
    };
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(())) {
        Ok(()) => {}
        Err(e) if is_hot_journal(&e) => {
            drop(conn);
            return inspect_with_hot_journal(path, &e);
        }
        Err(e) => return IndexState::Unreadable(e.to_string()),
    }
    let state = classify_index(&conn);
    drop(conn);
    state
}

/// CREATE を付けないので無いファイルを作らない。path を URI として読まないよう URI も付けない。
fn open_probe(path: &Path, mode: OpenFlags) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, mode | OpenFlags::SQLITE_OPEN_NO_MUTEX)
}

/// 巻き戻しの要る journal が残っていて、読み取り専用では読めない。
fn is_hot_journal(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if f.extended_code == rusqlite::ffi::SQLITE_READONLY_ROLLBACK)
}

/// hot journal 付きのファイル。journal を無視して (immutable) 形だけ見て、mikke の index か
/// 作りかけの時だけ読み書きで開き直し、旧版 (最終 path に直接書いていた) の journal を巻き戻す。
/// それ以外のファイルと journal には触らない。
fn inspect_with_hot_journal(path: &Path, hot: &rusqlite::Error) -> IndexState {
    let unreadable =
        || IndexState::Unreadable(format!("{hot} (巻き戻しの要る journal があります)"));
    let shape = match Connection::open_with_flags(
        immutable_uri(path),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(conn) => classify_index(&conn),
        Err(_) => return unreadable(),
    };
    match shape {
        IndexState::Index { .. } | IndexState::Residue => {}
        IndexState::Foreign(r) => return IndexState::Foreign(r),
        _ => return unreadable(),
    }
    let conn = match open_probe(path, OpenFlags::SQLITE_OPEN_READ_WRITE) {
        Ok(c) => c,
        Err(e) => return IndexState::Unreadable(e.to_string()),
    };
    let state = classify_index(&conn);
    drop(conn);
    state
}

/// `file:<path>?immutable=1`。英数字以外は %HH にする (`?` `#` `%` と Windows の `\` を含む)。
fn immutable_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for &b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    uri
}

fn classify_index(conn: &Connection) -> IndexState {
    let tables = match query_names(conn, "SELECT name FROM sqlite_master WHERE type = 'table'") {
        Ok(names) => names
            .into_iter()
            .filter(|n| !n.starts_with("sqlite_"))
            .collect::<HashSet<String>>(),
        Err(e) => return IndexState::Unreadable(e.to_string()),
    };
    if tables.is_empty() {
        return IndexState::Residue;
    }
    // 指紋: 表の名前がすべて mikke のもので、notes の列が順番まで一致する
    if !tables.contains("notes") {
        return IndexState::Foreign("mikke の index の表がありません");
    }
    if !tables.iter().all(|t| MIKKE_TABLES.contains(&t.as_str())) {
        return IndexState::Foreign("表の形が mikke の index と違います");
    }
    match query_names(
        conn,
        "SELECT name FROM pragma_table_info('notes') ORDER BY cid",
    ) {
        Ok(cols)
            if cols
                .iter()
                .map(String::as_str)
                .eq(NOTES_COLUMNS.iter().copied()) => {}
        Ok(_) => return IndexState::Foreign("表の形が mikke の index と違います"),
        Err(e) => return IndexState::Unreadable(e.to_string()),
    }
    // v0.3.1 以前の build が SCHEMA の途中で止まった跡 (meta は最後に作っていた)
    if !tables.contains("meta") {
        return IndexState::Residue;
    }
    let rows = (|| -> rusqlite::Result<Vec<(String, Option<String>)>> {
        let mut stmt = conn.prepare(
            "SELECT key, value FROM meta WHERE key IN ('index_format', 'generated', 'scanned_paths_sha256')",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    })();
    let Ok(rows) = rows else {
        return IndexState::Foreign("meta の形が mikke の index と違います");
    };
    let value = |key: &str| {
        rows.iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.clone())
    };
    IndexState::Index {
        format: value(META_INDEX_FORMAT),
        generated: value("generated").and_then(|v| v.parse::<f64>().ok()),
        scanned_hash: value(META_SCANNED_PATHS),
        tables_ok: REQUIRED_TABLES.iter().all(|t| tables.contains(*t)),
    }
}

fn query_names(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    rows.collect()
}

// --- 経路ごとの決定 ---

enum Need {
    Skip,
    Build(Option<&'static str>),
    Fail(String),
}

const NOTICE_MISSING: &str = "インデックスが無いため生成しています...";
const NOTICE_RESIDUE: &str = "index の生成が途中で止まっているため再構築しています...";
const NOTICE_BROKEN: &str = "index が壊れているため再構築しています...";
const NOTICE_INCOMPLETE: &str = "前回の index 生成が完了していないため再構築しています...";
const NOTICE_FORMAT: &str = "索引の形式が変わったため index を再構築しています...";
const NOTICE_STALE: &str = "ソースの更新を検知したため index を再構築しています...";

// Fail の文言には「再構築」「生成しています」を含めない
const KEEP_HINT: &str = "  ファイルは変更していません。[index] path の設定を確かめてください。mikke が作った index なら、そのファイルを消してから mikke index を実行してください。";

fn foreign_msg(path: &Path, reason: &str) -> String {
    format!(
        "Error: [index] path が mikke の index でない既存ファイルを指しています: {} ({reason})\n{KEEP_HINT}",
        display_path(path)
    )
}

fn unreadable_msg(path: &Path, e: &str) -> String {
    format!(
        "Error: index を確かめられません: {}: {e}\n{KEEP_HINT}",
        display_path(path)
    )
}

fn residue_msg(path: &Path) -> String {
    format!(
        "Error: index が壊れています (生成が途中で止まった可能性): {}。mikke index で作り直してください",
        display_path(path)
    )
}

fn broken_msg(path: &Path) -> String {
    format!(
        "Error: index が壊れています (必要なテーブルがありません): {}。mikke index で作り直してください",
        display_path(path)
    )
}

/// 検索系 (find / tag / title / semantic / hybrid / list-tags / recent / links / backlinks)。
fn decide_search(cfg: &Config, path: &Path, state: &IndexState) -> Need {
    match state {
        IndexState::Missing => Need::Build(Some(NOTICE_MISSING)),
        IndexState::Foreign(r) => Need::Fail(foreign_msg(path, r)),
        IndexState::Unreadable(e) => Need::Fail(unreadable_msg(path, e)),
        IndexState::Residue if cfg.auto_rebuild => Need::Build(Some(NOTICE_RESIDUE)),
        IndexState::Residue => Need::Fail(residue_msg(path)),
        IndexState::Index {
            format,
            generated,
            scanned_hash,
            tables_ok,
        } => {
            if !tables_ok {
                Need::Build(Some(NOTICE_BROKEN))
            } else if generated.is_none() {
                Need::Build(Some(NOTICE_INCOMPLETE))
            } else if format.as_deref() != Some(INDEX_FORMAT) {
                Need::Build(Some(NOTICE_FORMAT))
            } else if cfg.auto_rebuild && index_is_stale(cfg, *generated, scanned_hash.as_deref()) {
                Need::Build(Some(NOTICE_STALE))
            } else {
                Need::Skip
            }
        }
    }
}

/// health。形式違いと generated 無しは作り直さない (health 自身が鮮度欄で知らせる)。
fn decide_health(path: &Path, state: &IndexState) -> Need {
    match state {
        IndexState::Missing => Need::Build(Some(NOTICE_MISSING)),
        IndexState::Foreign(r) => Need::Fail(foreign_msg(path, r)),
        IndexState::Unreadable(e) => Need::Fail(unreadable_msg(path, e)),
        IndexState::Residue => Need::Fail(residue_msg(path)),
        IndexState::Index {
            tables_ok: false, ..
        } => Need::Fail(broken_msg(path)),
        IndexState::Index { .. } => Need::Skip,
    }
}

/// `mikke index`。mikke の index・作りかけ・無い path は作り直す (告知なし)。
fn decide_index(path: &Path, state: &IndexState) -> Need {
    match state {
        IndexState::Foreign(r) => Need::Fail(foreign_msg(path, r)),
        IndexState::Unreadable(e) => Need::Fail(unreadable_msg(path, e)),
        _ => Need::Build(None),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

/// 判定 → (要る時だけ) ロック → 判定し直し → 作り直し。
/// 最初の判定までは、ファイルもフォルダも作らない。告知は実際に作るプロセスだけが出す。
fn prepare(
    cfg: &Config,
    decide: impl Fn(&IndexState) -> Need,
    use_stderr: bool,
) -> Option<BuildIssues> {
    let index_path = cfg.index_path();
    match decide(&inspect_index(&index_path)) {
        Need::Fail(msg) => fail(&msg),
        Need::Skip => return None,
        Need::Build(_) => {}
    }
    if let Some(parent) = index_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let lock = acquire_lock(&index_path);
    // すぐにロックを取れた時も判定し直す (最初の判定の後に他のプロセスが作り終えている場合がある)
    let notice = match decide(&inspect_index(&index_path)) {
        Need::Fail(msg) => fail(&msg),
        Need::Skip => return None,
        Need::Build(notice) => notice,
    };
    if let Some(msg) = notice {
        eprintln!("{msg}");
    }
    Some(write_index(cfg, use_stderr, &lock))
}

// --- ロックと一時ファイル ---

/// path のファイル名に suffix を足した、同じフォルダの path (非 UTF-8 のパスも壊さない)。
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let Some(name) = path.file_name() else {
        fail(&format!(
            "Error: [index] path がファイル名を持ちません: {}",
            display_path(path)
        ));
    };
    let mut name = name.to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// 作り直しを 1 本にするための排他ロック。持っていることを write_index の引数で示す
/// (同じプロセスで 2 回取ると Linux の flock は自分をブロックする)。
struct BuildLock {
    _file: Option<File>,
    held: bool,
}

/// `<path>.lock` を排他ロックする。ロックファイルは消さない (消すと、待つ側と新しく作った側で
/// 別の inode をロックしてしまう)。kill や exit で止まっても OS がロックを外す。
fn acquire_lock(index_path: &Path) -> BuildLock {
    let lock_path = sibling(index_path, ".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|e| {
            fail(&format!(
                "Error: index のロックを取れません: {}: {e}",
                display_path(&lock_path)
            ))
        });
    let unsupported = |e: io::Error| {
        eprintln!(
            "Warning: index のロックを使えないため、ロック無しで作り直します: {}: {e}",
            display_path(&lock_path)
        );
    };
    let held = match file.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => {
            eprintln!("他の mikke が index を作り直しているため待っています...");
            match file.lock() {
                Ok(()) => true,
                Err(e) => {
                    unsupported(e);
                    false
                }
            }
        }
        Err(TryLockError::Error(e)) => {
            unsupported(e);
            false
        }
    };
    BuildLock {
        _file: Some(file),
        held,
    }
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

fn remove_tmp(tmp: &Path) {
    let _ = fs::remove_file(tmp);
    let _ = fs::remove_file(sibling(tmp, "-journal"));
}

/// 一時ファイルを消してから exit 2 にする (tmp を作った後のエラー出口)。
fn abort_build(tmp: &Path, msg: &str) -> ! {
    remove_tmp(tmp);
    fail(msg);
}

/// panic の後始末。成功したら disarm する。
struct TmpGuard {
    tmp: PathBuf,
    armed: bool,
}

impl Drop for TmpGuard {
    fn drop(&mut self) {
        if self.armed {
            remove_tmp(&self.tmp);
        }
    }
}

/// tmp を dst へ rename する。Windows では開いているファイルを置き換えられないので、
/// ACCESS_DENIED (5) と SHARING_VIOLATION (32) の時だけ 100ms 間隔で最大 100 回リトライする。
/// Unix では読み手が開いていても成功し、読み手は旧い inode を最後まで読む。
fn replace_index(tmp: &Path, dst: &Path) -> io::Result<()> {
    let mut retries = 0;
    loop {
        match fs::rename(tmp, dst) {
            Ok(()) => return Ok(()),
            Err(e)
                if cfg!(windows)
                    && matches!(e.raw_os_error(), Some(5) | Some(32))
                    && retries < 100 =>
            {
                if retries == 0 {
                    eprintln!("index を他のプロセスが開いているため、差し替えを待っています...");
                }
                retries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

/// 名前が `<name>.tmp-<10進数>` か、それに `-journal` が付いたものか。
/// 利用者のファイル (`<name>.tmp-backup` など) を消さないよう、mikke が作る形だけに絞る。
fn is_leftover_tmp(name: &std::ffi::OsStr, prefix: &std::ffi::OsStr) -> bool {
    let Some(rest) = name
        .as_encoded_bytes()
        .strip_prefix(prefix.as_encoded_bytes())
    else {
        return false;
    };
    let pid = rest.strip_suffix(b"-journal").unwrap_or(rest);
    !pid.is_empty() && pid.iter().all(u8::is_ascii_digit)
}

/// 同じフォルダの `<name>.tmp-<pid>` に 1 transaction で書き、閉じてから rename で差し替える。
/// 最中の読み手は直前の index を読み、止まっても直前の index が残る。
fn write_index(cfg: &Config, use_stderr: bool, lock: &BuildLock) -> BuildIssues {
    let index_path = cfg.index_path();
    let tmp_prefix = sibling(&index_path, ".tmp-");
    // 残骸を消す。ロックを持たずに他の pid の分を消すと、他の作り直しを壊す
    if lock.held {
        let dir = match index_path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let prefix = tmp_prefix.file_name().unwrap_or_default();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if is_leftover_tmp(&entry.file_name(), prefix) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
    let tmp = sibling(&index_path, &format!(".tmp-{}", std::process::id()));
    for p in [tmp.clone(), sibling(&tmp, "-journal")] {
        if let Err(e) = remove_if_exists(&p) {
            fail(&format!(
                "Error: index を作成できません: {}: {e}",
                display_path(&p)
            ));
        }
    }
    // 走査中の設定エラー (scan の中の process::exit) と kill では後始末が走らない。
    // 残ったファイルは次の作り直しで消す
    let mut guard = TmpGuard {
        tmp: tmp.clone(),
        armed: true,
    };
    let conn = Connection::open(&tmp).unwrap_or_else(|e| {
        abort_build(
            &tmp,
            &format!("Error: index を作成できません: {}: {e}", display_path(&tmp)),
        )
    });
    let commit_err =
        |e: rusqlite::Error| format!("Error: index を確定できません: {}: {e}", display_path(&tmp));
    if let Err(e) = conn.execute_batch("BEGIN") {
        abort_build(&tmp, &commit_err(e));
    }
    conn.execute_batch(SCHEMA).expect("schema 作成に失敗");
    // 一時ファイルに 1 つの transaction で書き、rename で差し替えるまで読み手には見えない。
    // 書く順番は読み手に影響しない
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)",
        params![META_INDEX_FORMAT, INDEX_FORMAT],
    )
    .expect("meta への INSERT に失敗");

    let mut note_count = 0i64;
    let mut tag_set: HashSet<String> = HashSet::new();
    let mut issues = BuildIssues::default();
    // スナップショットは走査した全 path (読込不可で notes から除外される分も含む)。
    // notes 行数と違い読込不可ファイルの有無で恒常 stale 化しない。
    let mut scanned_paths: Vec<String> = Vec::new();

    for (md_file, rel) in scan::iter_notes(cfg) {
        let rel_posix = to_posix(&rel);
        scanned_paths.push(rel_posix.clone());
        if let Some((kind, detail)) = scan::scan_frontmatter_issue(&md_file) {
            eprintln!("Warning: {rel_posix}: frontmatter {kind} — {detail}");
            issues.frontmatter.push((rel_posix.clone(), kind, detail));
        }
        let note = match scan::load_note(&md_file, &rel) {
            Some(n) => n,
            None => {
                eprintln!("Warning: {rel_posix} の読み込みに失敗 (index から除外)");
                continue;
            }
        };
        if note.empty_tag_count > 0 {
            eprintln!(
                "Warning: {rel_posix}: tags に空要素が{}件あります (`- #Dog` は YAML では空要素です。`- Dog` に修正してください)",
                note.empty_tag_count
            );
            issues
                .empty_tags
                .push((rel_posix.clone(), note.empty_tag_count));
        }

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
                "INSERT INTO tags (path, tag, tag_search) VALUES (?1, ?2, ?3)",
                params![note.path_rel, tag, search_text(tag)],
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
        let fts_title = search_text(&note.title);
        let fts_content = search_text(&note.content);
        conn.execute(
            "INSERT INTO notes_fts (path, title, content) VALUES (?1, ?2, ?3)",
            params![note.path_rel, fts_title, fts_content],
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
    if let Err(e) = conn.execute_batch("COMMIT") {
        abort_build(&tmp, &commit_err(e));
    }
    // Windows では閉じてからでないと rename できない
    if let Err((_, e)) = conn.close() {
        abort_build(&tmp, &commit_err(e));
    }

    // 差し替え直前に判定し直す (build の最中に置かれたよそのファイルを消さないため)
    match inspect_index(&index_path) {
        IndexState::Foreign(r) => abort_build(&tmp, &foreign_msg(&index_path, r)),
        IndexState::Unreadable(e) => abort_build(&tmp, &unreadable_msg(&index_path, &e)),
        _ => {}
    }
    // 旧版は最終 path に書いていたので hot journal を残し得る。新しいファイルに巻き戻しがかからないよう消す
    let old_journal = sibling(&index_path, "-journal");
    if let Err(e) = remove_if_exists(&old_journal) {
        abort_build(
            &tmp,
            &format!(
                "Error: 古い journal を消せません: {}: {e}",
                display_path(&old_journal)
            ),
        );
    }
    // 先に消してから作り直す方式へは戻らない (読み手に作りかけが見える)
    if let Err(e) = replace_index(&tmp, &index_path) {
        let mut msg = format!(
            "Error: index を差し替えられません: {}: {e}",
            display_path(&index_path)
        );
        if cfg!(windows) {
            msg.push_str("\n  他のプロセスが index を開いている可能性があります。直前の index はそのまま残っています。少し待ってから実行し直してください。");
        }
        abort_build(&tmp, &msg);
    }
    guard.armed = false;

    emit(
        use_stderr,
        &format!("インデックスを生成しました: {}", display_path(&index_path)),
    );
    emit(use_stderr, &format!("  ノート数: {note_count}"));
    emit(use_stderr, &format!("  タグ数: {}", tag_set.len()));
    // silent skip の可視化: 破損は握り潰さず必ず件数を出す (0 件でも明示)
    if issues.frontmatter.is_empty() {
        emit(use_stderr, "  frontmatter 破損: 0件");
    } else {
        emit(
            use_stderr,
            &format!(
                "  ⚠ frontmatter 破損: {}件 (詳細は上の Warning / mikke health)",
                issues.frontmatter.len()
            ),
        );
    }
    if !issues.empty_tags.is_empty() {
        let elements: usize = issues.empty_tags.iter().map(|(_, count)| count).sum();
        emit(
            use_stderr,
            &format!(
                "  ⚠ tags の空要素: {elements}件 ({}ファイル、詳細は上の Warning / mikke health)",
                issues.empty_tags.len()
            ),
        );
    }
    issues
}

/// index を全再構築し、frontmatter 破損と tags 空要素の検出結果を返す。
pub fn build(cfg: &Config) -> BuildIssues {
    build_to(cfg, false)
}

pub fn cmd_index(cfg: &Config, check: bool) {
    let issues = build(cfg);
    if check && issues.count() > 0 {
        eprintln!(
            "Error: 修正対象 {}件 (frontmatter 破損: {}件、tags の空要素: {}ファイル; --check 指定のため非 0 で終了)",
            issues.count(),
            issues.frontmatter.len(),
            issues.empty_tags.len()
        );
        // 「検出 = 結果」なので 1 (構築自体の失敗 = 2 と区別 — docs/SPEC.md「exit code」)
        std::process::exit(1);
    }
}

/// 既存 index が stale かを判定する (auto_rebuild 用)。meta の値は inspect_index が読んだものを受け取る。
/// stale 条件: meta['generated'] / スナップショット欠落、いずれかの md の
/// mtime > generated、走査 path 集合のハッシュがスナップショットと不一致。
fn index_is_stale(cfg: &Config, generated: Option<f64>, saved_hash: Option<&str>) -> bool {
    // スナップショットが無い index は stale 扱い
    let (Some(generated), Some(saved_hash)) = (generated, saved_hash) else {
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
/// 形式違いと generated 無しも作り直さない (health の鮮度欄で知らせる)。
/// 作りかけ・表の欠けた・mikke の index でないファイルは exit 2。
pub fn ensure_index_exists(cfg: &Config) {
    let index_path = cfg.index_path();
    prepare(cfg, |s| decide_health(&index_path, s), true);
}

/// 検索系コマンドの index 準備。形式と生成の完了は毎回確認し、不一致なら設定によらず再構築する。
/// 形式一致後、[index] auto_rebuild = true なら鮮度も判定する。
/// mikke の index でないファイルは消さずに exit 2。作りかけは auto_rebuild の時だけ作り直す。
pub fn ensure_index(cfg: &Config) {
    let index_path = cfg.index_path();
    prepare(cfg, |s| decide_search(cfg, &index_path, s), true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mikke-unit-{}-index-safety-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sqlite(path: &Path, sql: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(sql).unwrap();
    }

    /// write_index で完成した index を作り、その path を返す。
    fn built_index(tag: &str) -> (PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        fs::write(dir.join("a.md"), "# A\n\nquokka のメモ。\n").unwrap();
        let cfg = crate::config::load_config(dir.clone());
        fs::create_dir_all(cfg.index_path().parent().unwrap()).unwrap();
        let lock = BuildLock {
            _file: None,
            held: false,
        };
        write_index(&cfg, true, &lock);
        (dir, cfg.index_path())
    }

    fn label(state: &IndexState) -> String {
        match state {
            IndexState::Missing => "Missing".into(),
            IndexState::Index {
                format,
                generated,
                tables_ok,
                ..
            } => format!(
                "Index(format={format:?}, generated={}, tables_ok={tables_ok})",
                generated.is_some()
            ),
            IndexState::Residue => "Residue".into(),
            IndexState::Foreign(r) => format!("Foreign({r})"),
            IndexState::Unreadable(e) => format!("Unreadable({e})"),
        }
    }

    const NOTES_DDL: &str = "CREATE TABLE notes (path TEXT PRIMARY KEY, title TEXT NOT NULL, date TEXT, updated TEXT, summary TEXT, word_count INTEGER);";

    /// hot journal の経路は path を URI にする。記号を含む path でも形を読み、ファイルは変えない。
    #[test]
    fn index_safety_probe_hot_journal_with_symbols() {
        // Windows のファイル名には `?` を使えない
        let name = if cfg!(windows) {
            "a#c%d e"
        } else {
            "a?b#c%d e"
        };
        let dir = temp_dir("hot").join(name);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.db");
        let dst = dir.join("dst.db");
        let conn = Connection::open(&src).unwrap();
        conn.execute_batch(
            "CREATE TABLE other (x TEXT); WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c WHERE i < 2000) INSERT INTO other SELECT printf('%0200d', i) FROM c; PRAGMA cache_size = 2; BEGIN; UPDATE other SET x = 'b';",
        )
        .unwrap();
        fs::copy(&src, &dst).unwrap();
        fs::copy(sibling(&src, "-journal"), sibling(&dst, "-journal")).unwrap();
        drop(conn);
        let before = (
            fs::read(&dst).unwrap(),
            fs::read(sibling(&dst, "-journal")).unwrap(),
        );
        let s = inspect_index(&dst);
        assert!(matches!(s, IndexState::Foreign(_)), "{}", label(&s));
        let after = (
            fs::read(&dst).unwrap(),
            fs::read(sibling(&dst, "-journal")).unwrap(),
        );
        assert!(before == after, "ファイルが変わった");
    }

    #[test]
    fn index_safety_probe_files() {
        let dir = temp_dir("files");
        let p = dir.join("x");
        assert!(matches!(inspect_index(&p), IndexState::Missing));

        fs::create_dir(&p).unwrap();
        let s = inspect_index(&p);
        assert!(matches!(s, IndexState::Foreign(_)), "{}", label(&s));
        fs::remove_dir(&p).unwrap();

        fs::write(&p, b"").unwrap();
        let s = inspect_index(&p);
        assert!(matches!(s, IndexState::Residue), "{}", label(&s));

        fs::write(&p, "# 大事なメモ\n").unwrap();
        let s = inspect_index(&p);
        assert!(matches!(s, IndexState::Foreign(_)), "{}", label(&s));

        let mut garbage = SQLITE_HEADER.to_vec();
        garbage.extend(std::iter::repeat_n(0xAB, 4096));
        fs::write(&p, &garbage).unwrap();
        let s = inspect_index(&p);
        assert!(matches!(s, IndexState::Unreadable(_)), "{}", label(&s));
        // 判定はファイルを書き換えない
        assert_eq!(fs::read(&p).unwrap(), garbage);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_safety_probe_partial_sqlite() {
        let dir = temp_dir("partial");
        // 表の無い SQLite (page を持つよう user_version だけ書く)
        let p = dir.join("empty.sqlite");
        sqlite(&p, "PRAGMA user_version = 1;");
        let s = inspect_index(&p);
        assert!(matches!(s, IndexState::Residue), "{}", label(&s));

        let p = dir.join("notes-only.sqlite");
        sqlite(&p, NOTES_DDL);
        let s = inspect_index(&p);
        assert!(matches!(s, IndexState::Residue), "{}", label(&s));

        // 全表あり・meta 空・notes に行あり
        let p = dir.join("meta-empty.sqlite");
        sqlite(&p, SCHEMA);
        sqlite(&p, "INSERT INTO notes (path, title) VALUES ('a.md', 'A');");
        let s = inspect_index(&p);
        assert!(
            matches!(
                s,
                IndexState::Index {
                    format: None,
                    generated: None,
                    tables_ok: true,
                    ..
                }
            ),
            "{}",
            label(&s)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_safety_probe_built_index() {
        let (dir, p) = built_index("built");
        let s = inspect_index(&p);
        match &s {
            IndexState::Index {
                format,
                generated,
                scanned_hash,
                tables_ok,
            } => {
                assert_eq!(format.as_deref(), Some(INDEX_FORMAT));
                assert!(generated.is_some());
                assert!(scanned_hash.is_some());
                assert!(tables_ok);
            }
            _ => panic!("{}", label(&s)),
        }
        // 一時ファイルは残らない
        let leftovers: Vec<_> = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        sqlite(&p, "DELETE FROM meta WHERE key = 'index_format';");
        let s = inspect_index(&p);
        assert!(
            matches!(
                s,
                IndexState::Index {
                    format: None,
                    generated: Some(_),
                    tables_ok: true,
                    ..
                }
            ),
            "{}",
            label(&s)
        );

        sqlite(&p, "DROP TABLE tags;");
        let s = inspect_index(&p);
        assert!(
            matches!(
                s,
                IndexState::Index {
                    tables_ok: false,
                    ..
                }
            ),
            "{}",
            label(&s)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_safety_probe_foreign_sqlite() {
        let dir = temp_dir("foreign");
        let cases = [
            (
                "meta-no-notes",
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT); INSERT INTO meta VALUES ('generated', '1.0');",
            ),
            ("other", "CREATE TABLE other (x TEXT);"),
            (
                "notes-columns",
                "CREATE TABLE notes (path TEXT, body TEXT); CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
            ),
            (
                "meta-other",
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT); CREATE TABLE other (x TEXT);",
            ),
        ];
        for (name, sql) in cases {
            let p = dir.join(format!("{name}.sqlite"));
            sqlite(&p, sql);
            let s = inspect_index(&p);
            assert!(matches!(s, IndexState::Foreign(_)), "{name}: {}", label(&s));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
