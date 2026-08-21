# mikke

日本語 Markdown ノートのためのローカル検索 CLI。「みっけ👀」

Markdown ノートの入ったフォルダ (YAML frontmatter + wikilink、Obsidian 互換) を対象に、BM25 全文検索・タグ/タイトル検索・semantic 検索を単一バイナリで提供する。Claude Code / Codex CLI / Cursor 等の **AI コーディングエージェントに、人のノート資産を検索させる**ことを主眼に設計している (人間が直接叩いてもよい)。

- **日本語対応の BM25 全文検索** — SQLite FTS5 (trigram tokenizer)。形態素解析なしで日本語・英語混在ノートを検索できる
- **即起動・依存ゼロ** — 単一バイナリ。エージェントが 1 セッションに何十回叩いても待ちがない
- **semantic / hybrid 検索** (optional, `--features semantic`) — ローカル embedding + RRF 融合。外部 API 不使用でノートが外に出ない
- **graceful degradation** — 埋め込みが無い環境では自動的に BM25 のみで動く
- **フォルダごとに独立・git 不要** — 対象はただのフォルダでよく、git repo である必要はない。index はルート直下 `.mikke/` に生成 (git 管理下なら gitignore する)。設定はルートの `mikke.toml` 1 枚 (全キー省略可、隠したい場合は `.mikke.toml`)。ノートフォルダを別のノートフォルダの下に置いた場合、親からの走査はネストされた側の `[scan]` 設定を尊重する
- **health チェック** — frontmatter 破損・タグ/要約欠落などを設定駆動で検査 (CI 向け `index --check` あり)。md レポートは決定的に生成され「変化した時だけ commit」運用ができる

設計思想と運用フローは [docs/concept.md](docs/concept.md)、挙動の正確な仕様は [docs/SPEC.md](docs/SPEC.md)。

## インストール

Linux (x86_64) / macOS (Apple Silicon) はインストールスクリプトで入る (Releases から自環境向けバイナリを取得し、`SHA256SUMS` を検証して `~/.local/bin` に配置):

```bash
curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh
```

既定で semantic 検索入りの full 版が入る。BM25 のみの slim 版は `| sh -s -- --slim`。Linux では glibc バージョン非依存の `-musl` (完全静的リンク) が選ばれるため、distro の glibc は気にしなくてよい。

配置先やバージョン固定などのオプションは、ワンライナーのまま一覧できる:

```bash
curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh -s -- --help
```

同じ指定は環境変数 `MIKKE_VARIANT` / `MIKKE_VERSION` / `MIKKE_INSTALL_DIR` / `MIKKE_TARGET` でもできる。

Windows (x86_64) は PowerShell で:

```powershell
irm https://raw.githubusercontent.com/kimushun1101/mikke/main/install.ps1 | iex
```

`iex` 経由ではパラメータを渡せないため、オプションは実行前に環境変数で指定する:

```powershell
$env:MIKKE_VARIANT     = "slim"      # full (既定) / slim
$env:MIKKE_VERSION     = "v0.3.0"    # 既定: latest
$env:MIKKE_INSTALL_DIR = "C:/tools"  # 既定: $env:LOCALAPPDATA/Programs/mikke
```

> バイナリは `SHA256SUMS` で検証されるが、入口の `install.sh` / `install.ps1` 自体は main 追従で取得される。入口も固定したい場合は commit SHA 付き raw URL を使う。

cargo でビルドして入れる場合 (既定はスクリプトと違い BM25 のみ)。ビルドにはシステムの C コンパイラ (`cc`) が要る (rusqlite の bundled SQLite ビルド用)。BM25 のみ:

```bash
cargo install --git https://github.com/kimushun1101/mikke --locked
```

semantic 検索も使う場合 (embedding スタックを同梱):

```bash
cargo install --git https://github.com/kimushun1101/mikke --locked --features semantic
```

`--locked` は `Cargo.lock` の依存バージョンをそのまま使う (CI と同じ組み合わせ)。既定では main HEAD が入るので、リリース版に固定するなら `--tag v0.3.0` を足す。

> 同じコマンドの再実行で入れ直せる。feature 構成や commit が変わっていれば `--force` 無しでも `Replacing` となって入れ替わる。source と feature が完全に同一の時だけ `Ignored package ... is already installed` と出て何もせず終わるので、そこだけ `--force` が要る (この時も exit 0 のため、スクリプトからは成功と区別できない)。

> `install.sh` は `~/.local/bin`、`cargo install` は `~/.cargo/bin` と配置先が違う。両方に入れると PATH の先頭側だけが使われる (rustup の shell 設定は `~/.cargo/bin` を先頭に足す)。実際に動く方は `command -v mikke` と `mikke --version` (full 版は `+semantic` 表記) で確認する。

Releases には target 別のビルド済みバイナリのアーカイブ (`mikke-{slim,full}-<target>.tar.gz` / Windows は `.zip`、slim = BM25 のみ / full = semantic 入り、`SHA256SUMS` 付き) を置く。展開して PATH の通った場所に置けばよい。更新はバイナリ差し替え、または `cargo install` の再実行。

> Linux 用は `-gnu` (glibc 動的リンク) と `-musl` (完全静的リンク) の 2 系統がある。スクリプト経由なら `-musl` が選ばれるので意識不要。手動で `-gnu` を選んで古い distro で `GLIBC_X.YY not found` が出る場合は `-musl` に替える。

## 使い方

```bash
cd <ノートフォルダ>
mikke index               # index 生成 (初回は検索時に自動生成される)
mikke find 検索 語        # 全文検索 (BM25 順)
mikke tag タグ名         # タグ検索
mikke title キーワード     # タイトル検索
mikke recent 10           # 最近のノート
mikke list-tags           # タグ一覧
mikke links ノート         # 発リンク (wikilink 先をノートへ解決して表示)
mikke backlinks ノート     # 被リンク (このノートを指すノートの一覧)
mikke health              # 健全性チェック
mikke embed               # 埋め込み生成 (semantic feature 必須)
mikke semantic クエリ      # 意味検索
mikke hybrid クエリ        # BM25 + semantic の RRF 融合
```

検索系コマンド (find / tag / title / semantic / hybrid / recent) と `list-tags` は `--json` で JSON Lines (1 行目メタ行 + 1 件 1 行) を stdout に出力する。jq やスクリプトから安全にパースでき、exit code は変わらない。スキーマは [docs/SPEC.md](docs/SPEC.md) の「出力フォーマット」。

ルートは `--root PATH` 明示指定 → 環境変数 `MIKKE_ROOT` → cwd からの `mikke.toml` (設定を隠したい場合は `.mikke.toml`) 上方探索 → git root の順で決める。git 管理でないフォルダも、`mikke.toml` を置くか `--root` を指定すればそのまま対象にできる。

### 設定 (mikke.toml)

ノートフォルダのルートに置く。全キー省略可で、**空ファイルでもルートマーカーとして機能する**。

```toml
# mikke 設定 — https://github.com/kimushun1101/mikke
[semantic]
enabled = true   # semantic / hybrid の意味検索を使う場合のみ (既定 false。mikke embed の前提)
```

全キーの注釈付きサンプルは [docs/SPEC.md](docs/SPEC.md) の「設定スキーマ」を参照。

## semantic / hybrid 検索

`--features semantic` 付きビルドで、ローカル embedding (candle 製・純 Rust) による意味検索が使える。外部 API は使わずノートは外に出ない。

```bash
mikke embed                # 埋め込みを生成 (2 回目以降は変更ノートのみ差分更新)
mikke embed --force        # 全件再構築
mikke semantic あの摩擦補償っぽいやつ   # 意味検索 (言い換え・表記ゆれ・日英混在に強い)
mikke hybrid 発振 対策      # BM25 と semantic の RRF 融合
```

- モデルは既定で `intfloat/multilingual-e5-small` (`mikke.toml` の `[semantic] model` で変更可。BERT 系アーキテクチャのみ対応)
- **初回の `embed`/`semantic` 実行時にモデルを Hugging Face から自動ダウンロード**する (約 470MB → `~/.cache/huggingface/`)。以降はオフラインで動く
- **オフライン / 社内網**: ダウンロードには huggingface.co への HTTPS 直接続が必要。proxy 等で取得できない場合は、取得済みマシンの `~/.cache/huggingface/hub/models--intfloat--multilingual-e5-small/` をそのままコピーすれば動く
- 差分検出はファイル内容の SHA-256。モデルや prefix を変えた場合は自動で全再構築する

## AI エージェントから使う

mikke は AI コーディングエージェントにノート資産を検索させる用途を主眼にしている。組み込みは 2 段階:

1. **指示書にスニペットを貼る** — [examples/agents/mikke.md](examples/agents/mikke.md) を、ノートフォルダの `CLAUDE.md` / `AGENTS.md` など使っているツールが読む指示書にそのまま貼る。ツール非依存の検索手順 (root 解決から検証まで) で、どのエージェント CLI でも使える。

2. **再利用可能な手順書として組み込む** — ツールが手順書の仕組みを持つ場合 (例: Claude Code の skill) は、[examples/skills/mikke/SKILL.md](examples/skills/mikke/SKILL.md) を土台にできる。起動条件・find のクエリセマンティクス (語ごと quote の AND 連結)・0 件時のフォールバック手順 (find → hybrid → Grep) まで含む実戦形の例なので、自分の運用・ツールに合わせて調整して使う (Claude Code ならノートフォルダの `.claude/skills/mikke/SKILL.md` にコピー)。

検索系コマンド (find / tag / title / semantic / hybrid) は grep の慣習で **1 件以上ヒット = exit 0 / 0 件 = exit 1 / エラー = exit 2** を返すため、0 件時のフォールバック判定は出力文言でなく exit code で書ける (`if mikke find ...; then` や `mikke find ... || mikke hybrid ...`)。詳細は [docs/SPEC.md](docs/SPEC.md) の「exit code」。

## 開発

```bash
cargo build            # BM25 のみ
cargo test             # golden 統合テスト (tests/golden/ の期待出力と厳密比較)
cargo test --features semantic                # semantic 込み (モデル不要のテストまで)
cargo test --features semantic -- --ignored   # embed/semantic/hybrid e2e (要 モデル cache かネットワーク)
```

システムに C コンパイラ (`cc`) が必要 (rusqlite の bundled SQLite ビルド用)。

CLI 表面・設定キー・出力の意味は安定インターフェース (`docs/SPEC.md` が正本)。挙動を変える時は仕様と `tests/golden/` を意図して同時に更新する。
