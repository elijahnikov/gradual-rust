use std::collections::HashMap;
use std::sync::Arc;
use reqwest::Client as HttpClient;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock, Notify};
use tokio::time::{interval, Duration};
use tokio::sync::mpsc;

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

    /// Stop polling and flush pending events.
    pub async fn close(self) {
        let _ = self.stop_tx.send(()).await;
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

    // Fetch initial snapshot
    if let Err(e) = fetch_snapshot(&inner).await {
        let mut err = inner.init_err.write().await;
        *err = Some(format!("gradual: snapshot fetch failed: {}", e));
        inner.ready.notify_waiters();
        return;
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

    // Start polling if enabled
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
