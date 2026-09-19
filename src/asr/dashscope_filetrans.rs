use crate::asr::{AsrConfig, FileAsrProvider, FileTranscriptionResult, TranscriptionSentence};
use crate::config::OssConfig;
use crate::oss::OssUploader;
use crate::{Error, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::hash::{Hash, Hasher};
use std::path::Path;
use tokio::time::{sleep, Duration};

pub struct DashScopeFileTransProvider {
    pub oss_config: Option<OssConfig>,
}

/// filetrans 提交 parameters（纯函数便于单测）：说话人分离按开关携带
fn build_submit_parameters(language: &str, diarization: bool) -> serde_json::Value {
    let mut params = json!({
        "channel_id": [0],
        "language_hints": [language],
    });
    if diarization {
        params["diarization_enabled"] = json!(true);
    }
    params
}

#[async_trait]
impl FileAsrProvider for DashScopeFileTransProvider {
    async fn transcribe_file(&self, config: &AsrConfig, audio_path: &Path) -> Result<FileTranscriptionResult> {
        // 提交/轮询/下载结果均为小请求，超时兜底防永久悬死
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client");
        let api_key = config.api_key.trim();
        
        // Build base URL with workspace ID
        let base_url = if let Some(workspace_id) = &config.workspace_id {
            format!("https://{}.cn-beijing.maas.aliyuncs.com/api/v1", workspace_id)
        } else {
            return Err(Error::Asr("Workspace ID is required for file transcription".to_string()));
        };
        
        // Step 1: Upload audio file to OSS to get a public URL
        let oss_config = self.oss_config.as_ref()
            .ok_or_else(|| Error::Asr("OSS config is required for file transcription".to_string()))?;
        
        let uploader = OssUploader::new(oss_config.clone());
        let object_name = oss_object_name(audio_path);
        let file_url = uploader.upload_file(audio_path, &object_name).await?;
        tracing::info!("Uploaded audio file to OSS: {}", file_url);
        
        // Step 2: Submit transcription task
        let submit_url = format!("{}/services/audio/asr/transcription", base_url);
        let submit_body = json!({
            "model": config.model,
            "input": {
                "file_urls": [file_url.clone()]
            },
            "parameters": build_submit_parameters(&config.language, config.diarization)
        });
        
        let submit_resp = client
            .post(&submit_url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .header("X-DashScope-Async", "enable")
            .json(&submit_body)
            .send()
            .await
            .map_err(|e| Error::Asr(format!("Failed to submit task: {}", e)))?;
        
        if !submit_resp.status().is_success() {
            let status = submit_resp.status();
            let error_text = submit_resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Asr(format!("Submit task failed {}: {}", status, error_text)));
        }
        
        let submit_json: serde_json::Value = submit_resp
            .json()
            .await
            .map_err(|e| Error::Asr(format!("Failed to parse submit response: {}", e)))?;
        
        let task_id = submit_json["output"]["task_id"]
            .as_str()
            .ok_or_else(|| Error::Asr("No task_id in submit response".to_string()))?;
        
        tracing::info!("Submitted transcription task: {}", task_id);
        
        // Step 3: Poll task status until completed
        let query_url = format!("{}/tasks/{}", base_url, task_id);
        let transcription_url = loop {
            sleep(Duration::from_secs(2)).await;
            
            let query_resp = client
                .get(&query_url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| Error::Asr(format!("Failed to query task: {}", e)))?;
            
            if !query_resp.status().is_success() {
                let status = query_resp.status();
                let error_text = query_resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                return Err(Error::Asr(format!("Query task failed {}: {}", status, error_text)));
            }
            
            let query_json: serde_json::Value = query_resp
                .json()
                .await
                .map_err(|e| Error::Asr(format!("Failed to parse query response: {}", e)))?;
            
            let task_status = query_json["output"]["task_status"]
                .as_str()
                .unwrap_or("UNKNOWN");
            
            tracing::debug!("Task status: {}", task_status);
            
            match task_status {
                "SUCCEEDED" => {
                    let url = query_json["output"]["results"][0]["transcription_url"]
                        .as_str()
                        .ok_or_else(|| Error::Asr("No transcription_url in response".to_string()))?;
                    break url.to_string();
                }
                "FAILED" | "UNKNOWN" => {
                    let error_msg = query_json["output"]["message"]
                        .as_str()
                        .unwrap_or("Unknown error");
                    return Err(Error::Asr(format!("Transcription failed: {}", error_msg)));
                }
                _ => {
                    // PENDING or RUNNING, continue polling
                    continue;
                }
            }
        };
        
        tracing::info!("Transcription completed, downloading result from: {}", transcription_url);
        
        // Step 4: Download transcription result
        let result_resp = client
            .get(&transcription_url)
            .send()
            .await
            .map_err(|e| Error::Asr(format!("Failed to download result: {}", e)))?;
        
        if !result_resp.status().is_success() {
            let status = result_resp.status();
            let error_text = result_resp.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Asr(format!("Download result failed {}: {}", status, error_text)));
        }
        
        let result_json: serde_json::Value = result_resp
            .json()
            .await
            .map_err(|e| Error::Asr(format!("Failed to parse result JSON: {}", e)))?;
        
        // Step 5: Parse sentences
        let mut sentences = Vec::new();
        if let Some(transcripts) = result_json["transcripts"].as_array() {
            for transcript in transcripts {
                if let Some(sents) = transcript["sentences"].as_array() {
                    for sent in sents {
                        let text = sent["text"].as_str().unwrap_or("").to_string();
                        let begin_time = sent["begin_time"].as_f64().unwrap_or(0.0) / 1000.0;
                        let end_time = sent["end_time"].as_f64().unwrap_or(0.0) / 1000.0;
                        
                        sentences.push(TranscriptionSentence {
                            text,
                            begin_time,
                            end_time,
                        });
                    }
                }
            }
        }
        
        tracing::info!("Parsed {} sentences from transcription result", sentences.len());

        // Step 6: Clean up OSS file (optional, ignore errors)
        tracing::info!("Deleting OSS file: {}", object_name);
        if let Err(e) = uploader.delete_file(&object_name).await {
            tracing::warn!("Failed to delete OSS file {}: {}", object_name, e);
        } else {
            tracing::info!("OSS file deleted successfully: {}", object_name);
        }

        Ok(FileTranscriptionResult { sentences })
    }
}

/// OSS 对象名按音频路径确定性派生：转写中暂停会丢弃 in-flight future、
/// 跳过 Step 6 清理；确定性命名让泄漏有界（同视频重跑直接覆盖同名对象）
fn oss_object_name(audio_path: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    audio_path.hash(&mut hasher);
    format!("shiyane-{:x}.wav", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oss_object_name_is_deterministic_per_audio() {
        let a = Path::new("/tmp/shiyane-abc.wav");
        assert_eq!(oss_object_name(a), oss_object_name(a));
        assert_ne!(oss_object_name(a), oss_object_name(Path::new("/tmp/other.wav")));
        let n = oss_object_name(a);
        assert!(n.starts_with("shiyane-") && n.ends_with(".wav"));
    }

    #[test]
    fn submit_parameters_omit_diarization_when_off() {
        let p = build_submit_parameters("zh", false);
        assert!(p.get("diarization_enabled").is_none(), "关闭时不得携带该键");
        assert_eq!(p["language_hints"][0], "zh");
        assert_eq!(p["channel_id"][0], 0);
    }

    #[test]
    fn submit_parameters_enable_diarization_when_on() {
        let p = build_submit_parameters("auto", true);
        assert_eq!(p["diarization_enabled"], true);
        assert_eq!(p["language_hints"][0], "auto");
    }
}
