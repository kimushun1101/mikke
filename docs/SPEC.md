# mikke 仕様

mikke の挙動の正本。CLI 表面・設定キー・出力の意味は安定インターフェースであり、利用者のスクリプト・エージェント指示書・health 運用が依存する。挙動を変える時は本仕様と `tests/golden/` を意図して同時に更新する。設計の背景は [concept.md](concept.md)。

## CLI 表面

グローバル: `--version`(`mikke <version>` を出力。semantic feature 有効ビルドは `mikke <version> (+semantic)` と表示し、slim/full どちらのバイナリか判別できる) / `--root PATH`(省略時: 環境変数 `MIKKE_ROOT` → cwd から `mikke.toml` / `.mikke.toml` 上方探索 → git root)。

| サブコマンド | 引数 | 意味 |
|---|---|---|
| `index` | `--check` | index 全再構築。`--check` は frontmatter 破損があれば非 0 終了(CI 用) |
| `embed` | `--force` | 埋め込み差分更新(`--force` で全件)。semantic feature 必須 |
| `find` | `<検索語...>` | 全文検索(FTS5 trigram, BM25 順。短語混在時は LIKE fallback/date 順) |
| `tag` | `<タグ名>` | タグ部分一致検索(date 降順) |
| `title` | `<キーワード>` | タイトル部分一致検索(date 降順) |
| `semantic` | `<クエリ...> --top N(=5)` | 意味検索(cosine 類似度順) |
| `hybrid` | `<クエリ...> --top N(=5)` | BM25 + semantic の RRF 融合 |
| `list-tags` | — | タグ一覧(使用回数降順、同数は tag 名昇順) |
| `recent` | `[件数(=10)]` | date 降順の最近ノート(date 空は除外) |
| `health` | `--md-report PATH` | 健全性チェック(決定的 md レポート出力可) |

## 設定スキーマ (`mikke.toml`、全キー省略可)

設定ファイル名は `mikke.toml` または `.mikke.toml`(設定を隠したい repo 向けの dotfile 変種)。ルートマーカーとしての上方探索も両名を対象とする。同一ディレクトリに両方あれば `mikke.toml` を優先し、stderr に警告を出す。設定読み込みのエラーは対象ファイルのパス付きで報告する。

```toml
[scan]
include = ["."]                 # ルート相対の走査起点(複数可、重複除去)
exclude_dirs = []            # 指定でデフォルト置換。既定: .obsidian .claude .agents .codex .cursor .gemini .git .venv __pycache__ node_modules templates dist build
exclude_files = ["README.md", "CLAUDE.md", "AGENTS.md", "GEMINI.md"]

[index]
path = ".mikke/index.sqlite"           # gitignore すること
embeddings_dir = ".mikke/embeddings"

[semantic]
enabled = false                        # embed/hybrid の semantic 経路を有効化
model = "intfloat/multilingual-e5-small"
query_prefix = "query: "
passage_prefix = "passage: "

[search]
bm25_limit = 50                        # find の取得上限
rrf_k = 60
bm25_weight = 0.4
vector_weight = 0.6
candidate_factor = 4                   # hybrid で各ストリームから top_n*factor 取る

[health]
scan_skip_prefixes = []                # frontmatter 破損スキャン除外 path prefix
quality_skip_prefixes = []             # 品質チェック除外 path prefix
min_words = 50                         # 低ボリューム閾値
exec_bit_prefixes = []                 # 配下 tracked *.sh に実行 bit を要求する prefix
```

**設定読み込みの厳格さ**: 型不一致は silent に誤動作させず即エラー終了する。特に「文字列配列指定に文字列を渡すと 1 文字ずつに分解される」事故を型検査で弾く。`bool` は整数指定に紛れ込ませない。BOM 付き TOML (utf-8-sig) を許容。
**常に除外**: 設定に関わらず `.git` と `.mikke` は走査対象外(`exclude_dirs` をデフォルト置換しても、ネスト repo の設定でも生きる)。index/embeddings の出力先は「そのパス配下」を**root 相対 prefix 一致**で除外する(ディレクトリ名一致だと同名ディレクトリが任意の深さで全消えする事故になるため)。**ディレクトリ symlink は辿らない**(ループ防止。ファイル symlink の `.md` は対象)。

## ネストしたノート repo

走査中に `mikke.toml` / `.mikke.toml` を持つサブディレクトリへ入った場合、その配下は「ネストしたノート repo」として走査を**その repo の設定へ委譲**する:

- 委譲されるのは `[scan]`(include / exclude_dirs / exclude_files)と `[index]` の出力先除外(ネスト repo ルート相対の prefix で判定)。孫 repo 以深も同ルールで再帰する
- 親の exclude はネスト境界の**入口まで**効く(親の `exclude_dirs` にディレクトリ名を挙げればネスト repo ごと除外できる)。境界検知は親の除外判定より後
- index は最上位の**単一 index のまま**で、path は最上位 root 相対。BM25 スコアの一貫性(単一 corpus)を保つため、ネスト repo が持つ index を別に引くフェデレーションは行わない
- ネスト repo の `[health]` / `[search]` / `[semantic]` は無視する(health の除外は最上位の `scan_skip_prefixes` / `quality_skip_prefixes` で指定する)
- ネスト repo の設定が壊れている場合も通常どおり対象ファイルの path 付きで即エラー終了する(silent に親規則で走査しない)

## ノートの解釈

- **title 抽出優先順**: frontmatter `title` → 最初の `# ` 見出し → ファイル名 stem。
- **date 正規化**: date/updated は `YYYY-MM-DD` 文字列へ。YAML が date 型で解釈した場合も文字列化して揃える。
- **wikilink 抽出**: `\[\[([^\]|]+?)(?:\|[^\]]+?)?\]\]`、set 化して sorted。
- **語数カウント (word_count)**: `[　-鿿豈-﫿ｦ-ﾟ]` の CJK 文字数 + `[a-zA-Z0-9]+` の語数。文字クラスの範囲は**エスケープ表記で**書く(生リテラルは Unicode NFC 正規化で別文字に化ける)。health の低ボリューム判定の基準値。

## index スキーマ(SQLite)

```sql
notes(path PRIMARY KEY, title NOT NULL, date, updated, summary, word_count)
  INDEX idx_notes_date ON notes(date DESC)
tags(path, tag, PRIMARY KEY(path, tag))  INDEX idx_tags_tag ON tags(tag)
links(path, target, PRIMARY KEY(path, target))
notes_fts USING fts5(path UNINDEXED, title, content, tokenize='trigram')
meta(key PRIMARY KEY, value)     -- meta['generated'] に index 生成時刻 (health の鮮度判定に使用)
```

index が無い場合は検索時に自動 build(clone 直後フォールバック)。`mikke index` は drop→再作成の全再構築。`meta['generated']` は epoch 秒の小数文字列(秒精度だと mtime との比較で秒未満の更新を取りこぼすため)。index フォーマットは内部表現であり互換保証しない(`.mikke/` は常に再生成可能)。

## 検索セマンティクス

- **FTS 変換 (`fts_query`)**: 空白で語分割し、各語を個別に `"..."` quote して `AND` 連結。入力全体を 1 つの quote で囲むと連続一致要求で 0 件化するため**語ごと quote が必須**。内部の `"` は `""` にエスケープ。
- **trigram 制約**: FTS5 trigram は各語 3 文字以上必須。**語ごとに**長さ判定し、1 語でも 3 文字未満を含む場合は LIKE フォールバック(全語 AND、`LOWER(content/title) LIKE %term%`、date 降順、relevance 無し)。全語 3 文字以上なら `ORDER BY rank`(FTS5 の BM25)。
- **find の順序表示**: BM25 順か「date 降順(短語で relevance 算出不可)」かを**正直に出す**。`bm25_limit` 到達時は「上位 N 件で打ち切り(全ヒット数不明)」と明示(打ち切りを全件数と誤読させない)。
- **tag/title**: `LOWER(...) LIKE %kw%` 部分一致、date 降順。
- **recent**: `date != ''` を date 降順で LIMIT。
- **list-tags**: `GROUP BY tag ORDER BY COUNT(*) DESC, tag`。
- **semantic**: クエリを `query_prefix + query` で encode(normalize)、保存ベクトルとの内積(= 正規化済 cosine)で降順 top_n。**モデル/prefix は保存 metadata を優先**(生成時とエンコード条件を揃える)。
- **hybrid (RRF)**: 各ストリームから `top_n * candidate_factor` 取得 → rank を 1 始まりで付与 → `score += weight * 1/(rrf_k + rank)`。semantic 未構築時は vector 重み 0 で**再正規化**し BM25 のみへ degrade(`semantic.enabled` が true なら Note を stderr へ)。結果に `via`(bm25 / vec / bm25+vec)と score を表示。

## 出力フォーマット

各ヒットは以下を表示:

```
  <title> (<date>)  [score: 0.1234 via bm25+vec]
    path: <root 相対 path>
    tags: a, b, c
    summary: <summary>            # 空なら「(なし — 要約未設定。内容は path を開いて確認)」
```

score/via は semantic/hybrid のみ。**summary 欠落を空白で黙らせない**(本文未読の内容捏造を誘発するため明示)。

## embedding(feature `semantic`)

バックエンドは candle(純 Rust — 単一バイナリの配布を壊さない)。

- 埋め込みテキスト = `title\n summary\n 本文`。E5 系仕様で**ドキュメント側に passage_prefix、クエリ側に query_prefix**。normalize して保存。
- 差分検出はファイル内容の SHA-256。model/passage_prefix 変更時は全再構築(既存ベクトルと混ぜると比較不能)。削除ノートは metadata から除外し、**削除のみの更新でも保存し直す**(消えたベクトルが結果に残り続けるのを防ぐ)。
- 保存: `embeddings.safetensors`(vectors 行列)+ `metadata.json`(generated, model, query_prefix, passage_prefix, note_count, notes[{path,title,hash}])。順序 = vectors 行と一致。
- 初回はモデルを HF から cache へ DL する(オフライン/社内網の考慮を README に)。
- semantic feature 無しビルド・バックエンド未実装の経路は silent 劣化させず、明示エラーで exit する。

## health 判定(決定的に)

- **frontmatter 破損**: index 非依存で filesystem を直接スキャン(古い index に騙されない)。判定: 先頭が `---` で始まるのに閉じ `---` が無い → 「閉じ---欠落」。YAML パース失敗 → 「YAMLエラー」。読込不可 → その旨。先頭 `---` 無しは破損ではない(タグ/要約欠落として別途拾う)。
- **実行bit欠落**: `exec_bit_prefixes` 配下の tracked `*.sh` の git index mode が `100755` でないものを検出。`git -C root -c core.quotepath=off ls-files -s -- <prefixes>` を UTF-8 で読む(後述の encoding 注意)。index(tree) 依存でホスト非依存 → レポートに含めて commit 経由通知に乗せる。
- **品質チェック(index ベース)**: タグなし / 要約なし / `word_count < min_words` / updated 未設定。各々 `quality_skip_prefixes` を適用。
- **index 鮮度**: `meta['generated']` より mtime が新しい md の件数。実行時依存なので **stdout のみ**、md レポートには含めない(レポートの差分 = 実質的状態変化、にするため)。
- **md レポート (`--md-report`)**: 揮発情報(実行時刻・鮮度)を含めず決定的に生成 → 「内容が変わった時だけ commit」運用。改行は **LF 固定**(Windows CRLF と Linux nightly LF で差分が出て決定性が壊れるのを防ぐ)。パスはレポート置き場からの相対 md リンク(`#` は %23、空白/括弧は `<>` wrap、`[]` はエスケープ、基底名衝突回避のため wikilink でなくパスリンク)。

## cross-platform 正しさ(テストで固定)

silent に壊れやすい箇所。いずれも過去に実運用で踏んだ hard-won な知見であり、退行させないためテストで固定する:

- **BOM**: md/TOML は utf-8-sig 相当で読む(BOM を content の一部にしない)。BOM を素の utf-8 で読むと先頭の `---` が認識されず frontmatter が silent 喪失する。
- **CJK 文字クラスのエスケープ表記**: 「ノートの解釈」の語数カウント参照。生リテラルの範囲指定はエディタ・ツールの NFC 正規化で別文字に化け、判定が silent にずれる。
- **git 出力の encoding**: `--show-toplevel` / `ls-files` は UTF-8 で decode し、`core.quotepath=off`。非 ASCII パスが silent に壊れる/skip されるのを防ぐ。
- **stdout encoding**: Windows で符号化不能文字(em dash 等)が混ざっても crash させず継続(console/リダイレクト時の挙動をテスト)。
- **改行**: md レポートは LF 固定(health 参照)。

## golden テスト

`tests/golden/` が仕様の期待出力(`manifest.tsv` がコマンド → golden ファイルの対応表)。`tests/fixture/` の小さなノート集合に対し各コマンドの stdout を厳密比較する。出力を変える変更は、意図した差分であることを確認した上で golden を更新する。
