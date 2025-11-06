//! Event queue for batching and background processing

use crate::client::HttpClient;
use crate::error::Result;
use crate::types::SpanEvent;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// Default batch size (number of events)
const DEFAULT_BATCH_SIZE: usize = 100;

/// Maximum batch size in bytes (3MB = half of 6MB AWS Lambda limit)
const MAX_BATCH_BYTES: usize = 3 * 1024 * 1024;

/// Event queue for batching and sending events
pub struct EventQueue {
    /// Internal queue of events
    queue: Arc<Mutex<VecDeque<SpanEvent>>>,

    /// Maximum batch size
    batch_size: usize,

    /// Notification for new events
    notify: Arc<Notify>,
}

impl EventQueue {
    /// Create a new event queue
    pub fn new(batch_size: usize) -> Self {
        EventQueue {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            batch_size,
            notify: Arc::new(Notify::new()),
        }
    }

    /// Enqueue an event for processing
    pub fn enqueue(&self, event: SpanEvent) {
        let mut queue = self.queue.lock().unwrap();
        queue.push_back(event);
        drop(queue); // Release lock before notifying
        self.notify.notify_one();
    }

    /// Drain all events from the queue (raw, unmerged)
    pub fn drain(&self) -> Vec<SpanEvent> {
        let mut queue = self.queue.lock().unwrap();
        queue.drain(..).collect()
    }

    /// Drain and merge all events from the queue
    ///
    /// This is useful for testing - it drains events and merges them by ID
    /// just like the background processor would do.
    pub fn drain_merged(&self) -> Vec<SpanEvent> {
        let events = self.drain();
        merge_events_by_id(events)
    }

    /// Get the current queue size
    pub fn len(&self) -> usize {
        let queue = self.queue.lock().unwrap();
        queue.len()
    }

    /// Check if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Start background processing with the given HTTP client
    pub fn start_background_processor(
        self: Arc<Self>,
        client: Arc<HttpClient>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                // Wait for notification of new events or flush
                self.notify.notified().await;

                // Process events
                if let Err(e) = self.process_batch(&client).await {
                    eprintln!("Error processing batch: {}", e);
                }
            }
        })
    }

    /// Process a batch of events
    async fn process_batch(&self, client: &HttpClient) -> Result<()> {
        let events = self.drain();
        if events.is_empty() {
            return Ok(());
        }

        // Merge events by ID
        let merged_events = merge_events_by_id(events);

        // Create batches
        let batches = create_batches(merged_events, self.batch_size);

        // Send batches in parallel
        let mut tasks = Vec::new();
        for batch in batches {
            let client = client.clone();
            tasks.push(tokio::spawn(async move {
                client.send_batch(batch).await
            }));
        }

        // Wait for all tasks to complete
        for task in tasks {
            if let Err(e) = task.await {
                eprintln!("Task error: {}", e);
            }
        }

        Ok(())
    }

    /// Flush all pending events
    pub async fn flush(&self, client: &HttpClient) -> Result<()> {
        // Notify the background processor
        self.notify.notify_one();

        // Process any remaining events immediately
        self.process_batch(client).await
    }
}

/// Merge events by their ID (row ID)
/// Events with the same ID are merged, with is_merge flag controlling behavior
fn merge_events_by_id(events: Vec<SpanEvent>) -> Vec<SpanEvent> {
    let mut event_map: HashMap<String, SpanEvent> = HashMap::new();

    for event in events {
        let id = event.id.clone();

        if let Some(existing) = event_map.get_mut(&id) {
            // Merge this event into the existing one
            existing.merge(&event);
        } else {
            // First event with this ID
            event_map.insert(id, event);
        }
    }

    event_map.into_values().collect()
}

/// Create batches from events, respecting size and byte limits
fn create_batches(events: Vec<SpanEvent>, batch_size: usize) -> Vec<Vec<SpanEvent>> {
    let mut batches = Vec::new();
    let mut current_batch = Vec::new();
    let mut current_size = 0;

    for event in events {
        // Estimate event size (rough approximation)
        let event_size = estimate_event_size(&event);

        // Check if adding this event would exceed limits
        if !current_batch.is_empty()
            && (current_batch.len() >= batch_size || current_size + event_size > MAX_BATCH_BYTES)
        {
            // Start a new batch
            batches.push(std::mem::take(&mut current_batch));
            current_size = 0;
        }

        current_batch.push(event);
        current_size += event_size;
    }

    // Add the last batch if not empty
    if !current_batch.is_empty() {
        batches.push(current_batch);
    }

    batches
}

/// Estimate the size of an event in bytes (rough approximation)
fn estimate_event_size(event: &SpanEvent) -> usize {
    // Use JSON serialization size as estimate
    serde_json::to_string(event)
        .map(|s| s.len())
        .unwrap_or(1024) // Default estimate if serialization fails
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_creation() {
        let queue = EventQueue::new(100);
        assert_eq!(queue.len(), 0);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_enqueue() {
        let queue = EventQueue::new(100);
        let event = SpanEvent::new();
        queue.enqueue(event);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn test_drain() {
        let queue = EventQueue::new(100);
        let event1 = SpanEvent::new();
        let event2 = SpanEvent::new();

        queue.enqueue(event1);
        queue.enqueue(event2);

        let drained = queue.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn test_merge_events_by_id() {
        let id = uuid::Uuid::new_v4().to_string();

        let mut event1 = SpanEvent::new();
        event1.id = id.clone();
        event1.input = Some(serde_json::json!("input1"));
        event1.is_merge = Some(false);

        let mut event2 = SpanEvent::new();
        event2.id = id.clone();
        event2.output = Some(serde_json::json!("output2"));
        event2.is_merge = Some(true);

        let merged = merge_events_by_id(vec![event1, event2]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].input, Some(serde_json::json!("input1")));
        assert_eq!(merged[0].output, Some(serde_json::json!("output2")));
    }

    #[test]
    fn test_create_batches() {
        let mut events = Vec::new();
        for _ in 0..250 {
            events.push(SpanEvent::new());
        }

        let batches = create_batches(events, 100);

        // Should create 3 batches: 100 + 100 + 50
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 100);
        assert_eq!(batches[1].len(), 100);
        assert_eq!(batches[2].len(), 50);
    }
}
