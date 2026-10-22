# mikke 仕様

mikke の挙動の正本。CLI 表面・設定キー・出力の意味は安定インターフェースであり、利用者のスクリプト・エージェント指示書・health 運用が依存する。挙動を変える時は本仕様と `tests/golden/` を意図して同時に更新する。設計の背景は [concept.md](concept.md)。

## CLI 表面

グローバル: `--version`(`mikke <version>` を出力。semantic feature 有効ビルドは `mikke <version> (+semantic)` と表示し、slim/full どちらのバイナリか判別できる) / `--root PATH`(省略時: 環境変数 `MIKKE_ROOT` → cwd から `mikke.toml` / `.mikke.toml` 上方探索 → git root)。

| サブコマンド | 引数 | 意味 |
|---|---|---|
| `index` | `--check` | index 全再構築。`--check` は frontmatter 破損・tags 空要素があれば exit 1(CI 用) |
| `embed` | `--force` | 埋め込み差分更新(`--force` で全件)。semantic feature 必須 |
| `find` | `<検索語...> --top N --json` | 全文検索(FTS5 trigram, BM25 順。短語混在時は LIKE fallback/簡易スコア順)。`--top` 未指定は `bm25_limit` 件まで |
| `tag` | `<タグ名> --top N --json` | タグ部分一致検索(date (空なら updated) 降順)。`--top` 未指定は全件 |
| `title` | `<キーワード> --top N --json` | タイトル部分一致検索(date (空なら updated) 降順)。`--top` 未指定は全件 |
| `semantic` | `<クエリ...> --top N(=5) --json` | 意味検索(cosine 類似度順) |
| `hybrid` | `<クエリ...> --top N(=5) --json` | BM25 + semantic の RRF 融合 |
| `list-tags` | `--json` | タグ一覧(使用回数降順、同数は tag 名昇順) |
| `recent` | `[件数(=10)] --json` | date (空なら updated) 降順の最近ノート(どちらも空なら除外) |
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
| エラー(設定の型不一致・`[scan] exclude_paths` の不正な値 (空文字・絶対パス・`..`・`\`・制御文字・root そのもの)・ルート未特定・index open 失敗・`[index] path` が mikke の index でない既存ファイルや `.md` で終わる名前を指す・作りかけや壊れた index を既定の検索や health で開いた・index を差し替えられない(Windows で他のプロセスが開いている)・semantic 無効ビルド/無効 repo での semantic / embed 等) | 2 |

- links / backlinks の「結果」は検索系のヒットに準ずる: links は対象 target 数(解決可否を問わず — 未解決の列挙も結果)、backlinks は被リンク元ノート数。`<note>` 引数が解決できない場合(該当なし・複数一致)は結果 0 件でなく入力エラーの 2(「リンクグラフ」参照)
- 一覧系(list-tags / recent)は「0 件も正常な状態報告」なので 0 のまま(recent は index 生存確認にも使う)
- `index --check` の frontmatter 破損・tags 空要素検出は「検出 = 結果」として 1。index 構築自体の失敗は 2。`mikke index`(`--check` 無し)は修正対象があっても 0
- health は問題件数によらず 0(md レポート書き出し失敗等のエラーは 2)
- clap の引数パースエラーは 2(`--help` / `--version` は 0)
- hybrid の semantic ストリーム失敗は Warning を stderr に出して BM25 で継続する現行挙動のまま。degrade はエラー扱いせず、ヒット有無のみで判定する
- `--json` は exit code を変えない(検索系の 0 件時はメタ行のみ出して 1)。JSON への変換失敗は panic(101)にせずエラーの 2(通常経路では起きない — 非有限 f64 も serde_json は null として出力する)
- **パイプの早期終了は panic させない**: `| head` / `| less` 等で読み手が先に降りた場合、unix では SIGPIPE の既定動作でそのまま終了する(exit 101 の panic にしない)。パイプライン全体の exit code は後段コマンドのもの(`| head` なら 0)。ただし `set -o pipefail` 下では mikke 側の SIGPIPE 終了が拾われて 141(128+13)になる。SIGPIPE を持たない Windows は対象外
- 内部エラーによる panic 終了(Rust 既定 101)は「非 0 だが値は保証外」

## 設定スキーマ (`mikke.toml`、全キー省略可)

設定ファイル名は `mikke.toml` または `.mikke.toml`(設定を隠したい repo 向けの dotfile 変種)。ルートマーカーとしての上方探索も両名を対象とする。同一ディレクトリに両方あれば `mikke.toml` を優先し、stderr に警告を出す。設定読み込みのエラーは対象ファイルのパス付きで報告する。

```toml
[scan]
include = ["."]                 # ルート相対の走査起点(ディレクトリのみ。複数可、重複除去)
exclude_dirs = []            # ディレクトリ名の完全一致(任意の深さ。パス・glob は効かない)。指定でデフォルト置換。既定: .obsidian .claude .agents .codex .cursor .gemini .git .venv __pycache__ node_modules templates dist build
exclude_files = ["README.md", "CLAUDE.md", "AGENTS.md", "GEMINI.md"]   # ファイル名の完全一致(パス・glob は効かない)
exclude_paths = []              # root 相対のパス(区切りは /)。そのパスと配下を外す(要素単位)。exclude_dirs の既定とは独立
respect_gitignore = false       # true で .gitignore と .git/info/exclude が無視するパスを走査しない

[index]
path = ".mikke/index.sqlite"           # gitignore すること (同じフォルダに <path>.lock と、作り直し中の <path>.tmp-<pid> もできる)
embeddings_dir = ".mikke/embeddings"
auto_rebuild = false                   # true で検索時にソース更新を検知したら自動で全再構築

[semantic]
enabled = false                        # embed/hybrid の semantic 経路を有効化
model = "intfloat/multilingual-e5-small"
query_prefix = "query: "
passage_prefix = "passage: "

[search]
bm25_limit = 50                        # --top 未指定時の find の取得上限
rrf_k = 60
bm25_weight = 0.4
vector_weight = 0.6
candidate_factor = 4                   # hybrid で各ストリームから top_n*factor 取る

[health]
scan_skip_prefixes = []                # frontmatter 破損スキャン除外 path prefix
quality_skip_prefixes = []             # 品質チェック除外 path prefix
min_words = 50                         # 低ボリューム閾値
exec_bit_prefixes = []                 # 配下 tracked *.sh に実行 bit を要求する prefix
disable = []                           # 項目単位で無効化するチェック名 (health 判定を参照)
```

**設定読み込みの厳格さ**: 型不一致は silent に誤動作させず即エラー終了する。特に「文字列配列指定に文字列を渡すと 1 文字ずつに分解される」事故を型検査で弾く。`bool` は整数指定に紛れ込ませない。BOM 付き TOML (utf-8-sig) を許容。エラーと警告の線引きは次のとおり:

- エラー(exit 2、設定ファイルの path 付き): TOML の構文、型の不一致(`exclude_paths` は文字列の配列、`respect_gitignore` は真偽値)、`[health] disable` の未知の名前(最上位の設定だけ)、`[index] path` の `.md`、`exclude_paths` の不正な値(空文字・制御文字・`\`・絶対パス・`..`・root そのもの)
- 警告(stderr。exit code と stdout は変えない): 未知の節・キーと節の外のキー(近い既知のキーや、そのキーを持つ節があれば `(exclude_dirs ?)` / `([index] auto_rebuild ?)` / `([scan] ?)` の形で添える)、`exclude_dirs` / `exclude_files` の `/`(Windows では `\` も。名前と一致しえない)と `*` `?`(glob として扱わない)、`exclude_paths` の `*` `?`、`include` に書いたファイル、ネスト repo の中を指す親の `exclude_paths`、解釈できない `.gitignore` の行
- 未知のキーをエラーにしないのは、新しい版で足したキーを書いた設定を古い版で読んでも止めないため。存在しない `include` / `exclude_paths` と root の外を指す `include` は警告しない(マシンによって無いフォルダや後から作るフォルダを書く使い方を壊さないため)
- 同じ警告は 1 回の実行で 1 度だけ出す(ネスト repo の設定は 1 回の実行で何度か読み直すため)。実行のたびには出る(`auto_rebuild` の検索ごとにも出る)

**常に除外**: 設定に関わらず `.git` と `.mikke` は走査対象外(`exclude_dirs` をデフォルト置換しても、ネスト repo の設定でも生きる)。index/embeddings の出力先は「そのパス配下」を**root 相対 prefix 一致**で除外する(ディレクトリ名一致だと同名ディレクトリが任意の深さで全消えする事故になるため)。**ディレクトリ symlink は辿らない**(ループ防止。ファイル symlink の `.md` は対象)。
**パスでの除外**: 名前一致の `exclude_dirs` / `exclude_files` は任意の深さに効く。1 か所だけを外すなら、root 相対の `exclude_paths` を使う。判定は要素単位で、`state` は `state/...` に当たり `statement/...` に当たらない。ディレクトリは入る前に外す。ファイルを指す値はそのファイルだけを外す(出力先の除外は今までどおり配下の `.md` ファイルにだけ当て、出力先の下のネスト repo は歩く)。include の起点が配下にあれば、起点も外す(除外は include に勝つ)。末尾の `/`、先頭の `./`、重複した `/`、途中の `.` は吸収する。大文字小文字は区別する。`exclude_dirs` の既定とは独立で、書いても既定は残る。
**`.gitignore`**(`[scan] respect_gitignore`。既定 false で `.gitignore` を見ない): true の時は、root から上で最も近い `.git` を持つディレクトリから下の各 `.gitignore` と、その `.git/info/exclude` を git と同じ優先順で読む(深い `.gitignore` が勝ち、`info/exclude` が最も弱い。`!` の否定も効くが、除外されたディレクトリの中は否定で戻せない)。`.git` が見つからなければ root から下の `.gitignore` だけを読む(異常終了も警告もしない。コピーや展開で `.git` が無くなっても宣言は残るため)。root 自身と root より上は判定しない(上の `.gitignore` は root の下のパスにだけ当てる)。追跡済みのファイルでも、パターンに一致すれば外す(git の追跡状態は見ない。残すには `!path` を書く)。global excludes(`core.excludesFile`)・`.ignore` は読まない(索引の中身をマシンと利用者の設定に依存させないため)。大文字小文字はどの OS でも区別する(`core.ignorecase` は見ない)。`.git` を持つ下位のディレクトリ(ネストした git repo・submodule)では外側の規則を捨てて読み直し、その入口は外側の規則で判定する。`.git` がファイルの時は `info/exclude` を読まない。判定の順は、名前(`exclude_dirs` / `exclude_files`)→ パス(`exclude_paths`。ファイルは出力先も)→ `.gitignore` → ネスト境界。どれに当たっても結果は「外す」なので、順は性能にだけ効く。
**`[index] path` の検査**: `.md` で終わる path は、大文字小文字を区別せず、ファイルの有無によらず設定エラーにする(走査は名前が `.md` で終わるファイルを拾うので、index がノートとして走査され、既存のノートを上書きするおそれがあるため)。既存のファイルは、mikke の index か作りかけの index と確かめられた時だけ作り直す。それ以外は消さずに exit 2 にし、path と理由を出す(判定は「index スキーマ」の自動再構築を参照)。root の外の絶対パスも使える(同じ検査で守る)。

## ネストしたノート repo

走査中に `mikke.toml` / `.mikke.toml` を持つサブディレクトリへ入った場合、その配下は「ネストしたノート repo」として走査を**その repo の設定へ委譲**する:

- 委譲されるのは `[scan]`(include / exclude_dirs / exclude_files / exclude_paths / respect_gitignore)と `[index]` の出力先除外。`exclude_paths` と出力先は、ネスト repo の root 相対で判定する。子で書かないキーは既定に戻り、親の値を継がない(`respect_gitignore` も子の値に従う。子が true なら、同じ作業ツリーの親の `.gitignore` も子の中に効く)。孫 repo 以深も同ルールで再帰する
- 親の除外(`exclude_dirs` / `exclude_paths` / `.gitignore`)はネスト境界の**入口まで**効く(親の `exclude_dirs` にディレクトリ名を挙げるか、`exclude_paths = ["sub"]` でネスト repo ごと除外できる)。境界検知は親の除外判定より後。境界の内側を指す親の `exclude_paths` は効かず、警告を出す
- index は最上位の**単一 index のまま**で、path は最上位 root 相対。BM25 スコアの一貫性(単一 corpus)を保つため、ネスト repo が持つ index を別に引くフェデレーションは行わない
- ネスト repo の設定は `[scan]` と `[index]` だけを読む。`[health]` / `[search]` / `[semantic]` は読まない(型も未知のキーも検査しない。health の除外は最上位の `scan_skip_prefixes` / `quality_skip_prefixes` で指定する)。その repo を直接開けば最上位として全部を検査するので、親の実行で二重に報告しない
- TOML の構文エラーと `[scan]` / `[index]` の型エラー・不正な値は、path 付きで即エラー終了する(silent に親規則で走査しない)。未知の節と `[scan]` / `[index]` の未知のキー・効かない値は、子の path で警告する

## ノートの解釈

- **title 抽出優先順**: frontmatter `title` → 最初の `# ` 見出し → ファイル名 stem。
- **tags**: YAML sequence の null / 空文字列要素は登録しない。特に `- #Dog` は YAML ではコメントだけの null 要素であり、`Dog` へ復元しない (`- Dog` が修正例)。`- "#Dog"` は有効な文字列としてそのまま登録する。有効な重複タグは最初の出現を残して重複除去する (tags を文字列で書いた場合の 1 文字分解 quirk にも同じ重複除去を適用)。出現順は登録するタグ集合の決定にのみ効き、検索出力のタグ表示は従来どおりタグ名順。空要素は通常の `index` で path 付き警告、`health` で修正対象として報告し、`index --check` は構築完了後に exit 1 とする。
- **date 正規化**: date/updated は `YYYY-MM-DD` 文字列へ。YAML が date 型で解釈した場合も文字列化して揃える。
- **wikilink 抽出**: `\[\[([^\]|]+?)(?:\|[^\]]+?)?\]\]`、set 化して sorted。
- **語数カウント (word_count)**: `[　-鿿豈-﫿ｦ-ﾟ]` の CJK 文字数 + `[a-zA-Z0-9]+` の語数。文字クラスの範囲は**エスケープ表記で**書く(生リテラルは Unicode NFC 正規化で別文字に化ける)。health の低ボリューム判定の基準値。

## index スキーマ(SQLite)

```sql
notes(path PRIMARY KEY, title NOT NULL, date, updated, summary, word_count)
  INDEX idx_notes_date ON notes(date DESC)
tags(path, tag, tag_search, PRIMARY KEY(path, tag))  INDEX idx_tags_tag ON tags(tag)
links(path, target, PRIMARY KEY(path, target))
notes_fts USING fts5(path UNINDEXED, title, content, tokenize='trigram')
meta(key PRIMARY KEY, value)     -- meta['generated'] に index 生成時刻 (health の鮮度判定と
                                 -- auto_rebuild の stale 判定に使用)。meta['scanned_paths_sha256'] に
                                 -- 走査 path 集合 (root 相対、ソート済リスト) の SHA-256
                                 -- (auto_rebuild の stale 判定に使用)、meta['index_format'] に索引形式
```

index が無い場合は検索時に自動 build(clone 直後フォールバック)。`mikke index` は全再構築で、次の手順で作り直す: `<path>.lock` を排他ロックする(作り直しは 1 本ずつ。待つ間は stderr に告知する) → 同じフォルダの `<path>.tmp-<pid>` に 1 つの transaction で書く → 閉じる → rename で差し替える。最中の検索は直前の index を読む。中断・panic・走査中の設定エラーの後は、直前の index が残る(初回は無いまま)。残った一時ファイル(`<path>.tmp-<数字>` とその `-journal`)は、次の作り直しで消す。この形でないファイルは消さない。ロック非対応の FS では、Warning を出してロック無しで作る。Windows で差し替えられない時は約 10 秒リトライし、だめなら直前の index を残して exit 2 にする。`notes` / `tags.tag` / `links` と `summary` / `word_count` は原文由来の値を保ち、`notes_fts.title` / `notes_fts.content` / `tags.tag_search` は検索用に和欧間スペースを正規化する。正規化は U+0020 1 個の両隣が非 whitespace かつ少なくとも片側が和文クラスの場合にその空白を削除する(和文クラスは U+3001–U+303F、U+3040–U+30FF、U+31F0–U+31FF、U+3400–U+4DBF、U+4E00–U+9FFF、U+F900–U+FAFF、U+FF00–U+FFEF)。全角スペース・連続空白・行頭行末・タブ・NBSP は保存し、正規化は冪等。`meta['generated']` は epoch 秒の小数文字列(秒精度だと mtime との比較で秒未満の更新を取りこぼすため)。index フォーマットは内部表現であり互換保証しない(`.mikke/` は常に再生成可能)。

**index の判定**: `[index] path` のファイルを、無い / mikke の index / 作りかけ / それ以外(mikke の index でない・読めない)に分ける。mikke の index は、SQLite で、表の名前がすべて mikke のもの、notes の列が一致、meta テーブルがあるもの。作りかけは、0 バイト、表が無い、mikke の表だけで meta テーブルが無いもの。毎回の確認は、短命の接続で sqlite_master・notes の列・meta の 3 行(`index_format` / `generated` / `scanned_paths_sha256`)を読むだけで、md の走査はしない(既定経路の即起動を保つ)。判定は読み取り専用で開き、ファイルと付随ファイルを書き換えない。WAL モードの SQLite は開かずに mikke の index でないとする(mikke は WAL を使わない。開くと SQLite が `-wal` を書き戻して消すため)。巻き戻しの要る journal が残っている時は、journal を無視して形だけ見て、mikke の index か作りかけの時だけ巻き戻す(旧版は最終 path に直接書いていたため)。

**自動再構築**: 検索系コマンド(find / tag / title / semantic / hybrid / list-tags / recent / links / backlinks)の index 準備経路では、`auto_rebuild` の設定に関わらず毎回 index を判定し、上から順に当てはめる(告知は stderr。stdout の出力フォーマットは変えない):

1. 無い → 作る(「インデックスが無いため生成しています...」)
2. mikke の index でない・読めない → 消さずに exit 2(path と理由を出す)。`auto_rebuild` によらない
3. 作りかけ → `auto_rebuild = true` なら作り直す(「index の生成が途中で止まっているため再構築しています...」)。既定では exit 2 で `mikke index` を案内する
4. 検索と health が読む表(notes / tags / links / notes_fts / meta)が欠けている → 作り直す(「index が壊れているため再構築しています...」)
5. `meta['generated']` が無い → 作り直す(「前回の index 生成が完了していないため再構築しています...」)。設定によらない
6. `meta['index_format']` が無い・値が違う → 作り直す(「索引の形式が変わったため index を再構築しています...」)。設定によらない
7. `[index] auto_rebuild = true` (既定 false)で stale → 作り直す(「ソースの更新を検知したため index を再構築しています...」)

`health` は、無ければ作り、作りかけ・表の欠けた・mikke の index でない・読めない index は exit 2 にする。形式違いと generated 無しは作り直さず、鮮度判定も通らない(再構築すると index 鮮度診断がマスクされるため)。`mikke index` は、mikke の index でない・読めないファイルは exit 2 にし、それ以外(作りかけを含む)は告知なしで作り直す。作り直す前にロックを取って判定し直すので、並行した検索でも作り直しは 1 回で、告知も 1 本になる。

stale 判定は次のいずれか: (a) いずれかの md の mtime が `meta['generated']` より新しい、(b) 走査 path 集合の SHA-256 が `meta['scanned_paths_sha256']` と不一致(mtime が保存される移動・リネームや追加・削除も検知)、(c) path 集合の meta キーが無い index。`.gitignore` の編集や `exclude_paths` / `respect_gitignore` の変更で走査対象が変わった時も、走査 path 集合のハッシュで検知する。走査 md 件数と notes 行数の比較は行わない(frontmatter 破損で index から除外されるファイルがあると恒常 stale 化するため、スナップショットは破損分も含む走査 path 全体で取る)。対象は BM25 index のみで embeddings は対象外(semantic 側の鮮度は `mikke embed` の差分更新に委ねる)。限界と副作用: mtime を保存する copy(rsync -t 等)による内容変更は検知できない。有効時は検索のたびに全 md 走査 + metadata 取得のコストがかかり、ネスト repo の設定が壊れている場合は検索コマンド自体が path 付きで即エラー終了するようになる(fail-loud 方針と整合)。

## 検索セマンティクス

- **FTS 変換 (`fts_query`)**: 空白で語分割し、各語を個別に `"..."` quote して `AND` 連結。入力全体を 1 つの quote で囲むと連続一致要求で 0 件化するため**語ごと quote が必須**。内部の `"` は `""` にエスケープ。
- **和欧間スペース**: `find` はクエリを正規化せず、空白入りクエリの AND 意味論と語長による MATCH / LIKE 選択を維持する。`title` は正規化済み `notes_fts.title` に対してキーワード全体を同じ規則で正規化して LIKE 検索する。`tag` は `tag_search` とキーワードの両方を正規化して比較し、表示には原文の `tag` を使う。
- **trigram 制約**: FTS5 trigram は各語 3 文字以上必須。**語ごとに**長さ判定し、1 語でも 3 文字未満を含む場合は LIKE フォールバック(全語 AND の `mikke_lower(content/title) LIKE %term%` で絞り、簡易スコア順)。全語 3 文字以上なら `ORDER BY rank`(FTS5 の BM25)。
- **簡易スコア**(LIKE フォールバック): 全候補を読み、語ごとに BM25 の tf の項(本文での出現回数の飽和と、候補の平均に対する本文長の正規化。k1 = 1.2、b = 0.75)を足し、title に語を含めば語ごとに 1 点を足す。idf は使わない(全候補が全語を含むため)。スコア降順 → date (空なら updated) 降順 → path 昇順に並べてから上限で切る(date で並べてから切ると、古い本命が圏外に落ちるため)。
- **find の順序表示**: BM25 順か「簡易スコア順(短語を含み BM25 算出不可)」かを**正直に出す**。上限(`--top`、未指定なら `bm25_limit`)到達時は「上位 N 件で打ち切り(全ヒット数不明)」と明示(打ち切りを全件数と誤読させない)。`--top` は `bm25_limit` より大きくてもよい(`bm25_limit` は既定値で天井ではない)。`--top` は 1 以上。
- **tag/title**: 正規化したキーワードによる `mikke_lower(...) LIKE %kw%` 部分一致、date (空なら updated) 降順、同じキーは path 昇順。`--top N` で N 件に打ち切る。N 件に達したら find と同じ打ち切りの注記を見出しに付ける。
- **並びのキー**(tag / title / recent / backlinks と簡易スコアの同点): `date`、空なら `updated`(SQL では `COALESCE(NULLIF(date, ''), NULLIF(updated, ''), '')`)。date を書いたノートの並びは変わらず、date が空で updated のあるノートだけがその日付で並ぶ。どちらも空なら末尾。
- **LIKE の比べ方**(tag / title / find の LIKE フォールバック): 列とキーワードの両方を Rust の `to_lowercase` で小文字にして比べる(`mikke_lower` は検索の接続に登録する関数。SQLite の `LOWER` は ASCII しか変えないため使わない)。ASCII 以外の大文字小文字(`Ä` と `ä`、全角の `Ｇ` と `ｇ`)も同一視し、MATCH 経路と揃う。全角と半角は同一視しない。キーワードの `%` `_` `\` は `ESCAPE '\'` で逃がし、ワイルドカードでなく文字そのものに当てる。
- **recent**: 並びのキーが空でないノートを、キーの降順(同じキーは path 昇順)で LIMIT。
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
- **backlinks**: 見出し `'<path>' の被リンク (N件):`。N = 対象ノートを指しうる target を持つノート数。並びのキー(date、空なら updated)の降順(キーが空なら末尾、同じキーは path 昇順)でテキスト形式で表示。0 件は「該当するノートが見つかりませんでした。」
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

- **メタ行(常に 1 行目)**: `{"type":"meta","command":"<サブコマンド名>","count":N}`。テキスト見出しが持つ情報を JSON でも失わないため、find は `"order"`(`"relevance"` = BM25 順 / `"approx"` = 短語 fallback の簡易スコア順。v0.4.1 までは短語 fallback を date 降順に並べ `"date"` を出していた)と `"capped"`(上限 = `--top` か `bm25_limit` 到達 = true。ちょうど limit 件で打ち切りが無い場合も true になる保守的判定 — テキスト出力の打ち切り表示と同一条件。true のとき count を全ヒット数と誤読しない)を必ず含む。tag / title は `--top` を付けた時だけ `"capped"`(判定は find と同じ)を含む。hybrid は BM25 ストリームの順序を find と同じ値の `"order"` で、`"degraded"`(semantic ストリームが使えず BM25 のみになった場合に true。埋め込み未構築のほか、構築済みでも semantic 検索の実行時失敗で true になる)を必ず含む。0 件時はメタ行のみ(hit 行 0 行)
- **hit 行(2 行目以降、1 件 1 行)**: `{"path":"...","title":"...","date":"...","updated":"...","tags":["a","b"],"summary":"...","score":0.0123,"via":"bm25+vec"}`。`type` フィールドは付けない。score は semantic/hybrid のみ、via は hybrid のみ(semantic は via を設定しない)。updated と summary の空は `""` のまま(テキスト出力の sentinel 文は出さない — JSON では空文字列が欠落の明示になる)。score は f64 全精度で、テキスト出力の 4 桁丸めとは表記が異なる
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
- **品質チェック**: tags の YAML null / 空文字列要素は filesystem から検出し、タグなし / 要約なし / `word_count < min_words` / updated 未設定は index ベースで検出する。各々 `quality_skip_prefixes` を適用。filesystem 検出の tags 空要素は frontmatter 破損スキャンと同じ走査に乗るため `scan_skip_prefixes` も併せて効く (index build の警告と `index --check` の非 0 終了はどちらの prefix にも影響されない)。
- **チェックの無効化(`disable`)**: `[health] disable` に挙げたチェック名を項目単位でスキップする(既定は空 = 全チェック有効)。許容名は `frontmatter` / `tags` / `summary` / `low_words` / `updated` の 5 種(完全一致・大文字小文字を区別)。未知名は「設定読み込みの厳格さ」に従い即エラー終了する(typo を silent no-op にしない)。効くのは health コマンドのみで、`mikke index --check` の非 0 終了や index build 時の警告は変わらない(CI 併用時の混乱防止)。実行bit欠落は `exec_bit_prefixes` による opt-in、index 鮮度は情報表示のため対象外。
- **index 鮮度**: `meta['generated']` より mtime が新しい md の件数。`meta['generated']` が無い時は、生成が完了していない(途中で止まった可能性)と出す。実行時依存なので **stdout のみ**、md レポートには含めない(レポートの差分 = 実質的状態変化、にするため)。`auto_rebuild` 有効でも health は再構築しない(「index スキーマ」参照。作りかけ・壊れた・索引でない index はエラー (2))。
- **md レポート (`--md-report`)**: 揮発情報(実行時刻・鮮度)を含めず決定的に生成 → 「内容が変わった時だけ commit」運用。改行は **LF 固定**(Windows CRLF と Linux nightly LF で差分が出て決定性が壊れるのを防ぐ)。パスはレポート置き場からの相対 md リンク(`#` は %23、空白/括弧は `<>` wrap、`[]` はエスケープ、基底名衝突回避のため wikilink でなくパスリンク)。

## cross-platform 正しさ(テストで固定)

silent に壊れやすい箇所。いずれも過去に実運用で踏んだ hard-won な知見であり、退行させないためテストで固定する:

- **BOM**: md/TOML は utf-8-sig 相当で読む(BOM を content の一部にしない)。BOM を素の utf-8 で読むと先頭の `---` が認識されず frontmatter が silent 喪失する。
- **CJK 文字クラスのエスケープ表記**: 「ノートの解釈」の語数カウント参照。生リテラルの範囲指定はエディタ・ツールの NFC 正規化で別文字に化け、判定が silent にずれる。
- **git 出力の encoding**: `--show-toplevel` / `ls-files` は UTF-8 で decode し、`core.quotepath=off`。非 ASCII パスが silent に壊れる/skip されるのを防ぐ。
- **stdout encoding**: Windows で符号化不能文字(em dash 等)が混ざっても crash させず継続(console/リダイレクト時の挙動をテスト)。
- **改行**: md レポートは LF 固定(health 参照)。
- **パスの区切り**: `exclude_paths` は OS によらず `/` で書く(`\` はエラー)。判定は root 相対の posix 表現で比べる。`.gitignore` は各ファイルの置き場所からの相対パスで照らす(Windows の verbatim パスでも一致させるため)。大文字小文字はどの OS でも区別する。
- **index の差し替え**: Windows では SQLite が FILE_SHARE_DELETE 無しで開くので、開いている index を rename で置き換えられない。リトライして、だめなら直前の index を残してエラーにする(`index_safety_` のテストで固定する。CI は Windows でも全テストを流す)。

## golden テスト

`tests/golden/` が仕様の期待出力(`manifest.tsv` がコマンド → golden ファイルの対応表)。`tests/fixture/` の小さなノート集合に対し各コマンドの stdout を厳密比較する。出力を変える変更は、意図した差分であることを確認した上で golden を更新する。
