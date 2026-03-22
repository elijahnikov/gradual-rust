use gradual_sdk::{GradualClient, GradualOptions};
use serde_json::json;

#[tokio::test]
#[ignore]
async fn test_get_flag_value() {
    let client = GradualClient::new(GradualOptions {
        api_key: "grdl_x5Hf17fFpgjkUykTZa7bGerkZsCd-OJX".to_string(),
        environment: "testing".to_string(),
        polling_enabled: false,
        realtime_enabled: false,
        ..Default::default()
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
