//! Managed blocking invocation on the run's fixed execution binding.
use crate::internal::execution::ExecutionBinding;

pub(crate) async fn invoke<T: Send + 'static>(
    binding: Option<&ExecutionBinding>,
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let binding = binding.ok_or_else(|| {
        anyhow::anyhow!("blocking callbacks require a managed evaluation executor")
    })?;
    let task = binding.spawn_blocking(work);
    // Abort queued work on run cancellation. Tokio cannot interrupt a closure
    // already running; its owned data remains valid until that closure returns.
    struct AbortOnDrop(tokio::task::AbortHandle);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _abort = AbortOnDrop(task.abort_handle());
    task.await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn dropping_an_invocation_cancels_queued_work_before_it_can_start() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let binding = ExecutionBinding::external(runtime.handle().clone());
        let (release, waiting) = std::sync::mpsc::channel();
        let (entered, started) = std::sync::mpsc::channel();
        let occupied = binding.spawn_blocking(move || {
            entered.send(()).unwrap();
            waiting
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()
        });
        started
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        let called = Arc::new(AtomicBool::new(false));
        runtime.block_on(async {
            {
                let called = called.clone();
                let callback = invoke(Some(&binding), move || {
                    called.store(true, Ordering::SeqCst);
                    Ok(())
                });
                futures::pin_mut!(callback);
                assert!(futures::poll!(callback).is_pending());
            }
            release.send(()).unwrap();
            occupied.await.unwrap();
            binding.spawn_blocking(|| ()).await.unwrap();
        });
        assert!(!called.load(Ordering::SeqCst));
    }
}
