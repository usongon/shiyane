use crate::asr::{AsrConfig, AsrEvent, AsrProvider, AsrStream};
use crate::{Error, Result};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use uuid::Uuid;

pub struct DashScopeAsrProvider;

#[async_trait]
impl AsrProvider for DashScopeAsrProvider {
    async fn start_stream(&self, config: &AsrConfig) -> Result<Box<dyn AsrStream>> {
        let task_id = Uuid::new_v4().to_string();
        
        // WebSocket URL with Workspace ID (required for Beijing region)
        let url = if let Some(workspace_id) = &config.workspace_id {
            format!("wss://{}.cn-beijing.maas.aliyuncs.com/api-ws/v1/inference", workspace_id)
        } else {
            // Fallback to generic domain (may not work for all regions)
            "wss://dashscope.aliyuncs.com/api-ws/v1/inference".to_string()
        };
        
        // Create request and add Authorization header
        let api_key = config.api_key.trim();
        let mut request = url
            .into_client_request()
            .map_err(|e| Error::Asr(format!("Failed to create request: {}", e)))?;
        
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", api_key)
                .parse()
                .map_err(|e| Error::Asr(format!("Invalid header value: {}", e)))?,
        );
        
        tracing::debug!("WebSocket request headers: {:?}", request.headers());
        tracing::debug!("API key length: {} chars", api_key.len());
        
        // Create WebSocket connection
        let (ws_stream, response) = connect_async(request)
            .await
            .map_err(|e| Error::Asr(format!("WebSocket connection failed: {}", e)))?;
        
        tracing::debug!("WebSocket handshake response: {:?}", response);
        
        let (mut write, mut read) = ws_stream.split();
        
        // Send run-task message
        // 参数对齐 Qwen-Audio-ASR-Streaming 官方 schema：
        // https://help.aliyun.com/zh/model-studio/fun-asr-client-events
        // - 不传旧版字段（punctuation_prediction_enabled 等，会报 InvalidParameter）
        // - language_hints 只接受语种码，auto 时省略让模型自判
        // - heartbeat 保活，避免静音期被服务端断连
        let mut parameters = json!({
            "format": "pcm",
            "sample_rate": 16000,
            "semantic_punctuation_enabled": false,
            "max_sentence_silence": 1300,
            "heartbeat": true
        });
        if config.language != "auto" && !config.language.is_empty() {
            parameters["language_hints"] = json!([config.language]);
        }
        let run_task = json!({
            "header": {
                "action": "run-task",
                "task_id": task_id,
                "streaming": "duplex"
            },
            "payload": {
                "task_group": "audio",
                "task": "asr",
                "function": "recognition",
                "model": config.model,
                "input": {},
                "parameters": parameters
            }
        });
        
        write
            .send(Message::Text(run_task.to_string()))
            .await
            .map_err(|e| Error::Asr(format!("Failed to send run-task: {}", e)))?;
        
        // Wait for task-started
        let (tx, rx) = mpsc::channel(100);
        
        // Spawn task to read WebSocket messages
        tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(event) = json["header"]["event"].as_str() {
                                match event {
                                    "task-started" => {
                                        // Task is ready to receive audio; no event needed.
                                    }
                                    "result-generated" => {
                                        tracing::debug!("DashScope result-generated raw JSON: {}", serde_json::to_string_pretty(&json).unwrap_or_default());
                                        
                                        if let Some(sentence) = json["payload"]["output"]["sentence"].as_object() {
                                            let text = sentence["text"].as_str().unwrap_or("").to_string();
                                            let begin_time = sentence["begin_time"].as_f64().unwrap_or(0.0) / 1000.0;
                                            let end_time = sentence["end_time"].as_f64().unwrap_or(0.0) / 1000.0;
                                            let sentence_end = sentence["sentence_end"].as_bool().unwrap_or(false);
                                            
                                            tracing::debug!("Parsed sentence: text='{}' ({} chars), begin={}, end={}, sentence_end={}", text, text.len(), begin_time, end_time, sentence_end);
                                            
                                            if sentence_end {
                                                let _ = tx.send(Ok(AsrEvent::Final {
                                                    text,
                                                    ts_start: begin_time,
                                                    ts_end: end_time,
                                                })).await;
                                            } else {
                                                let _ = tx.send(Ok(AsrEvent::Partial {
                                                    text,
                                                    ts_start: begin_time,
                                                    ts_end: end_time,
                                                })).await;
                                            }
                                        } else {
                                            tracing::warn!("No 'sentence' object in payload.output");
                                        }
                                    }
                                    "task-finished" => {
                                        let _ = tx.send(Ok(AsrEvent::EndOfStream)).await;
                                        break;
                                    }
                                    "task-failed" => {
                                        let error_code = json["header"]["error_code"].as_str().unwrap_or("UNKNOWN").to_string();
                                        let error_message = json["header"]["error_message"].as_str().unwrap_or("Unknown error").to_string();
                                        let _ = tx.send(Ok(AsrEvent::Error {
                                            code: error_code,
                                            message: error_message,
                                        })).await;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        let _ = tx.send(Ok(AsrEvent::EndOfStream)).await;
                        break;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Error::Asr(format!("WebSocket error: {}", e)))).await;
                        break;
                    }
                    _ => {}
                }
            }
        });
        
        Ok(Box::new(DashScopeAsrStream {
            write,
            rx,
            task_id,
        }))
    }
}

struct DashScopeAsrStream {
    write: futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>,
    rx: mpsc::Receiver<Result<AsrEvent>>,
    task_id: String,
}

#[async_trait]
impl AsrStream for DashScopeAsrStream {
    async fn send_audio(&mut self, pcm: &[i16]) -> Result<()> {
        // Convert i16 samples to bytes (little-endian)
        let bytes: Vec<u8> = pcm.iter()
            .flat_map(|&sample| sample.to_le_bytes())
            .collect();
        
        // Log first call details to verify audio data
        if pcm.len() > 0 {
            let non_zero_count = pcm.iter().filter(|&&s| s != 0).count();
            tracing::debug!("Sending audio: {} samples ({} bytes), {} non-zero samples, first 10: {:?}", 
                pcm.len(), bytes.len(), non_zero_count, &pcm[..std::cmp::min(10, pcm.len())]);
        }
        
        self.write
            .send(Message::Binary(bytes))
            .await
            .map_err(|e| Error::Asr(format!("Failed to send audio: {}", e)))?;
        
        Ok(())
    }
    
    async fn next_event(&mut self) -> Result<AsrEvent> {
        self.rx.recv().await
            .ok_or_else(|| Error::Asr("WebSocket channel closed".to_string()))?
    }

    async fn finish(&mut self) -> Result<()> {
        let finish_task = json!({
            "header": {
                "action": "finish-task",
                "task_id": self.task_id,
                "streaming": "duplex"
            },
            "payload": {
                "input": {}
            }
        });
        self.write
            .send(Message::Text(finish_task.to_string()))
            .await
            .map_err(|e| Error::Asr(format!("Failed to send finish-task: {}", e)))?;
        Ok(())
    }
}
