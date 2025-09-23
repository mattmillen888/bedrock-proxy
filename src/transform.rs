use serde_json::{json, Value};

pub fn transform_payload(mut payload: Value) -> Value {
    if let Some(prompt) = payload.get("prompt").and_then(|p| p.as_str()) {
        payload = json!({
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": payload.get("max_tokens_to_sample").unwrap_or(&json!(200))
        });
    }

    if payload.get("max_tokens_to_sample").is_some() && payload.get("max_tokens").is_none() {
        if let Some(max_tokens) = payload.get("max_tokens_to_sample") {
            payload["max_tokens"] = max_tokens.clone();
            payload
                .as_object_mut()
                .unwrap()
                .remove("max_tokens_to_sample");
        }
    }

    payload["anthropic_version"] = json!("bedrock-2023-05-31");

    payload
}