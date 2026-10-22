//! Golden 統合テスト — CLI 出力が tests/golden/ の期待出力と一致することを検証する。
//!
//! tests/fixture/ をテンポラリへ複製 → `mikke index` → 各コマンドを実行し、
//! stdout を tests/golden/<name>.txt と厳密比較する (対応表は manifest.tsv)。
//! 出力を変える変更は golden の意図的な更新を伴う (docs/SPEC.md「golden テスト」)。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

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

/// 和欧間スペースの有無を索引側で吸収し、原文テーブルと表示は維持する。
#[test]
fn japanese_spacing_searches_are_symmetric() {
    let root = temp_repo(
        "japanese-spacing",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/nospace.md",
                "---\ntitle: 比較用ノート\nsummary: 要約 は原文\ntags: [組版]\n---\n\nTypstは組版システムです。LaTeXよりも高速です。バージョンは0.13です。[[リンク 先]]\n",
            ),
            (
                "notes/withspace.md",
                "---\ntitle: スペース あり\nsummary: 空白 入りの要約\ntags: [組版]\n---\n\nTypst は組版システムです。LaTeX よりも高速です。バージョンは 0.13 です。\n",
            ),
        ],
    );
    run(&root, &["index"]);

    for query in ["Typstは", "Typst は", "LaTeXよりも", "バージョンは0.13"] {
        let args = ["find", query];
        let out = run_raw(&root, &args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success()
                && stdout.contains("notes/nospace.md")
                && stdout.contains("notes/withspace.md")
                && stdout.contains("結果 (2件"),
            "'{query}' が両方の表記に一致しない:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // 末尾空白付きは trim 後に正規化する (索引側で空白が消えているため trim しないと落ちる)。
    for query in [
        "スペースあり",
        "スペース あり",
        "スペース ",
        " スペース あり ",
    ] {
        let out = run_raw(&root, &["title", query]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success()
                && stdout.contains("結果 (1件)")
                && stdout.contains("スペース あり"),
            "title '{query}' が原文 title の 1 件に一致しない:\n{stdout}"
        );
    }

    let conn = Connection::open(root.join(".mikke/index.sqlite")).unwrap();
    let note: (String, String) = conn
        .query_row(
            "SELECT title, summary FROM notes WHERE path = 'notes/withspace.md'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let fts_title: String = conn
        .query_row(
            "SELECT title FROM notes_fts WHERE path = 'notes/withspace.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let link: String = conn
        .query_row(
            "SELECT target FROM links WHERE path = 'notes/nospace.md'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(note, ("スペース あり".into(), "空白 入りの要約".into()));
    assert_eq!(fts_title, "スペースあり");
    assert_eq!(link, "リンク 先");

    let _ = fs::remove_dir_all(&root);
}

/// find の空白は従来どおり AND であり、連続フレーズへ融合しない。
#[test]
fn find_preserves_and_semantics_and_english_spaces() {
    let root = temp_repo(
        "spacing-semantics",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/apart.md",
                "# 離れた語\n\nLaTeX の概要を説明する。別の段落では従来方式よりも高速と評価した。\n",
            ),
            (
                "notes/english.md",
                "# English\n\nhello world are two words.\n",
            ),
        ],
    );
    run(&root, &["index"]);

    let spaced = run_raw(&root, &["find", "LaTeX よりも高速"]);
    let compact = run_raw(&root, &["find", "LaTeXよりも高速"]);
    let english = run_raw(&root, &["find", "helloworld"]);
    assert!(
        spaced.status.success()
            && String::from_utf8_lossy(&spaced.stdout).contains("notes/apart.md"),
        "離れた 2 語の AND 検索がヒットしない:\n{}",
        String::from_utf8_lossy(&spaced.stdout)
    );
    assert_eq!(compact.status.code(), Some(1));
    assert_eq!(english.status.code(), Some(1));

    let _ = fs::remove_dir_all(&root);
}

/// tag は検索列とキーワードを正規化し、表示には原文を使う。
#[test]
fn tag_normalizes_stored_value_and_keyword() {
    let root = temp_repo(
        "tag-spacing",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/tag.md",
                "---\ntitle: Tag\ntags: [機械 学習]\n---\n\n本文。\n",
            ),
        ],
    );
    run(&root, &["index"]);
    for query in ["機械 学習", "機械学習"] {
        let out = run_raw(&root, &["tag", query]);
        assert!(out.status.success(), "tag '{query}' が失敗");
        assert!(String::from_utf8_lossy(&out.stdout).contains("notes/tag.md"));
    }
    let tags = run_raw(&root, &["list-tags"]);
    assert!(String::from_utf8_lossy(&tags.stdout).contains("機械 学習 (1件)"));

    let _ = fs::remove_dir_all(&root);
}

/// 空白だけの title 検索は全件でなく、空白を含む title だけに一致する。
#[test]
fn title_whitespace_keyword_is_not_trimmed_to_empty() {
    let root = temp_repo(
        "title-whitespace",
        &[
            ("mikke.toml", "[scan]\n"),
            ("notes/with-space.md", "# Hello World\n\n本文。\n"),
            ("notes/without-space.md", "# HelloWorld\n\n本文。\n"),
        ],
    );
    run(&root, &["index"]);
    let out = run_raw(&root, &["title", " "]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "空白だけの title 検索が失敗:\n{stdout}"
    );
    assert!(stdout.contains("結果 (1件)"));
    assert!(stdout.contains("notes/with-space.md"));
    assert!(!stdout.contains("notes/without-space.md"));

    let _ = fs::remove_dir_all(&root);
}

/// 検索結果の `path:` 行を並べて返す (0 件なら空)。
fn hit_paths(root: &Path, args: &[&str]) -> Vec<String> {
    let out = run_raw(root, args);
    let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("path: ").map(str::to_string))
        .collect();
    paths.sort();
    paths
}

/// LIKE の経路 (title / tag / 短語の find) でも、ASCII 以外の大文字小文字を同一視する (#69)。
#[test]
fn like_matches_non_ascii_case() {
    let root = temp_repo(
        "like-non-ascii-case",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/umlaut.md",
                "---\ntitle: Äpfel und Birnen\ndate: 2026-01-10\ntags: [Äpfel]\n---\nÄpfel sind rot.\n",
            ),
            (
                "notes/zen.md",
                "---\ntitle: ＧＰＳ首輪の記録\ndate: 2026-01-09\ntags: [ＧＰＳ]\n---\nキツネにＧＰＳ首輪を付けた。\n",
            ),
            (
                "notes/ascii.md",
                "---\ntitle: GPS collar log\ndate: 2026-01-08\ntags: [GPS]\n---\ngps collar on a fox.\n",
            ),
        ],
    );
    run(&root, &["index"]);
    let umlaut = vec!["notes/umlaut.md".to_string()];
    let zen = vec!["notes/zen.md".to_string()];
    let ascii = vec!["notes/ascii.md".to_string()];
    for args in [
        ["title", "Äpfel"],
        ["title", "äpfel"],
        ["tag", "Äpfel"],
        ["tag", "äpfel"],
        ["find", "Äp"],
        ["find", "äp"],
    ] {
        assert_eq!(hit_paths(&root, &args), umlaut, "{args:?}");
    }
    // 全角と半角は同一視しない
    for args in [
        ["title", "ＧＰＳ"],
        ["tag", "ＧＰＳ"],
        ["find", "ＧＰ"],
        ["find", "ｇｐ"],
    ] {
        assert_eq!(hit_paths(&root, &args), zen, "{args:?}");
    }
    for args in [["title", "GPS"], ["tag", "gps"], ["find", "Gp"]] {
        assert_eq!(hit_paths(&root, &args), ascii, "{args:?}");
    }

    let _ = fs::remove_dir_all(&root);
}

/// LIKE の経路で、キーワードの `%` `_` `\` はワイルドカードでなく文字そのものに当たる (#70)。
#[test]
fn like_escapes_wildcards() {
    let root = temp_repo(
        "like-escape",
        &[
            ("mikke.toml", "[scan]\n"),
            (
                "notes/pct.md",
                "---\ntitle: 100% の観察\ndate: 2026-01-07\ntags: [観察]\n---\n巣箱を 100% 見回った。\n",
            ),
            (
                "notes/thousand.md",
                "---\ntitle: 1000 羽の観察\ndate: 2026-01-06\ntags: [観察]\n---\nカモが 1000 羽いた。\n",
            ),
            (
                "notes/us.md",
                "---\ntitle: snake_case の名前\ndate: 2026-01-05\ntags: [a_b]\n---\nヘビの名前。\n",
            ),
            (
                "notes/usx.md",
                "---\ntitle: snakeXcase の名前\ndate: 2026-01-04\ntags: [aXb]\n---\nヘビの名前。\n",
            ),
            (
                "notes/bs.md",
                "---\ntitle: a\\b の巣\ndate: 2026-01-03\n---\nカラスの巣。\n",
            ),
            (
                "notes/nobs.md",
                "---\ntitle: ab の巣\ndate: 2026-01-02\n---\nカラスの巣。\n",
            ),
        ],
    );
    run(&root, &["index"]);
    let pct = vec!["notes/pct.md".to_string()];
    let us = vec!["notes/us.md".to_string()];
    for args in [["title", "100%"], ["find", "0%"]] {
        assert_eq!(hit_paths(&root, &args), pct, "{args:?}");
    }
    for args in [["title", "snake_case"], ["tag", "a_b"], ["find", "e_"]] {
        assert_eq!(hit_paths(&root, &args), us, "{args:?}");
    }
    assert_eq!(
        hit_paths(&root, &["title", "a\\b"]),
        vec!["notes/bs.md".to_string()]
    );
    // 記号を含まない語は今までどおり
    assert_eq!(
        hit_paths(&root, &["title", "の観察"]),
        vec!["notes/pct.md".to_string(), "notes/thousand.md".to_string()]
    );

    let _ = fs::remove_dir_all(&root);
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

/// [index] path が mikke の index でない既存ファイルを指す時、既定設定の検索は
/// 形式の確認でそのファイルを作り直さない (md も、meta の無い SQLite も残す)。
#[test]
fn format_check_does_not_rebuild_over_non_index_file() {
    for (tag, sqlite) in [("md", false), ("sqlite", true)] {
        let root = temp_repo(
            &format!("format-check-non-index-{tag}"),
            &[
                ("mikke.toml", "[index]\npath = \"keep/target.md\"\n"),
                ("keep/target.md", "# 大事\n\n消えては困る。\n"),
                ("notes/other.md", "# 別\n\nキリン。\n"),
            ],
        );
        let target = root.join("keep/target.md");
        if sqlite {
            fs::remove_file(&target).unwrap();
            let conn = Connection::open(&target).unwrap();
            conn.execute("CREATE TABLE other (x TEXT)", []).unwrap();
            drop(conn);
        }
        let before = fs::read(&target).unwrap();

        let out = run_raw(&root, &["find", "キリン"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("再構築"),
            "{tag}: 索引でないファイルを作り直そうとした:\n{stderr}"
        );
        assert_eq!(
            fs::read(&target).unwrap(),
            before,
            "{tag}: [index] path の既存ファイルが書き換わった"
        );
        let _ = fs::remove_dir_all(&root);
    }
}

/// index_format が無い旧索引は auto_rebuild 未設定でも再構築する。
#[test]
fn old_index_format_rebuilds_before_title_search() {
    let root = temp_repo(
        "old-format-title",
        &[
            ("mikke.toml", "[scan]\n"),
            ("a.md", "# ROS 2 入門\n\n本文。\n"),
        ],
    );
    run(&root, &["index"]);

    let conn = Connection::open(root.join(".mikke/index.sqlite")).unwrap();
    conn.execute("DELETE FROM meta WHERE key = 'index_format'", [])
        .unwrap();
    // 旧形式相当: notes_fts.title を未正規化の原文に戻す
    conn.execute("DELETE FROM notes_fts", []).unwrap();
    conn.execute(
        "INSERT INTO notes_fts (path, title, content) VALUES ('a.md', 'ROS 2 入門', '本文。')",
        [],
    )
    .unwrap();
    drop(conn);

    let out = run_raw(&root, &["title", "ROS 2 入門"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "旧索引の title 検索:\n{stdout}\n{stderr}"
    );
    assert!(stdout.contains("結果 (1件)"));
    assert!(stdout.contains("ROS 2 入門"));
    assert!(
        stderr.contains("索引の形式が変わったため"),
        "再構築告知:\n{stderr}"
    );

    let conn = Connection::open(root.join(".mikke/index.sqlite")).unwrap();
    let format: String = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'index_format'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(format, "2");
    let _ = fs::remove_dir_all(&root);
}

// --- index の作り直しの安全性 (#51 / #52 / #53) ---
//
// 名前は index_safety_ で始める (Windows の CI がこれで絞る)。

/// stdout / stderr を pipe にして mikke を起動する (並列実行の検証用)。
fn spawn(root: &Path, args: &[&str]) -> std::process::Child {
    Command::new(bin())
        .arg("--root")
        .arg(root)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn mikke")
}

fn index_sql(root: &Path, sql: &str) {
    let conn = Connection::open(root.join(".mikke/index.sqlite")).unwrap();
    conn.execute_batch(sql).unwrap();
}

fn meta_value(path: &Path, key: &str) -> Option<String> {
    let conn = Connection::open(path).unwrap();
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .ok()
}

/// 同じ語を本文に持つノートを n 件作る。
fn notes_with(word: &str, n: usize) -> Vec<(String, String)> {
    (0..n)
        .map(|i| {
            (
                format!("notes/n{i:04}.md"),
                format!("---\ntitle: note {i}\n---\n\n{word} の観察記録。\n"),
            )
        })
        .collect()
}

fn repo_with(tag: &str, config: &str, notes: &[(String, String)]) -> PathBuf {
    let mut files: Vec<(&str, &str)> = vec![("mikke.toml", config)];
    files.extend(notes.iter().map(|(p, c)| (p.as_str(), c.as_str())));
    temp_repo(tag, &files)
}

fn count_lines(text: &str, needle: &str) -> usize {
    text.lines().filter(|l| l.contains(needle)).count()
}

/// dir に名前が `.tmp-` を含む entry があるか。
fn has_tmp(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".tmp-"))
    })
}

/// auto_rebuild = true の find を並列に流しても panic せず、作り直しは各回 1 本 (#51 A-1)。
#[test]
fn index_safety_parallel_auto_rebuild_rebuilds_once() {
    let root = repo_with(
        "safety-par-arb",
        "[index]\nauto_rebuild = true\n",
        &notes_with("ペンギン", 40),
    );
    run(&root, &["index"]);
    for round in 1..=3 {
        // 未来の mtime だと作り直した後も stale のまま残るので、generated を戻して stale にする
        index_sql(&root, "UPDATE meta SET value = '0' WHERE key = 'generated'");
        let children: Vec<_> = (0..4)
            .map(|_| spawn(&root, &["find", "ペンギン"]))
            .collect();
        let mut all_stderr = String::new();
        for child in children {
            let out = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && stdout.contains("(40件"),
                "round {round}: {:?}\n{stdout}\n{stderr}",
                out.status
            );
            assert!(!stderr.contains("panicked"), "round {round}:\n{stderr}");
            all_stderr.push_str(&stderr);
        }
        assert_eq!(
            count_lines(&all_stderr, "再構築しています"),
            1,
            "round {round}: 作り直しの告知が 1 本でない:\n{all_stderr}"
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// 旧形式 (index_format 無し) から上げた直後の既定設定の並列 find でも、作り直しは 1 本。
#[test]
fn index_safety_old_format_parallel_rebuilds_once() {
    let root = repo_with("safety-par-format", "[scan]\n", &notes_with("キリン", 200));
    run(&root, &["index"]);
    index_sql(&root, "DELETE FROM meta WHERE key = 'index_format'");
    let children: Vec<_> = (0..4).map(|_| spawn(&root, &["find", "キリン"])).collect();
    let mut all_stderr = String::new();
    for child in children {
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{:?}\n{stderr}", out.status);
        all_stderr.push_str(&stderr);
    }
    assert_eq!(
        count_lines(&all_stderr, "索引の形式が変わったため"),
        1,
        "{all_stderr}"
    );
    let _ = fs::remove_dir_all(&root);
}

/// 作り直しはロックを待つ。読むだけの検索はロックを待たない (#51 A-2)。
#[test]
fn index_safety_waits_for_lock() {
    use std::io::{BufRead, BufReader};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let root = repo_with("safety-lock", "[scan]\n", &notes_with("カワウソ", 3));
    fs::create_dir_all(root.join(".mikke")).unwrap();
    let index = root.join(".mikke/index.sqlite");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".mikke/index.sqlite.lock"))
        .unwrap();
    lock.lock().unwrap();

    // 前半: index が無い時の find は、ロックが外れるまで作らずに待つ
    let mut child = spawn(&root, &["find", "カワウソ"]);
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap_or_default());
        }
    });
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !seen.iter().any(|l: &String| l.contains("待っています")) {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(line) => seen.push(line),
            Err(_) => panic!("待つ告知が出ない: {seen:?}"),
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        child.try_wait().unwrap().is_none(),
        "ロックを待たずに終わった"
    );
    assert!(!index.exists(), "ロックを待つ間に index ができた");
    lock.unlock().unwrap();
    let out = child.wait_with_output().unwrap();
    reader.join().unwrap();
    seen.extend(rx.try_iter());
    let all = seen.join("\n");
    assert!(
        out.status.success(),
        "{:?}\n{}\n{all}",
        out.status,
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(count_lines(&all, "待っています"), 1, "{all}");
    assert_eq!(count_lines(&all, "インデックスが無いため"), 1, "{all}");

    // 後半: 完成した index がある時、既定の find はロックを取らない
    lock.lock().unwrap();
    let mut child = spawn(&root, &["find", "カワウソ"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("読むだけの find がロックを待った");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    lock.unlock().unwrap();
    assert!(status.success(), "{status:?}");
    drop(lock);
    let _ = fs::remove_dir_all(&root);
}

/// generated の無い index (生成が完了していない) は、既定の find が作り直す。health は作り直さない。
#[test]
fn index_safety_incomplete_index_rebuilds_by_default() {
    let root = repo_with("safety-incomplete", "[scan]\n", &notes_with("ラッコ", 3));
    let index = root.join(".mikke/index.sqlite");
    run(&root, &["index"]);

    // (a) generated だけ無い
    index_sql(&root, "DELETE FROM meta WHERE key = 'generated'");
    let out = run_raw(&root, &["health"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{:?}\n{stderr}", out.status);
    assert!(stdout.contains("生成が完了していない"), "{stdout}");
    assert!(!stdout.contains("旧形式"), "{stdout}");
    assert!(!stderr.contains("再構築"), "health が作り直した:\n{stderr}");
    assert!(meta_value(&index, "generated").is_none());

    let out = run_raw(&root, &["find", "ラッコ"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("(3件"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("前回の index 生成が完了していないため"),
        "{stderr}"
    );
    assert!(!stdout.contains("前回の index 生成"), "{stdout}");
    assert!(meta_value(&index, "generated").is_some());

    // (b) meta の行が全部無い (v0.3.1 で SCHEMA の後に止まった跡)
    index_sql(&root, "DELETE FROM meta");
    let out = run_raw(&root, &["find", "ラッコ"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("(3件"),
        "{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("前回の index 生成が完了していないため"),
        "{stderr}"
    );
    let _ = fs::remove_dir_all(&root);
}

const NOTES_ONLY_DDL: &str = "CREATE TABLE notes (path TEXT PRIMARY KEY, title TEXT NOT NULL, date TEXT, updated TEXT, summary TEXT, word_count INTEGER)";

/// 作りかけの index (notes だけの空の DB・0 バイト) は、既定では exit 2 で mikke index を案内する。
#[test]
fn index_safety_residue_index_exits_2() {
    for kind in ["notes-only", "empty"] {
        let root = repo_with(
            &format!("safety-residue-{kind}"),
            "[scan]\n",
            &notes_with("ビーバー", 3),
        );
        let index = root.join(".mikke/index.sqlite");
        let place = || {
            fs::create_dir_all(index.parent().unwrap()).unwrap();
            let _ = fs::remove_file(&index);
            if kind == "notes-only" {
                Connection::open(&index)
                    .unwrap()
                    .execute_batch(NOTES_ONLY_DDL)
                    .unwrap();
            } else {
                fs::write(&index, b"").unwrap();
            }
            fs::read(&index).unwrap()
        };
        let before = place();
        for args in [&["find", "ビーバー"][..], &["tag", "x"], &["health"]] {
            let out = run_raw(&root, args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(2), "{kind} {args:?}:\n{stderr}");
            assert!(stderr.contains("mikke index"), "{kind} {args:?}:\n{stderr}");
            assert!(!stderr.contains("panicked"), "{kind} {args:?}:\n{stderr}");
            assert_eq!(fs::read(&index).unwrap(), before, "{kind} {args:?}");
            assert!(
                !root.join(".mikke/index.sqlite.lock").exists(),
                "{kind} {args:?}: lock が作られた"
            );
        }

        fs::write(root.join("mikke.toml"), "[index]\nauto_rebuild = true\n").unwrap();
        let out = run_raw(&root, &["find", "ビーバー"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stdout.contains("(3件"),
            "{kind}: {stdout}\n{stderr}"
        );
        assert!(stderr.contains("途中で止まっている"), "{kind}: {stderr}");

        fs::write(root.join("mikke.toml"), "[scan]\n").unwrap();
        place();
        let out = run_raw(&root, &["index"]);
        assert!(
            out.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = fs::remove_dir_all(&root);
    }
}

/// 表の欠けた index: health は exit 2、検索は作り直す。
#[test]
fn index_safety_broken_index_rebuilds_on_search() {
    let root = repo_with("safety-broken", "[scan]\n", &notes_with("アザラシ", 3));
    run(&root, &["index"]);
    index_sql(&root, "DROP TABLE tags");
    let out = run_raw(&root, &["health"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("必要なテーブル"), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");

    let out = run_raw(&root, &["find", "アザラシ"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("(3件"),
        "{stdout}\n{stderr}"
    );
    assert!(stderr.contains("壊れているため"), "{stderr}");
    let _ = fs::remove_dir_all(&root);
}

/// 走査中の設定エラーで止まった build は、元の index を残す (#51 B-3)。
#[test]
fn index_safety_failed_build_keeps_previous_index() {
    let root = temp_repo(
        "safety-cfgerr",
        &[
            ("mikke.toml", "[scan]\ninclude = [\".\"]\n"),
            ("notes/alpha.md", "---\ntitle: Alpha\n---\nbanana alpha\n"),
            ("work/beta.md", "---\ntitle: Beta\n---\nbanana beta\n"),
            (
                "work/sub/gamma.md",
                "---\ntitle: Gamma\n---\nbanana gamma\n",
            ),
        ],
    );
    let index = root.join(".mikke/index.sqlite");
    run(&root, &["index"]);
    let before = fs::read(&index).unwrap();
    fs::write(root.join("work/sub/.mikke.toml"), "[scan\n").unwrap();

    let out = run_raw(&root, &["index"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(&index).unwrap(), before, "元の index が変わった");
    assert!(!root.join(".mikke/index.sqlite-journal").exists());

    let out = run_raw(&root, &["find", "banana"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("(3件"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_dir_all(&root);
}

/// 前回の build が残した一時ファイルは、次の build が消す。
/// `<name>.tmp-` の後が数字でない利用者のファイルは消さない。
#[test]
fn index_safety_leftover_tmp_is_removed() {
    let root = repo_with("safety-leftover", "[scan]\n", &notes_with("イルカ", 2));
    let tmp = root.join(".mikke/index.sqlite.tmp-99999");
    let tmp_journal = root.join(".mikke/index.sqlite.tmp-99999-journal");
    fs::create_dir_all(root.join(".mikke")).unwrap();
    fs::write(&tmp, "ごみ").unwrap();
    fs::write(&tmp_journal, "ごみ").unwrap();
    let keep: Vec<PathBuf> = [
        "index.sqlite.tmp-backup",
        "index.sqlite.tmp-2026-10-01.bak",
        "index.sqlite.tmp-mine.txt",
        "index.sqlite.tmp-",
        "index.sqlite.tmp--journal",
        "index.sqlite.tmp-12x",
    ]
    .iter()
    .map(|n| root.join(".mikke").join(n))
    .collect();
    for p in &keep {
        fs::write(p, "keep").unwrap();
    }

    let out = run_raw(&root, &["index"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !tmp.exists() && !tmp_journal.exists(),
        "一時ファイルが残った"
    );
    for p in &keep {
        assert_eq!(
            fs::read_to_string(p).ok().as_deref(),
            Some("keep"),
            "{} が消えた",
            p.display()
        );
    }
    let out = run_raw(&root, &["find", "イルカ"]);
    assert!(out.status.success());
    let _ = fs::remove_dir_all(&root);
}

/// 読み手が index を開いている間の作り直し。Unix では差し替えが成功し、読み手は旧い index を読み続ける。
/// Windows では差し替えられず、直前の index を残して exit 2 になる。
#[test]
fn index_safety_reader_open_during_rebuild() {
    let root = repo_with("safety-reader", "[scan]\n", &notes_with("トナカイ", 3));
    let index = root.join(".mikke/index.sqlite");
    run(&root, &["index"]);
    fs::write(
        root.join("notes/new.md"),
        "---\ntitle: new\n---\n\nヘラジカ の観察記録。\n",
    )
    .unwrap();

    let reader = Connection::open(&index).unwrap();
    let count = |c: &Connection| -> i64 {
        c.query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(&reader), 3);
    #[cfg(windows)]
    let before = fs::read(&index).unwrap();

    let out = run_raw(&root, &["index"]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    #[cfg(unix)]
    {
        assert!(out.status.success(), "{stderr}");
        assert_eq!(count(&reader), 3, "読み手が旧い index を読めない");
        drop(reader);
        let out = run_raw(&root, &["find", "ヘラジカ"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[cfg(windows)]
    {
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("差し替えられません"), "{stderr}");
        assert!(stderr.contains("差し替えを待っています"), "{stderr}");
        assert_eq!(fs::read(&index).unwrap(), before);
        assert!(!has_tmp(&root.join(".mikke")), "一時ファイルが残った");
        drop(reader);
        let out = run_raw(&root, &["index"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = fs::remove_dir_all(&root);
}

/// build を kill しても、最終 path に作りかけの index は出ない (#51 B-1 / B-2、best-effort)。
#[cfg(unix)]
#[test]
fn index_safety_killed_build_never_exposes_partial_index() {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, Instant};

    let n = 1500;
    let root = repo_with(
        "safety-kill",
        "[search]\nbm25_limit = 5000\n",
        &notes_with("ペリカン", n),
    );
    let dir = root.join(".mikke");
    let index = dir.join("index.sqlite");
    run(&root, &["index"]);
    let ino = fs::metadata(&index).unwrap().ino();

    // 書き始めの合図は実装に依らないものにする: 一時ファイルか journal ができた、
    // または最終 path の index が消えた・置き換わった
    let started = || {
        has_tmp(&dir)
            || dir.join("index.sqlite-journal").exists()
            || fs::metadata(&index).map_or(true, |m| m.ino() != ino)
    };
    // 書き始めてすぐと、少し書き進めた後の 2 回止める
    for delay in [0, 50] {
        let mut child = spawn(&root, &["index"]);
        let tmp = dir.join(format!("index.sqlite.tmp-{}", child.id()));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if child.try_wait().unwrap().is_some() || Instant::now() > deadline {
                break; // 間に合わず build が終わってもよい
            }
            if started() {
                std::thread::sleep(Duration::from_millis(delay));
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let _ = child.wait();

        // 検索の自動 build で隠れないよう、最終 path の index を先に直接読む
        assert!(index.exists(), "最終 path の index が消えた");
        let conn = Connection::open(&index).unwrap();
        let notes: i64 = conn
            .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
            .unwrap_or(-1);
        drop(conn);
        assert_eq!(notes, n as i64, "最終 path に作りかけの index が出た");
        assert!(meta_value(&index, "generated").is_some());

        // 一時ファイルも 1 transaction なので、途中までの行は確定していない (#52)。
        // 読み書きで開くと hot journal が巻き戻る。止める前に終わって無い回は見ない
        if tmp.exists() {
            let conn = Connection::open(&tmp).unwrap();
            let rows: i64 = conn
                .query_row("SELECT count(*) FROM notes", [], |r| r.get(0))
                .unwrap_or(0);
            drop(conn);
            assert_eq!(
                rows, 0,
                "delay={delay}: 一時ファイルに途中までの行が確定した"
            );
        }
    }

    let out = run_raw(&root, &["find", "ペリカン", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains(&format!("\"count\":{n},")),
        "{}\n{}",
        stdout.lines().next().unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(meta_value(&index, "generated").is_some());
    let _ = fs::remove_dir_all(&root);
}

/// [index] path が .md で終わる時は、ファイルの有無・大文字小文字によらず設定エラー (#53)。
#[test]
fn index_safety_md_index_path_is_rejected() {
    for (tag, path) in [
        ("existing", "notes/important.md"),
        ("missing", "top.md"),
        ("upper", "x/IDX.MD"),
    ] {
        let root = temp_repo(
            &format!("safety-md-{tag}"),
            &[
                ("mikke.toml", ""),
                (
                    "notes/important.md",
                    "---\ntitle: 大事なメモ\n---\nペンギンの観察記録。消えては困る。\n",
                ),
                (
                    "notes/other.md",
                    "---\ntitle: 別のメモ\n---\nキリンの観察記録。\n",
                ),
            ],
        );
        let important = root.join("notes/important.md");
        let before = fs::read(&important).unwrap();
        for (auto, args) in [
            (false, &["index"][..]),
            (false, &["find", "キリン"]),
            (true, &["find", "キリン"]),
            (false, &["health"]),
        ] {
            let mut config = format!("[index]\npath = \"{path}\"\n");
            if auto {
                config.push_str("auto_rebuild = true\n");
            }
            fs::write(root.join("mikke.toml"), config).unwrap();
            let out = run_raw(&root, args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(2), "{path} {args:?}:\n{stderr}");
            assert!(
                stderr.contains("[index] path") && stderr.contains(path),
                "{path} {args:?}:\n{stderr}"
            );
            assert_eq!(fs::read(&important).unwrap(), before, "{path} {args:?}");
            assert!(!root.join("top.md").exists(), "{path} {args:?}");
            assert!(!root.join("x").exists(), "{path} {args:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }
}

/// [index] path が mikke の index でない既存ファイルを指す時は、どの経路でも消さずに exit 2 (#53)。
#[test]
fn index_safety_non_index_file_is_not_replaced() {
    // (tag, [index] path, 用意する中身。None はそのまま使う)
    let cases: [(&str, &str, Option<&str>); 6] = [
        ("text", "keep/target.db", Some("text")),
        ("other", "keep/target.db", Some("CREATE TABLE other (x TEXT);")),
        (
            "meta-only",
            "keep/target.db",
            Some("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT); INSERT INTO meta VALUES ('generated', '1.0');"),
        ),
        (
            "meta-other",
            "keep/target.db",
            Some("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT); CREATE TABLE other (x TEXT);"),
        ),
        ("config", "mikke.toml", None),
        ("dir", "keep", None),
    ];
    for (tag, path, content) in cases {
        let root = temp_repo(
            &format!("safety-foreign-{tag}"),
            &[
                ("mikke.toml", ""),
                ("keep/readme.txt", "フォルダの中身\n"),
                (
                    "notes/other.md",
                    "---\ntitle: 別のメモ\n---\nキリンの観察記録。\n",
                ),
            ],
        );
        let target = root.join(path);
        match content {
            Some("text") => fs::write(&target, "消えては困る。\n").unwrap(),
            Some(sql) => Connection::open(&target)
                .unwrap()
                .execute_batch(sql)
                .unwrap(),
            None => {}
        }
        let parent = target.parent().unwrap().to_path_buf();
        let lock = parent.join(format!(
            "{}.lock",
            target.file_name().unwrap().to_string_lossy()
        ));
        for auto in [false, true] {
            let mut config = format!("[index]\npath = \"{path}\"\n");
            if auto {
                config.push_str("auto_rebuild = true\n");
            }
            fs::write(root.join("mikke.toml"), config).unwrap();
            let snapshot = |p: &Path| -> Vec<u8> {
                if p.is_dir() {
                    fs::read(p.join("readme.txt")).unwrap()
                } else {
                    fs::read(p).unwrap()
                }
            };
            let before = snapshot(&target);
            let commands: Vec<&[&str]> = if auto {
                vec![&["find", "キリン"]]
            } else {
                vec![&["index"], &["find", "キリン"], &["health"]]
            };
            for args in commands {
                let out = run_raw(&root, args);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let ctx = format!("{tag} auto={auto} {args:?}");
                assert_eq!(out.status.code(), Some(2), "{ctx}:\n{stderr}");
                assert!(!stderr.contains("panicked"), "{ctx}:\n{stderr}");
                assert!(
                    // Windows では path の区切りが `\` で出る
                    stderr.replace('\\', "/").contains(path)
                        && stderr.contains("mikke の index でない"),
                    "{ctx}:\n{stderr}"
                );
                assert_eq!(snapshot(&target), before, "{ctx}: 中身が変わった");
                assert!(!lock.exists(), "{ctx}: lock が作られた");
                assert!(!has_tmp(&parent), "{ctx}: 一時ファイルが作られた");
            }
        }
        let _ = fs::remove_dir_all(&root);
    }
}

/// SQLite のファイルと付随ファイルを、書き手が落ちた時の形で dst へ写す。
/// 書き手の接続は開いたまま写すので、写した側には誰も lock を持たない。
fn crashed_sqlite(src: &Path, dst: &Path, setup: &str, pending: &str, sidecar: &str) {
    let conn = Connection::open(src).unwrap();
    conn.execute_batch(setup).unwrap();
    conn.execute_batch(pending).unwrap();
    fs::create_dir_all(dst.parent().unwrap()).unwrap();
    fs::copy(src, dst).unwrap();
    let side = |p: &Path| PathBuf::from(format!("{}{sidecar}", p.display()));
    fs::copy(side(src), side(dst)).unwrap();
    assert!(fs::metadata(side(dst)).unwrap().len() > 0, "{sidecar} が空");
    drop(conn);
}

/// dir の中の名前とバイト列。
fn dir_bytes(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

/// 書きかけの WAL や hot journal を持つ索引でない SQLite を、判定のために書き換えない (#53)。
#[test]
fn index_safety_crashed_foreign_sqlite_is_untouched() {
    let fill = "CREATE TABLE other (x TEXT); WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM c WHERE i < 2000) INSERT INTO other SELECT printf('%0200d', i) FROM c;";
    let cases = [
        (
            "wal",
            "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;",
            fill,
            "-wal",
        ),
        (
            "hot",
            fill,
            "PRAGMA cache_size = 2; BEGIN; UPDATE other SET x = 'b';",
            "-journal",
        ),
    ];
    for (tag, setup, pending, sidecar) in cases {
        let root = temp_repo(
            &format!("safety-crashed-{tag}"),
            &[
                ("mikke.toml", ""),
                (
                    "notes/other.md",
                    "---\ntitle: 別のメモ\n---\nキリンの観察記録。\n",
                ),
            ],
        );
        let keep = root.join("keep");
        crashed_sqlite(
            &root.join("src.db"),
            &keep.join("target.db"),
            setup,
            pending,
            sidecar,
        );
        let before = dir_bytes(&keep);
        for auto in [false, true] {
            let mut config = "[index]\npath = \"keep/target.db\"\n".to_string();
            if auto {
                config.push_str("auto_rebuild = true\n");
            }
            fs::write(root.join("mikke.toml"), config).unwrap();
            let commands: Vec<&[&str]> = if auto {
                vec![&["find", "キリン"]]
            } else {
                vec![&["index"], &["find", "キリン"], &["health"]]
            };
            for args in commands {
                let out = run_raw(&root, args);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let ctx = format!("{tag} auto={auto} {args:?}");
                assert_eq!(out.status.code(), Some(2), "{ctx}:\n{stderr}");
                assert!(
                    stderr.replace('\\', "/").contains("keep/target.db"),
                    "{ctx}:\n{stderr}"
                );
                assert!(
                    stderr.contains("ファイルは変更していません"),
                    "{ctx}:\n{stderr}"
                );
                assert!(dir_bytes(&keep) == before, "{ctx}: ファイルが変わった");
            }
        }
        let _ = fs::remove_dir_all(&root);
    }
}

/// 旧版が最終 path に残した hot journal は、mikke の index なら巻き戻して読む。
#[test]
fn index_safety_old_hot_journal_on_index_is_rolled_back() {
    let root = repo_with(
        "safety-old-hot",
        "[search]\nbm25_limit = 5000\n",
        &notes_with("ペリカン", 300),
    );
    run(&root, &["index"]);
    let index = root.join(".mikke/index.sqlite");
    let built = root.join("built.sqlite");
    fs::rename(&index, &built).unwrap();
    crashed_sqlite(
        &built,
        &index,
        "",
        "PRAGMA cache_size = 2; BEGIN; DELETE FROM notes; DELETE FROM notes_fts;",
        "-journal",
    );
    let out = run_raw(&root, &["find", "ペリカン", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("\"count\":300,"),
        "{}\n{}",
        stdout.lines().next().unwrap_or_default(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!root.join(".mikke/index.sqlite-journal").exists());
    let _ = fs::remove_dir_all(&root);
}

/// root の外の絶対パス: 既存のファイルは守り、まだ無いファイルには作る (#53)。
#[test]
fn index_safety_outside_root() {
    let root = temp_repo(
        "safety-outside",
        &[
            ("mikke.toml", ""),
            (
                "notes/other.md",
                "---\ntitle: 別のメモ\n---\nキリンの観察記録。\n",
            ),
        ],
    );
    let outside = temp_repo(
        "safety-outside-ext",
        &[("precious.txt", "消えては困る。\n")],
    );
    let precious = outside.join("precious.txt");
    let before = fs::read(&precious).unwrap();
    // TOML の literal string にして、Windows の \ をそのまま渡す
    let config = |p: &Path, auto: bool| {
        let mut c = format!("[index]\npath = '{}'\n", p.display());
        if auto {
            c.push_str("auto_rebuild = true\n");
        }
        c
    };
    for (auto, args) in [(false, &["index"][..]), (true, &["find", "キリン"])] {
        fs::write(root.join("mikke.toml"), config(&precious, auto)).unwrap();
        let out = run_raw(&root, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}:\n{stderr}");
        assert_eq!(fs::read(&precious).unwrap(), before, "{args:?}");
    }

    let fresh = outside.join("sub/fresh.sqlite");
    fs::write(root.join("mikke.toml"), config(&fresh, false)).unwrap();
    let out = run_raw(&root, &["index"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(fs::read(&fresh).unwrap().starts_with(b"SQLite format 3\0"));
    let out = run_raw(&root, &["find", "キリン"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = fs::remove_dir_all(&root);
    let _ = fs::remove_dir_all(&outside);
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

// --- 設定の書き間違いの警告 (#55)・.gitignore (#56)・exclude_paths (#57)・ネスト設定 (#58) ---

/// index の stdout から「ノート数: N」を読む。
fn note_count(stdout: &str) -> Option<usize> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("ノート数: "))
        .and_then(|n| n.trim().parse().ok())
}

/// stdout の「path: X」を集める (X は root 相対の posix パス)。
fn result_paths(stdout: &str) -> std::collections::BTreeSet<String> {
    stdout
        .lines()
        .filter_map(|l| l.trim().strip_prefix("path: "))
        .map(|s| s.trim().to_string())
        .collect()
}

fn set_of(items: &[&str]) -> std::collections::BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// index を流し、(exit code, stdout, stderr) を返す。
fn index_raw(root: &Path) -> (Option<i32>, String, String) {
    let out = run_raw(root, &["index"]);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// SPEC の「設定スキーマ」の toml ブロック。
fn spec_schema_toml() -> String {
    // Windows の checkout では CRLF になりうる
    let spec = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/SPEC.md"))
        .unwrap()
        .replace("\r\n", "\n");
    let after = &spec[spec.find("## 設定スキーマ").unwrap()..];
    let start = after.find("```toml\n").unwrap() + "```toml\n".len();
    let end = after[start..].find("```").unwrap();
    after[start..start + end].to_string()
}

const NOTE_A: &str = "---\ntitle: a\ndate: 2026-01-01\n---\nwalrus a\n";
const NOTE_S: &str = "---\ntitle: s\ndate: 2026-01-01\n---\nwalrus s\n";

/// 黙って効かない設定 (issue #55 の表) は stderr に path 付きで警告し、exit code と stdout は変えない。
#[test]
fn config_ineffective_settings_warn() {
    let cases: &[(&str, &str, usize, &[&str])] = &[
        (
            "key",
            "[scan]\nexclude_dir = [\"scratch\"]\n",
            2,
            &["未知のキー [scan] exclude_dir", "exclude_dirs ?"],
        ),
        (
            "section",
            "[scna]\ninclude = [\"notes\"]\n",
            2,
            &["未知の節 [scna]", "[scan] ?"],
        ),
        (
            "index-key",
            "[index]\nauto_rebuld = true\n",
            2,
            &["auto_rebuld", "auto_rebuild ?"],
        ),
        (
            "wrong-section",
            "[search]\nauto_rebuild = true\n",
            2,
            &["[search] auto_rebuild", "[index] auto_rebuild ?"],
        ),
        (
            "include-file",
            "[scan]\ninclude = [\"notes/a.md\"]\n",
            0,
            &["include の \"notes/a.md\""],
        ),
        (
            "dir-path",
            "[scan]\nexclude_dirs = [\"scratch/\"]\n",
            2,
            &["\"scratch/\"", "exclude_paths"],
        ),
        (
            "file-glob",
            "[scan]\nexclude_files = [\"*.md\"]\n",
            2,
            &["\"*.md\"", "glob"],
        ),
    ];
    for (tag, config, want, needles) in cases {
        let root = temp_repo(
            &format!("warn-{tag}"),
            &[
                ("mikke.toml", config),
                ("notes/a.md", NOTE_A),
                ("scratch/s.md", NOTE_S),
            ],
        );
        let (code, stdout, stderr) = index_raw(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(code, Some(0), "{config}: exit 0 のはず:\n{stderr}");
        assert_eq!(
            note_count(&stdout),
            Some(*want),
            "{config}: ノート数が違う:\n{stdout}"
        );
        assert!(
            stderr.contains("mikke.toml") && needles.iter().all(|n| stderr.contains(n)),
            "{config}: 警告が無い ({needles:?}):\n{stderr}"
        );
        assert!(
            !stdout.contains("Warning"),
            "{config}: 警告が stdout に混ざっている:\n{stdout}"
        );
    }
}

/// 節を置き違えた auto_rebuild は効かない (警告だけ出し、挙動は既定のまま)。
#[test]
fn config_wrong_section_does_not_enable_auto_rebuild() {
    let root = temp_repo(
        "warn-arb",
        &[
            ("mikke.toml", "[search]\nauto_rebuild = true\n"),
            ("notes/a.md", NOTE_A),
        ],
    );
    run(&root, &["index"]);
    fs::write(root.join("notes/b.md"), NOTE_S).unwrap();
    bump_mtime(&root.join("notes/b.md"), 10);
    let (stdout, stderr) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(
        stdout.contains("(1件") && !stdout.contains("notes/b.md"),
        "置き違えた auto_rebuild が効いている:\n{stdout}"
    );
    assert!(!stderr.contains("再構築"), "再構築が走っている:\n{stderr}");
    assert!(
        stderr.contains("[index] auto_rebuild ?"),
        "警告が無い:\n{stderr}"
    );
}

/// 節の外のキーも警告し、候補の節を示す。
#[test]
fn config_top_level_key_warns() {
    let root = temp_repo(
        "warn-top",
        &[
            ("mikke.toml", "auto_rebuild = true\n"),
            ("notes/a.md", NOTE_A),
        ],
    );
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("節の外の未知のキー auto_rebuild")
            && stderr.contains("[index] auto_rebuild ?"),
        "警告が無い:\n{stderr}"
    );
}

/// `[` を含む名前は glob 扱いの警告を出さず、名前として一致する。
#[test]
fn config_bracket_name_does_not_warn() {
    let root = temp_repo(
        "warn-bracket",
        &[
            ("mikke.toml", "[scan]\nexclude_dirs = [\"[old]\"]\n"),
            ("notes/a.md", NOTE_A),
            ("[old]/x.md", NOTE_S),
        ],
    );
    let (code, stdout, stderr) = index_raw(&root);
    let (found, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!stderr.contains("Warning"), "警告が出ている:\n{stderr}");
    assert_eq!(note_count(&stdout), Some(1), "{stdout}");
    assert!(!found.contains("[old]/"), "[old] が外れていない:\n{found}");
}

/// 既知のキーだけの設定 (SPEC のスキーマ・README の例・fixture) は警告しない。
#[test]
fn config_known_keys_do_not_warn() {
    let schema = spec_schema_toml();
    for (tag, config) in [
        ("spec", schema.as_str()),
        ("readme", "[semantic]\nenabled = true\n"),
    ] {
        let root = temp_repo(
            &format!("known-{tag}"),
            &[("mikke.toml", config), ("notes/a.md", NOTE_A)],
        );
        let (code, _, stderr) = index_raw(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(code, Some(0), "{tag}: {stderr}");
        for bad in ["未知の", "一致しません", "glob として"] {
            assert!(!stderr.contains(bad), "{tag}: 警告が出ている:\n{stderr}");
        }
    }
    let root = temp_copy("known-fixture");
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    for bad in ["未知の", "一致しません", "glob として"] {
        assert!(!stderr.contains(bad), "fixture: 警告が出ている:\n{stderr}");
    }
}

/// ネスト repo の設定を何度読み直しても、同じ警告は 1 回の実行で 1 度だけ出る。
#[test]
fn config_warning_printed_once_per_run() {
    let files: &[(&str, &str)] = &[
        ("mikke.toml", "[index]\nauto_rebuild = true\n"),
        ("sub/mikke.toml", "[scna]\n"),
        ("sub/.mikke.toml", "[scan]\n"),
        ("notes/a.md", NOTE_A),
        ("sub/b.md", NOTE_S),
    ];
    let root = temp_repo("warn-once", files);
    run(&root, &["index"]);
    bump_mtime(&root.join("notes/a.md"), 10);
    let (_, stderr) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert!(stderr.contains("再構築"), "再構築が走っていない:\n{stderr}");
    assert_eq!(count_lines(&stderr, "未知の節 [scna]"), 1, "{stderr}");
    assert_eq!(count_lines(&stderr, "両方あります"), 1, "{stderr}");

    let root = temp_repo("warn-once-health", files);
    let out = run_raw(&root, &["health"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&root);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(count_lines(&stderr, "未知の節 [scna]"), 1, "{stderr}");
    assert_eq!(count_lines(&stderr, "両方あります"), 1, "{stderr}");
}

/// Windows では exclude_dirs の `\` も名前と一致しえないので警告する。
#[cfg(windows)]
#[test]
fn config_warns_backslash_on_windows() {
    let root = temp_repo(
        "warn-backslash",
        &[
            ("mikke.toml", "[scan]\nexclude_dirs = ['a\\b']\n"),
            ("notes/a.md", NOTE_A),
        ],
    );
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("一致しません"), "警告が無い:\n{stderr}");
}

/// issue #56 の再現の木。.git は git コマンドを呼ばずにディレクトリだけ作る。
fn gitignore_repo(tag: &str, config: &str, with_git: bool) -> PathBuf {
    let root = temp_repo(
        tag,
        &[
            ("mikke.toml", config),
            (
                "notes/penguin.md",
                "---\ntitle: ペンギン観察メモ\ndate: 2026-01-10\ntags: [penguin]\n---\n架空の島に住むペンギン zephyrpenguin の観察記録。\n",
            ),
            (
                "notes/memo.draft.md",
                "---\ntitle: 下書きメモ\ndate: 2026-02-01\n---\nzephyrpenguin zephyrpenguin の未整理メモ\n",
            ),
            (
                "scratch/draft.md",
                "---\ntitle: 試しの下書き\ndate: 2026-02-02\n---\nzephyrpenguin zephyrpenguin zephyrpenguin の試し書き\n",
            ),
            (
                "work/tmp/trial.md",
                "---\ntitle: 作業用コピー\ndate: 2026-02-03\n---\nzephyrpenguin zephyrpenguin の作業用コピー\n",
            ),
            (".gitignore", "scratch/\nwork/tmp/\n*.draft.md\n.mikke/\n"),
        ],
    );
    if with_git {
        fs::create_dir_all(root.join(".git")).unwrap();
    }
    root
}

const RESPECT: &str = "[scan]\nrespect_gitignore = true\n";

/// 既定 (respect_gitignore を書かない) では .gitignore を見ない。
#[test]
fn gitignore_default_off() {
    let root = gitignore_repo("gi-off", "[scan]\n", true);
    let (code, stdout, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(note_count(&stdout), Some(4), "{stdout}");
}

/// respect_gitignore = true で .gitignore が無視するパスを走査しない。
#[test]
fn gitignore_respected() {
    let root = gitignore_repo("gi-on", RESPECT, true);
    let (code, stdout, stderr) = index_raw(&root);
    let (found, _) = run(&root, &["find", "zephyrpenguin"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(note_count(&stdout), Some(1), "{stdout}");
    assert!(found.contains("(1件"), "{found}");
    assert_eq!(
        result_paths(&found),
        set_of(&["notes/penguin.md"]),
        "{found}"
    );
}

/// .gitignore の編集は path 集合のハッシュで stale として検知する (mtime は古いまま)。
#[test]
fn gitignore_change_triggers_auto_rebuild() {
    let root = gitignore_repo(
        "gi-arb",
        "[scan]\nrespect_gitignore = true\n[index]\nauto_rebuild = true\n",
        true,
    );
    run(&root, &["index"]);
    fs::write(root.join(".gitignore"), "work/tmp/\n*.draft.md\n.mikke/\n").unwrap();
    let (stdout, stderr) = run(&root, &["find", "zephyrpenguin"]);
    let _ = fs::remove_dir_all(&root);
    assert!(stderr.contains("再構築"), "再構築が走っていない:\n{stderr}");
    assert!(
        result_paths(&stdout).contains("scratch/draft.md"),
        "{stdout}"
    );
}

/// .git/info/exclude も読み、.gitignore の `!` で戻せる。
#[test]
fn gitignore_info_exclude_and_negation() {
    let root = gitignore_repo("gi-neg", RESPECT, true);
    fs::create_dir_all(root.join(".git/info")).unwrap();
    fs::write(root.join(".git/info/exclude"), "work/\n").unwrap();
    fs::write(
        root.join(".gitignore"),
        "*.draft.md\n!notes/keep.draft.md\n.mikke/\n",
    )
    .unwrap();
    fs::write(
        root.join("notes/keep.draft.md"),
        "---\ntitle: 残す下書き\n---\nzephyrpenguin\n",
    )
    .unwrap();
    run(&root, &["index"]);
    let (stdout, _) = run(&root, &["find", "zephyrpenguin"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        result_paths(&stdout),
        set_of(&[
            "notes/penguin.md",
            "notes/keep.draft.md",
            "scratch/draft.md"
        ]),
        "{stdout}"
    );
}

/// `.git` を持つ下位のディレクトリでは外側の規則を捨てる。入口は外側の規則で判定する。
#[test]
fn gitignore_nested_git_repo_boundary() {
    let root = temp_repo(
        "gi-nested-git",
        &[
            ("mikke.toml", RESPECT),
            (".gitignore", "secret/\n*.draft.md\nvendor/\n"),
            ("notes/a.md", NOTE_A),
            ("secret/x.md", NOTE_S),
            ("lib/.gitignore", "local/\n"),
            ("lib/secret/y.md", NOTE_S),
            ("lib/n.draft.md", NOTE_S),
            ("lib/local/z.md", NOTE_S),
            ("vendor/v.md", NOTE_S),
        ],
    );
    for d in [".git", "lib/.git", "vendor/.git"] {
        fs::create_dir_all(root.join(d)).unwrap();
    }
    run(&root, &["index"]);
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        result_paths(&stdout),
        set_of(&["notes/a.md", "lib/secret/y.md", "lib/n.draft.md"]),
        "{stdout}"
    );
}

/// git repo の外でも異常終了・警告せず、root から下の .gitignore だけを読む。
#[test]
fn gitignore_without_git_dir() {
    let root = gitignore_repo("gi-nogit", RESPECT, false);
    // 一時フォルダの上に .git があると、その .gitignore も効いて前提が崩れる
    if root.ancestors().skip(1).any(|a| a.join(".git").exists()) {
        eprintln!("skip: 一時フォルダの上に .git がある");
        let _ = fs::remove_dir_all(&root);
        return;
    }
    fs::write(root.join(".gitignore"), "scratch/\n").unwrap();
    let (code, _, stderr) = index_raw(&root);
    let (stdout, _) = run(&root, &["find", "zephyrpenguin"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!stderr.contains("Warning"), "警告が出ている:\n{stderr}");
    let paths = result_paths(&stdout);
    assert!(paths.contains("notes/penguin.md"), "{stdout}");
    assert!(!paths.iter().any(|p| p.starts_with("scratch/")), "{stdout}");
}

/// UTF-8 でない行 (Shift_JIS のコメントなど) があっても、残りの行は効く。
#[test]
fn gitignore_non_utf8_line_keeps_rest() {
    let root = gitignore_repo("gi-nonutf8", RESPECT, true);
    // "# " + Shift_JIS の「メモ」+ CRLF のあとに規則を並べる
    let mut bytes = b"# \x83\x81\x83\x82\r\n".to_vec();
    bytes.extend_from_slice(b"scratch/\r\nwork/tmp/\r\n*.draft.md\r\n.mikke/\r\n");
    fs::write(root.join(".gitignore"), bytes).unwrap();
    let (code, _, stderr) = index_raw(&root);
    let (stdout, _) = run(&root, &["find", "zephyrpenguin"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        result_paths(&stdout),
        set_of(&["notes/penguin.md"]),
        "{stdout}"
    );
}

/// index / embeddings の出力先は配下の .md ファイルだけを外し、出力先の下のネスト repo は今までどおり歩く。
#[test]
fn output_dir_does_not_prune_nested_repo() {
    let root = temp_repo(
        "out-nested",
        &[
            ("mikke.toml", "[index]\nembeddings_dir = \"work\"\n"),
            ("a.md", NOTE_A),
            ("work/w.md", NOTE_S),
            ("work/sub/mikke.toml", ""),
            ("work/sub/s.md", NOTE_S),
        ],
    );
    run(&root, &["index"]);
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        result_paths(&stdout),
        set_of(&["a.md", "work/sub/s.md"]),
        "{stdout}"
    );
}

/// root より上の .gitignore も root の下に当てるが、root 自身は判定しない。
#[test]
fn gitignore_ancestor_rules_apply_but_root_not_judged() {
    let top = temp_repo(
        "gi-ancestor",
        &[
            (".gitignore", "notes/scratch/\nnotes/\n"),
            ("notes/mikke.toml", RESPECT),
            ("notes/a.md", NOTE_A),
            ("notes/scratch/x.md", NOTE_S),
        ],
    );
    fs::create_dir_all(top.join(".git")).unwrap();
    let root = top.join("notes");
    run(&root, &["index"]);
    let (stdout, _) = run(&root, &["find", "walrus"]);
    let _ = fs::remove_dir_all(&top);
    assert_eq!(result_paths(&stdout), set_of(&["a.md"]), "{stdout}");
}

/// ネストしたノート repo の中は子の respect_gitignore に従う。親の .gitignore は入口まで効く。
#[test]
fn gitignore_nested_mikke_repo_uses_child_setting() {
    let root = temp_repo(
        "gi-nested-mikke",
        &[
            ("mikke.toml", RESPECT),
            (".gitignore", "*.draft.md\n"),
            ("notes/a.md", NOTE_A),
            ("sub/mikke.toml", "[scan]\n"),
            ("sub/ok.md", NOTE_S),
            ("sub/x.draft.md", NOTE_S),
        ],
    );
    fs::create_dir_all(root.join(".git")).unwrap();
    let paths = |root: &Path| {
        run(root, &["index"]);
        result_paths(&run(root, &["find", "walrus"]).0)
    };
    assert_eq!(
        paths(&root),
        set_of(&["notes/a.md", "sub/ok.md", "sub/x.draft.md"]),
        "子が書かない respect_gitignore を親から継いでいる"
    );
    fs::write(root.join("sub/mikke.toml"), RESPECT).unwrap();
    assert_eq!(
        paths(&root),
        set_of(&["notes/a.md", "sub/ok.md"]),
        "子の respect_gitignore = true が効いていない"
    );
    fs::write(root.join(".gitignore"), "*.draft.md\nsub/\n").unwrap();
    let got = paths(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        got,
        set_of(&["notes/a.md"]),
        "親の .gitignore で入口を塞げない"
    );
}

/// include の起点が .gitignore に当たる時も起点ごと外す。
#[test]
fn gitignore_include_base_ignored() {
    let root = gitignore_repo(
        "gi-include",
        "[scan]\ninclude = [\".\", \"scratch\"]\nrespect_gitignore = true\n",
        true,
    );
    run(&root, &["index"]);
    let (stdout, _) = run(&root, &["find", "zephyrpenguin"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        result_paths(&stdout),
        set_of(&["notes/penguin.md"]),
        "{stdout}"
    );
}

/// respect_gitignore の型の違いは exit 2。
#[test]
fn gitignore_type_error() {
    let root = gitignore_repo("gi-type", "[scan]\nrespect_gitignore = \"yes\"\n", true);
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("真偽値"), "{stderr}");
}

/// issue #57 の再現の木。config は mikke.toml の中身。
fn exclude_paths_repo(tag: &str, config: &str, extra: &[(&str, &str)]) -> PathBuf {
    let mut files: Vec<(&str, &str)> = vec![
        ("mikke.toml", config),
        ("top.md", "# top\n\nwalrus 直下のノート。\n"),
        ("notes/a.md", "# a\n\nwalrus 普通のノート。\n"),
        (
            "notes/app/state/store.md",
            "# store\n\nwalrus 状態の設計メモ。\n",
        ),
        (
            "notes/projects/build/log.md",
            "# log\n\nwalrus 手順の記録。\n",
        ),
        (
            "notes/templates/howto.md",
            "# howto\n\nwalrus 雛形の使い方。\n",
        ),
        ("state/cache/tmp.md", "# tmp\n\nwalrus 一時ファイル。\n"),
        (
            "vendor/node_modules/pkg/CHANGELOG.md",
            "# changelog\n\nwalrus 変更履歴。\n",
        ),
    ];
    files.extend_from_slice(extra);
    temp_repo(tag, &files)
}

fn indexed_paths(root: &Path) -> std::collections::BTreeSet<String> {
    run(root, &["index"]);
    result_paths(&run(root, &["find", "walrus"]).0)
}

/// exclude_paths は root 相対の 1 か所だけを外し、exclude_dirs の既定は残る。
#[test]
fn exclude_paths_root_relative() {
    for (i, v) in ["state", "state/", "./state"].iter().enumerate() {
        let root = exclude_paths_repo(
            &format!("ep-root-{i}"),
            &format!("[scan]\nexclude_paths = [\"{v}\"]\n"),
            &[],
        );
        let got = indexed_paths(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(
            got,
            set_of(&["top.md", "notes/a.md", "notes/app/state/store.md"]),
            "{v:?}"
        );
    }
}

/// 判定は要素単位 (state は statement に当たらない)。
#[test]
fn exclude_paths_element_wise() {
    let root = exclude_paths_repo(
        "ep-element",
        "[scan]\nexclude_paths = [\"state\"]\n",
        &[("statement/x.md", "# x\n\nwalrus 声明。\n")],
    );
    let got = indexed_paths(&root);
    let _ = fs::remove_dir_all(&root);
    assert!(got.contains("statement/x.md"), "{got:?}");
    assert!(!got.contains("state/cache/tmp.md"), "{got:?}");
}

/// 深いパスはその場所だけを外し、ファイルを指す値はそのファイルだけを外す。
#[test]
fn exclude_paths_deep_and_file() {
    let root = exclude_paths_repo(
        "ep-deep",
        "[scan]\nexclude_paths = [\"notes/app/state\"]\n",
        &[],
    );
    let got = indexed_paths(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        got,
        set_of(&["top.md", "notes/a.md", "state/cache/tmp.md"]),
        "{got:?}"
    );

    let root = exclude_paths_repo("ep-file", "[scan]\nexclude_paths = [\"notes/a.md\"]\n", &[]);
    let got = indexed_paths(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(
        got,
        set_of(&["top.md", "notes/app/state/store.md", "state/cache/tmp.md"]),
        "{got:?}"
    );
}

/// include の起点が exclude_paths の配下なら起点ごと外す (除外は include に勝つ)。
#[test]
fn exclude_paths_include_under_excluded() {
    let root = exclude_paths_repo(
        "ep-include",
        "[scan]\ninclude = [\".\", \"state/cache\"]\nexclude_paths = [\"state\"]\n",
        &[],
    );
    let got = indexed_paths(&root);
    let _ = fs::remove_dir_all(&root);
    assert!(!got.contains("state/cache/tmp.md"), "{got:?}");
    assert!(got.contains("top.md"), "{got:?}");
}

/// ネスト repo の exclude_paths はその repo の root 相対。親の値は入口まで効く。
#[test]
fn exclude_paths_nested() {
    let child_notes: &[(&str, &str)] = &[
        ("sub/ok.md", "# ok\n\nwalrus 子のノート。\n"),
        ("sub/state/s.md", "# s\n\nwalrus 子の一時ファイル。\n"),
    ];
    let mut extra = child_notes.to_vec();
    extra.push(("sub/mikke.toml", "[scan]\nexclude_paths = [\"state\"]\n"));
    let root = exclude_paths_repo("ep-nested-child", "[scan]\n", &extra);
    let got = indexed_paths(&root);
    let _ = fs::remove_dir_all(&root);
    assert!(got.contains("sub/ok.md"), "{got:?}");
    assert!(!got.contains("sub/state/s.md"), "{got:?}");
    assert!(got.contains("state/cache/tmp.md"), "{got:?}");

    let mut extra = child_notes.to_vec();
    extra.push(("sub/mikke.toml", "[scan]\n"));
    let root = exclude_paths_repo(
        "ep-nested-parent",
        "[scan]\nexclude_paths = [\"sub\"]\n",
        &extra,
    );
    let got = indexed_paths(&root);
    let _ = fs::remove_dir_all(&root);
    assert!(!got.iter().any(|p| p.starts_with("sub/")), "{got:?}");

    let root = exclude_paths_repo(
        "ep-nested-inside",
        "[scan]\nexclude_paths = [\"sub/state\"]\n",
        &extra,
    );
    let (code, _, stderr) = index_raw(&root);
    let got = result_paths(&run(&root, &["find", "walrus"]).0);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(count_lines(&stderr, "ネストしたノート repo"), 1, "{stderr}");
    assert!(got.contains("sub/state/s.md"), "{got:?}");
}

/// 不正な exclude_paths は設定ファイルの path 付きで exit 2。
#[test]
fn exclude_paths_invalid_values_exit_2() {
    for (i, line) in [
        "exclude_paths = [\"\"]",
        "exclude_paths = [\".\"]",
        "exclude_paths = [\"/state\"]",
        "exclude_paths = [\"C:/state\"]",
        "exclude_paths = [\"../x\"]",
        "exclude_paths = [\"a/../b\"]",
        "exclude_paths = ['state\\cache']",
        "exclude_paths = [\"a\\tb\"]",
        "exclude_paths = \"state\"",
    ]
    .iter()
    .enumerate()
    {
        let root = temp_repo(
            &format!("ep-invalid-{i}"),
            &[
                ("mikke.toml", &format!("[scan]\n{line}\n")),
                ("a.md", NOTE_A),
            ],
        );
        let (code, _, stderr) = index_raw(&root);
        let _ = fs::remove_dir_all(&root);
        assert_eq!(code, Some(2), "{line}: {stderr}");
        assert!(
            stderr.contains("mikke.toml") && stderr.contains("exclude_paths"),
            "{line}: {stderr}"
        );
    }

    let root = temp_repo(
        "ep-invalid-child",
        &[
            ("mikke.toml", "[scan]\n"),
            ("a.md", NOTE_A),
            ("sub/mikke.toml", "[scan]\nexclude_paths = [\"../x\"]\n"),
            ("sub/b.md", NOTE_S),
        ],
    );
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("sub") && stderr.contains("exclude_paths"),
        "{stderr}"
    );
}

/// exclude_paths の glob の文字は警告にとどめる。
#[test]
fn exclude_paths_glob_warns() {
    let root = temp_repo(
        "ep-glob",
        &[
            ("mikke.toml", "[scan]\nexclude_paths = [\"x/*\"]\n"),
            ("a.md", NOTE_A),
        ],
    );
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("glob として扱いません"), "{stderr}");
}

/// issue #58 の再現の木。child は work/sub/.mikke.toml の中身。
fn nested_cfg_repo(tag: &str, child: &str) -> PathBuf {
    temp_repo(
        tag,
        &[
            ("mikke.toml", "[scan]\ninclude = [\".\"]\n"),
            ("notes/alpha.md", "---\ntitle: Alpha\n---\nbanana alpha\n"),
            ("work/beta.md", "---\ntitle: Beta\n---\nbanana beta\n"),
            (
                "work/sub/gamma.md",
                "---\ntitle: Gamma\n---\nbanana gamma\n",
            ),
            ("work/sub/.mikke.toml", child),
        ],
    )
}

/// ネスト repo の設定の [health] / [search] / [semantic] は読まず、親を止めない。
#[test]
fn nested_config_unused_sections_ignored() {
    for (i, child) in [
        "[health]\ndisable = [\"typo\"]\n",
        "[semantic]\nenabled = \"yes\"\n",
        "[search]\nbm25_limit = \"50\"\n",
        "health = 1\n",
    ]
    .iter()
    .enumerate()
    {
        let root = nested_cfg_repo(&format!("nested-unused-{i}"), child);
        let (code, stdout, stderr) = index_raw(&root);
        let health = run_raw(&root, &["health"]);
        let health_err = String::from_utf8_lossy(&health.stderr).into_owned();
        let _ = fs::remove_dir_all(&root);
        assert_eq!(code, Some(0), "{child}: {stderr}");
        assert_eq!(note_count(&stdout), Some(3), "{child}: {stdout}");
        assert_eq!(health.status.code(), Some(0), "{child}: {health_err}");
        for err in [&stderr, &health_err] {
            assert!(
                !err.lines()
                    .any(|l| l.contains("Warning") && l.contains(".mikke.toml")),
                "{child}: 子の使わない節で警告している:\n{err}"
            );
        }
    }
}

/// ネスト repo の [index] の型の違いは path 付きで exit 2。
#[test]
fn nested_config_index_type_error_fails() {
    let root = nested_cfg_repo("nested-index-type", "[index]\nauto_rebuild = \"yes\"\n");
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains(".mikke.toml") && stderr.contains("真偽値"),
        "{stderr}"
    );
}

/// ネスト repo の設定の構文エラーは path 付きで exit 2。
#[test]
fn nested_config_syntax_error_fails() {
    let root = nested_cfg_repo("nested-syntax", "[scan\n");
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains(".mikke.toml"), "{stderr}");
}

/// ネスト repo の [scan] の未知のキーは子の path で警告し、読まない節は警告しない。
#[test]
fn nested_config_unknown_key_warns() {
    let root = nested_cfg_repo("nested-unknown", "[scan]\nexclude_dir = []\n");
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr
            .lines()
            .any(|l| l.contains(".mikke.toml") && l.contains("exclude_dir")),
        "{stderr}"
    );

    let root = nested_cfg_repo("nested-unknown-health", "[health]\nwalrus = 1\n");
    let (code, _, stderr) = index_raw(&root);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!stderr.contains("Warning"), "{stderr}");
}
