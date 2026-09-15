//! Checkpoint：断点续传事件日志。
//!
//! `progress.jsonl` 格式（追加式）：
//! - 第 1 行 meta：`{"task_id","video_path","fingerprint":{...}}`
//! - 后续行：句子事件。ASR 完成后整批写 Pending（含 source/时间戳），
//!   每句翻译完成追加更新行（同 segment_id，加载时折叠：status/translated
//!   取最新，source/时间戳取首次）。崩溃撕裂的尾部行跳过；meta 损坏则
//!   归档为 `.corrupt` 并视为无 checkpoint。
//! 电影规模（~2500 句）下每句落盘 O(1)；save() 全量重写用于终态压缩与指纹重置。

use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SegmentStatus {
    Pending,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentProgress {
    pub segment_id: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_time: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_time: Option<f64>,
    pub status: SegmentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated: Option<String>,
}

/// 配置指纹：任一字段变化会使对应阶段的缓存结果失效。
/// - source_language / asr_model 变化 → ASR 结果无效（全量重跑）
/// - target_lang / translate_provider / translate_model 变化 → 仅译文无效（重翻）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CheckpointFingerprint {
    #[serde(default)]
    pub source_language: String,
    #[serde(default)]
    pub target_lang: String,
    #[serde(default)]
    pub translate_provider: String,
    #[serde(default)]
    pub translate_model: String,
    #[serde(default)]
    pub asr_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointMeta {
    task_id: String,
    video_path: PathBuf,
    #[serde(default)]
    fingerprint: CheckpointFingerprint,
}

#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub task_id: String,
    pub video_path: PathBuf,
    pub fingerprint: CheckpointFingerprint,
    /// 折叠后的句子状态，按 segment_id 升序
    pub segments: Vec<SegmentProgress>,
}

impl Checkpoint {
    pub fn new(task_id: String, video_path: PathBuf, fingerprint: CheckpointFingerprint) -> Self {
        Self {
            task_id,
            video_path,
            fingerprint,
            segments: Vec::new(),
        }
    }

    /// 加载并折叠事件日志。
    /// - 文件不存在 / meta 损坏（改名 `.corrupt` 归档）→ `Ok(None)`
    /// - 句子行损坏（如崩溃撕裂的半行）→ 跳过该行并 warn
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };

        let mut lines = content.lines().filter(|l| !l.trim().is_empty());
        let meta_line = match lines.next() {
            Some(l) => l,
            None => return Ok(None), // 空文件，视为无 checkpoint
        };
        let meta: CheckpointMeta = match serde_json::from_str(meta_line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("checkpoint meta 行损坏（{e}），归档 {} 并从头开始", path.display());
                let mut corrupt = path.as_os_str().to_os_string();
                corrupt.push(".corrupt");
                let _ = std::fs::rename(path, corrupt);
                return Ok(None);
            }
        };

        let mut folded: Vec<SegmentProgress> = Vec::new();
        let mut index: HashMap<usize, usize> = HashMap::new();
        for line in lines {
            match serde_json::from_str::<SegmentProgress>(line) {
                Ok(seg) => {
                    if let Some(&i) = index.get(&seg.segment_id) {
                        fold_into(&mut folded[i], seg);
                    } else {
                        index.insert(seg.segment_id, folded.len());
                        folded.push(seg);
                    }
                }
                Err(e) => tracing::warn!("跳过损坏的 checkpoint 行: {e}"),
            }
        }
        folded.sort_by_key(|s| s.segment_id);

        Ok(Some(Self {
            task_id: meta.task_id,
            video_path: meta.video_path,
            fingerprint: meta.fingerprint,
            segments: folded,
        }))
    }

    /// 全量重写（.tmp + rename 原子替换）。用于：ASR 后写 Pending 批、
    /// 指纹重置、全部完成后的终态压缩。
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            let meta = CheckpointMeta {
                task_id: self.task_id.clone(),
                video_path: self.video_path.clone(),
                fingerprint: self.fingerprint.clone(),
            };
            writeln!(file, "{}", serde_json::to_string(&meta)?)?;
            for seg in &self.segments {
                writeln!(file, "{}", serde_json::to_string(seg)?)?;
            }
            file.flush()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 追加句子事件行（单次 write 每行，不 fsync）。
    pub fn append_updates(path: &Path, updates: &[SegmentProgress]) -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        for u in updates {
            writeln!(file, "{}", serde_json::to_string(u)?)?;
        }
        Ok(())
    }

    pub fn completed_count(&self) -> usize {
        self.segments
            .iter()
            .filter(|s| s.status == SegmentStatus::Completed)
            .count()
    }

    pub fn is_all_completed(&self) -> bool {
        !self.segments.is_empty()
            && self.segments.iter().all(|s| s.status == SegmentStatus::Completed)
    }

    pub fn percent(&self) -> f64 {
        if self.segments.is_empty() {
            0.0
        } else {
            self.completed_count() as f64 / self.segments.len() as f64
        }
    }
}

/// 更新行并入已折叠的句子：status 总是取最新；Option 字段仅在新行有值时覆盖。
fn fold_into(existing: &mut SegmentProgress, next: SegmentProgress) {
    if next.start_time.is_some() {
        existing.start_time = next.start_time;
    }
    if next.end_time.is_some() {
        existing.end_time = next.end_time;
    }
    if next.source.is_some() {
        existing.source = next.source;
    }
    if next.translated.is_some() {
        existing.translated = next.translated;
    }
    existing.status = next.status;
}

/// task_id = FNV-1a(路径 + 文件大小 + mtime 纳秒)。
/// 同文件重跑命中同一 checkpoint；同路径换文件天然隔离。
pub fn compute_task_id(video_path: &Path) -> Result<String> {
    let md = std::fs::metadata(video_path)?;
    let modified_nanos = md
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8], hash: &mut u64| {
        for b in bytes {
            *hash ^= *b as u64;
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    feed(video_path.to_string_lossy().as_bytes(), &mut hash);
    feed(&md.len().to_le_bytes(), &mut hash);
    feed(&modified_nanos.to_le_bytes(), &mut hash);
    Ok(format!("{:x}", hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_keeps_first_times_and_latest_status() {
        let mut existing = SegmentProgress {
            segment_id: 0,
            start_time: Some(1.0),
            end_time: Some(2.0),
            status: SegmentStatus::Pending,
            source: Some("src".into()),
            translated: None,
        };
        let next = SegmentProgress {
            segment_id: 0,
            start_time: None,
            end_time: None,
            status: SegmentStatus::Completed,
            source: None,
            translated: Some("译文".into()),
        };
        fold_into(&mut existing, next);
        assert_eq!(existing.start_time, Some(1.0));
        assert_eq!(existing.source.as_deref(), Some("src"));
        assert_eq!(existing.status, SegmentStatus::Completed);
        assert_eq!(existing.translated.as_deref(), Some("译文"));
    }
}
