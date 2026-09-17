pub mod keystore;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsrConfig {
    pub provider: String,
    pub api_key: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub file_model: String,
    pub realtime_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateConfig {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub target_lang: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OssConfig {
    pub endpoint: String,
    pub bucket: String,
    pub access_key_id: String,
    pub access_key_secret: String,
    #[serde(default)]
    pub path_prefix: Option<String>,
}

/// Preset base_url and default model for known translate providers.
/// Returns (base_url, default_model).
pub fn translate_provider_preset(provider: &str) -> (&'static str, &'static str) {
    match provider {
        "openai" => ("https://api.openai.com/v1", "gpt-3.5-turbo"),
        "dashscope" => ("https://dashscope.aliyuncs.com/compatible-mode/v1", "qwen-turbo"),
        "deepseek" => ("https://api.deepseek.com/v1", "deepseek-chat"),
        "kimi" => ("https://api.moonshot.cn/v1", "moonshot-v1-8k"),
        _ => ("https://api.openai.com/v1", "gpt-3.5-turbo"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub asr: AsrConfig,
    pub translate: TranslateConfig,
    #[serde(default)]
    pub oss: Option<OssConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            asr: AsrConfig {
                provider: "dashscope".to_string(),
                api_key: "".to_string(),
                workspace_id: None,
                file_model: "qwen-audio-3.0-asr-flash-filetrans".to_string(),
                realtime_model: "qwen-audio-3.0-asr-flash-streaming".to_string(),
            },
            translate: TranslateConfig {
                provider: "openai".to_string(),
                model: "gpt-3.5-turbo".to_string(),
                api_key: "".to_string(),
                target_lang: "zh".to_string(),
            },
            oss: None,
        }
    }
}

impl AppConfig {
    pub fn config_path() -> Result<PathBuf> {
        let home = dirs::home_dir()
            .ok_or_else(|| Error::Config("Cannot find home directory".to_string()))?;
        Ok(home.join("Library/Application Support/pick-up-sound-text/config.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)?;
        let mut config: AppConfig = serde_json::from_str(&content)?;

        // 存量配置硬迁移：2026-09 模型更名，老值会被服务端拒为 InvalidParameter
        if config.asr.realtime_model == "qwen-audio-3.0-asr-flash" {
            config.asr.realtime_model = "qwen-audio-3.0-asr-flash-streaming".to_string();
        }

        // Decrypt API keys
        let keystore = keystore::KeyStore::new()?;
        if !config.asr.api_key.is_empty() {
            config.asr.api_key = keystore.decrypt(&config.asr.api_key)?;
        }
        if !config.translate.api_key.is_empty() {
            config.translate.api_key = keystore.decrypt(&config.translate.api_key)?;
        }
        
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        
        // Encrypt API keys before saving
        let keystore = keystore::KeyStore::new()?;
        let mut config = self.clone();
        if !config.asr.api_key.is_empty() {
            config.asr.api_key = keystore.encrypt(&config.asr.api_key)?;
        }
        if !config.translate.api_key.is_empty() {
            config.translate.api_key = keystore.encrypt(&config.translate.api_key)?;
        }
        
        let content = serde_json::to_string_pretty(&config)?;
        std::fs::write(&path, content)?;
        
        // Set permissions to 0600 (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&path, perms)?;
        }
        
        Ok(())
    }
}
