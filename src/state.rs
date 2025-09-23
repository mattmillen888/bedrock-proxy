use reqwest::Client;

#[derive(Clone)]
pub struct AppState {
    pub client: Client,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    pub inference_profile: String,
}

impl AppState {
    pub fn from_env() -> Self {
        Self {
            client: Client::new(),
            region: std::env::var("AWS_REGION").expect("AWS_REGION must be set"),
            access_key: std::env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID must be set"),
            secret_key: std::env::var("AWS_SECRET_ACCESS_KEY")
                .expect("AWS_SECRET_ACCESS_KEY must be set"),
            session_token: std::env::var("AWS_SESSION_TOKEN").ok(),
            inference_profile: std::env::var("INFERENCE_PROFILE")
                .unwrap_or_else(|_| "apac.anthropic.claude-sonnet-4-20250514-v1:0".to_string()),
        }
    }
}