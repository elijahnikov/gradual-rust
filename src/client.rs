use std::collections::HashMap;
use std::sync::Arc;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock, Notify};
use tokio::time::{interval, Duration};
use tokio::sync::mpsc;
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

use crate::evaluator::evaluate_flag;
use crate::event_buffer::{EventBuffer, EventBufferOptions, EventMeta};
use crate::types::*;

const DEFAULT_BASE_URL: &str = "https://worker.gradual.so/api/v1";

/// Options for configuring the Gradual client.
pub struct GradualOptions {
    pub api_key: String,
    pub environment: String,
    pub base_url: Option<String>,
    pub polling_enabled: bool,
    pub polling_interval_ms: Option<u64>,
    pub events_enabled: bool,
    pub events_flush_ms: Option<u64>,
    pub events_max_batch: Option<usize>,
    pub realtime_enabled: bool,
}

impl Default for GradualOptions {
    fn default() -> Self {
        GradualOptions {
            api_key: String::new(),
            environment: String::new(),
            base_url: None,
            polling_enabled: true,
            polling_interval_ms: None,
            events_enabled: false,
            events_flush_ms: None,
            events_max_batch: None,
            realtime_enabled: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitResponse {
    valid: bool,
    #[serde(default)]
    error: Option<String>,
}

/// A feature flag client that connects to the Gradual edge service.
pub struct GradualClient {
    inner: Arc<ClientInner>,
    stop_tx: mpsc::Sender<()>,
}

struct ClientInner {
    api_key: String,
    environment: String,
    base_url: String,
    http: HttpClient,
    snapshot: RwLock<Option<EnvironmentSnapshot>>,
    etag: RwLock<Option<String>>,
    identified_context: RwLock<Option<EvaluationContext>>,
    event_buffer: Mutex<Option<EventBuffer>>,
    ready: Notify,
    init_err: RwLock<Option<String>>,
    update_listeners: Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
    polling_enabled: bool,
    polling_interval: Duration,
    events_enabled: bool,
    events_flush_ms: u64,
    events_max_batch: usize,
    realtime_enabled: bool,
    ws_close_tx: Mutex<Option<mpsc::Sender<()>>>,
}

impl GradualClient {
    /// Create a new client that begins initialization immediately.
    /// The client spawns a background task for init (and optionally polling).
    pub fn new(opts: GradualOptions) -> Self {
        let base_url = opts.base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let polling_interval = Duration::from_millis(opts.polling_interval_ms.unwrap_or(10_000));

        let inner = Arc::new(ClientInner {
            api_key: opts.api_key,
            environment: opts.environment,
            base_url,
            http: HttpClient::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            snapshot: RwLock::new(None),
            etag: RwLock::new(None),
            identified_context: RwLock::new(None),
            event_buffer: Mutex::new(None),
            ready: Notify::new(),
            init_err: RwLock::new(None),
            update_listeners: Mutex::new(Vec::new()),
            polling_enabled: opts.polling_enabled,
            polling_interval,
            events_enabled: opts.events_enabled,
            events_flush_ms: opts.events_flush_ms.unwrap_or(30_000),
            events_max_batch: opts.events_max_batch.unwrap_or(100),
            realtime_enabled: opts.realtime_enabled,
            ws_close_tx: Mutex::new(None),
        });

        let (stop_tx, stop_rx) = mpsc::channel::<()>(1);

        // Spawn init task
        {
            let inner = Arc::clone(&inner);
            tokio::spawn(async move {
                init(inner, stop_rx).await;
            });
        }

        GradualClient { inner, stop_tx }
    }

    /// Block until the client is initialized or an error occurs.
    pub async fn wait_until_ready(&self) -> Result<(), String> {
        self.inner.ready.notified().await;
        let err = self.inner.init_err.read().await;
        match &*err {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    /// Returns true if the client has been successfully initialized.
    pub async fn is_ready(&self) -> bool {
        let snap = self.inner.snapshot.read().await;
        snap.is_some()
    }

    /// Check if a boolean flag is enabled.
    pub async fn is_enabled(&self, key: &str, context: Option<EvaluationContext>) -> bool {
        let merged = self.merge_context(context).await;
        let snap = self.inner.snapshot.read().await;
        let snap = match &*snap {
            Some(s) => s,
            None => return false,
        };
        let flag = match snap.flags.get(key) {
            Some(f) => f,
            None => return false,
        };
        let result = evaluate_flag(flag, &merged, &snap.segments, None);
        result.value.as_bool().unwrap_or(false)
    }

    /// Get a flag value, falling back to the provided default.
    pub async fn get(
        &self,
        key: &str,
        fallback: serde_json::Value,
        context: Option<EvaluationContext>,
    ) -> serde_json::Value {
        let merged = self.merge_context(context).await;
        let snap = self.inner.snapshot.read().await;
        let snap = match &*snap {
            Some(s) => s,
            None => return fallback,
        };
        let flag = match snap.flags.get(key) {
            Some(f) => f,
            None => return fallback,
        };
        let result = evaluate_flag(flag, &merged, &snap.segments, None);
        if result.value.is_null() {
            fallback
        } else {
            result.value
        }
    }

    /// Set persistent user context for all evaluations.
    pub async fn identify(&self, context: EvaluationContext) {
        let mut id = self.inner.identified_context.write().await;
        *id = Some(context);
    }

    /// Clear the identified user context.
    pub async fn reset(&self) {
        let mut id = self.inner.identified_context.write().await;
        *id = None;
    }

    /// Subscribe to snapshot updates. Returns an index that can be used for unsubscription.
    pub async fn on_update<F: Fn() + Send + Sync + 'static>(&self, callback: F) -> usize {
        let mut listeners = self.inner.update_listeners.lock().await;
        listeners.push(Box::new(callback));
        listeners.len() - 1
    }

    /// Stop polling, close WebSocket, and flush pending events.
    pub async fn close(self) {
        let _ = self.stop_tx.send(()).await;

        // Close WebSocket if active
        {
            let mut ws_tx = self.inner.ws_close_tx.lock().await;
            if let Some(tx) = ws_tx.take() {
                let _ = tx.send(()).await;
            }
        }

        let mut eb = self.inner.event_buffer.lock().await;
        if let Some(buffer) = eb.take() {
            buffer.destroy().await;
        }
    }

    async fn merge_context(&self, context: Option<EvaluationContext>) -> EvaluationContext {
        let identified = self.inner.identified_context.read().await;
        let mut merged = EvaluationContext::new();

        let mut all_kinds = std::collections::HashSet::new();
        if let Some(ref id) = *identified {
            for k in id.keys() {
                all_kinds.insert(k.clone());
            }
        }
        if let Some(ref ctx) = context {
            for k in ctx.keys() {
                all_kinds.insert(k.clone());
            }
        }

        for kind in all_kinds {
            let mut attrs = HashMap::new();
            if let Some(ref id) = *identified {
                if let Some(id_attrs) = id.get(&kind) {
                    for (k, v) in id_attrs {
                        attrs.insert(k.clone(), v.clone());
                    }
                }
            }
            if let Some(ref ctx) = context {
                if let Some(ctx_attrs) = ctx.get(&kind) {
                    for (k, v) in ctx_attrs {
                        attrs.insert(k.clone(), v.clone());
                    }
                }
            }
            merged.insert(kind, attrs);
        }

        merged
    }
}

/// Build the WebSocket URL from the base HTTP URL.
fn build_ws_url(base_url: &str, api_key: &str, environment: &str) -> String {
    let ws_base = base_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");

    let encoded_api_key = urlencoding::encode(api_key);
    let encoded_env = urlencoding::encode(environment);

    format!(
        "{}/sdk/connect?apiKey={}&environment={}",
        ws_base, encoded_api_key, encoded_env
    )
}

/// Attempt to connect via WebSocket and receive the initial snapshot.
/// Returns true if the initial snapshot was successfully received and stored.
async fn try_ws_init(inner: &Arc<ClientInner>) -> bool {
    let url = build_ws_url(&inner.base_url, &inner.api_key, &inner.environment);

    let connect_result = tokio_tungstenite::connect_async(&url).await;
    let (ws_stream, _response) = match connect_result {
        Ok(pair) => pair,
        Err(_) => return false,
    };

    let (_write, mut read) = ws_stream.split();

    // Wait for the first message (initial snapshot)
    let first_msg = match read.next().await {
        Some(Ok(msg)) => msg,
        _ => return false,
    };

    let text = match first_msg {
        WsMessage::Text(t) => t.to_string(),
        _ => return false,
    };

    let snap: EnvironmentSnapshot = match serde_json::from_str(&text) {
        Ok(s) => s,
        Err(_) => return false,
    };

    {
        let mut snapshot = inner.snapshot.write().await;
        *snapshot = Some(snap);
    }

    // Store the close channel
    let (ws_close_tx, ws_close_rx) = mpsc::channel::<()>(1);
    {
        let mut close_tx = inner.ws_close_tx.lock().await;
        *close_tx = Some(ws_close_tx);
    }

    // Spawn the background WebSocket listener task
    let inner_clone = Arc::clone(inner);
    tokio::spawn(async move {
        ws_run(inner_clone, read, ws_close_rx).await;
    });

    true
}

type WsRead = futures_util::stream::SplitStream<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
>;

/// Listen on a WebSocket read stream, handling reconnection with exponential backoff.
async fn ws_run(
    inner: Arc<ClientInner>,
    mut read: WsRead,
    mut close_rx: mpsc::Receiver<()>,
) {
    loop {
        // Listen for messages on the current connection
        let needs_reconnect = loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            handle_ws_snapshot(&inner, &text).await;
                        }
                        Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => {
                            break true;
                        }
                        _ => {} // Ping/Pong/Binary — ignore
                    }
                }
                _ = close_rx.recv() => {
                    return; // Client closing
                }
            }
        };

        if !needs_reconnect {
            return;
        }

        // Reconnect with exponential backoff
        let mut backoff_secs: u64 = 1;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => {}
                _ = close_rx.recv() => { return; }
            }

            let url = build_ws_url(&inner.base_url, &inner.api_key, &inner.environment);
            if let Ok((ws_stream, _)) = tokio_tungstenite::connect_async(&url).await {
                let (_write, mut new_read) = ws_stream.split();

                // Read initial snapshot on reconnect
                if let Some(Ok(WsMessage::Text(text))) = new_read.next().await {
                    handle_ws_snapshot(&inner, &text).await;
                    read = new_read;
                    break; // Back to the listen loop
                }
            }

            backoff_secs = (backoff_secs * 2).min(30);
        }
    }
}

async fn handle_ws_snapshot(inner: &ClientInner, text: &str) {
    if let Ok(snap) = serde_json::from_str::<EnvironmentSnapshot>(text) {
        let prev_version = {
            let s = inner.snapshot.read().await;
            s.as_ref().map(|s| s.version).unwrap_or(0)
        };
        let new_version = snap.version;
        {
            let mut snapshot = inner.snapshot.write().await;
            *snapshot = Some(snap);
        }
        if new_version != prev_version {
            let listeners = inner.update_listeners.lock().await;
            for cb in listeners.iter() {
                cb();
            }
        }
    }
}

async fn init(inner: Arc<ClientInner>, mut stop_rx: mpsc::Receiver<()>) {
    // Validate API key
    let init_result = inner
        .http
        .post(format!("{}/sdk/init", inner.base_url))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "apiKey": inner.api_key }))
        .send()
        .await;

    match init_result {
        Ok(resp) => {
            if !resp.status().is_success() {
                let mut err = inner.init_err.write().await;
                *err = Some(format!("gradual: init returned {}", resp.status()));
                inner.ready.notify_waiters();
                return;
            }
            match resp.json::<InitResponse>().await {
                Ok(data) => {
                    if !data.valid {
                        let mut err = inner.init_err.write().await;
                        *err = Some(format!(
                            "gradual: invalid API key - {}",
                            data.error.unwrap_or_default()
                        ));
                        inner.ready.notify_waiters();
                        return;
                    }
                }
                Err(e) => {
                    let mut err = inner.init_err.write().await;
                    *err = Some(format!("gradual: init decode failed: {}", e));
                    inner.ready.notify_waiters();
                    return;
                }
            }
        }
        Err(e) => {
            let mut err = inner.init_err.write().await;
            *err = Some(format!("gradual: init failed: {}", e));
            inner.ready.notify_waiters();
            return;
        }
    }

    // Try WebSocket first if realtime is enabled
    let ws_connected = if inner.realtime_enabled {
        try_ws_init(&inner).await
    } else {
        false
    };

    // If WebSocket didn't work (or not enabled), fall back to fetch snapshot
    if !ws_connected {
        if let Err(e) = fetch_snapshot(&inner).await {
            let mut err = inner.init_err.write().await;
            *err = Some(format!("gradual: snapshot fetch failed: {}", e));
            inner.ready.notify_waiters();
            return;
        }
    }

    // Initialize event buffer
    if inner.events_enabled {
        let snap = inner.snapshot.read().await;
        if let Some(ref snap) = *snap {
            let buffer = EventBuffer::new(EventBufferOptions {
                base_url: inner.base_url.clone(),
                api_key: inner.api_key.clone(),
                meta: EventMeta {
                    project_id: snap.meta.project_id.clone(),
                    organization_id: snap.meta.organization_id.clone(),
                    environment_id: snap.meta.environment_id.clone(),
                    sdk_platform: "rust".to_string(),
                },
                flush_interval_ms: inner.events_flush_ms,
                max_batch_size: inner.events_max_batch,
            });
            let mut eb = inner.event_buffer.lock().await;
            *eb = Some(buffer);
        }
    }

    // Signal ready
    inner.ready.notify_waiters();

    // If WebSocket is connected, no need for polling — just wait for stop signal
    if ws_connected {
        let _ = stop_rx.recv().await;
        return;
    }

    // Start polling if enabled (fallback when WebSocket is not active)
    if inner.polling_enabled {
        let mut tick = interval(inner.polling_interval);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let prev_version = {
                        let snap = inner.snapshot.read().await;
                        snap.as_ref().map(|s| s.version).unwrap_or(0)
                    };

                    if fetch_snapshot(&inner).await.is_ok() {
                        let new_version = {
                            let snap = inner.snapshot.read().await;
                            snap.as_ref().map(|s| s.version).unwrap_or(0)
                        };
                        if new_version != prev_version {
                            let listeners = inner.update_listeners.lock().await;
                            for cb in listeners.iter() {
                                cb();
                            }
                        }
                    }
                }
                _ = stop_rx.recv() => {
                    return;
                }
            }
        }
    }
}

async fn fetch_snapshot(inner: &ClientInner) -> Result<(), String> {
    let url = format!(
        "{}/sdk/snapshot?environment={}",
        inner.base_url, inner.environment
    );

    let mut req = inner.http.get(&url).header("Authorization", format!("Bearer {}", inner.api_key));

    {
        let etag = inner.etag.read().await;
        if let Some(ref e) = *etag {
            req = req.header("If-None-Match", e.clone());
        }
    }

    let resp = req.send().await.map_err(|e| e.to_string())?;

    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(());
    }
    if !resp.status().is_success() {
        return Err(format!("snapshot returned {}", resp.status()));
    }

    let new_etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let snap: EnvironmentSnapshot = resp.json().await.map_err(|e| e.to_string())?;

    {
        let mut snapshot = inner.snapshot.write().await;
        *snapshot = Some(snap);
    }
    if let Some(e) = new_etag {
        let mut etag = inner.etag.write().await;
        *etag = Some(e);
    }

    Ok(())
}
