//! セマンティック検索用の埋め込み生成 (`mikke embed`) と semantic ランキング。
//! feature = "semantic" 専用。バックエンドは candle (純 Rust — 単一バイナリ配布を壊さない)。
//!
//! 契約 (docs/SPEC.md embedding 節):
//!   埋め込みテキスト = "title\n summary\n 本文"。E5 仕様でドキュメント側に passage_prefix、
//!   クエリ側に query_prefix。attention mask で mean pooling → L2 normalize して保存。
//!   差分検出はファイル内容の SHA-256。model / passage_prefix 変更時は全再構築。
//!   削除のみの更新でも保存し直す (消えたベクトルが結果に残り続けるのを防ぐ)。
//!   保存: embeddings_dir/embeddings.safetensors ("vectors" 行列 f32 [n, d]) + metadata.json
//!   (generated, model, query_prefix, passage_prefix, note_count, notes[{path,title,hash}])。
//!   順序 = vectors 行と一致。semantic_ranked は metadata の model/query_prefix を優先して
//!   エンコード (保存済みベクトルとエンコード条件を揃える)。

use crate::config::{display_path, to_posix, Config};
use crate::scan;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config as BertConfig, DTYPE};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

/// 1 バッチのテキスト数。attention は系列長の 2 乗でメモリを食うため控えめに
/// (最悪 8 × 12head × 512² × 4B ≈ 400MB の一時領域)。
const BATCH_SIZE: usize = 8;

/// 埋め込みを永続化する単位。推論 32 バッチごとに中間結果を残す。
const SAVE_CHUNK_SIZE: usize = BATCH_SIZE * 32;

/// E5 系の系列長上限。モデル config の max_position_embeddings とこの値の小さい方で切る。
const MAX_SEQ_LEN: usize = 512;

// --- metadata.json (Python 版と互換のスキーマ) ---

#[derive(Serialize, Deserialize)]
struct MetaNote {
    path: String,
    title: String,
    hash: String,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    #[serde(default)]
    generated: String,
    // Python 版同様「キー欠落は設定値へフォールバック」を残すため Option (欠落 ≠ 空文字)。
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    query_prefix: Option<String>,
    #[serde(default)]
    passage_prefix: Option<String>,
    #[serde(default)]
    note_count: usize,
    #[serde(default)]
    notes: Vec<MetaNote>,
}

// --- candle エンコーダ ---

/// tokenizer + BERT 系モデル一式。E5 系 (multilingual-e5-*) を想定。
struct Encoder {
    tokenizer: Tokenizer,
    model: BertModel,
    device: Device,
}

impl Encoder {
    /// HF Hub cache からモデル一式をロードする (cache に無いファイルのみダウンロード)。
    fn load(model_id: &str) -> Result<Encoder, String> {
        let device = Device::Cpu;
        let api = hf_hub::api::sync::Api::new()
            .map_err(|e| format!("Hugging Face Hub API の初期化に失敗: {e}"))?;
        let repo = api.model(model_id.to_string());
        let fetch = |file: &str| {
            repo.get(file).map_err(|e| {
                format!(
                    "モデル {model_id} の {file} を取得できません: {e}\n  \
                     初回は Hugging Face からのダウンロードが必要です (オフライン/社内網は README 参照)。"
                )
            })
        };
        let config_path = fetch("config.json")?;
        let tokenizer_path = fetch("tokenizer.json")?;
        let weights_path = fetch("model.safetensors")?;

        let config_text = std::fs::read_to_string(&config_path)
            .map_err(|e| format!("config.json の読み込みに失敗: {e}"))?;
        let bert_config: BertConfig = serde_json::from_str(&config_text)
            .map_err(|e| format!("config.json の解釈に失敗 (BERT 系モデルのみ対応): {e}"))?;
        let max_len = serde_json::from_str::<serde_json::Value>(&config_text)
            .ok()
            .and_then(|v| v.get("max_position_embeddings").and_then(|n| n.as_u64()))
            .unwrap_or(MAX_SEQ_LEN as u64)
            .min(MAX_SEQ_LEN as u64) as usize;

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| format!("tokenizer の読み込みに失敗: {e}"))?;
        let pad_id = tokenizer
            .token_to_id("<pad>")
            .or_else(|| tokenizer.token_to_id("[PAD]"))
            .unwrap_or(0);
        let pad_token = tokenizer.id_to_token(pad_id).unwrap_or_default();
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: max_len,
                ..Default::default()
            }))
            .map_err(|e| format!("truncation 設定に失敗: {e}"))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id,
            pad_token,
            ..Default::default()
        }));

        // mmap ロード: ファイルは HF cache 管理下で実行中は不変とみなす
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)
                .map_err(|e| format!("モデル重みの読み込みに失敗: {e}"))?
        };
        let model = BertModel::load(vb, &bert_config)
            .map_err(|e| format!("モデルの構築に失敗 (BERT 系モデルのみ対応): {e}"))?;
        Ok(Encoder {
            tokenizer,
            model,
            device,
        })
    }

    /// texts を L2 正規化済み埋め込みへ (入力順を保持)。
    /// progress = Some((それまでに終えた件数, 全体の件数)) で stderr に全体の進捗を出す (改行はしない)。
    fn encode(
        &self,
        texts: &[String],
        progress: Option<(usize, usize)>,
    ) -> Result<Vec<Vec<f32>>, String> {
        // 長さの近いテキスト同士でバッチを組み padding の無駄を減らす (出力は元順に戻す)
        let mut order: Vec<usize> = (0..texts.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(texts[i].chars().count()));

        let mut out: Vec<Vec<f32>> = vec![Vec::new(); texts.len()];
        let mut done = 0usize;
        for chunk in order.chunks(BATCH_SIZE) {
            let batch: Vec<&str> = chunk.iter().map(|&i| texts[i].as_str()).collect();
            let encodings = self
                .tokenizer
                .encode_batch(batch, true)
                .map_err(|e| format!("tokenize に失敗: {e}"))?;
            let rows = self
                .forward_pooled(&encodings)
                .map_err(|e| format!("埋め込み計算に失敗: {e}"))?;
            for (&i, row) in chunk.iter().zip(rows) {
                out[i] = row;
            }
            done += chunk.len();
            if let Some((before, total)) = progress {
                eprint!("\r  {}/{total} 件", before + done);
                let _ = std::io::stderr().flush();
            }
        }
        Ok(out)
    }

    /// 1 バッチの forward → attention mask で mean pooling → L2 normalize。
    fn forward_pooled(
        &self,
        encodings: &[tokenizers::Encoding],
    ) -> candle_core::Result<Vec<Vec<f32>>> {
        let b = encodings.len();
        let t = encodings.first().map(|e| e.get_ids().len()).unwrap_or(0);
        let flat = |f: fn(&tokenizers::Encoding) -> &[u32]| -> Vec<u32> {
            encodings
                .iter()
                .flat_map(|e| f(e).iter().copied())
                .collect()
        };
        let input_ids = Tensor::from_vec(flat(|e| e.get_ids()), (b, t), &self.device)?;
        let type_ids = Tensor::from_vec(flat(|e| e.get_type_ids()), (b, t), &self.device)?;
        let mask = Tensor::from_vec(flat(|e| e.get_attention_mask()), (b, t), &self.device)?;

        let hidden = self.model.forward(&input_ids, &type_ids, Some(&mask))?; // [b, t, h]
        let mask_f = mask.to_dtype(DType::F32)?.unsqueeze(2)?; // [b, t, 1]

        // ゼロ除算ガード (sentence-transformers の clamp(min=1e-9) と同等)。
        // 通常は special token で mask 和 >= 2 だが、NaN 混入を構造的に防ぐ
        let counts = mask_f.sum(1)?.maximum(1e-9)?;
        let mean = hidden
            .broadcast_mul(&mask_f)?
            .sum(1)?
            .broadcast_div(&counts)?; // [b, h]
        let norm = mean.sqr()?.sum_keepdim(1)?.sqrt()?.maximum(1e-12)?;
        mean.broadcast_div(&norm)?.to_vec2::<f32>()
    }
}

// --- 埋め込みの保存/読み込み ---

/// tmp へ書いて rename する時の一時パス (書きかけファイルが本体名で残るのを防ぐ)。
fn tmp_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn save_vectors(path: &Path, vectors: &[Vec<f32>]) -> Result<(), String> {
    let n = vectors.len();
    let d = vectors.first().map(|v| v.len()).unwrap_or(0);
    let flat: Vec<f32> = vectors.iter().flatten().copied().collect();
    let t = Tensor::from_vec(flat, (n, d), &Device::Cpu)
        .map_err(|e| format!("ベクトル行列の構築に失敗: {e}"))?;
    let tmp = tmp_path(path);
    candle_core::safetensors::save(&HashMap::from([("vectors".to_string(), t)]), &tmp)
        .map_err(|e| format!("safetensors の保存に失敗: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("safetensors の配置に失敗: {e}"))
}

fn load_vectors(path: &Path) -> Result<Vec<Vec<f32>>, String> {
    let tensors = candle_core::safetensors::load(path, &Device::Cpu)
        .map_err(|e| format!("safetensors の読み込みに失敗: {e}"))?;
    let t = tensors
        .get("vectors")
        .ok_or("embeddings.safetensors に 'vectors' がありません")?;
    t.to_dtype(DType::F32)
        .and_then(|t| t.to_vec2::<f32>())
        .map_err(|e| format!("ベクトル行列の読み込みに失敗: {e}"))
}

fn read_metadata(path: &Path) -> Result<Metadata, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("metadata.json の読み込みに失敗: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("metadata.json の解釈に失敗: {e}"))
}

// --- ノート収集 ---

struct CollectedNote {
    path: String,
    title: String,
    text: String,
    hash: String,
}

struct EmbeddingSnapshot {
    vectors: Vec<Vec<f32>>,
    notes: Vec<MetaNote>,
}

/// 現在までの計算結果を notes 順に並べる。未計算の更新ノートを旧ベクトルで残す場合は、
/// 次回も更新対象になるよう旧 hash を維持する。
fn assemble_embedding_snapshot(
    notes: &[CollectedNote],
    new_embeddings: &HashMap<&str, Vec<f32>>,
    old_embeddings: &HashMap<String, Vec<f32>>,
    old_hashes: &HashMap<&str, &str>,
) -> EmbeddingSnapshot {
    let mut vectors = Vec::with_capacity(notes.len());
    let mut meta_notes = Vec::with_capacity(notes.len());
    for note in notes {
        let (vector, hash) = if let Some(vector) = new_embeddings.get(note.path.as_str()) {
            (vector, note.hash.as_str())
        } else if let (Some(vector), Some(hash)) = (
            old_embeddings.get(&note.path),
            old_hashes.get(note.path.as_str()),
        ) {
            (vector, *hash)
        } else {
            continue;
        };
        vectors.push(vector.clone());
        meta_notes.push(MetaNote {
            path: note.path.clone(),
            title: note.title.clone(),
            hash: hash.to_string(),
        });
    }
    EmbeddingSnapshot {
        vectors,
        notes: meta_notes,
    }
}

/// ノートを収集し、埋め込みテキストとファイル内容 SHA-256 を組み立てる。
fn collect_notes(cfg: &Config) -> Vec<CollectedNote> {
    let mut notes = Vec::new();
    for (md_file, rel) in scan::iter_notes(cfg) {
        let rel_posix = to_posix(&rel);
        let raw = match std::fs::read(&md_file) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Warning: {rel_posix} の読み込みに失敗: {e}");
                continue;
            }
        };
        let note = match scan::load_note(&md_file, &rel) {
            Some(n) => n,
            None => {
                eprintln!("Warning: {rel_posix} の読み込みに失敗 (embed から除外)");
                continue;
            }
        };
        let text = format!("{}\n{}\n{}", note.title, note.summary, note.content)
            .trim()
            .to_string();
        let hash = Sha256::digest(&raw)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        notes.push(CollectedNote {
            path: note.path_rel,
            title: note.title,
            text,
            hash,
        });
    }
    notes
}

/// epoch 秒 → "YYYY-MM-DDTHH:MM:SSZ" (UTC)。外部 crate を足さないための最小実装
/// (civil_from_days — Howard Hinnant のアルゴリズム)。
fn iso8601_utc(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86400) as i64;
    let secs = epoch_secs % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// 現在までの計算結果を vectors / metadata の両方へ保存する。
fn save_embedding_snapshot(
    cfg: &Config,
    notes: &[CollectedNote],
    new_embeddings: &HashMap<&str, Vec<f32>>,
    old_embeddings: &HashMap<String, Vec<f32>>,
    old_hashes: &HashMap<&str, &str>,
) -> Result<(usize, usize), String> {
    let snapshot = assemble_embedding_snapshot(notes, new_embeddings, old_embeddings, old_hashes);
    let embeddings_file = cfg.embeddings_dir().join("embeddings.safetensors");
    let metadata_file = cfg.embeddings_dir().join("metadata.json");

    std::fs::create_dir_all(cfg.embeddings_dir())
        .map_err(|e| format!("embeddings ディレクトリを作成できません: {e}"))?;
    save_vectors(&embeddings_file, &snapshot.vectors)?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let metadata = Metadata {
        generated: iso8601_utc(now),
        model: Some(cfg.model.clone()),
        query_prefix: Some(cfg.query_prefix.clone()),
        passage_prefix: Some(cfg.passage_prefix.clone()),
        note_count: snapshot.notes.len(),
        notes: snapshot.notes,
    };
    let json = serde_json::to_string_pretty(&metadata)
        .map_err(|e| format!("metadata の生成に失敗: {e}"))?;
    // tmp + rename (書きかけの metadata が残るのを防ぐ。vectors と metadata の 2 ファイル間の
    // 版ずれは、読み側の件数一致チェック → 全再構築で自己回復する)
    let tmp = tmp_path(&metadata_file);
    std::fs::write(&tmp, json)
        .and_then(|_| std::fs::rename(&tmp, &metadata_file))
        .map_err(|e| format!("metadata.json の保存に失敗: {e}"))?;

    Ok((
        metadata.note_count,
        snapshot.vectors.first().map(|v| v.len()).unwrap_or(0),
    ))
}

fn exit_err(msg: &str) -> ! {
    eprintln!("Error: {msg}");
    std::process::exit(2);
}

// --- コマンド本体 ---

pub fn cmd_embed(cfg: &Config, force: bool) {
    if !cfg.semantic_enabled {
        eprintln!("Error: この root では semantic が無効です (mikke.toml の [semantic] enabled = true で有効化)。");
        std::process::exit(2);
    }
    build_embeddings(cfg, force);
}

fn build_embeddings(cfg: &Config, force: bool) {
    let embeddings_file = cfg.embeddings_dir().join("embeddings.safetensors");
    let metadata_file = cfg.embeddings_dir().join("metadata.json");

    let notes = collect_notes(cfg);
    if notes.is_empty() {
        println!("対象ノートが見つかりませんでした。");
        // 全ノート削除後も旧ベクトルが semantic 結果に残り続けるのを防ぐ (削除のみ更新と同じ原則)
        if embeddings_file.exists() || metadata_file.exists() {
            let _ = std::fs::remove_file(&embeddings_file);
            let _ = std::fs::remove_file(&metadata_file);
            println!("既存の埋め込みを削除しました (対象ノートが無いため)。");
        }
        return;
    }

    // 既存 metadata (壊れていたら警告して全再構築 — silent に古い結果を使い続けない)
    let mut old_meta: Option<Metadata> = None;
    if !force && metadata_file.exists() {
        match read_metadata(&metadata_file) {
            Ok(m) => old_meta = Some(m),
            Err(e) => eprintln!("Warning: {e} — 全件再構築します。"),
        }
    }

    // モデル/prefix が変わったら全再構築 (既存ベクトルと混ぜると比較不能)
    if let Some(m) = &old_meta {
        let old_model = m.model.as_deref().unwrap_or(&cfg.model);
        let old_ppfx = m.passage_prefix.as_deref().unwrap_or(&cfg.passage_prefix);
        if old_model != cfg.model || old_ppfx != cfg.passage_prefix {
            eprintln!("モデル/プレフィックス設定が変わったため全件再構築します。");
            old_meta = None;
        }
    }

    // 既存ベクトル (path → vec)。読めない/件数不一致 (途中クラッシュ等の破損) は全再構築へ倒す
    // — zip で短い方に黙って合わせると metadata と行の対応がずれたベクトルを再利用しうる
    let mut old_embeddings: HashMap<String, Vec<f32>> = HashMap::new();
    if let Some(m) = &old_meta {
        if !m.notes.is_empty() && embeddings_file.exists() {
            match load_vectors(&embeddings_file) {
                Ok(vectors) if vectors.len() != m.notes.len() => {
                    eprintln!(
                        "Warning: 既存埋め込み ({}行) と metadata ({}件) が一致しません — 全件再構築します。",
                        vectors.len(),
                        m.notes.len()
                    );
                    old_meta = None;
                }
                Ok(vectors) => {
                    for (item, vec) in m.notes.iter().zip(vectors) {
                        old_embeddings.insert(item.path.clone(), vec);
                    }
                }
                Err(e) => {
                    eprintln!("Warning: {e} — 全件再構築します。");
                    old_meta = None;
                }
            }
        }
    }
    let old_hashes: HashMap<&str, &str> = old_meta
        .as_ref()
        .map(|m| {
            m.notes
                .iter()
                .map(|n| (n.path.as_str(), n.hash.as_str()))
                .collect()
        })
        .unwrap_or_default();

    // 差分検出: hash 一致かつ既存ベクトルが実在するもののみ再利用
    let (reuse, to_embed): (Vec<&CollectedNote>, Vec<&CollectedNote>) =
        notes.iter().partition(|n| {
            old_hashes.get(n.path.as_str()).copied() == Some(n.hash.as_str())
                && old_embeddings.contains_key(&n.path)
        });

    // 削除ノートは notes に無いので自動的に落ちるが、「削除のみ」の更新でも保存し直さないと
    // 消えたベクトルが semantic 結果に残り続ける → ノート数の減少も更新扱いにする
    if to_embed.is_empty() && old_hashes.len() == notes.len() {
        println!("すべてのノートが最新です。更新の必要はありません。");
        return;
    }

    println!(
        "ノート数: {} (再利用: {}, 新規/更新: {})",
        notes.len(),
        reuse.len(),
        to_embed.len()
    );

    // 新規/更新分の埋め込み生成 (E5 仕様: ドキュメント側は passage_prefix 付き)
    let mut new_embed_map: HashMap<&str, Vec<f32>> = HashMap::new();
    let mut saved_stats = None;
    if !to_embed.is_empty() {
        println!("モデルをロード中: {}", cfg.model);
        let encoder = Encoder::load(&cfg.model).unwrap_or_else(|e| exit_err(&e));
        println!("埋め込みを計算中 ({}件)...", to_embed.len());
        let mut done = 0usize;
        for chunk in to_embed.chunks(SAVE_CHUNK_SIZE) {
            let texts: Vec<String> = chunk
                .iter()
                .map(|n| format!("{}{}", cfg.passage_prefix, n.text))
                .collect();
            let vectors = encoder
                .encode(&texts, Some((done, to_embed.len())))
                .unwrap_or_else(|e| exit_err(&e));
            for (note, vector) in chunk.iter().zip(vectors) {
                new_embed_map.insert(note.path.as_str(), vector);
            }
            saved_stats = Some(
                save_embedding_snapshot(cfg, &notes, &new_embed_map, &old_embeddings, &old_hashes)
                    .unwrap_or_else(|e| exit_err(&e)),
            );
            done += chunk.len();
        }
        eprintln!();
    }

    // 削除のみの更新では計算 chunk が無いため、ここで最終状態を保存する。
    let (note_count, vector_dim) = saved_stats.unwrap_or_else(|| {
        save_embedding_snapshot(cfg, &notes, &new_embed_map, &old_embeddings, &old_hashes)
            .unwrap_or_else(|e| exit_err(&e))
    });

    println!("埋め込みを保存しました: {}", display_path(&embeddings_file));
    println!("  ノート数: {note_count}");
    println!("  ベクトル次元: {vector_dim}");
}

/// semantic 検索の (path, cosine 類似度) を best-first で返す。
/// 埋め込み未構築・破損は Err (呼び出し側で fallback / 明示エラー)。
/// モデルと query prefix は埋め込み生成時の metadata を優先する。
pub fn semantic_ranked(
    cfg: &Config,
    query: &str,
    top_n: usize,
) -> Result<Vec<(String, f64)>, String> {
    let embeddings_file = cfg.embeddings_dir().join("embeddings.safetensors");
    let metadata_file = cfg.embeddings_dir().join("metadata.json");
    if !embeddings_file.exists() || !metadata_file.exists() {
        return Err("埋め込みデータが見つかりません".to_string());
    }
    let meta = read_metadata(&metadata_file)?;
    let vectors = load_vectors(&embeddings_file)?;
    if vectors.len() != meta.notes.len() {
        return Err(format!(
            "埋め込み ({}行) と metadata ({}件) が一致しません。mikke embed --force で再構築してください",
            vectors.len(),
            meta.notes.len()
        ));
    }

    let model_id = meta.model.as_deref().unwrap_or(&cfg.model);
    let query_prefix = meta.query_prefix.as_deref().unwrap_or(&cfg.query_prefix);
    let encoder = Encoder::load(model_id)?;
    let query_vec = encoder
        .encode(&[format!("{query_prefix}{query}")], None)?
        .pop()
        .ok_or("クエリの埋め込みに失敗")?;
    // 次元不一致 (破損・別モデルの残骸) は zip で黙って切り詰めず明示エラー
    if let Some(first) = vectors.first() {
        if first.len() != query_vec.len() {
            return Err(format!(
                "埋め込み次元 ({}) とクエリ次元 ({}) が一致しません。mikke embed --force で再構築してください",
                first.len(),
                query_vec.len()
            ));
        }
    }

    // 保存ベクトルは正規化済み → 内積 = cosine 類似度
    let mut sims: Vec<(usize, f64)> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let dot: f64 = v
                .iter()
                .zip(&query_vec)
                .map(|(a, b)| (*a as f64) * (*b as f64))
                .sum();
            (i, dot)
        })
        .collect();
    sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    sims.truncate(top_n);
    Ok(sims
        .into_iter()
        .map(|(i, s)| (meta.notes[i].path.clone(), s))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_epoch_zero() {
        assert_eq!(iso8601_utc(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso8601_known_dates() {
        // date -u -d '2026-07-20 12:34:56' +%s = 1784550896
        assert_eq!(iso8601_utc(1_784_550_896), "2026-07-20T12:34:56Z");
        // うるう年 2 月末日: 2024-02-29 23:59:59 = 1709251199
        assert_eq!(iso8601_utc(1_709_251_199), "2024-02-29T23:59:59Z");
        // 年初 (m <= 2 の分岐): 2025-01-01 00:00:00 = 1735689600
        assert_eq!(iso8601_utc(1_735_689_600), "2025-01-01T00:00:00Z");
    }

    #[test]
    fn snapshot_keeps_old_hash_for_pending_updated_note() {
        let notes = vec![
            CollectedNote {
                path: "computed.md".to_string(),
                title: "computed".to_string(),
                text: String::new(),
                hash: "new-computed".to_string(),
            },
            CollectedNote {
                path: "pending-updated.md".to_string(),
                title: "pending updated".to_string(),
                text: String::new(),
                hash: "new-pending".to_string(),
            },
            CollectedNote {
                path: "pending-new.md".to_string(),
                title: "pending new".to_string(),
                text: String::new(),
                hash: "new-note".to_string(),
            },
        ];
        let new_embeddings = HashMap::from([("computed.md", vec![1.0])]);
        let old_embeddings = HashMap::from([
            ("computed.md".to_string(), vec![9.0]),
            ("pending-updated.md".to_string(), vec![2.0]),
            ("deleted.md".to_string(), vec![3.0]),
        ]);
        let old_hashes = HashMap::from([
            ("computed.md", "old-computed"),
            ("pending-updated.md", "old-pending"),
            ("deleted.md", "old-deleted"),
        ]);

        let snapshot =
            assemble_embedding_snapshot(&notes, &new_embeddings, &old_embeddings, &old_hashes);

        assert_eq!(snapshot.vectors, vec![vec![1.0], vec![2.0]]);
        assert_eq!(snapshot.notes.len(), 2);
        assert_eq!(snapshot.notes[0].path, "computed.md");
        assert_eq!(snapshot.notes[0].hash, "new-computed");
        assert_eq!(snapshot.notes[1].path, "pending-updated.md");
        assert_eq!(snapshot.notes[1].hash, "old-pending");
    }
}
