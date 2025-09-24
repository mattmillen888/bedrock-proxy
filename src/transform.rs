use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Deserialize, Serialize, Clone)]
pub struct OpenAIMessage {
    pub role: String,
    pub content: Option<Value>, // Can be string, array, or object
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String, // always "function"
    pub function: FunctionCall,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String, // JSON string
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Tool {
    pub r#type: String, // "function"
    pub function: FunctionDefinition,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: Option<String>,
    pub parameters: Option<Value>,
}

#[derive(Deserialize, Serialize)]
pub struct OpenAIRequest {
    pub messages: Vec<OpenAIMessage>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stream: Option<bool>,
    #[allow(dead_code)]
    pub tools: Option<Vec<Tool>>,
    #[allow(dead_code)]
    pub tool_choice: Option<Value>,
}

#[derive(Serialize)]
pub struct OpenAIChoice {
    pub index: i32,
    pub message: OpenAIMessage,
    pub finish_reason: String,
}

#[derive(Serialize)]
pub struct OpenAIUsage {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub total_tokens: i32,
}

#[derive(Serialize)]
pub struct OpenAIResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIChoice>,
    pub usage: OpenAIUsage,
}

// Streaming types must be public
#[derive(Serialize, Clone)] // Make the struct public for external use
pub struct OpenAIStreamChoice {
    pub delta: serde_json::Value,
    pub index: i32,
    pub finish_reason: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct OpenAIStreamResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<OpenAIStreamChoice>,
}

// --------------------------------------------------
// Transform raw payload into Bedrock-compatible format
// --------------------------------------------------
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

// --------------------------------------------------
// Convert OpenAIRequest → Bedrock JSON
// --------------------------------------------------
pub fn openai_to_bedrock(req: &OpenAIRequest) -> Value {
    let mut system_prompts: Vec<String> = Vec::new();

    let messages: Vec<Value> = req
        .messages
        .iter()
        .filter_map(|m| {
            if m.role == "system" {
                if let Some(c) = &m.content {
                    match c {
                        Value::String(s) => system_prompts.push(s.clone()),
                        _ => system_prompts.push(c.to_string()),
                    }
                }
                None
            } else if m.role == "assistant" {
                // Handle assistant tool calls
                if let Some(Value::Object(map)) = &m.content {
                    if let Some(tool_calls) = map.get("tool_calls") {
                        let default_tool_calls = vec![];
                        let tool_calls_array = tool_calls.as_array().unwrap_or(&default_tool_calls);

                        let tool_use_contents: Vec<Value> = tool_calls_array
                            .iter()
                            .map(|tc| {
                                let func_default = json!({});
                                let func = tc.get("function").unwrap_or(&func_default);

                                json!({
                                    "type": "tool_use",
                                    "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or("tool_call_1"),
                                    "name": func.get("name").and_then(|v| v.as_str()).unwrap_or("unknown_tool"),
                                    "input": func.get("arguments").unwrap_or(&json!({}))
                                })
                            })
                            .collect();

                        Some(json!({
                            "role": "assistant",
                            "content": tool_use_contents
                        }))
                    } else {
                        Some(json!({
                            "role": "assistant",
                            "content": m.content.clone().unwrap_or(Value::String("".to_string()))
                        }))
                    }
                } else {
                    Some(json!({
                        "role": "assistant",
                        "content": m.content.clone().unwrap_or(Value::String("".to_string()))
                    }))
                }
            } else if m.role == "tool" {
                Some(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": m.tool_call_id.clone().unwrap_or_else(|| "tool_call_1".to_string()),
                        "content": m.content.clone().unwrap_or(Value::String("".to_string()))
                    }]
                }))
            } else {
                Some(json!({
                    "role": m.role,
                    "content": m.content.clone().unwrap_or(Value::String("".to_string()))
                }))
            }
        })
        .collect();

    let merged_system = if !system_prompts.is_empty() {
        Some(system_prompts.join("\n\n"))
    } else {
        None
    };

    let mut payload = json!({
        "anthropic_version": "bedrock-2023-05-31",
        "messages": messages,
        "max_tokens": req.max_tokens.unwrap_or(512),
        "temperature": req.temperature.unwrap_or(0.7),
    });

    if let Some(sys) = merged_system {
        payload["system"] = Value::String(sys);
    }

    payload
}

// --------------------------------------------------
// Convert Bedrock JSON → OpenAIResponse
// --------------------------------------------------
pub fn bedrock_to_openai(resp: &Value, model: &str) -> OpenAIResponse {
    let mut message_content: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut finish_reason = "stop";

    if let Some(content_array) = resp.get("content").and_then(|c| c.as_array()) {
        for block in content_array {
            if let Some(block_type) = block.get("type").and_then(|t| t.as_str()) {
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            message_content = Some(text.to_string());
                        }
                    }
                    "tool_use" => {
                        if let (Some(id), Some(name), Some(input)) = (
                            block.get("id").and_then(|i| i.as_str()),
                            block.get("name").and_then(|n| n.as_str()),
                            block.get("input"),
                        ) {
                            tool_calls.push(ToolCall {
                                id: id.to_string(),
                                r#type: "function".to_string(),
                                function: FunctionCall {
                                    name: name.to_string(),
                                    arguments: serde_json::to_string(input)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                },
                            });
                            finish_reason = "tool_calls";
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let prompt_tokens = resp
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0) as i32;

    let completion_tokens = resp
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|t| t.as_i64())
        .unwrap_or(0) as i32;

    let total_tokens = prompt_tokens + completion_tokens;

    let message = OpenAIMessage {
        role: "assistant".to_string(),
        content: message_content.map(Value::String),
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
    };

    let choice = OpenAIChoice {
        index: 0,
        message,
        finish_reason: finish_reason.to_string(),
    };

    OpenAIResponse {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
        model: model.to_string(),
        choices: vec![choice],
        usage: OpenAIUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        },
    }
}

// --------------------------------------------------
// Convert Bedrock streaming chunk → OpenAI streaming chunk
// --------------------------------------------------
pub fn bedrock_chunk_to_openai(chunk: &Value) -> Option<OpenAIStreamResponse> {
    let mut delta = serde_json::Map::new();
    let mut finish_reason = None;

    match chunk.get("type").and_then(|t| t.as_str()) {
        Some("message_start") => {
            delta.insert("role".to_string(), Value::String("assistant".to_string()));
        }
        Some("content_block_delta") => {
            if let Some(text) = chunk
                .get("delta")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
            {
                delta.insert("content".to_string(), Value::String(text.to_string()));
            }
        }
        Some("message_stop") => {
            finish_reason = Some("stop".to_string());
        }
        _ => {}
    }

    if delta.is_empty() && finish_reason.is_none() {
        return None;
    }

    Some(OpenAIStreamResponse {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        object: "chat.completion.chunk".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: "claude-via-bedrock".to_string(),
        choices: vec![OpenAIStreamChoice {
            delta: Value::Object(delta),
            index: 0,
            finish_reason,
        }],
    })
}
