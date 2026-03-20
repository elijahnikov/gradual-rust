# Gradual Rust SDK

[![CI](https://github.com/elijahnikov/gradual-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/elijahnikov/gradual-rust/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/gradual-sdk.svg)](https://crates.io/crates/gradual-sdk)

The official Rust SDK for [Gradual](https://github.com/elijahnikov/gradual) feature flags.

## Installation

Add the SDK to your `Cargo.toml`:

```toml
[dependencies]
gradual-sdk = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

## Usage

```rust
use gradual_sdk::{GradualClient, GradualOptions};

#[tokio::main]
async fn main() {
    let client = GradualClient::new(GradualOptions {
        api_key: "your-api-key".to_string(),
        environment: "production".to_string(),
        base_url: None,
        polling_enabled: true,
        polling_interval_ms: None,
        events_enabled: true,
        events_flush_ms: None,
        events_max_batch: None,
    });

    // Wait for initialization
    client.wait_until_ready().await.expect("Failed to init");

    // Check a boolean flag
    let enabled = client.is_enabled("my-flag", None).await;

    // Get a flag value with fallback
    let value = client.get("my-flag", serde_json::json!("default"), None).await;

    // Set persistent user context
    let mut context = std::collections::HashMap::new();
    let mut user = std::collections::HashMap::new();
    user.insert("key".to_string(), serde_json::json!("user-123"));
    context.insert("user".to_string(), user);
    client.identify(context).await;

    // Clean up
    client.close().await;
}
```

## API

| Method | Description |
|--------|-------------|
| `GradualClient::new(opts)` | Create client, begins init in background |
| `wait_until_ready().await` | Block until initialized |
| `is_ready().await` | Check if initialized |
| `is_enabled(key, context).await` | Check boolean flag |
| `get(key, fallback, context).await` | Get flag value with default |
| `identify(context).await` | Set persistent user context |
| `reset().await` | Clear identified context |
| `on_update(callback).await` | Subscribe to snapshot updates |
| `close().await` | Stop polling, flush events |

## Development

Run the test suite (includes hash and evaluator conformance tests):

```sh
cargo test
```

## License

MIT
