# mikke — エージェント指示書スニペット

ノートフォルダの `AGENTS.md` / `CLAUDE.md` にそのまま貼れる、ツール非依存の検索手順 (運用に合わせ調整可)。出典: https://github.com/kimushun1101/mikke (examples/agents 同梱例)。検索セマンティクスの正本は同 repo の docs/SPEC.md。index / embed / health の定期メンテはスニペットに含めない — docs/concept.md「運用フローイメージ」を参照。

--- ここから下を貼り付ける (見出しレベルは貼り先に合わせて調整する) ---

## ノート検索 (mikke)

過去の知見・記録を引き出す時は `mikke` CLI で検索する。最新仕様・時事・バージョン依存の情報は Web を正とし、ノート検索で代替しない。

前提: `mikke` が PATH にある。root 解決順: `--root` → 環境変数 `MIKKE_ROOT` → cwd から `mikke.toml` (または `.mikke.toml`) 上方探索 → git root (cwd 外の KB は `mikke --root <path>`)。index は無ければ検索時に自動生成される。重い `mikke embed` (初回はモデル DL あり) はユーザー確認後に実行する。

コマンド一覧は `mikke --help`、個別の引数・オプションは `mikke <サブコマンド> --help` で確認する (ここに列挙しない — 実装を正として同期ズレを防ぐ)。

### 検索ルール

- `.mikke/` 配下は検索用の内部データ (index 等の binary)。直接読まず、検索は必ず mikke コマンド経由で行う
- ヒットの title/tags/summary で当たりを付け、必要なノートだけ path を開いて読む。全件読み禁止
- find の対象は title + 本文のみ。tags/summary だけの語は find に当たらない (`tag` / `title` で引く)
- 複数語は各語 phrase quote の AND 連結。全語共起が必要で、語を増やすほど絞られる。3 文字未満の語が混ざると relevance 無しの date 降順 (出力に明示される)
- 固有名詞・型番 → `find`。自然文・概念的な問い → `hybrid` (全語共起が稀で find は空振りしやすい)
- 0 件時の fallback 順: 語を減らす/変える → `hybrid` → 最終手段でファイル内容の直接走査 (grep 等。summary/tags も対象。範囲を絞る)。0 件でも該当ノートは存在しうる
- 引き当てたい語が本文に無かったら、該当ノート本文へのシノニム追記をユーザーに提案する (将来の検索性向上。無断で書き込まない)
- トピックが曖昧なら `recent` / `list-tags` で当たりを付けるか、対象をユーザーに確認する

### 検証

- `mikke recent 3` が直近ノートを返す (index 生存確認)
- `mikke find <KB に実在する語>` が path 付きでヒットし、そのノートを開いて読める
