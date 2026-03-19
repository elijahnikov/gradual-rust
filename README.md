# Gradual Rust SDK

[![CI](https://github.com/elijahnikov/gradual-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/elijahnikov/gradual-rust/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/gradual-sdk.svg)](https://crates.io/crates/gradual-sdk)

The official Rust SDK for [Gradual](https://github.com/elijahnikov/gradual) feature flags.

> **Early development** — the hash function, types, and evaluator are complete. The full HTTP client is not yet implemented.

## Installation

Add the SDK to your `Cargo.toml`:

```toml
[dependencies]
gradual-sdk = "0.1"
```

## Usage

```rust
use gradual_sdk::{evaluate_flag, hash_string, EvaluationContext, SnapshotFlag, SnapshotSegment};
use std::collections::HashMap;

// Hash function for deterministic bucketing
let bucket = hash_string("my-flag::user-123") % 100_000;

// Evaluate a flag (requires a SnapshotFlag from the API)
let context: EvaluationContext = HashMap::new();
let segments: HashMap<String, SnapshotSegment> = HashMap::new();
let result = evaluate_flag(&flag, &context, &segments, None);
println!("value: {:?}, variation: {:?}", result.value, result.variation_key);
```

Note that the full HTTP client is not yet implemented.

## Development

Run the test suite (includes hash and evaluator conformance tests):

```sh
cargo test
```

## License

MIT
