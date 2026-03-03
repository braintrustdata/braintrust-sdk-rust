use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use super::queue::LogQueueCore;

/// Commands sent through the worker channel (flush coordination only).
pub(super) enum LogCommand {
    Flush(oneshot::Sender<std::result::Result<(), anyhow::Error>>),
    TriggerFlush,
}

pub(super) async fn run_worker(mut receiver: mpsc::Receiver<LogCommand>, core: Arc<LogQueueCore>) {
    loop {
        match receiver.recv().await {
            Some(LogCommand::Flush(response)) => {
                let result = core.flush().await;
                let _ = response.send(result);
            }
            Some(LogCommand::TriggerFlush) => {
                if let Err(e) = core.flush().await {
                    warn!(error = %e, "background flush failed");
                }
            }
            None => {
                // Channel closed — flush any remaining items and exit.
                if let Err(e) = core.flush().await {
                    warn!(error = %e, "final flush failed");
                }
                break;
            }
        }
    }
}
