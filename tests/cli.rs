//! Golden 統合テスト — CLI 出力が tests/golden/ の期待出力と一致することを検証する。
//!
//! tests/fixture/ をテンポラリへ複製 → `mikke index` → 各コマンドを実行し、
//! stdout を tests/golden/<name>.txt と厳密比較する (対応表は manifest.tsv)。
//! 出力を変える変更は golden の意図的な更新を伴う (docs/SPEC.md「golden テスト」)。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mikke")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture")
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap();
        }
    }
}

fn temp_copy(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mikke-it-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    copy_dir(&fixture_dir(), &dir);
    dir
}

fn run(root: &Path, args: &[&str]) -> (String, String) {
    let out = Command::new(bin())
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("failed to run mikke");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// --root 無しで cwd から起動する (ルート上方探索の検証用)。MIKKE_ROOT は外す。
fn run_in(cwd: &Path, args: &[&str]) -> (String, String) {
    let out = Command::new(bin())
        .current_dir(cwd)
        .env_remove("MIKKE_ROOT")
        .args(args)
        .output()
        .expect("failed to run mikke");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// テスト用の最小ノート repo を temp に組み立てる。files は (相対パス, 内容)。
fn temp_repo(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mikke-it-{}-{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&dir);
    for (rel, content) in files {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
    }
    dir
}

/// exit status も見たい場合の実行 (異常終了系の検証用)。
fn run_raw(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("failed to run mikke")
}

/// manifest.tsv の各コマンド出力を golden と比較。
#[test]
fn golden_commands() {
    let root = temp_copy("cmds");
    // index を先に作る (auto-build ノイズを排除)
    let (_o, _e) = run(&root, &["index"]);

    let manifest = fs::read_to_string(golden_dir().join("manifest.tsv")).unwrap();
    let mut failures = Vec::new();
    for line in manifest.lines() {
        let (name, argstr) = line.split_once('\t').unwrap();
        let args: Vec<&str> = argstr.split_whitespace().collect();
        let (stdout, stderr) = run(&root, &args);
        let expected = fs::read_to_string(golden_dir().join(format!("{name}.txt"))).unwrap();
        if stdout != expected {
            failures.push(format!(
                "--- {name} (mikke {argstr}) ---\nEXPECTED:\n{expected}\nGOT stdout:\n{stdout}\nGOT stderr:\n{stderr}"
            ));
        }
    }
    let _ = fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "{} golden mismatch:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// .mikke.toml がルートマーカー (上方探索) として効き、[scan] 設定も適用される。
#[test]
fn hidden_config_upward_search() {
    let root = temp_repo(
        "hidden",
        &[
            (".mikke.toml", "[scan]\nexclude_dirs = [\"drafts\"]\n"),
            ("notes/hit.md", "# Hit\n\nquokka のメモ。\n"),
            ("drafts/miss.md", "# Miss\n\nquokka の下書き。\n"),
        ],
    );
    // --root 無し・サブディレクトリ起動: 上方探索で .mikke.toml を発見できること
    let (stdout, stderr) = run_in(&root.join("notes"), &["find", "quokka"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("notes/hit.md"),
        "hit が出ない:\n{stdout}\n{stderr}"
    );
    assert!(
        !stdout.contains("drafts/miss.md"),
        ".mikke.toml の exclude_dirs が効いていない:\n{stdout}"
    );
}

/// mikke.toml と .mikke.toml が両方あれば mikke.toml 優先 + stderr 警告。
#[test]
fn both_configs_prefer_visible() {
    let root = temp_repo(
        "both",
        &[
            ("mikke.toml", "[scan]\nexclude_dirs = [\"drafts\"]\n"),
            (".mikke.toml", "[scan]\nexclude_dirs = [\"notes\"]\n"),
            ("notes/hit.md", "# Hit\n\nquokka のメモ。\n"),
            ("drafts/miss.md", "# Miss\n\nquokka の下書き。\n"),
        ],
    );
    let (stdout, stderr) = run(&root, &["find", "quokka"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("notes/hit.md") && !stdout.contains("drafts/miss.md"),
        "mikke.toml (visible) が優先されていない:\n{stdout}"
    );
    assert!(
        stderr.contains("両方あります"),
        "両立警告が出ていない:\n{stderr}"
    );
}

/// 親の exclude_dirs はネスト repo の入口ごと塞げる (境界検知より親の除外が先)。
#[test]
fn parent_exclude_blocks_nested_entrance() {
    let root = temp_repo(
        "entrance",
        &[
            ("mikke.toml", "[scan]\nexclude_dirs = [\"vault\"]\n"),
            ("notes/a.md", "# A\n\nwalrus のノート。\n"),
            ("vault/mikke.toml", "[scan]\n"),
            ("vault/notes/b.md", "# B\n\nwalrus の秘匿ノート。\n"),
        ],
    );
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(stdout.contains("notes/a.md"), "親ノートが出ない:\n{stdout}");
    assert!(
        !stdout.contains("vault/"),
        "親の exclude_dirs で入口ごと除外されていない:\n{stdout}"
    );
}

/// 親の exclude はネスト境界の内側へ漏れず、子は子の exclude だけが効く。
#[test]
fn parent_exclude_not_leaked_into_nested() {
    let root = temp_repo(
        "leak",
        &[
            ("mikke.toml", "[scan]\nexclude_dirs = [\"drafts\"]\n"),
            ("drafts/parent-draft.md", "# PD\n\nwalrus 親下書き。\n"),
            ("sub/mikke.toml", "[scan]\nexclude_dirs = [\"other\"]\n"),
            ("sub/drafts/child-note.md", "# CN\n\nwalrus 子ノート。\n"),
            ("sub/other/child-other.md", "# CO\n\nwalrus 子除外。\n"),
        ],
    );
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("sub/drafts/child-note.md"),
        "親の exclude が子内部へ漏れている:\n{stdout}"
    );
    assert!(
        !stdout.contains("drafts/parent-draft.md"),
        "親側の exclude が効いていない:\n{stdout}"
    );
    assert!(
        !stdout.contains("sub/other/"),
        "子の exclude が効いていない:\n{stdout}"
    );
}

/// 壊れたネスト repo 設定は silent に親規則で走査せず、path 付きで非 0 終了する。
#[test]
fn broken_nested_config_fails_loudly() {
    let root = temp_repo(
        "brokencfg",
        &[
            ("mikke.toml", "[scan]\n"),
            ("notes/a.md", "# A\n\n通常ノート。\n"),
            // include を文字列で書く型エラー (配列必須)
            ("bad/mikke.toml", "[scan]\ninclude = \"notes\"\n"),
            ("bad/notes/b.md", "# B\n\n子ノート。\n"),
        ],
    );
    let out = run_raw(&root, &["index"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(!out.status.success(), "型エラーの子設定で成功してしまった");
    assert!(
        stderr.contains("bad") && stderr.contains("配列"),
        "エラーに子設定の path / 型メッセージが無い:\n{stderr}"
    );
}

/// ネスト repo の include がその repo のルート外を指しても取り込まれない。
#[test]
fn nested_include_outside_root_ignored() {
    let root = temp_repo(
        "escape",
        &[
            ("mikke.toml", "[scan]\nexclude_dirs = [\"outside\"]\n"),
            ("outside/leak.md", "# Leak\n\nwalrus 域外ノート。\n"),
            (
                "sub/mikke.toml",
                "[scan]\ninclude = [\".\", \"../outside\"]\n",
            ),
            ("sub/ok.md", "# OK\n\nwalrus 子ノート。\n"),
        ],
    );
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(stdout.contains("sub/ok.md"), "子ノートが出ない:\n{stdout}");
    assert!(
        !stdout.contains("outside/leak.md"),
        "子 include がルート外へ抜けている:\n{stdout}"
    );
}

/// include 起点が exclude_dirs 名の配下を直接指す場合は対象外 (旧実装と同値)。
#[test]
fn include_under_excluded_name_skipped() {
    let root = temp_repo(
        "incexcl",
        &[
            (
                "mikke.toml",
                "[scan]\ninclude = [\".\", \"templates/sub\"]\n",
            ),
            ("notes/a.md", "# A\n\nwalrus のノート。\n"),
            // templates は既定 exclude_dirs (置換していないので有効)
            ("templates/sub/t.md", "# T\n\nwalrus テンプレ。\n"),
        ],
    );
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(stdout.contains("notes/a.md"), "親ノートが出ない:\n{stdout}");
    assert!(
        !stdout.contains("templates/"),
        "除外名配下の include 起点が取り込まれている:\n{stdout}"
    );
}

/// .git / .mikke の常時除外は exclude_dirs をデフォルト置換しても (子 repo でも) 生きる。
#[test]
fn always_exclude_survives_replacement() {
    let root = temp_repo(
        "always",
        &[
            ("mikke.toml", "[scan]\nexclude_dirs = [\"x\"]\n"),
            (".git/leak.md", "# G\n\nwalrus git 内。\n"),
            ("ok.md", "# OK\n\nwalrus のノート。\n"),
            ("sub/mikke.toml", "[scan]\nexclude_dirs = [\"y\"]\n"),
            ("sub/.git/leak2.md", "# G2\n\nwalrus 子 git 内。\n"),
            ("sub/ok2.md", "# OK2\n\nwalrus 子ノート。\n"),
        ],
    );
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("ok.md") && stdout.contains("sub/ok2.md"),
        "通常ノートが出ない:\n{stdout}"
    );
    assert!(
        !stdout.contains(".git/"),
        ".git の常時除外が置換で消えている:\n{stdout}"
    );
}

/// 子の [index] 出力先 (root 相対 prefix) 配下は親 index に混入しない。
#[test]
fn nested_index_output_excluded() {
    let root = temp_repo(
        "childidx",
        &[
            ("mikke.toml", "[scan]\n"),
            ("sub/mikke.toml", "[index]\nembeddings_dir = \"vecs\"\n"),
            ("sub/ok.md", "# OK\n\nwalrus 子ノート。\n"),
            ("sub/vecs/leak.md", "# L\n\nwalrus 出力先内。\n"),
        ],
    );
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(stdout.contains("sub/ok.md"), "子ノートが出ない:\n{stdout}");
    assert!(
        !stdout.contains("sub/vecs/"),
        "子の [index] 出力先除外が委譲されていない:\n{stdout}"
    );
}

/// exclude_dirs の名前一致は任意の深さの中間ディレクトリにも効く (旧コンポーネント一致と同値)。
#[test]
fn exclude_name_matches_at_any_depth() {
    let root = temp_repo(
        "depth",
        &[
            ("mikke.toml", "[scan]\n"),
            ("a/ok.md", "# OK\n\nwalrus のノート。\n"),
            // templates は既定 exclude_dirs — 深い中間ディレクトリでも除外される
            ("a/b/templates/deep/t.md", "# T\n\nwalrus テンプレ。\n"),
        ],
    );
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(stdout.contains("a/ok.md"), "通常ノートが出ない:\n{stdout}");
    assert!(
        !stdout.contains("templates/"),
        "深い中間ディレクトリの除外名一致が効いていない:\n{stdout}"
    );
}

// --- version (slim/full 判別) ---

/// --version は subcommand 探索より前に判定され、slim ビルドでは従来通りの表記のまま。
#[cfg(not(feature = "semantic"))]
#[test]
fn version_slim() {
    let (stdout, stderr) = run_in(&std::env::temp_dir(), &["--version"]);
    let expected = fs::read_to_string(golden_dir().join("version_slim.txt")).unwrap();
    assert_eq!(stdout, expected, "stderr:\n{stderr}");
}

/// --version が semantic feature 有効ビルドでは判別可能な表記になる。
#[cfg(feature = "semantic")]
#[test]
fn version_full() {
    let (stdout, stderr) = run_in(&std::env::temp_dir(), &["--version"]);
    let expected = fs::read_to_string(golden_dir().join("version_full.txt")).unwrap();
    assert_eq!(stdout, expected, "stderr:\n{stderr}");
}

// --- semantic (embed / semantic / hybrid) ---

/// semantic 無効 repo での embed は明示エラーで exit 1 (feature 有効ビルド)。
#[cfg(feature = "semantic")]
#[test]
fn embed_disabled_repo_errors() {
    let root = temp_repo(
        "emb-disabled",
        &[("mikke.toml", "[scan]\n"), ("a.md", "# A\n\nメモ。\n")],
    );
    let out = run_raw(&root, &["embed"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(
        !out.status.success(),
        "semantic 無効 repo で成功してしまった"
    );
    assert!(
        stderr.contains("semantic が無効"),
        "無効メッセージが無い:\n{stderr}"
    );
}

/// slim ビルドの embed / semantic はビルド無効の明示エラーで exit 1 (silent 劣化させない)。
#[cfg(not(feature = "semantic"))]
#[test]
fn embed_slim_build_errors() {
    let root = temp_repo(
        "emb-slim",
        &[
            ("mikke.toml", "[semantic]\nenabled = true\n"),
            ("a.md", "# A\n\nメモ。\n"),
        ],
    );
    let embed_out = run_raw(&root, &["embed"]);
    let semantic_out = run_raw(&root, &["semantic", "クエリ"]);
    let embed_err = String::from_utf8_lossy(&embed_out.stderr).into_owned();
    let semantic_err = String::from_utf8_lossy(&semantic_out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(
        !embed_out.status.success(),
        "slim ビルドの embed が成功してしまった"
    );
    assert!(
        embed_err.contains("このビルドは semantic 無効です"),
        "embed のビルド無効メッセージが無い:\n{embed_err}"
    );
    assert!(
        !semantic_out.status.success(),
        "slim ビルドの semantic が成功してしまった"
    );
    assert!(
        semantic_err.contains("このビルドで無効です"),
        "semantic のビルド無効メッセージが無い:\n{semantic_err}"
    );
}

/// embed → semantic → hybrid → 差分更新 → 削除 → prefix 変更 の一連 e2e。
/// モデル取得が必要 (HF cache かネットワーク) なため既定では走らせない:
///   cargo test --features semantic -- --ignored
/// 出力の厳密比較 (golden) は score の f32 演算がプラットフォーム間で揺れうるため行わず、
/// 構造 (順位・件数・メッセージ) を検証する。
#[cfg(feature = "semantic")]
#[test]
#[ignore = "モデル取得が必要 (HF cache かネットワーク)"]
fn semantic_e2e() {
    let root = temp_repo(
        "sem-e2e",
        &[
            ("mikke.toml", "[semantic]\nenabled = true\n"),
            (
                "robot.md",
                "---\ntitle: ロボットアームの制御\nsummary: PID ゲイン調整の記録\ntags: [robotics]\n---\n\n発振を抑えるためにゲインを調整した。\n",
            ),
            (
                "curry.md",
                "---\ntitle: カレーの作り方\nsummary: 夕食のレシピ\ntags: [cooking]\n---\n\n玉ねぎを飴色になるまで炒めてから煮込む。\n",
            ),
            (
                "trip.md",
                "---\ntitle: 旅行の計画\nsummary: 秋の旅程メモ\ntags: [travel]\n---\n\n海沿いの街を訪れて景色を眺める。\n",
            ),
        ],
    );

    // 1. embed: 全件新規
    let (stdout, stderr) = run(&root, &["embed"]);
    assert!(
        stdout.contains("ノート数: 3 (再利用: 0, 新規/更新: 3)"),
        "初回 embed の件数表示:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("埋め込みを保存しました"),
        "保存メッセージが無い:\n{stdout}\n{stderr}"
    );
    assert!(root
        .join(".mikke/embeddings/embeddings.safetensors")
        .exists());
    assert!(root.join(".mikke/embeddings/metadata.json").exists());

    // 2. semantic: 料理クエリで curry.md が最上位
    let (stdout, stderr) = run(&root, &["semantic", "夕食", "の", "料理", "--top", "2"]);
    let first_path = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("path:"))
        .unwrap_or("")
        .to_string();
    assert!(
        first_path.contains("curry.md"),
        "料理クエリの最上位が curry.md でない:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("[score: "), "score 表示が無い:\n{stdout}");

    // 3. hybrid: semantic ストリーム有効 (via bm25+vec)。
    // クエリ語は title/本文に実在するものを選ぶ (FTS は summary を見ないため、
    // summary にしか無い語だと BM25 ストリームが 0 件になり via vec のみになる)
    let (stdout, stderr) = run(&root, &["hybrid", "カレー", "玉ねぎ", "--top", "3"]);
    assert!(
        stdout.contains("BM25 + semantic"),
        "hybrid が semantic 有効になっていない:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("via bm25+vec"),
        "両ストリーム融合 (via bm25+vec) が無い:\n{stdout}"
    );

    // 4. 再実行: 差分なし
    let (stdout, _) = run(&root, &["embed"]);
    assert!(
        stdout.contains("すべてのノートが最新です"),
        "無差分メッセージが無い:\n{stdout}"
    );

    // 5. 1 件更新 → 再利用 2 / 新規 1
    fs::write(
        root.join("trip.md"),
        "---\ntitle: 旅行の計画\nsummary: 秋の旅程メモ\n---\n\n日程を 11 月下旬に変更。\n",
    )
    .unwrap();
    let (stdout, _) = run(&root, &["embed"]);
    assert!(
        stdout.contains("ノート数: 3 (再利用: 2, 新規/更新: 1)"),
        "差分更新の件数表示:\n{stdout}"
    );

    // 6. 1 件削除 → 「削除のみ」でも保存し直し、semantic 結果から消える
    fs::remove_file(root.join("curry.md")).unwrap();
    let (stdout, _) = run(&root, &["embed"]);
    assert!(
        stdout.contains("ノート数: 2 (再利用: 2, 新規/更新: 0)"),
        "削除のみ更新の件数表示:\n{stdout}"
    );
    let (_o, _e) = run(&root, &["index"]); // 削除を index にも反映
    let (stdout, _) = run(&root, &["semantic", "夕食", "の", "料理", "--top", "3"]);
    assert!(
        !stdout.contains("curry.md"),
        "削除ノートが semantic 結果に残っている:\n{stdout}"
    );

    // 7. passage_prefix 変更 → 全件再構築
    fs::write(
        root.join("mikke.toml"),
        "[semantic]\nenabled = true\npassage_prefix = \"doc: \"\n",
    )
    .unwrap();
    let (stdout, stderr) = run(&root, &["embed"]);
    assert!(
        stderr.contains("全件再構築します"),
        "prefix 変更で全再構築が告知されない:\n{stderr}"
    );
    assert!(
        stdout.contains("ノート数: 2 (再利用: 0, 新規/更新: 2)"),
        "prefix 変更後の件数表示:\n{stdout}"
    );

    // 8. 全ノート削除 → 埋め込みファイルごと削除 (旧ベクトルの残骸を残さない)
    fs::remove_file(root.join("robot.md")).unwrap();
    fs::remove_file(root.join("trip.md")).unwrap();
    let (stdout, _) = run(&root, &["embed"]);
    assert!(
        stdout.contains("既存の埋め込みを削除しました"),
        "全削除時に埋め込みが削除されない:\n{stdout}"
    );
    assert!(
        !root
            .join(".mikke/embeddings/embeddings.safetensors")
            .exists()
            && !root.join(".mikke/embeddings/metadata.json").exists(),
        "全削除後も埋め込みファイルが残っている"
    );

    let _ = fs::remove_dir_all(&root);
}

/// health --md-report の生成ファイルを golden と比較 (決定的・可搬リンク)。
#[test]
fn golden_md_report() {
    let root = temp_copy("mdreport");
    run(&root, &["index"]);
    let report = root.join("health-report.md");
    run(&root, &["health", "--md-report", report.to_str().unwrap()]);
    let got = fs::read_to_string(&report).unwrap();
    let expected = fs::read_to_string(golden_dir().join("health-report.md")).unwrap();
    let _ = fs::remove_dir_all(&root);
    assert_eq!(got, expected, "md-report mismatch");
}
