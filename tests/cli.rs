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

/// 壊れたネスト repo 設定は silent に親規則で走査せず、path 付きで exit 2 する。
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
    assert_eq!(
        out.status.code(),
        Some(2),
        "型エラーの子設定はエラー (exit 2) のはず:\n{stderr}"
    );
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

// --- [index] auto_rebuild ---

/// mtime を未来へ明示的に進める (index 生成時刻との前後関係を時刻粒度に依存させず flaky を防ぐ)。
fn bump_mtime(path: &Path, secs_ahead: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(secs_ahead))
        .unwrap();
}

/// auto_rebuild 有効時: 編集後の find が新内容を返し、告知は stderr のみ (stdout に混ぜない)。
#[test]
fn auto_rebuild_reflects_edit() {
    let root = temp_repo(
        "arb-edit",
        &[
            ("mikke.toml", "[index]\nauto_rebuild = true\n"),
            ("a.md", "# A\n\nquokka のメモ。\n"),
        ],
    );
    run(&root, &["index"]);
    // 未変更なら再構築は走らない
    let (stdout, stderr) = run(&root, &["find", "quokka"]);
    assert!(stdout.contains("a.md"), "初回検索でヒットしない:\n{stdout}");
    assert!(
        !stderr.contains("再構築"),
        "未変更なのに再構築された:\n{stderr}"
    );
    // 編集して mtime を index 生成時刻より確実に新しくする
    fs::write(root.join("a.md"), "# A\n\nwalrus のメモ。\n").unwrap();
    bump_mtime(&root.join("a.md"), 10);
    let (stdout, stderr) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("a.md"),
        "編集が検索に反映されていない:\n{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("再構築"),
        "再構築の告知が stderr に無い:\n{stderr}"
    );
    assert!(
        !stdout.contains("再構築"),
        "再構築の告知が stdout に混ざっている:\n{stdout}"
    );
}

/// auto_rebuild 有効時: mtime が保存されるリネームと削除も path 集合の変化で検知する。
#[test]
fn auto_rebuild_detects_rename_and_delete() {
    let root = temp_repo(
        "arb-move",
        &[
            ("mikke.toml", "[index]\nauto_rebuild = true\n"),
            ("a.md", "# A\n\nquokka のメモ。\n"),
            ("b.md", "# B\n\nquokka の別メモ。\n"),
        ],
    );
    run(&root, &["index"]);
    // rename は mtime を保存し、削除は mtime 判定では拾えない — path 集合比較の担当領域
    fs::rename(root.join("a.md"), root.join("c.md")).unwrap();
    fs::remove_file(root.join("b.md")).unwrap();
    let (stdout, stderr) = run(&root, &["find", "quokka"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("c.md"),
        "リネームが検索に反映されていない:\n{stdout}\n{stderr}"
    );
    assert!(
        !stdout.contains("a.md") && !stdout.contains("b.md"),
        "旧 path が結果に残っている:\n{stdout}"
    );
    assert!(
        stderr.contains("再構築"),
        "再構築の告知が stderr に無い:\n{stderr}"
    );
}

/// auto_rebuild 有効でも health は再構築せず、index 鮮度の診断がマスクされない。
#[test]
fn auto_rebuild_does_not_mask_health() {
    let root = temp_repo(
        "arb-health",
        &[
            ("mikke.toml", "[index]\nauto_rebuild = true\n"),
            ("a.md", "# A\n\nquokka のメモ。\n"),
        ],
    );
    run(&root, &["index"]);
    fs::write(root.join("a.md"), "# A\n\nwalrus のメモ。\n").unwrap();
    bump_mtime(&root.join("a.md"), 10);
    let (stdout, stderr) = run(&root, &["health"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("[index鮮度]"),
        "stale なのに index 鮮度の診断が出ない (health が再構築している?):\n{stdout}"
    );
    assert!(
        !stderr.contains("再構築しています"),
        "health で自動再構築が走っている:\n{stderr}"
    );
}

/// 既定 (auto_rebuild 無指定 = false) では従来どおり古い index の結果を返し、再構築しない。
#[test]
fn auto_rebuild_default_off_keeps_old_index() {
    let root = temp_repo(
        "arb-off",
        &[
            ("mikke.toml", "[scan]\n"),
            ("a.md", "# A\n\nquokka のメモ。\n"),
        ],
    );
    run(&root, &["index"]);
    fs::write(root.join("a.md"), "# A\n\nwalrus のメモ。\n").unwrap();
    bump_mtime(&root.join("a.md"), 10);
    let (old_hit, _) = run(&root, &["find", "quokka"]);
    let (new_hit, stderr) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        old_hit.contains("a.md"),
        "既定で古い index の結果が返らない (勝手に再構築された?):\n{old_hit}"
    );
    assert!(
        new_hit.contains("(0件"),
        "既定なのに新内容が反映されている:\n{new_hit}"
    );
    assert!(
        !stderr.contains("再構築"),
        "既定なのに再構築の告知が出ている:\n{stderr}"
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

/// semantic 無効 repo での embed は明示エラーで exit 2 (feature 有効ビルド)。
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
    assert_eq!(
        out.status.code(),
        Some(2),
        "semantic 無効 repo はエラー (exit 2) のはず:\n{stderr}"
    );
    assert!(
        stderr.contains("semantic が無効"),
        "無効メッセージが無い:\n{stderr}"
    );
}

/// slim ビルドの embed / semantic はビルド無効の明示エラーで exit 2 (silent 劣化させない)。
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
    assert_eq!(
        embed_out.status.code(),
        Some(2),
        "slim ビルドの embed はエラー (exit 2) のはず:\n{embed_err}"
    );
    assert!(
        embed_err.contains("このビルドは semantic 無効です"),
        "embed のビルド無効メッセージが無い:\n{embed_err}"
    );
    assert_eq!(
        semantic_out.status.code(),
        Some(2),
        "slim ビルドの semantic はエラー (exit 2) のはず:\n{semantic_err}"
    );
    assert!(
        semantic_err.contains("このビルドで無効です"),
        "semantic のビルド無効メッセージが無い:\n{semantic_err}"
    );
}

/// slim ビルドの semantic --json もビルド無効の明示エラー (stderr + exit 2) のままで、
/// stdout に部分的な JSON (メタ行等) を出さない (JSON モードの stdout 純度をエラー経路でも保つ)。
#[cfg(not(feature = "semantic"))]
#[test]
fn semantic_slim_json_errors_without_stdout() {
    let root = temp_repo(
        "sem-slim-json",
        &[("mikke.toml", "[scan]\n"), ("a.md", "# A\n\nメモ。\n")],
    );
    let out = run_raw(&root, &["semantic", "クエリ", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        out.status.code(),
        Some(2),
        "slim ビルドの semantic --json はエラー (exit 2) のはず:\n{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "エラー時の stdout に部分的な JSON が出ている:\n{stdout}"
    );
    assert!(
        stderr.contains("このビルドで無効です"),
        "ビルド無効メッセージが stderr に無い:\n{stderr}"
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

/// [health] disable でチェックを項目単位で無効化できる (frontmatter 無し md 運用)。
#[test]
fn health_disable_checks() {
    let root = temp_repo(
        "health-disable",
        &[
            (
                "mikke.toml",
                // 低ボリュームは min_words = 0 で発火させない (disable との併用例)
                "[health]\ndisable = [\"tags\", \"summary\", \"updated\"]\nmin_words = 0\n",
            ),
            (
                "notes/plain.md",
                "# Plain\n\nfrontmatter を持たない運用のノート。\n",
            ),
        ],
    );
    run(&root, &["index"]);
    let out = run_raw(&root, &["health"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(
        out.status.success(),
        "health が非 0 で終了した:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("問題のあるノートはありません。"),
        "disable が効いていない:\n{stdout}\n{stderr}"
    );
}

/// 単項目 disable の回帰: low_words だけを抑制し、他の品質チェックは生きている。
#[test]
fn health_disable_single_check() {
    let root = temp_repo(
        "health-disable-single",
        &[
            ("mikke.toml", "[health]\ndisable = [\"low_words\"]\n"),
            // タグなし・要約なし・低ボリュームを同時に満たす短いノート
            ("notes/short.md", "# Short\n\n短いメモ。\n"),
        ],
    );
    run(&root, &["index"]);
    let out = run_raw(&root, &["health"]);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(
        out.status.success(),
        "health が非 0 で終了した:\n{stdout}\n{stderr}"
    );
    assert!(
        !stdout.contains("低ボリューム"),
        "disable した low_words が報告されている:\n{stdout}"
    );
    assert!(
        stdout.contains("タグなし") && stdout.contains("要約なし"),
        "disable していないチェックまで消えている:\n{stdout}"
    );
}

/// --md-report でも disable 指定のチェックはレポートから消える。
#[test]
fn md_report_respects_disable() {
    let root = temp_repo(
        "health-disable-mdreport",
        &[
            ("mikke.toml", "[health]\ndisable = [\"low_words\"]\n"),
            ("notes/short.md", "# Short\n\n短いメモ。\n"),
        ],
    );
    run(&root, &["index"]);
    let report = root.join("health-report.md");
    run(&root, &["health", "--md-report", report.to_str().unwrap()]);
    let got = fs::read_to_string(&report).unwrap();
    let _ = fs::remove_dir_all(&root);
    assert!(
        !got.contains("低ボリューム"),
        "disable した low_words がレポートに残っている:\n{got}"
    );
    assert!(
        got.contains("タグなし") && got.contains("要約なし"),
        "disable していないチェックがレポートから消えている:\n{got}"
    );
}

/// [health] disable の未知チェック名は typo を silent no-op にせず非 0 で終了する。
#[test]
fn health_disable_unknown_name_errors() {
    let root = temp_repo(
        "health-disable-unknown",
        &[
            ("mikke.toml", "[health]\ndisable = [\"tag\"]\n"),
            ("notes/a.md", "# A\n\nメモ。\n"),
        ],
    );
    let out = run_raw(&root, &["health"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(!out.status.success(), "未知のチェック名で成功してしまった");
    assert!(
        stderr.contains("未知のチェック名") && stderr.contains("tag"),
        "未知チェック名のエラーメッセージが無い:\n{stderr}"
    );
}

/// disable = ["frontmatter"] は health の報告だけを止め、
/// index --check の破損検出 (非 0 終了) はゲートしない。
#[test]
fn index_check_ignores_frontmatter_disable() {
    let root = temp_repo(
        "health-disable-fm-check",
        &[
            ("mikke.toml", "[health]\ndisable = [\"frontmatter\"]\n"),
            // 閉じ --- が無い破損 frontmatter
            ("notes/broken.md", "---\ntitle: Broken\n\n# 本文\n"),
        ],
    );
    run(&root, &["index"]);
    let health = run_raw(&root, &["health"]);
    let health_stdout = String::from_utf8_lossy(&health.stdout).into_owned();
    let check = run_raw(&root, &["index", "--check"]);
    let check_stderr = String::from_utf8_lossy(&check.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(
        health.status.success(),
        "health が非 0 で終了した:\n{health_stdout}"
    );
    assert!(
        !health_stdout.contains("frontmatter 破損"),
        "disable しても health が破損を報告している:\n{health_stdout}"
    );
    assert!(
        !check.status.success(),
        "破損があるのに index --check が成功してしまった:\n{check_stderr}"
    );
    assert!(
        check_stderr.contains("frontmatter 破損"),
        "index --check の破損エラーメッセージが無い:\n{check_stderr}"
    );
}

/// disable = ["tags"] は health の空要素報告だけを止め、
/// index --check の空要素検出 (非 0 終了) はゲートしない。
#[test]
fn index_check_ignores_tags_disable() {
    let root = temp_repo(
        "health-disable-tags-check",
        &[
            ("mikke.toml", "[health]\ndisable = [\"tags\"]\n"),
            // issue #37 の最小再現: コメントだけの null 要素が 2 つ
            (
                "notes/hashtag.md",
                "---\ntitle: Hashtag\ntags:\n  - #Dog\n  - #Cat\n---\n\nbody\n",
            ),
        ],
    );
    run(&root, &["index"]);
    let health = run_raw(&root, &["health"]);
    let health_stdout = String::from_utf8_lossy(&health.stdout).into_owned();
    let check = run_raw(&root, &["index", "--check"]);
    let check_stderr = String::from_utf8_lossy(&check.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert!(
        health.status.success(),
        "health が非 0 で終了した:\n{health_stdout}"
    );
    assert!(
        !health_stdout.contains("tags の空要素"),
        "disable しても health が空要素を報告している:\n{health_stdout}"
    );
    assert_eq!(
        check.status.code(),
        Some(1),
        "空要素があるのに index --check が exit 1 でない:\n{check_stderr}"
    );
    assert!(
        check_stderr.contains("tags の空要素"),
        "index --check の空要素エラーメッセージが無い:\n{check_stderr}"
    );
}

// --- exit code (grep 慣習: ヒット=0 / 0件=1 / エラー=2 — docs/SPEC.md「exit code」) ---

/// 検索系はヒットで 0、0 件で 1。一覧系 (list-tags / recent) は 0 件も正常で 0。
#[test]
fn search_exit_codes() {
    let root = temp_repo(
        "exitcode",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/a.md",
                "---\ntitle: Hit ノート\ntags: [walrus]\n---\n\nquokka のメモ。\n",
            ),
            (
                "notes/b.md",
                "---\ntitle: Link 元ノート\n---\n\n[[a]] と未解決の [[gone]] へのリンク。\n",
            ),
        ],
    );
    for (args, expected, label) in [
        (&["find", "quokka"][..], 0, "find ヒット"),
        (&["find", "pangolin"][..], 1, "find 0 件"),
        (&["tag", "walrus"][..], 0, "tag ヒット"),
        // tag は 0 件専用文言の early-return 経路
        (&["tag", "pangolin"][..], 1, "tag 0 件"),
        (&["title", "Hit"][..], 0, "title ヒット"),
        (&["title", "pangolin"][..], 1, "title 0 件"),
        (&["hybrid", "quokka"][..], 0, "hybrid ヒット"),
        (&["hybrid", "pangolin"][..], 1, "hybrid 0 件"),
        // links は未解決 target も結果に数える (壊れたリンクとは限らない — docs/SPEC.md)
        (&["links", "notes/b.md"][..], 0, "links ヒット"),
        (&["links", "notes/a.md"][..], 1, "links 0 件"),
        (&["backlinks", "notes/a.md"][..], 0, "backlinks ヒット"),
        (&["backlinks", "notes/b.md"][..], 1, "backlinks 0 件"),
        (&["list-tags"][..], 0, "list-tags"),
        // date 付きノートが無く 0 件だが、状態報告として 0 (index 生存確認に使う)
        (&["recent", "5"][..], 0, "recent 0 件"),
    ] {
        let out = run_raw(&root, args);
        assert_eq!(
            out.status.code(),
            Some(expected),
            "{label} の exit code が {expected} でない:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// index --check の frontmatter 破損検出は「検出 = 結果」の exit 1、--check 無しは 0。
#[test]
fn index_check_exit_codes() {
    let root = temp_repo(
        "exitcode-chk",
        &[
            ("mikke.toml", "[scan]\n"),
            // 閉じ --- 欠落の frontmatter 破損
            ("bad.md", "---\ntitle: Bad\n\n本文。\n"),
        ],
    );
    let out = run_raw(&root, &["index", "--check"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "--check の破損検出は exit 1 のはず:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_raw(&root, &["index"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "--check 無しは破損があっても exit 0 のはず:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_dir_all(&root);
}

/// tags のコメントだけの要素・空文字列・重複を安全に扱い、各 CLI の契約を保つ。
#[test]
fn empty_and_duplicate_tags_are_reported_without_panicking() {
    let root = temp_repo(
        "empty-tags",
        &[
            ("mikke.toml", "[health]\nmin_words = 0\n"),
            (
                "notes/mixed.md",
                "---\ntitle: Mixed\nsummary: test\nupdated: 2026-08-23\ntags:\n  - #Dog\n  - valid\n  - #Cat\n  - valid\n  - \"#Dog\"\n  - \"\"\n  - other\n---\n\nbody\n",
            ),
            (
                "notes/duplicate.md",
                "---\ntitle: Duplicate\nsummary: test\nupdated: 2026-08-23\ntags: [alpha, beta, alpha]\n---\n\nbody\n",
            ),
            // 文字列 tags は 1 文字ずつ分解される歴史的 quirk。重複文字 (a) は
            // 重複除去が無いと tags PK 違反で panic する (issue #37 と同一機構)
            (
                "notes/string-quirk.md",
                "---\ntitle: String quirk\nsummary: test\nupdated: 2026-08-23\ntags: aba\n---\n\nbody\n",
            ),
        ],
    );

    let index = run_raw(&root, &["index"]);
    let index_stdout = String::from_utf8_lossy(&index.stdout);
    let index_stderr = String::from_utf8_lossy(&index.stderr);
    assert!(index.status.success(), "通常 index が失敗:\n{index_stderr}");
    assert!(
        index_stdout.contains("ノート数: 3"),
        "index が完走していない:\n{index_stdout}"
    );
    assert!(
        index_stderr.contains("notes/mixed.md"),
        "警告に path が無い:\n{index_stderr}"
    );
    assert!(
        index_stderr.contains("`- Dog` に修正"),
        "修正方法が無い:\n{index_stderr}"
    );
    assert!(
        !index_stderr.contains("notes/duplicate.md")
            && !index_stderr.contains("notes/string-quirk.md"),
        "重複だけを警告している:\n{index_stderr}"
    );

    let (quoted, quoted_stderr) = run(&root, &["tag", "#Dog"]);
    assert!(
        quoted.contains("notes/mixed.md"),
        "引用された #Dog が登録されていない:\n{quoted}\n{quoted_stderr}"
    );
    let (valid, _) = run(&root, &["tag", "valid"]);
    assert_eq!(
        valid.matches("notes/mixed.md").count(),
        1,
        "重複タグが残っている:\n{valid}"
    );
    let (chars, _) = run(&root, &["tag", "a"]);
    assert_eq!(
        chars.matches("notes/string-quirk.md").count(),
        1,
        "文字列 tags の重複文字が残っている:\n{chars}"
    );

    let health = run_raw(&root, &["health"]);
    let health_stdout = String::from_utf8_lossy(&health.stdout);
    assert!(health.status.success(), "health が非 0:\n{health_stdout}");
    assert!(
        health_stdout.contains("[tags の空要素 (1ファイル)]"),
        "health が問題を報告しない:\n{health_stdout}"
    );
    assert!(
        health_stdout.contains("notes/mixed.md"),
        "health に path が無い:\n{health_stdout}"
    );
    assert!(
        health_stdout.contains("`- Dog` に修正"),
        "health に修正方法が無い:\n{health_stdout}"
    );

    let check = run_raw(&root, &["index", "--check"]);
    let check_stderr = String::from_utf8_lossy(&check.stderr);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        check.status.code(),
        Some(1),
        "index --check が exit 1 でない:\n{check_stderr}"
    );
    assert!(
        check_stderr.contains("tags の空要素: 1ファイル"),
        "--check の検出結果が無い:\n{check_stderr}"
    );
}

/// 設定の型エラーは検索系でもエラーとして exit 2 (0 件の 1 と区別する)。
#[test]
fn config_error_exit_code() {
    let root = temp_repo(
        "exitcode-cfg",
        &[
            // include を文字列で書く型エラー (配列必須)
            ("mikke.toml", "[scan]\ninclude = \"notes\"\n"),
            ("notes/a.md", "# A\n\nquokka のメモ。\n"),
        ],
    );
    let out = run_raw(&root, &["find", "quokka"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        out.status.code(),
        Some(2),
        "壊れ設定はエラー (exit 2) のはず:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// --- links / backlinks (リンクグラフ — docs/SPEC.md「リンクグラフ」) ---

/// `<note>` 引数の複数一致は silent に 1 件を選ばず、候補を stderr へ列挙して exit 2。
#[test]
fn links_ambiguous_note_arg_errors() {
    let root = temp_repo(
        "links-ambig-arg",
        &[
            ("mikke.toml", "[scan]\n"),
            ("x/dup.md", "# X\n\n重複名ノートその 1。\n"),
            ("y/dup.md", "# Y\n\n重複名ノートその 2。\n"),
        ],
    );
    let out = run_raw(&root, &["links", "dup"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        out.status.code(),
        Some(2),
        "複数一致は入力エラー (exit 2) のはず:\n{stderr}"
    );
    assert!(
        stderr.contains("x/dup.md") && stderr.contains("y/dup.md"),
        "候補の path が stderr に列挙されていない:\n{stderr}"
    );
}

/// `<note>` が見つからない場合も入力エラーとして exit 2 (結果 0 件の 1 と区別)。
#[test]
fn links_unknown_note_errors() {
    let root = temp_repo(
        "links-unknown",
        &[
            ("mikke.toml", "[scan]\n"),
            ("notes/a.md", "# A\n\n通常ノート。\n"),
        ],
    );
    let out = run_raw(&root, &["links", "nosuch"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        out.status.code(),
        Some(2),
        "未知ノートは入力エラー (exit 2) のはず:\n{stderr}"
    );
    assert!(
        stderr.contains("見つかりません"),
        "未発見メッセージが無い:\n{stderr}"
    );
}

/// 複数ノートに一致する target は全件表示する (曖昧さを隠さない)。backlinks 側も
/// 「指しうる」で判定するため、同名ノートのどちらからも被リンクに挙がる。
#[test]
fn links_ambiguous_target_lists_all() {
    let root = temp_repo(
        "links-ambig-target",
        &[
            ("mikke.toml", "[scan]\n"),
            ("hub.md", "# ハブ\n\n同名ノートへの [[dup]] を張る。\n"),
            ("x/dup.md", "# X\n\n重複名ノートその 1。\n"),
            ("y/dup.md", "# Y\n\n重複名ノートその 2。\n"),
        ],
    );
    let (stdout, stderr) = run(&root, &["links", "hub.md"]);
    assert!(
        stdout.contains("x/dup.md") && stdout.contains("y/dup.md"),
        "複数一致 target の全件表示になっていない:\n{stdout}\n{stderr}"
    );
    let (stdout, stderr) = run(&root, &["backlinks", "x/dup.md"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("hub.md"),
        "曖昧 target でも被リンクに挙がるはず:\n{stdout}\n{stderr}"
    );
}

// --- --json (JSON Lines — docs/SPEC.md「出力フォーマット」) ---

/// --json は stdout の全行が JSON としてパース可能で、1 行目はコマンド名入りのメタ行。
#[test]
fn json_all_lines_parse() {
    let root = temp_copy("json");
    let (_o, _e) = run(&root, &["index"]);
    for (args, command) in [
        (&["find", "slam", "--json"][..], "find"),
        (&["tag", "robotics", "--json"][..], "tag"),
        (&["title", "メモ", "--json"][..], "title"),
        (&["hybrid", "ロボット", "制御", "--json"][..], "hybrid"),
        (&["list-tags", "--json"][..], "list-tags"),
        (&["recent", "5", "--json"][..], "recent"),
    ] {
        let (stdout, stderr) = run(&root, args);
        let lines: Vec<&str> = stdout.lines().collect();
        assert!(!lines.is_empty(), "{command}: 出力が空:\n{stderr}");
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
                panic!("{command}: JSON でない行 ({e}):\n{line}\nstderr:\n{stderr}")
            });
        }
        let meta: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(meta["type"], "meta", "{command}: 1 行目がメタ行でない");
        assert_eq!(
            meta["command"], command,
            "{command}: メタ行の command 不一致"
        );
        assert_eq!(
            meta["count"].as_u64().unwrap() as usize,
            lines.len() - 1,
            "{command}: メタ行の count と hit 行数の不一致:\n{stdout}"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// --json の 0 件時はメタ行 (count: 0) のみで、exit code は現行どおり 1。
/// tag は 0 件専用文言の early-return 経路、title は共通経路 — 各コマンドで対称に固定する。
#[test]
fn json_zero_hits_meta_only() {
    let root = temp_copy("json0");
    let (_o, _e) = run(&root, &["index"]);
    for (args, command) in [
        (&["find", "pangolin", "--json"][..], "find"),
        (&["tag", "pangolin", "--json"][..], "tag"),
        (&["title", "pangolin", "--json"][..], "title"),
    ] {
        let out = run_raw(&root, args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{command}: --json でも 0 件は exit 1 のはず"
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "{command}: 0 件時はメタ行のみのはず:\n{stdout}"
        );
        let meta: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(meta["type"], "meta", "{command}: 1 行目がメタ行でない");
        assert_eq!(
            meta["command"], command,
            "{command}: メタ行の command 不一致"
        );
        assert_eq!(meta["count"], 0, "{command}: count が 0 でない");
    }
    let _ = fs::remove_dir_all(&root);
}

/// index 未作成 repo での --json 初回実行 (auto-build 発動) でも stdout は JSON Lines のみで、
/// auto-build の告知は stderr に出る (SPEC「保証」: clone 直後の初回実行でも安全にパイプできる)。
#[test]
fn json_auto_build_stdout_stays_pure() {
    let root = temp_repo(
        "json-autobuild",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/a.md",
                "---\ntitle: Auto ノート\ntags: [quokka]\n---\n\nquokka のメモ。\n",
            ),
        ],
    );
    // index は事前に作らない — find --json 自体が auto-build を発動する
    let (stdout, stderr) = run(&root, &["find", "quokka", "--json"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stderr.contains("インデックスが無いため生成しています"),
        "auto-build 告知が stderr に無い (前提崩れ):\n{stderr}"
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty(), "出力が空:\n{stderr}");
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
            panic!("auto-build 時に stdout へ JSON でない行が混入 ({e}):\n{line}\nstdout 全体:\n{stdout}")
        });
    }
    let meta: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(meta["command"], "find", "1 行目がメタ行でない:\n{stdout}");
}

/// 複数行 summary もエスケープされて 1 件 1 行が保たれる (行指向パースが壊れない —
/// テキスト出力ではラベル無しの生テキスト行になる既知の弱点への対策)。
#[test]
fn json_multiline_summary_stays_single_line() {
    let root = temp_repo(
        "json-ml",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/a.md",
                "---\ntitle: 改行 summary\nsummary: |\n  1 行目\n  2 行目\ntags: [quokka]\n---\n\nquokka のメモ。\n",
            ),
        ],
    );
    let (stdout, stderr) = run(&root, &["find", "quokka", "--json"]);
    let _ = fs::remove_dir_all(&root);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines.len(),
        2,
        "メタ行 + hit 1 行のはず:\n{stdout}\n{stderr}"
    );
    let hit: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert!(
        hit["summary"].as_str().unwrap().contains('\n'),
        "summary の改行が保持されていない:\n{stdout}"
    );
}

/// パイプの早期終了 (`| head`) で panic (exit 101) しない — 読み手が降りただけでエラーではない
/// (docs/SPEC.md「exit code」)。text / --json / health の各 stdout 経路を横断で固定する。
/// SIGPIPE を持たない Windows は対象外。
#[cfg(unix)]
#[test]
fn broken_pipe_does_not_panic() {
    // head が降りた後も書き続ける量が要る。出力が Linux 既定の pipe バッファ (64KiB) に
    // 収まってしまうと書き手が書き切って正常終了でき、修正を外しても test が通ってしまう。
    // 600 ノート + 長めの title で全経路を 64KiB 超にする (実測: find 113KiB /
    // find --json 122KiB / recent 113KiB / health 84KiB)。
    // bm25_limit も上げて find のテキスト出力が打ち切られないようにする。
    let mut files: Vec<(String, String)> = vec![(
        "mikke.toml".to_string(),
        "[scan]\n[search]\nbm25_limit = 600\n".to_string(),
    )];
    for i in 0..600 {
        files.push((
            format!("notes/n{i}.md"),
            format!(
                "---\ntitle: パイプ早期終了の検証用ノート {i}\ndate: 2026-01-01\ntags: [quokka]\nsummary: パイプ早期終了の検証用に十分な長さを持たせた要約 {i}\n---\n\nquokka のメモ本文。\n"
            ),
        ));
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let root = temp_repo("broken-pipe", &refs);
    let (_o, _e) = run(&root, &["index"]);

    let mut failures = Vec::new();
    for args in ["find quokka", "find quokka --json", "recent 600", "health"] {
        // mikke 自身の終了ステータスを stderr 経由で拾う ($? はパイプ後段のものになるため)
        let script = format!(
            "{{ '{bin}' --root '{root}' {args}; echo \"mikke_status=$?\" >&2; }} | head -1 > /dev/null",
            bin = bin(),
            root = root.display(),
        );
        let out = Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("failed to run sh");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let status = stderr
            .lines()
            .find_map(|l| l.strip_prefix("mikke_status="))
            .map(|s| s.to_string())
            .unwrap_or_default();
        if stderr.contains("panicked") || status == "101" {
            failures.push(format!(
                "--- mikke {args} | head -1 (mikke_status={status}) ---\n{stderr}"
            ));
        }
        // パイプライン全体の exit code は後段 (head) のもの = 0
        if out.status.code() != Some(0) {
            failures.push(format!(
                "--- mikke {args} | head -1: パイプラインの exit code が {:?} ---\n{stderr}",
                out.status.code()
            ));
        }
    }
    let _ = fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "パイプ早期終了で panic した ({}件):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
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
