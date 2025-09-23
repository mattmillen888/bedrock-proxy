Got it ✅ — we’ll stick with .env for configuration (already wired up with dotenvy) and I’ll draft you a comprehensive README.md so you can run this project smoothly.

Here’s the README.md you can drop into your project root:

⸻

# 🦀 Bedrock Proxy (Rust + Axum)

A lightweight local proxy server that connects to **AWS Bedrock Claude 4** models with proper authentication.
Handles AWS Signature V4 signing and supports both JSON and streaming responses.

⸻

## ✨ Features
- 🔑 Loads AWS credentials from `.env` file
- 🤖 **Claude 4 Sonnet** via APAC inference profile
- 🚀 Fast Rust/Axum web server
- 🔐 Automatic AWS SigV4 request signing
- 📡 Both JSON and streaming (SSE) endpoints
- 🔄 Transforms legacy prompt format to messages API

⸻

## 📂 Project Structure

```
bedrock-proxy/
├── Cargo.toml          # Dependencies
├── .env               # AWS credentials
└── src/
    └── main.rs        # Proxy server
```

## ⚙️ Setup

### 1. Install Rust
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Configure AWS Credentials
Create `.env` file in project root:
```bash
AWS_REGION=us-east-1
AWS_ACCESS_KEY_ID=your_access_key_id
AWS_SECRET_ACCESS_KEY=your_secret_access_key
# Optional for temporary credentials:
AWS_SESSION_TOKEN=your_session_token
# Optional inference profile (defaults to APAC):
INFERENCE_PROFILE=apac.anthropic.claude-sonnet-4-20250514-v1:0
```

### 3. Build and Run
```bash
cargo build
cargo run
```

⸻

## 🚀 Usage

Server starts on `http://127.0.0.1:3000`

### Endpoints

#### `POST /invoke` - JSON Response
```bash
curl -X POST http://127.0.0.1:3000/invoke \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hi"}],
    "max_tokens": 10
  }'
```

#### `POST /invoke_stream` - Streaming Response (SSE)
```bash
curl -N -X POST http://127.0.0.1:3000/invoke_stream \
  -H "Content-Type: application/json" \
  -d '{
    "messages": [{"role": "user", "content": "Hi"}],
    "max_tokens": 10
  }'
```

### Request Format
Both endpoints accept:
```json
{
  "messages": [{"role": "user", "content": "Your question"}],
  "max_tokens": 100
}
```

Legacy format also supported (auto-converted):
```json
{
  "prompt": "Your question",
  "max_tokens_to_sample": 100
}
```


⸻

## 📋 Technical Details

### Model Configuration
- **Model**: Claude 4 Sonnet (`anthropic.claude-sonnet-4-20250514-v1:0`)
- **Inference Profile**: `apac.anthropic.claude-sonnet-4-20250514-v1:0` (required for on-demand throughput)
- **API Version**: `bedrock-2023-05-31`

### Request Processing
1. Transforms legacy `prompt` format to `messages` API
2. Adds required `anthropic_version` parameter
3. Signs requests with AWS SigV4
4. Routes to appropriate Bedrock endpoint

### Rate Limiting
Claude 4 has strict rate limits. If you see `429 Too Many Requests`, wait before retrying.

## ⚠️ Requirements

- **AWS IAM**: User needs `bedrock:InvokeModel` permission for Claude models
- **Region**: Claude 4 available in limited regions (us-east-1, us-west-2, etc.)
- **Security**: Never commit `.env` file to version control

## 🔧 Configuration

### Inference Profiles
Set `INFERENCE_PROFILE` in `.env` to use different regions:

```bash
# APAC (default)
INFERENCE_PROFILE=apac.anthropic.claude-sonnet-4-20250514-v1:0

# US regions
INFERENCE_PROFILE=us.anthropic.claude-sonnet-4-20250514-v1:0

# EU regions
INFERENCE_PROFILE=eu.anthropic.claude-sonnet-4-20250514-v1:0

# Global (cross-region)
INFERENCE_PROFILE=global.anthropic.claude-sonnet-4-20250514-v1:0
```


### NOTE: if looking for models Run
```sh
source .env
aws bedrock list-foundation-models
```
