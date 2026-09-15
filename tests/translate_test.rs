use pick_up_sound_text::translate::{TranslateProvider, TranslateRequest};
use pick_up_sound_text::translate::openai::OpenAiCompatibleProvider;

// This test requires a real API key and network access
// It is marked as ignored by default

#[tokio::test]
#[ignore]
async fn test_openai_provider_creation() {
    let provider = OpenAiCompatibleProvider {
        base_url: "https://api.openai.com/v1".to_string(),
        model: "gpt-3.5-turbo".to_string(),
        api_key: std::env::var("OPENAI_API_KEY").unwrap_or_else(|_| "test_key".to_string()),
        timeout_secs: 60,
    };

    let req = TranslateRequest {
        text: "Hello".to_string(),
        source_lang: "en".to_string(),
        target_lang: "zh".to_string(),
        context: vec![],
        glossary: None,
    };

    let result = provider.translate(req).await;
    assert!(result.is_ok());
}
