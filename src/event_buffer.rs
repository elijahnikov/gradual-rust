use reqwest::Client;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};

const SDK_VERSION: &str = "0.1.0";

#[derive(Clone)]
pub struct EventMeta {
    pub project_id: String,
    pub organization_id: String,
    pub environment_id: String,
    pub sdk_platform: String,
}

pub struct EventBufferOptions {
    pub base_url: String,
    pub api_key: String,
    pub meta: EventMeta,
    pub flush_interval_ms: u64,
    pub max_batch_size: usize,
}

pub struct EventBuffer {
    events: Arc<Mutex<Vec<Value>>>,
    stop_tx: mpsc::Sender<()>,
    opts: Arc<EventBufferInner>,
    client: Client,
}

struct EventBufferInner {
    base_url: String,
    api_key: String,
    meta: EventMeta,
    max_batch_size: usize,
}

impl EventBuffer {
    pub fn new(opts: EventBufferOptions) -> Self {
        let flush_interval = if opts.flush_interval_ms > 0 {
            opts.flush_interval_ms
        } else {
            30_000
        };
        let max_batch = if opts.max_batch_size > 0 {
            opts.max_batch_size
        } else {
            100
        };

        let inner = Arc::new(EventBufferInner {
            base_url: opts.base_url,
            api_key: opts.api_key,
            meta: opts.meta,
            max_batch_size: max_batch,
        });

        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let client = Client::new();

        // Spawn flush loop
        {
            let events = Arc::clone(&events);
            let inner = Arc::clone(&inner);
            let client = client.clone();
            tokio::spawn(async move {
                let mut tick = interval(Duration::from_millis(flush_interval));
                loop {
                    tokio::select! {
                        _ = tick.tick() => {
                            flush_batch(&events, &inner, &client).await;
                        }
                        _ = stop_rx.recv() => {
                            flush_batch(&events, &inner, &client).await;
                            return;
                        }
                    }
                }
            });
        }

        EventBuffer {
            events,
            stop_tx,
            opts: inner,
            client,
        }
    }

    pub fn push(&self, event: Value) {
        let batch = {
            let mut events = self.events.lock().unwrap();
            events.push(event);
            if events.len() >= self.opts.max_batch_size {
                let batch: Vec<Value> = events.drain(..self.opts.max_batch_size).collect();
                Some(batch)
            } else {
                None
            }
        };

        if let Some(batch) = batch {
            let inner = Arc::clone(&self.opts);
            let client = self.client.clone();
            tokio::spawn(async move {
                send_batch(&batch, &inner, &client).await;
            });
        }
    }

    pub async fn flush(&self) {
        flush_batch(&self.events, &self.opts, &self.client).await;
    }

    pub async fn destroy(self) {
        let _ = self.stop_tx.send(()).await;
    }
}

async fn flush_batch(
    events: &Arc<Mutex<Vec<Value>>>,
    inner: &EventBufferInner,
    client: &Client,
) {
    let batch = {
        let mut events = events.lock().unwrap();
        if events.is_empty() {
            return;
        }
        let n = inner.max_batch_size.min(events.len());
        events.drain(..n).collect::<Vec<_>>()
    };
    send_batch(&batch, inner, client).await;
}

async fn send_batch(batch: &[Value], inner: &EventBufferInner, client: &Client) {
    let payload = json!({
        "meta": {
            "projectId": inner.meta.project_id,
            "organizationId": inner.meta.organization_id,
            "environmentId": inner.meta.environment_id,
            "sdkVersion": SDK_VERSION,
            "sdkPlatform": inner.meta.sdk_platform,
        },
        "events": batch,
    });

    // Fire-and-forget
    let _ = client
        .post(format!("{}/sdk/evaluations", inner.base_url))
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", inner.api_key))
        .json(&payload)
        .send()
        .await;
}
