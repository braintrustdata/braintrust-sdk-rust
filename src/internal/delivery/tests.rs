use super::*;
use std::{
    future::IntoFuture,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::sync::mpsc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admission_does_not_wait_for_coordinator_lock() {
    let (service, mut entered, gate) = gated(4);
    let coordinator = lock(&service.inner.state);
    let producer = service.clone();
    let (sent, received) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let completion = producer.enqueue(7);
        sent.send(completion.submission().is_ok()).unwrap();
    });
    let admitted = received.recv_timeout(Duration::from_secs(2));
    drop(coordinator);
    thread.join().unwrap();
    assert!(admitted.unwrap());
    assert_eq!(entered.recv().await.unwrap(), vec![7]);
    gate.add_permits(1);
    service.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn shutdown_includes_a_publisher_already_in_progress() {
    let (service, mut entered, gate) = gated(4);
    let first = service.enqueue(1);
    assert_eq!(entered.recv().await.unwrap(), vec![1]);

    service.inner.begin_publication().unwrap();
    service.inner.reserve().unwrap();
    let (sender, second) = Completion::pair();
    let shutdown = service.shutdown(Duration::from_secs(2));
    let mut shutdown = Box::pin(shutdown.into_future());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut shutdown)
            .await
            .is_err()
    );
    service.inner.publish(Accepted { command: 2, sender });
    gate.add_permits(1);
    assert!(first.await.is_ok());
    assert_eq!(entered.recv().await.unwrap(), vec![2]);
    gate.add_permits(1);
    assert!(second.await.is_ok());
    assert_eq!(shutdown.await.unwrap().delivered(), 2);
}

#[tokio::test]
async fn flush_swaps_generation_and_waits_for_its_late_publisher() {
    let (service, mut entered, gate) = gated(8);
    let first = service.enqueue(1);
    assert_eq!(entered.recv().await.unwrap(), vec![1]);

    service.inner.begin_publication().unwrap();
    service.inner.reserve().unwrap();
    let old_epoch = service.inner.ingress.load_full();
    assert!(old_epoch.enter());
    let (sender, before) = Completion::pair();
    let mut flush = Box::pin(service.flush().into_future());
    assert!(!Arc::ptr_eq(&old_epoch, &service.inner.ingress.load_full()));
    let after = service.enqueue(3);
    service
        .inner
        .publish_to_epoch(&old_epoch, Accepted { command: 2, sender });

    gate.add_permits(1);
    first.await.unwrap();
    assert_eq!(entered.recv().await.unwrap(), vec![2]);
    assert!(futures::poll!(flush.as_mut()).is_pending());
    gate.add_permits(1);
    before.await.unwrap();
    assert_eq!(flush.await.unwrap().delivered(), 2);
    assert_eq!(entered.recv().await.unwrap(), vec![3]);
    gate.add_permits(1);
    after.await.unwrap();
    service.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn flush_detaches_without_draining_the_queue() {
    let (service, mut entered, gate) = gated(8);
    let writes: Vec<_> = (0..4).map(|item| service.enqueue(item)).collect();
    let old_epoch = service.inner.ingress.load_full();
    let flush = service.flush();
    assert!(!Arc::ptr_eq(&old_epoch, &service.inner.ingress.load_full()));
    // On the current-thread runtime, the worker has not run yet. Flush
    // must not have taken the first item out of the detached queue.
    assert!(old_epoch.queue.peek(&sdd::Guard::new()).is_some());
    assert_eq!(entered.recv().await.unwrap(), vec![0, 1, 2, 3]);
    gate.add_permits(1);
    assert_eq!(flush.await.unwrap().delivered(), 4);
    for write in writes {
        write.await.unwrap();
    }
    service.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn worker_failure_completes_a_late_publisher() {
    let (service, mut entered, _gate) = gated(4);
    let first = service.enqueue(1);
    assert_eq!(entered.recv().await.unwrap(), vec![1]);
    service.inner.begin_publication().unwrap();
    service.inner.reserve().unwrap();
    let old_epoch = service.inner.ingress.load_full();
    assert!(old_epoch.enter());
    let (sender, late) = Completion::pair();
    service.abort.abort();
    assert!(matches!(
        first.await,
        Err(WriteError::Execution(ExecutionError::WorkerLost))
    ));
    service
        .inner
        .publish_to_epoch(&old_epoch, Accepted { command: 2, sender });
    assert!(matches!(
        late.await,
        Err(WriteError::Execution(ExecutionError::WorkerLost))
    ));
    assert!(service.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_fails_items_taken_and_not_yet_taken_by_worker() {
    let (service, _entered, _gate) = gated(4);
    let (taken, wait_taken) = std::sync::mpsc::channel();
    let (resume, wait_resume) = std::sync::mpsc::channel();
    *lock(&service.inner.after_take) = Some((taken, wait_resume));
    let coordinator = lock(&service.inner.state);
    let first = service.enqueue(1);
    let second = service.enqueue(2);
    drop(coordinator);
    wait_taken.recv_timeout(Duration::from_secs(2)).unwrap();
    let shutdown = service.shutdown(Duration::from_millis(50));
    assert!(matches!(
        shutdown.await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    resume.send(()).unwrap();
    for completion in [first, second] {
        assert!(matches!(
            completion.await,
            Err(WriteError::Execution(ExecutionError::TimedOut))
        ));
    }
    let _ = service.exit.clone().await;
    assert!(service.is_empty());
}

#[tokio::test]
async fn terminal_stop_is_preserved_when_the_processor_returns_success() {
    let (service, mut entered, gate) = gated(4);
    let active = service.enqueue(1);
    assert_eq!(entered.recv().await.unwrap(), vec![1]);
    let pending = service.enqueue(2);
    let flush = service.flush();

    service.inner.stop(ExecutionError::TimedOut);
    gate.add_permits(1); // The processor returns Ok after its batch was failed.
    for write in [active, pending] {
        assert!(matches!(
            write.await,
            Err(WriteError::Execution(ExecutionError::TimedOut))
        ));
    }
    assert!(matches!(
        flush.await,
        Err(FlushError::Execution(ExecutionError::TimedOut))
    ));
    assert_eq!(service.exit.clone().await, Err(ExecutionError::TimedOut));
    assert!(service.is_empty());
    assert!(matches!(
        service.flush().submission(),
        Err(FlushError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        service.shutdown(Duration::from_secs(1)).await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
}

fn gated(
    capacity: usize,
) -> (
    WriteService<usize>,
    mpsc::UnboundedReceiver<Vec<usize>>,
    Arc<Semaphore>,
) {
    let (entered, receiver) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let process_gate = gate.clone();
    let service = WriteService::new(capacity, Handle::current(), move |commands| {
        let gate = process_gate.clone();
        let entered = entered.clone();
        Box::pin(async move {
            entered.send(commands).unwrap();
            gate.acquire().await.unwrap().forget();
            Ok(())
        })
    });
    (service, receiver, gate)
}

#[tokio::test]
async fn concurrent_flushes_observe_fixed_cutoffs_including_active_work() {
    let (service, mut entered, gate) = gated(8);
    let one = service.enqueue(1);
    let first = service.flush();
    assert_eq!(entered.recv().await.unwrap(), vec![1]);
    let concurrent = service.flush();
    let two = service.enqueue(2);
    let mut second = Box::pin(service.flush().into_future());
    let three = service.enqueue(3); // beyond second's cutoff
    gate.add_permits(1);
    assert_eq!(first.await.unwrap().delivered, 1);
    assert_eq!(concurrent.await.unwrap().delivered, 1);
    one.await.unwrap();
    assert_eq!(entered.recv().await.unwrap(), vec![2]);
    assert!(futures::poll!(second.as_mut()).is_pending());
    gate.add_permits(1);
    assert_eq!(second.await.unwrap().delivered, 2);
    two.await.unwrap();
    assert_eq!(entered.recv().await.unwrap(), vec![3]);
    // The second flush finished without waiting for the later arrival.
    gate.add_permits(1);
    three.await.unwrap();
    assert_eq!(service.flush().await.unwrap().delivered, 0);
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn a_new_flush_does_not_inherit_a_failed_prefix_from_an_older_active_flush() {
    let (entered, mut receiver) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let process_gate = gate.clone();
    let service = WriteService::new(4, Handle::current(), move |commands: Vec<usize>| {
        let entered = entered.clone();
        let gate = process_gate.clone();
        Box::pin(async move {
            entered.send(commands.clone()).unwrap();
            gate.acquire().await.unwrap().forget();
            if commands == [1] {
                anyhow::bail!("first batch failed");
            }
            Ok(())
        })
    });
    let first = service.enqueue(1);
    assert_eq!(receiver.recv().await.unwrap(), vec![1]);
    let second = service.enqueue(2);
    let covering = service.flush();
    gate.add_permits(1);
    assert!(matches!(first.await, Err(WriteError::Delivery(_))));
    assert_eq!(receiver.recv().await.unwrap(), vec![2]);
    let fresh = service.flush();
    gate.add_permits(1);
    second.await.unwrap();
    let Err(FlushError::Delivery(failure)) = covering.await else {
        panic!("expected captured failure");
    };
    assert_eq!(failure.failed, 1);
    assert_eq!(fresh.await.unwrap().delivered, 1);
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn graceful_shutdown_reports_its_failed_batch_after_notifying_all_writes() {
    let service = WriteService::new(4, Handle::current(), |_: Vec<usize>| {
        Box::pin(async {
            anyhow::bail!("terminal failure");
        })
    });
    let first = service.enqueue(1);
    let second = service.enqueue(2);
    let shutdown = service.shutdown(Duration::from_secs(1));
    let Err(ShutdownError::Delivery(failure)) = shutdown.await else {
        panic!("expected delivery failure");
    };
    assert_eq!(failure.failed, 2);
    for completion in [first, second] {
        assert!(matches!(completion.await, Err(WriteError::Delivery(_))));
    }
    assert!(service.is_empty());
}

#[tokio::test]
async fn capacity_includes_in_flight_work_and_rejection_is_observable_twice() {
    let (service, mut entered, gate) = gated(1);
    let first = service.enqueue(1);
    entered.recv().await.unwrap();
    let rejected = service.enqueue(2);
    assert!(matches!(
        &*rejected,
        Err(WriteError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        rejected.await,
        Err(WriteError::Admission(EnqueueError::Full))
    ));
    assert_eq!(service.outstanding(), 1);
    gate.add_permits(1);
    first.await.unwrap();
    assert!(service.is_empty());
    gate.add_permits(1);
    service.enqueue(3).await.unwrap();
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn dropped_observer_does_not_cancel_automatic_delivery_or_fill_the_queue() {
    let (service, mut entered, gate) = gated(2);
    drop(service.enqueue(1));
    assert_eq!(entered.recv().await.unwrap(), vec![1]);
    let flush = service.flush();
    drop(flush); // lifecycle observers also do not cancel accepted work
    gate.add_permits(1);
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    assert!(service.is_empty());
}

#[tokio::test]
async fn every_failed_batch_notifies_all_members_releases_errors_and_allows_fresh_work() {
    let fail = Arc::new(AtomicBool::new(true));
    let process_fail = fail.clone();
    let service = WriteService::new(4, Handle::current(), move |_: Vec<usize>| {
        let fail = process_fail.load(Ordering::Relaxed);
        Box::pin(async move {
            if fail {
                anyhow::bail!("terminal failure");
            }
            Ok(())
        })
    });
    for _ in 0..100 {
        let first = service.enqueue(1);
        let second = service.enqueue(2);
        let flush = service.flush();
        let Err(FlushError::Delivery(failure)) = flush.await else {
            panic!("expected failure");
        };
        assert_eq!(failure.failed, 2);
        let weak = Arc::downgrade(&failure.cause);
        for completion in [first, second] {
            let Err(WriteError::Delivery(error)) = completion.await else {
                panic!("expected failure");
            };
            assert!(Arc::ptr_eq(&error, &failure.cause));
        }
        drop(failure);
        assert!(weak.upgrade().is_none(), "SDK must release terminal errors");
        assert_eq!(service.outstanding(), 0);
        assert_eq!(service.flush().await.unwrap().delivered, 0);
    }
    fail.store(false, Ordering::Relaxed);
    let fresh = service.enqueue(3);
    assert_eq!(service.flush().await.unwrap().delivered, 1);
    fresh.await.unwrap();
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn shutdown_stops_admission_immediately_drains_and_is_shared_by_clones() {
    let (service, mut entered, gate) = gated(4);
    let write = service.enqueue(1);
    let shutdown = service.shutdown(Duration::from_secs(1));
    let concurrent = service.clone().shutdown(Duration::from_secs(1));
    assert!(shutdown.is_ok());
    assert!(matches!(
        service.enqueue(2).await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
    entered.recv().await.unwrap();
    gate.add_permits(1);
    write.await.unwrap();
    assert_eq!(shutdown.await.unwrap().delivered, 1);
    assert_eq!(concurrent.await.unwrap().delivered, 1);
    assert!(service.is_empty());
}

#[tokio::test]
async fn shutdown_timeout_fails_active_work_even_when_flush_slots_are_full() {
    let (service, mut entered, _) = gated(1);
    let write = service.enqueue(1);
    entered.recv().await.unwrap();
    let flush = service.flush();
    let rejected = service.flush();
    assert!(matches!(
        &*rejected,
        Err(FlushError::Admission(EnqueueError::Full))
    ));
    let shutdown = service.shutdown(Duration::from_millis(10));
    assert!(
        shutdown.is_ok(),
        "shutdown must not compete with flush slots"
    );
    assert!(matches!(
        shutdown.await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        write.await,
        Err(WriteError::Execution(ExecutionError::TimedOut))
    ));
    assert!(matches!(
        flush.await,
        Err(FlushError::Execution(ExecutionError::TimedOut))
    ));
    assert!(service.is_empty());
    assert!(matches!(
        service.enqueue(2).await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
}

#[tokio::test]
async fn shutdown_timeout_also_retires_batches_queued_behind_active_work() {
    let (service, mut entered, _) = gated(4);
    let first = service.enqueue(1);
    entered.recv().await.unwrap();
    let second = service.enqueue(2);
    let third = service.enqueue(3);
    let flush = service.flush();
    let shutdown = service.shutdown(Duration::from_millis(10));
    assert!(matches!(
        shutdown.await,
        Err(ShutdownError::Execution(ExecutionError::TimedOut))
    ));
    for completion in [first, second, third] {
        assert!(matches!(
            completion.await,
            Err(WriteError::Execution(ExecutionError::TimedOut))
        ));
    }
    assert!(matches!(
        flush.await,
        Err(FlushError::Execution(ExecutionError::TimedOut))
    ));
    assert_eq!(service.outstanding(), 0);
    assert!(lock(&service.inner.state).barriers.is_empty());
    assert!(lock(&service.inner.state).ready.is_empty());
}

#[tokio::test]
async fn very_large_write_capacity_does_not_overflow_lifecycle_control_limits() {
    let (service, _, _) = gated(usize::MAX);
    assert_eq!(service.inner.control_limit, 1024);
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
async fn worker_cancellation_before_first_poll_completes_every_accepted_sender() {
    let (service, _, _) = gated(4);
    let first = service.enqueue(1);
    let second = service.enqueue(2);
    let flush = service.flush();
    service.abort.abort();
    for completion in [first, second] {
        assert!(matches!(
            completion.await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
    }
    assert!(matches!(
        flush.await,
        Err(FlushError::Execution(ExecutionError::WorkerLost))
    ));
    assert!(service.is_empty());
    assert!(matches!(
        service.enqueue(3).await,
        Err(WriteError::Admission(EnqueueError::Closed))
    ));
}

#[tokio::test]
async fn worker_panic_notifies_active_and_pending_members() {
    let (entered, mut receiver) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let process_gate = gate.clone();
    let service = WriteService::new(4, Handle::current(), move |_: Vec<usize>| {
        let entered = entered.clone();
        let gate = process_gate.clone();
        Box::pin(async move {
            entered.send(()).unwrap();
            gate.acquire().await.unwrap().forget();
            panic!("test processor failed");
        })
    });
    let active = service.enqueue(1);
    receiver.recv().await.unwrap();
    let pending = service.enqueue(2);
    let flush = service.flush();
    gate.add_permits(1);
    for completion in [active, pending] {
        assert!(matches!(
            completion.await,
            Err(WriteError::Execution(ExecutionError::WorkerLost))
        ));
    }
    assert!(matches!(
        flush.await,
        Err(FlushError::Execution(ExecutionError::WorkerLost))
    ));
    assert!(service.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_last_producers_drains_work_and_stops_the_worker() {
    for _ in 0..64 {
        let (service, mut entered, gate) = gated(4);
        let write = service.enqueue(1);
        let clone = service.clone();
        let exit = service.exit.clone();
        std::thread::scope(|scope| {
            scope.spawn(move || drop(service));
            scope.spawn(move || drop(clone));
        });
        entered.recv().await.unwrap();
        gate.add_permits(1);
        write.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), exit)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fast_workers_and_racing_flushes_cannot_lose_registered_completions() {
    let seen = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let processed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let process_seen = seen.clone();
    let process_count = processed.clone();
    let service = WriteService::new(1024, Handle::current(), move |commands: Vec<usize>| {
        lock(&process_seen).extend(commands.iter().copied());
        process_count.fetch_add(commands.len(), Ordering::Relaxed);
        Box::pin(async { Ok(()) })
    });
    let (writes, flushes) = std::thread::scope(|scope| {
        let producers: Vec<_> = (0..4)
            .map(|producer| {
                let service = &service;
                scope.spawn(move || {
                    (0..50)
                        .map(|item| service.enqueue(producer * 50 + item))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let flushers: Vec<_> = (0..2)
            .map(|_| {
                let service = &service;
                scope.spawn(move || (0..50).map(|_| service.flush()).collect::<Vec<_>>())
            })
            .collect();
        (
            producers
                .into_iter()
                .flat_map(|producer| producer.join().unwrap())
                .collect::<Vec<_>>(),
            flushers
                .into_iter()
                .flat_map(|flusher| flusher.join().unwrap())
                .collect::<Vec<_>>(),
        )
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        for write in writes {
            write.await.unwrap();
        }
        for flush in flushes {
            flush.await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(lock(&seen).len(), 200);
    assert_eq!(processed.load(Ordering::Relaxed), 200);
    assert!(service.is_empty());
    service.shutdown(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn racing_producers_cannot_overbook_capacity() {
    let (service, _entered, gate) = gated(8);
    let completions = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..32)
            .map(|number| {
                let service = &service;
                scope.spawn(move || service.enqueue(number))
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        completions
            .iter()
            .filter(|completion| completion.is_ok())
            .count(),
        8
    );
    assert_eq!(service.outstanding(), 8);
    gate.add_permits(8);
    let mut delivered = 0;
    for completion in completions {
        if completion.await.is_ok() {
            delivered += 1;
        }
    }
    assert_eq!(delivered, 8);
    service.shutdown(Duration::from_secs(1)).await.unwrap();
    assert!(service.is_empty());
}
