use gradual_sdk::{GradualClient, GradualOptions};
use serde_json::json;

#[tokio::test]
#[ignore]
async fn test_get_flag_value() {
    let client = GradualClient::new(GradualOptions {
        api_key: "grdl_x5Hf17fFpgjkUykTZa7bGerkZsCd-OJX".to_string(),
        environment: "testing".to_string(),
        base_url: None,
        polling_enabled: false,
        polling_interval_ms: None,
        events_enabled: false,
        events_flush_ms: None,
        events_max_batch: None,
    });

    client
        .wait_until_ready()
        .await
        .expect("client should be ready");

    let fallback = json!("fallback-value");
    let value = client.get("testing", fallback, None).await;

    assert_eq!(value, json!("variation-value-2"));

    client.close().await;
}
