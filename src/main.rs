//! mikke CLI エントリポイント。
//!
//! サブコマンド・引数・出力の意味は docs/SPEC.md が正本 (安定インターフェース)。

mod config;
#[cfg(feature = "semantic")]
mod embed;
mod health;
mod index;
mod scan;
mod search;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// slim/full どちらのビルドかを `--version` で判別できるようにする (semantic feature の有無)。
#[cfg(feature = "semantic")]
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (+semantic)");
#[cfg(not(feature = "semantic"))]
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "mikke",
    version = VERSION,
    about = "Markdown ノート検索 CLI (BM25 + optional semantic hybrid, 日本語対応)"
)]
struct Cli {
    /// ノート repo のルート (省略時: MIKKE_ROOT → cwd から mikke.toml/.mikke.toml を上方探索 → git root)
    #[arg(long, global = true)]
    root: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// index を全再構築する
    Index {
        /// frontmatter 破損があれば非 0 で終了 (CI 用)
        #[arg(long)]
        check: bool,
    },
    /// semantic 検索用の埋め込みを差分更新する (semantic feature 必須)
    Embed {
        /// 全件再構築
        #[arg(long)]
        force: bool,
    },
    /// 全文検索 (FTS5 trigram, BM25 順。短語混在時は date 順)
    Find {
        #[arg(required = true, value_name = "検索語")]
        words: Vec<String>,
        /// JSON Lines で出力 (1 行目メタ行 + 1 件 1 行)
        #[arg(long)]
        json: bool,
    },
    /// タグ検索 (部分一致, date 降順)
    Tag {
        #[arg(value_name = "タグ名")]
        keyword: String,
        /// JSON Lines で出力 (1 行目メタ行 + 1 件 1 行)
        #[arg(long)]
        json: bool,
    },
    /// タイトル検索 (部分一致, date 降順)
    Title {
        #[arg(value_name = "キーワード")]
        keyword: String,
        /// JSON Lines で出力 (1 行目メタ行 + 1 件 1 行)
        #[arg(long)]
        json: bool,
    },
    /// セマンティック検索
    Semantic {
        #[arg(required = true, value_name = "クエリ")]
        query: Vec<String>,
        #[arg(long, default_value_t = 5, value_name = "N")]
        top: usize,
        /// JSON Lines で出力 (1 行目メタ行 + 1 件 1 行)
        #[arg(long)]
        json: bool,
    },
    /// ハイブリッド検索 (BM25 + semantic の RRF 融合)。埋め込み未構築なら BM25 のみへ degrade
    Hybrid {
        #[arg(required = true, value_name = "クエリ")]
        query: Vec<String>,
        #[arg(long, default_value_t = 5, value_name = "N")]
        top: usize,
        /// JSON Lines で出力 (1 行目メタ行 + 1 件 1 行)
        #[arg(long)]
        json: bool,
    },
    /// タグ一覧 (使用回数順)
    #[command(name = "list-tags")]
    ListTags {
        /// JSON Lines で出力 (1 行目メタ行 + 1 件 1 行)
        #[arg(long)]
        json: bool,
    },
    /// 最近のノート
    Recent {
        #[arg(default_value_t = 10, value_name = "件数")]
        count: usize,
        /// JSON Lines で出力 (1 行目メタ行 + 1 件 1 行)
        #[arg(long)]
        json: bool,
    },
    /// 発リンク一覧 (対象ノートの wikilink をリンク先ノートへ解決して表示)
    Links {
        #[arg(value_name = "ノート")]
        note: String,
    },
    /// 被リンク一覧 (対象ノートへ wikilink を張るノートを表示)
    Backlinks {
        #[arg(value_name = "ノート")]
        note: String,
    },
    /// ノート repo の健全性チェック
    Health {
        /// 決定的な md レポートも書き出す
        #[arg(long = "md-report", value_name = "PATH")]
        md_report: Option<PathBuf>,
    },
}

/// Rust が既定で無視する SIGPIPE を既定動作 (プロセス終了) へ戻す。
///
/// 既定のままだと `mikke find <語> | head` のように読み手が途中で降りた時点で
/// `println!` が Err を返し panic (exit 101) する。これはエラーではなく通常のパイプ利用なので、
/// Unix 慣習どおり SIGPIPE で静かに終わらせる (docs/SPEC.md「exit code」)。
/// stdout / stderr のどの出力経路にも一律で効くため、出力箇所を個別に直す必要がない。
#[cfg(unix)]
fn restore_sigpipe() {
    // SAFETY: 他スレッド起動前の main 冒頭で 1 回だけ、シグナル処理を既定へ戻すために呼ぶ。
    unsafe {
        // 戻り値は直前のハンドラ (ここでは Rust ランタイムが設定した SIG_IGN)。復元予定はないので捨てる。
        let _ = libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// Windows に SIGPIPE は無いので何もしない (docs/SPEC.md「exit code」)。
#[cfg(not(unix))]
fn restore_sigpipe() {}

fn main() {
    restore_sigpipe();
    let cli = Cli::parse();
    let root = config::resolve_root(cli.root.as_deref());
    let cfg = config::load_config(root);

    // 検索系 (find/tag/title/semantic/hybrid) と links/backlinks は結果件数を返し、
    // 0 件なら grep 慣習で exit 1 にする (docs/SPEC.md「exit code」)。
    // 一覧系等は 0 件も正常な状態報告なので None。
    let hits: Option<usize> = match cli.command {
        Command::Index { check } => {
            index::cmd_index(&cfg, check);
            None
        }
        Command::Find { words, json } => Some(search::cmd_find(&cfg, &words, json)),
        Command::Tag { keyword, json } => Some(search::cmd_tag(&cfg, &keyword, json)),
        Command::Title { keyword, json } => Some(search::cmd_title(&cfg, &keyword, json)),
        Command::ListTags { json } => {
            search::cmd_list_tags(&cfg, json);
            None
        }
        Command::Recent { count, json } => {
            search::cmd_recent(&cfg, count, json);
            None
        }
        Command::Semantic { query, top, json } => {
            Some(search::cmd_semantic(&cfg, &query.join(" "), top, json))
        }
        Command::Hybrid { query, top, json } => {
            Some(search::cmd_hybrid(&cfg, &query.join(" "), top, json))
        }
        Command::Links { note } => Some(search::cmd_links(&cfg, &note)),
        Command::Backlinks { note } => Some(search::cmd_backlinks(&cfg, &note)),
        Command::Health { md_report } => {
            health::cmd_health(&cfg, md_report.as_deref());
            None
        }
        Command::Embed { force } => {
            #[cfg(feature = "semantic")]
            {
                embed::cmd_embed(&cfg, force);
                None
            }
            #[cfg(not(feature = "semantic"))]
            {
                let _ = force;
                eprintln!("Error: このビルドは semantic 無効です (cargo build --features semantic で有効化)。");
                std::process::exit(2)
            }
        }
    };
    if hits == Some(0) {
        std::process::exit(1);
    }
}
