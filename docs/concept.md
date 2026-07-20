# mikke の思想と運用フロー

## 何のためのツールか

mikke は「AI エージェントに自分のノート資産を検索させる」ためのローカル検索 CLI。

背景にある運用: Markdown ノートフォルダ (YAML frontmatter + wikilink、Obsidian 互換) に知識・記録を一元化し、Claude Code 等の AI コーディングエージェントがそこを参照しながら作業する。エージェントの検索手段が grep だけだと、表記ゆれ・言い換えに弱く、日本語は分かち書きされないため単語一致すら効きにくい。mikke は BM25 (FTS5 trigram) + optional semantic 検索でこのギャップを埋め、frontmatter (title/tags/summary) を構造化された手がかりとして返す。

## 設計思想

### 1. 即起動・単一バイナリ

エージェントは 1 セッションで検索を何十回も叩く。1 回あたりの起動コストがそのまま応答性と体験に効くため、起動数 ms の単一バイナリにこだわる。既定ビルドの経路にランタイム・仮想環境・ML モデルの初期化を持ち込まない。

### 2. BM25 コアと semantic の分離 (graceful degradation)

BM25 コア (SQLite FTS5) は embedding に一切依存しない。semantic は Cargo feature でゲートし、埋め込み未構築・slim ビルドの環境では自動的に BM25 のみへ degrade する (`hybrid` は重みを再正規化して継続)。「どの環境でもまず動く」を最優先し、semantic は載せられる環境での上乗せとする。

### 3. 出力は AI が誤読しない形に

主要な読み手は LLM。誤読・捏造を誘発しない出力を仕様として持つ:

- summary 欠落は空白で黙らせず「(なし — 要約未設定。内容は path を開いて確認)」と明示する (本文未読のまま内容を捏造するのを防ぐ)
- 並び順が BM25 relevance 順か date 順か (短語で relevance 算出不可) を正直に表示する
- `bm25_limit` 到達時は「上位 N 件で打ち切り (全ヒット数不明)」と明示する (打ち切りを全件数と誤読させない)
- hybrid は各ヒットに由来 `via` (bm25 / vec / bm25+vec) と score を付ける

### 4. ローカル完結

検索も embedding も外部 API を使わない。ノート (私的知識・業務記録) がネットワークに出ない。semantic のモデルは初回のみ Hugging Face からローカル cache へダウンロードする (オフライン・社内網ではここだけ考慮が要る)。

### 5. フォルダごとに独立・設定 1 枚

index は各ノートフォルダ直下の `.mikke/` に生成する (gitignored、いつでも再生成可能)。設定はルートの `mikke.toml` 1 枚で全キー省略可。index が無ければ検索時に自動 build するので、clone 直後でもセットアップは「バイナリを置く」だけで済む。対象が git repo である必要はない — `mikke.toml` を置いたただのフォルダも同じに扱える (git root は root 解決の最終フォールバックにすぎない)。複数のノートフォルダ (個人 / 家族 / 会社など) も同じバイナリでそれぞれ独立に運用できる。

ノートフォルダを別のノートフォルダの下に置く (例: 親 KB の `repos/` に家族用・会社用を clone) 場合も、親からの走査はネストされた側の `[scan]` 設定を尊重する — 子のテスト fixture や下書き除外が親の index を汚染しない。index はあくまで親の単一 index (BM25 スコアの一貫性を優先) で、複数 index を統合するフェデレーションはしない。詳細は SPEC「ネストしたノート repo」。

### 6. health は決定的に

frontmatter 破損・タグ/要約欠落・低ボリュームといったノート品質の劣化は、放置すると検索品質を蝕む。`mikke health --md-report` は実行時刻や index 鮮度などの揮発情報を含めず決定的に生成する。「レポートの diff = 実質的な状態変化」になるため、nightly で回して変化した時だけ commit → 通知、という運用が組める。

## 運用フローイメージ

### ノートフォルダ側のセットアップ (初回のみ)

1. ノートフォルダのルートに `mikke.toml` を置く (全キー省略可。走査除外や health 閾値を必要になったら足す。設定ファイルを見せたくない場合は `.mikke.toml` でも可)
2. `.gitignore` に `.mikke/` を追加
3. エージェント指示書 (`CLAUDE.md` / `AGENTS.md`) に検索手順を書く。例:

   > 過去の知見・記録の検索は `mikke find <語...>` / `mikke hybrid <クエリ>` を使う。index (`.mikke/`) は binary なので直接読まない。

### 日常の検索 (人間・エージェント共通)

```bash
mikke find 逆運動学 特異点          # まず BM25 全文検索
mikke hybrid 手先が動かなくなる原因   # 言い換えに強い意味検索融合
mikke tag robotics                  # タグで絞る
mikke recent 10                     # 最近書いたものから辿る
```

ヒットの title/tags/summary で当たりを付け、本文は path を開いて読む。別のノートフォルダを cwd 外から検索する時は `mikke --root <path> find ...`。

### メンテナンス (定期実行)

- `mikke index` — ノートの大量追加・移動後に明示再構築。CI では `mikke index --check` で frontmatter 破損を非 0 終了として検知
- `mikke embed` — semantic 利用時、ノート更新後の差分更新 (nightly 推奨)
- `mikke health --md-report <path>` — nightly で回し、レポートに diff が出た時だけ commit して通知に乗せる
