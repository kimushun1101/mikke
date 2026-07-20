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

#[derive(Parser)]
#[command(
    name = "mikke",
    version,
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
    },
    /// タグ検索 (部分一致, date 降順)
    Tag {
        #[arg(value_name = "タグ名")]
        keyword: String,
    },
    /// タイトル検索 (部分一致, date 降順)
    Title {
        #[arg(value_name = "キーワード")]
        keyword: String,
    },
    /// セマンティック検索
    Semantic {
        #[arg(required = true, value_name = "クエリ")]
        query: Vec<String>,
        #[arg(long, default_value_t = 5, value_name = "N")]
        top: usize,
    },
    /// ハイブリッド検索 (BM25 + semantic の RRF 融合)。埋め込み未構築なら BM25 のみへ degrade
    Hybrid {
        #[arg(required = true, value_name = "クエリ")]
        query: Vec<String>,
        #[arg(long, default_value_t = 5, value_name = "N")]
        top: usize,
    },
    /// タグ一覧 (使用回数順)
    #[command(name = "list-tags")]
    ListTags,
    /// 最近のノート
    Recent {
        #[arg(default_value_t = 10, value_name = "件数")]
        count: usize,
    },
    /// ノート repo の健全性チェック
    Health {
        /// 決定的な md レポートも書き出す
        #[arg(long = "md-report", value_name = "PATH")]
        md_report: Option<PathBuf>,
    },
}

fn main() {
    let cli = Cli::parse();
    let root = config::resolve_root(cli.root.as_deref());
    let cfg = config::load_config(root);

    match cli.command {
        Command::Index { check } => index::cmd_index(&cfg, check),
        Command::Find { words } => search::cmd_find(&cfg, &words),
        Command::Tag { keyword } => search::cmd_tag(&cfg, &keyword),
        Command::Title { keyword } => search::cmd_title(&cfg, &keyword),
        Command::ListTags => search::cmd_list_tags(&cfg),
        Command::Recent { count } => search::cmd_recent(&cfg, count),
        Command::Semantic { query, top } => search::cmd_semantic(&cfg, &query.join(" "), top),
        Command::Hybrid { query, top } => search::cmd_hybrid(&cfg, &query.join(" "), top),
        Command::Health { md_report } => health::cmd_health(&cfg, md_report.as_deref()),
        Command::Embed { force } => {
            #[cfg(feature = "semantic")]
            {
                embed::cmd_embed(&cfg, force);
            }
            #[cfg(not(feature = "semantic"))]
            {
                let _ = force;
                eprintln!("Error: このビルドは semantic 無効です (cargo build --features semantic で有効化)。");
                std::process::exit(1);
            }
        }
    }
}
