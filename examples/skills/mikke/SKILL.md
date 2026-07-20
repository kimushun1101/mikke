---
name: mikke
description: ノートフォルダ (Markdown KB) を mikke CLI で検索する。「○○について調べて」「過去のメモは?」「以前どう解決したか」等、過去の知見・記録を引き出す時に起動。最新仕様・時事・バージョン依存の質問では起動しない (Web が真)。
allowed-tools: Read, Grep, Glob, Bash(mikke *)
---

# mikke

出典: https://github.com/kimushun1101/mikke (examples/skills 同梱例。運用に合わせ調整可。検索セマンティクスの正本は同 repo の docs/SPEC.md)

前提: `mikke` が PATH にある。root 解決順: `--root` → 環境変数 `MIKKE_ROOT` → cwd から `mikke.toml` (または `.mikke.toml`) 上方探索 → git root (cwd 外の KB は `mikke --root <path>`)。index は無ければ検索時に自動生成。Bash は `mikke` の実行のみに使い、重い `mikke embed` (初回はモデル DL あり) はユーザー確認後に実行。

## コマンド

まず `mikke --help` でコマンド一覧を把握し、個別の引数・オプションは `mikke <サブコマンド> --help` で確認する (例: `mikke hybrid --help`)。このファイルにコマンドを列挙しない — 実装 (`--help`) を正として SPEC / skill との同期ズレを防ぐ。

## 検索ルール

- index (`.mikke/index.sqlite`) は binary。直接 Read 禁止
- ヒットの title/tags/summary で当たりを付け、必要な path だけ Read。全件読み禁止
- find の対象は title + 本文のみ。tags/summary だけの語は find に当たらない (`tag` / `title` で引く)
- 複数語は各語 phrase quote の AND 連結。全語共起が必要で、語を増やすほど絞られる。3 文字未満の語が混ざると relevance 無しの date 降順 (出力に明示される)
- 固有名詞・型番 → `find`。自然文・概念的な問い → `hybrid` (全語共起が稀で find は空振りしやすい)
- 0 件時の fallback 順: 語を減らす/変える → `hybrid` → 最終手段で `Grep` 直接走査 (summary/tags も対象。範囲を絞る)。0 件でも該当ノートは存在しうる
- 引き当てたい語が本文に無かったら、該当ノート本文へのシノニム追記をユーザーに提案 (将来の検索性向上。無断で書き込まない)
- トピックが曖昧なら `recent` / `list-tags` で当たりを付けるか、対象をユーザーに確認

## 検証

- `mikke recent 3` が直近ノートを返す (index 生存確認)
- `mikke find <KB に実在する語>` が path 付きでヒットし、そのノートを Read できる
