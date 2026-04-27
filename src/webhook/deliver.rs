use std::{sync::Arc, time::Duration};
use std::sync::atomic::Ordering::Relaxed;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

use crate::webhook::metrics::Metrics;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookTask {
    pub url:     String,
    pub payload: String,
    #[serde(default)]
    pub headers: Vec<HttpHeader>,
    #[serde(default)]
    pub hook_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpHeader {
    pub key:   String,
    pub value: String,
}

pub struct DeliverQueue {
    client:      Client,
    semaphore:   Arc<Semaphore>,
    max_retries: usize,
    base_delay:  Duration,
    pub metrics: Arc<Metrics>,
}

impl DeliverQueue {
    pub fn new(workers: usize, timeout_secs: u64, max_retries: usize, base_delay_ms: u64) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .user_agent(concat!("Gitom-Webhook/", env!("CARGO_PKG_VERSION")))
            .use_rustls_tls()
            .build()
            .expect("Webhook HTTP client init failed");
        Self {
            client,
            semaphore:   Arc::new(Semaphore::new(workers)),
            max_retries,
            base_delay:  Duration::from_millis(base_delay_ms),
            metrics:     Arc::new(Metrics::default()),
        }
    }

    pub fn enqueue(&self, task: WebhookTask) {
        let client    = self.client.clone();
        let semaphore = self.semaphore.clone();
        let metrics   = self.metrics.clone();
        let retries   = self.max_retries;
        let delay     = self.base_delay;

        tokio::spawn(async move {
            let _permit = semaphore.acquire_owned().await.unwrap();
            deliver_with_retry(client, task, retries, delay, metrics).await;
        });
    }
}

async fn deliver_with_retry(
    client:      Client,
    task:        WebhookTask,
    max_retries: usize,
    base_delay:  Duration,
    metrics:     Arc<Metrics>,
) {
    metrics.in_flight.fetch_add(1, Relaxed);
    metrics.enqueued.fetch_add(1, Relaxed);
    struct Guard(Arc<Metrics>);
    impl Drop for Guard { fn drop(&mut self) { self.0.in_flight.fetch_sub(1, Relaxed); } }
    let _guard = Guard(metrics.clone());

    for attempt in 0..=max_retries {
        match do_deliver(&client, &task).await {
            Ok(status) => {
                metrics.delivered.fetch_add(1, Relaxed);
                info!(hook_id = task.hook_id, url = %task.url, status, attempt, "webhook delivered");
                return;
            }
            Err(e) if attempt < max_retries => {
                let wait = (base_delay * 2u32.pow(attempt as u32)).min(Duration::from_secs(60));
                warn!(hook_id = task.hook_id, url = %task.url, attempt, retry_in = ?wait, error = %e, "webhook failed, retrying");
                tokio::time::sleep(wait).await;
            }
            Err(e) => {
                metrics.failed.fetch_add(1, Relaxed);
                error!(hook_id = task.hook_id, url = %task.url, attempts = max_retries + 1, error = %e, "webhook permanently failed");
            }
        }
    }
}

async fn do_deliver(client: &Client, task: &WebhookTask) -> Result<u16, String> {
    let mut req = client
        .post(&task.url)
        .header("Content-Type", "application/json")
        .body(task.payload.clone());

    for h in &task.headers {
        req = req.header(&h.key, &h.value);
    }

    let resp   = req.send().await.map_err(|e| format!("network error: {e}"))?;
    let status = resp.status().as_u16();
    if resp.status().is_success() || resp.status().is_redirection() {
        Ok(status)
    } else {
        Err(format!("HTTP {status}"))
    }
}
