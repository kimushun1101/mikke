# インストール

最短手順は [README](../README.md#インストール) を参照。ここではオプション・環境変数・トラブルシューティングを扱う。

## Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh
```

Releases から自環境向けバイナリを取得し、`SHA256SUMS` を検証して `~/.local/bin` に配置する。配置先が PATH に無ければ、shell の設定ファイル (zsh は `~/.zshrc`、bash は `~/.bashrc`、macOS の bash は `~/.bash_profile`) に追記する 1 行を案内する (fish は `fish_add_path <dir>` を一度実行する。設定ファイルは自動では変更しない)。追記前でもフルパス `~/.local/bin/mikke` でそのまま使える。既定で semantic 検索入りの full 版が入る。BM25 のみの slim 版は `| sh -s -- --slim`。Linux では glibc バージョン非依存の `-musl` (完全静的リンク) が選ばれるため、distro の glibc は気にしなくてよい。

配置先やバージョン固定などのオプションは、ワンライナーのまま一覧できる:

```bash
curl -fsSL https://raw.githubusercontent.com/kimushun1101/mikke/main/install.sh | sh -s -- --help
```

同じ指定は環境変数 `MIKKE_VARIANT` / `MIKKE_VERSION` / `MIKKE_INSTALL_DIR` / `MIKKE_TARGET` でもできる。ただしパイプで渡す形では変数を前に置く `MIKKE_VARIANT=slim curl ... | sh` は効かない — その代入が付くのは `curl` の側で、スクリプトを実行する `sh` には渡らないため既定の full が入る。`curl ... | MIKKE_VARIANT=slim sh` と後段に付けるか、事前に `export` する。

## Windows

```powershell
irm https://raw.githubusercontent.com/kimushun1101/mikke/main/install.ps1 | iex
```

配置先は `%USERPROFILE%\.local\bin` (install.sh と同じ `~/.local/bin`)。配置先がユーザー PATH (レジストリ HKCU) に無ければ自動で追加する。追加は新しいターミナルから有効で、実行中のセッションの `$env:Path` にも足すのでその場で `mikke` が使える。PATH を触らせたくない場合は `$env:MIKKE_NO_MODIFY_PATH = "1"` を設定して実行すると、追加せず手動追加用のコマンドを案内する。

以前の既定 `%LOCALAPPDATA%\Programs\mikke` に入れていた場合も、再実行すると新しい場所に入る。旧場所の `mikke.exe` は残るので削除する。旧場所をユーザー PATH に追加していた場合は、新しい配置先は PATH の末尾に足されるため旧 `mikke.exe` が先に見つかり、スクリプトが `active:` と警告で知らせる (新版が効かない状態)。旧 `mikke.exe` を削除し、ユーザー PATH の旧エントリも外す (スクリプトは案内するだけで、上書きや PATH の並べ替えはしない)。

`iex` 経由ではパラメータを渡せないため、オプションは実行前に環境変数で指定する:

```powershell
$env:MIKKE_VARIANT        = "slim"      # full (既定) / slim
$env:MIKKE_VERSION        = "v0.3.0"    # 既定: latest
$env:MIKKE_INSTALL_DIR    = "C:/tools"  # 既定: $env:USERPROFILE/.local/bin
$env:MIKKE_NO_MODIFY_PATH = "1"         # 設定するとユーザー PATH を変更しない (既定: 未設定 = 自動追加)
```

## 入口の固定

バイナリは `SHA256SUMS` で検証されるが、入口の `install.sh` / `install.ps1` 自体は main 追従で取得される。入口も固定したい場合は commit SHA 付き raw URL を使う。

## cargo install

ビルドにはシステムの C コンパイラ (`cc`) が要る (rusqlite の bundled SQLite ビルド用)。BM25 のみ (既定はスクリプトと違い BM25 のみ):

```bash
cargo install --git https://github.com/kimushun1101/mikke --locked
```

semantic 検索も使う場合 (embedding スタックを同梱):

```bash
cargo install --git https://github.com/kimushun1101/mikke --locked --features semantic
```

`--locked` は `Cargo.lock` の依存バージョンをそのまま使う (CI と同じ組み合わせ)。既定では main HEAD が入るので、リリース版に固定するなら `--tag v0.3.0` を足す。

同じコマンドの再実行で入れ直せる。feature 構成や commit が変わっていれば `--force` 無しでも `Replacing` となって入れ替わる。source と feature が完全に同一の時だけ `Ignored package ... is already installed` と出て何もせず終わるので、そこだけ `--force` が要る (この時も exit 0 のため、スクリプトからは成功と区別できない)。

`install.sh` / `install.ps1` は `~/.local/bin`、`cargo install` は `~/.cargo/bin` と配置先が違う。両方に入れると PATH の先頭側だけが使われる (rustup の shell 設定は `~/.cargo/bin` を先頭に足す)。スクリプトは配置後に `installed:` (今回配置したファイル) と、PATH 上で `mikke` が見つかれば `active:` (`mikke` と打つと実際に起動するもの) を分けて表示し、両者が別の実体なら warning (install.sh は `warning:` 行、install.ps1 は PowerShell の警告ストリーム) でその場所を示す (両 OS 共通。相手のプログラムは実行しない)。`install.sh` が見るのはスクリプトを実行した shell から見える PATH 上のファイルだけで、`curl | sh` は子 shell で動くため親 shell の alias や関数は見えない。自分の shell で実際に何が起動するかは `command -v mikke` で確かめる (`install.ps1` は `iex` で同じセッション内で動くので alias / function も検出する)。上書きや PATH の書き換えはせず、所有元での対処を案内する: `~/.cargo/bin` の `mikke` は cargo 管理なので、cargo 版を使い続けるなら `cargo install` で更新し、スクリプト版へ移るなら先に `cargo uninstall mikke` で消す (スクリプトで `~/.cargo/bin` を上書きすると cargo の管理情報と食い違うため)。それ以外は所有元 (パッケージマネージャ等) で更新・削除するか、PATH の順序を見直す。実際に動く方は `command -v mikke` (PowerShell は `Get-Command mikke`) と `mikke --version` (full 版は `+semantic` 表記) で確認する。

## Releases から手動取得

target 別のビルド済みバイナリのアーカイブ (`mikke-{slim,full}-<target>.tar.gz` / Windows は `.zip`、slim = BM25 のみ / full = semantic 入り、`SHA256SUMS` 付き) を置く。展開して PATH の通った場所に置けばよい。更新はバイナリ差し替え、または `cargo install` の再実行。

Linux 用は `-gnu` (glibc 動的リンク) と `-musl` (完全静的リンク) の 2 系統がある。スクリプト経由なら `-musl` が選ばれるので意識不要。手動で `-gnu` を選んで古い distro で `GLIBC_X.YY not found` が出る場合は `-musl` に替える。
