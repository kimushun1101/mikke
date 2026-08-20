# mikke 仕様

mikke の挙動の正本。CLI 表面・設定キー・出力の意味は安定インターフェースであり、利用者のスクリプト・エージェント指示書・health 運用が依存する。挙動を変える時は本仕様と `tests/golden/` を意図して同時に更新する。設計の背景は [concept.md](concept.md)。

## CLI 表面

グローバル: `--version`(`mikke <version>` を出力。semantic feature 有効ビルドは `mikke <version> (+semantic)` と表示し、slim/full どちらのバイナリか判別できる) / `--root PATH`(省略時: 環境変数 `MIKKE_ROOT` → cwd から `mikke.toml` / `.mikke.toml` 上方探索 → git root)。

| サブコマンド | 引数 | 意味 |
|---|---|---|
| `index` | `--check` | index 全再構築。`--check` は frontmatter 破損があれば exit 1(CI 用) |
| `embed` | `--force` | 埋め込み差分更新(`--force` で全件)。semantic feature 必須 |
| `find` | `<検索語...> --json` | 全文検索(FTS5 trigram, BM25 順。短語混在時は LIKE fallback/date 順) |
| `tag` | `<タグ名> --json` | タグ部分一致検索(date 降順) |
| `title` | `<キーワード> --json` | タイトル部分一致検索(date 降順) |
| `semantic` | `<クエリ...> --top N(=5) --json` | 意味検索(cosine 類似度順) |
| `hybrid` | `<クエリ...> --top N(=5) --json` | BM25 + semantic の RRF 融合 |
| `list-tags` | `--json` | タグ一覧(使用回数降順、同数は tag 名昇順) |
| `recent` | `[件数(=10)] --json` | date 降順の最近ノート(date 空は除外) |
| `links` | `<ノート>` | 発リンク一覧(wikilink target をノートへ解決して表示、未解決は明示) |
| `backlinks` | `<ノート>` | 被リンク一覧(対象ノートを指しうる target を持つノート) |
| `health` | `--md-report PATH` | 健全性チェック(決定的 md レポート出力可) |

`--json` は stdout を JSON Lines に切り替える(スキーマは「出力フォーマット」参照。テキスト出力・exit code は変えない additive なフラグ)。

## exit code

grep の慣習に合わせる。呼び出し側は出力文言でなく exit code でヒット有無・エラーを判定できる:

| 状況 | exit code |
|---|---|
| 検索系(find / tag / title / semantic / hybrid)で 1 件以上ヒット。links / backlinks で結果 1 件以上。その他コマンドの正常終了 | 0 |
| 検索系で 0 件。links / backlinks で結果 0 件 | 1 |
| エラー(設定の型不一致・ルート未特定・index open 失敗・semantic 無効ビルド/無効 repo での semantic / embed 等) | 2 |

- links / backlinks の「結果」は検索系のヒットに準ずる: links は対象 target 数(解決可否を問わず — 未解決の列挙も結果)、backlinks は被リンク元ノート数。`<note>` 引数が解決できない場合(該当なし・複数一致)は結果 0 件でなく入力エラーの 2(「リンクグラフ」参照)
- 一覧系(list-tags / recent)は「0 件も正常な状態報告」なので 0 のまま(recent は index 生存確認にも使う)
- `index --check` の frontmatter 破損検出は「検出 = 結果」として 1。index 構築自体の失敗は 2。`mikke index`(`--check` 無し)は破損があっても 0
- health は問題件数によらず 0(md レポート書き出し失敗等のエラーは 2)
- clap の引数パースエラーは 2(`--help` / `--version` は 0)
- hybrid の semantic ストリーム失敗は Warning を stderr に出して BM25 で継続する現行挙動のまま。degrade はエラー扱いせず、ヒット有無のみで判定する
- `--json` は exit code を変えない(検索系の 0 件時はメタ行のみ出して 1)。JSON への変換失敗は panic(101)にせずエラーの 2(通常経路では起きない — 非有限 f64 も serde_json は null として出力する)
- 内部エラーによる panic 終了(Rust 既定 101)は「非 0 だが値は保証外」

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

## リンクグラフ(links / backlinks)

index の links テーブル(wikilink の生 target — 「ノートの解釈」参照)を読み、ノート間グラフを引く。出力はテキストのみ(`--json` は未対応。追加する場合も additive な拡張として扱う)。

### `<note>` 引数の解決

他コマンドが `path:` に出す root 相対 path をそのまま受ける。解決手順:

1. `notes.path` との完全一致
2. 一致しなければ、引数末尾の `.md` を除いた文字列で下記「target → ノートの解決規則」と同じ照合(stem 完全一致 / `.md` 抜きパスの成分末尾一致)

該当 0 件・複数一致は「結果 0 件」と区別して入力エラー(exit 2)。複数一致は silent に 1 件を選ばず、候補 path を stderr へ列挙する。

### target → ノートの解決規則

- `#fragment` は照合前に除去する。target 末尾の `.md` も除いて照合する(`[[notes/short.md]]` 形式を解決するため)。ノート部が空(`[[#anchor]]`)は自ノート内アンカーであり対象外(target として数えない)
- `notes.path` から `.md` を除いた文字列に対する、パス成分単位の末尾一致(`[[note-name]]` の stem 完全一致は 1 成分の末尾一致として包含)。大文字小文字は区別する
- 複数ノートに一致する target は全件表示する(曖昧さを隠さない)
- 解決できない target は「(未解決)」として明示的に列挙する(黙って落とさない)。**未解決 = 壊れたリンクとは限らない**: 既定 `exclude_files` の README.md / CLAUDE.md 等、索引対象外ファイルへのリンクは恒常的に未解決になる

### 出力と件数

- **links**: 見出し `'<path>' の発リンク (target N件, 解決 R件 / 未解決 U件):`。N = 対象 target 数(`[[#anchor]]` を除く。N = R + U)。解決したノートを target 昇順(同一 target の複数一致は path 昇順、同一ノートへの重複 target は 1 回)でテキスト形式(「出力フォーマット」)で表示し、続けて未解決 target を `  (未解決) [[<生 target>]]` の 1 行ずつで列挙する。N = 0 なら「wikilink がありません。」
- **backlinks**: 見出し `'<path>' の被リンク (N件):`。N = 対象ノートを指しうる target を持つノート数。date 降順(date 空は末尾、同 date は path 昇順)でテキスト形式で表示。0 件は「該当するノートが見つかりませんでした。」
- exit code は「exit code」参照(結果 1 件以上 = 0 / 結果 0 件 = 1 / `<note>` 引数が解決できない = 2)

## 出力フォーマット

### テキスト(既定)

各ヒットは以下を表示:

```
  <title> (<date>)  [score: 0.1234 via bm25+vec]
    path: <root 相対 path>
    tags: a, b, c
    summary: <summary>            # 空なら「(なし — 要約未設定。内容は path を開いて確認)」
```

score/via は semantic/hybrid のみ。**summary 欠落を空白で黙らせない**(本文未読の内容捏造を誘発するため明示)。

### JSON Lines(`--json`)

`find` / `tag` / `title` / `semantic` / `hybrid` / `recent` の 6 コマンドと `list-tags` は `--json` で stdout を JSON Lines(1 件 1 行、UTF-8、LF)に切り替える。jq 等の後段処理・エージェント指示書からの利用向け(テキスト出力は複数行 summary でラベル無し行が生じ、行指向パースが保証できない)。

**保証**: JSON モードの stdout には JSON Lines 以外を出さない。auto-build の告知・hybrid degrade の Note・Warning 類は従来どおり stderr のため、clone 直後の初回実行でも安全にパイプできる。

- **メタ行(常に 1 行目)**: `{"type":"meta","command":"<サブコマンド名>","count":N}`。テキスト見出しが持つ情報を JSON でも失わないため、find は `"order"`(`"relevance"` = BM25 順 / `"date"` = 短語 fallback の date 降順)と `"capped"`(`bm25_limit` 到達 = true。ちょうど limit 件で打ち切りが無い場合も true になる保守的判定 — テキスト出力の打ち切り表示と同一条件。true のとき count を全ヒット数と誤読しない)を必ず含み、hybrid は `"degraded"`(semantic ストリームが使えず BM25 のみになった場合に true。埋め込み未構築のほか、構築済みでも semantic 検索の実行時失敗で true になる)を必ず含む。0 件時はメタ行のみ(hit 行 0 行)
- **hit 行(2 行目以降、1 件 1 行)**: `{"path":"...","title":"...","date":"...","tags":["a","b"],"summary":"...","score":0.0123,"via":"bm25+vec"}`。`type` フィールドは付けない。score は semantic/hybrid のみ、via は hybrid のみ(semantic は via を設定しない)。summary 空は `""` のまま(テキスト出力の sentinel 文は出さない — JSON では空文字列が欠落の明示になる)。score は f64 全精度で、テキスト出力の 4 桁丸めとは表記が異なる
- **list-tags**: メタ行(count = タグ数)+ `{"tag":"...","count":N}` を 1 タグ 1 行
- **スキーマ安定性**: `--json` の出力も安定インターフェース。フィールド追加は互換、既存フィールドの改名・削除・意味変更は breaking

## embedding(feature `semantic`)

バックエンドは candle(純 Rust — 単一バイナリの配布を壊さない)。

- 埋め込みテキスト = `title\n summary\n 本文`。E5 系仕様で**ドキュメント側に passage_prefix、クエリ側に query_prefix**。normalize して保存。
- 差分検出はファイル内容の SHA-256。model/passage_prefix 変更時は全再構築(既存ベクトルと混ぜると比較不能)。削除ノートは metadata から除外し、**削除のみの更新でも保存し直す**(消えたベクトルが結果に残り続けるのを防ぐ)。
- 保存: `embeddings.safetensors`(vectors 行列)+ `metadata.json`(generated, model, query_prefix, passage_prefix, note_count, notes[{path,title,hash}])。順序 = vectors 行と一致。
- 初回はモデルを HF から cache へ DL する(オフライン/社内網の考慮を README に)。
- semantic feature 無しビルド・バックエンド未実装の経路は silent 劣化させず、明示エラーで exit する(exit 2 — 「exit code」参照)。

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
