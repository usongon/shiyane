use crate::error::transport_error;
use crate::translate::{TranslateProvider, TranslateRequest, TranslateResponse};
use crate::{Error, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 3;
const RETRY_BACKOFF_SECS: [u64; 2] = [2, 5];
/// 网络死路快速失败：连不上 10s 内报错，不空耗整个请求超时
const CONNECT_TIMEOUT_SECS: u64 = 10;

fn build_client(timeout_secs: u64) -> Client {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .build()
        .expect("failed to build reqwest client")
}

/// 兜底直连客户端：系统代理对个别请求可出现「连接建立但永不转发」的假死，
/// 最后一击绕开代理再试一次（国内直连 dashscope 可达）
fn build_client_no_proxy(timeout_secs: u64) -> Client {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
        .no_proxy()
        .build()
        .expect("failed to build reqwest client")
}

pub struct OpenAiCompatibleProvider {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub timeout_secs: u64,
}

impl OpenAiCompatibleProvider {
    /// DashScope qwen3.5+ 系列默认开启思考模式：非流式请求会等服务端完成完整推理
    /// （可达分钟级），对字幕翻译这类短输出是纯延迟，必须显式关闭。
    /// enable_thinking 是 DashScope 的私有扩展参数，按端点域判定，其余 OpenAI 兼容端点不发送。
    fn disables_thinking(&self) -> bool {
        self.base_url.contains("dashscope")
    }

    fn build_body(
        &self,
        messages: Vec<serde_json::Value>,
        temperature: f64,
        max_tokens: u32,
    ) -> serde_json::Value {
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": temperature,
            "max_tokens": max_tokens
        });
        if self.disables_thinking() {
            body["enable_thinking"] = json!(false);
        }
        body
    }
}

#[async_trait]
impl TranslateProvider for OpenAiCompatibleProvider {
    async fn translate(&self, req: TranslateRequest) -> Result<TranslateResponse> {
        // Build context from previous sentences
        let context_text = if req.context.is_empty() {
            String::new()
        } else {
            format!("Previous context:\n{}\n\n", req.context.join("\n"))
        };

        // Build messages with system prompt and few-shot examples
        let messages = vec![
            json!({
                "role": "system",
                "content": "You are a professional subtitle translator. Translate the given text naturally and accurately, preserving the tone, style, and context. Avoid literal word-for-word translation. Focus on conveying the meaning in fluent, idiomatic target language."
            }),
            json!({
                "role": "user",
                "content": "Translate from en to zh:\n\nWe have main engine start, 4, 3, 2, 1."
            }),
            json!({
                "role": "assistant",
                "content": "主发动机启动，4、3、2、1。"
            }),
            json!({
                "role": "user",
                "content": "Translate from en to zh:\n\nYou have your robotics, and I just want to be awesome in space."
            }),
            json!({
                "role": "assistant",
                "content": "你有你的机器人技术，而我只想在太空中大显身手。"
            }),
            json!({
                "role": "user",
                "content": format!("{}Translate from {} to {}:\n\n{}", context_text, req.source_lang, req.target_lang, req.text)
            }),
        ];

        // Build request body (OpenAI Chat Completions format)
        let body = self.build_body(messages, 0.3, 500);

        // Send request, retrying transient network errors (timeouts, dropped
        // connections). API-level errors are returned as-is without retry.
        // 第 3 击走无代理直连：排除「代理对个别请求假死」这一层
        let url = format!("{}/chat/completions", self.base_url);
        let mut attempt = 1u32;
        let response = loop {
            let client = if attempt == MAX_ATTEMPTS {
                build_client_no_proxy(self.timeout_secs)
            } else {
                build_client(self.timeout_secs)
            };
            match client
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => break resp,
                Err(e) if attempt < MAX_ATTEMPTS => {
                    tracing::warn!(
                        "Translate request failed (attempt {}/{}): {} — retrying in {}s",
                        attempt,
                        MAX_ATTEMPTS,
                        transport_error(&e),
                        RETRY_BACKOFF_SECS[(attempt - 1) as usize]
                    );
                    tokio::time::sleep(Duration::from_secs(
                        RETRY_BACKOFF_SECS[(attempt - 1) as usize],
                    ))
                    .await;
                    attempt += 1;
                }
                Err(e) => {
                    return Err(Error::Translate(format!(
                        "HTTP request failed after {} attempts: {}",
                        attempt,
                        transport_error(&e)
                    )));
                }
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Translate(format!(
                "API error {}: {}",
                status, error_text
            )));
        }

        // Parse response
        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Translate(format!("Failed to parse response: {}", e)))?;

        let translated_text = json["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| Error::Translate("Invalid response format".to_string()))?
            .to_string();

        Ok(TranslateResponse { translated_text })
    }

    async fn test_connection(&self) -> Result<()> {
        let client = build_client(self.timeout_secs);

        // Send minimal test request
        let body = self.build_body(
            vec![json!({
                "role": "user",
                "content": "hi"
            })],
            0.3,
            1,
        );

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                Error::Translate(format!("Connection test failed: {}", transport_error(&e)))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::Translate(format!(
                "API error {}: {}",
                status, error_text
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn provider_with_base(base_url: &str) -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider {
            base_url: base_url.to_string(),
            model: "qwen3.8-max".to_string(),
            api_key: "key".to_string(),
            timeout_secs: 1,
        }
    }

    // qwen3.5+ 默认开思考：非流式请求要等服务端推理完（分钟级），60s 客户端超时
    // 会被掐死——DashScope 端点必须显式 enable_thinking=false
    #[test]
    fn dashscope_body_disables_thinking() {
        let p = provider_with_base("https://dashscope.aliyuncs.com/compatible-mode/v1");
        let body = p.build_body(vec![json!({"role": "user", "content": "hi"})], 0.3, 500);
        assert_eq!(body["enable_thinking"], json!(false));
    }

    // enable_thinking 是 DashScope 私有扩展，其余 OpenAI 兼容端点不发送未知字段
    #[test]
    fn openai_body_omits_thinking_param() {
        let p = provider_with_base("https://api.openai.com/v1");
        let body = p.build_body(vec![json!({"role": "user", "content": "hi"})], 0.3, 500);
        assert!(body.get("enable_thinking").is_none());
    }

    // Regression: a server that accepts connections but never responds must
    // produce an error within a bounded time (timeout + retries), not hang.
    #[tokio::test]
    async fn translate_hanging_server_times_out_and_retries() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = connections.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                held.push(sock); // hold open so the client can only time out
            }
        });

        let provider = OpenAiCompatibleProvider {
            base_url: format!("http://{}", addr),
            model: "test-model".to_string(),
            api_key: "key".to_string(),
            timeout_secs: 1,
        };
        let req = TranslateRequest {
            text: "hi".to_string(),
            source_lang: "en".to_string(),
            target_lang: "zh".to_string(),
            context: vec![],
            glossary: None,
        };

        let start = std::time::Instant::now();
        let result = provider.translate(req).await;
        let elapsed = start.elapsed();

        assert!(result.is_err(), "expected error, got {:?}", result);
        // 3 attempts × 1s timeout + 2s/5s backoff ≈ 10s; must not hang
        assert!(elapsed >= Duration::from_secs(8), "too fast: {:?}", elapsed);
        assert!(elapsed < Duration::from_secs(30), "too slow: {:?}", elapsed);
        assert!(
            connections.load(Ordering::SeqCst) >= 2,
            "expected retries, connections = {}",
            connections.load(Ordering::SeqCst)
        );
    }
}
