use super::*;
use crate::log_queue::{Credential, LogQueue, LogQueueConfig};
use crate::LoginState;
fn queue() -> LogQueue {
    LogQueue::new(
        LogQueueConfig::builder().sync_flush(true).build(),
        LoginState::new(),
        crate::http::build_http_client(std::time::Duration::from_secs(1), None, None).unwrap(),
        32,
        "http://localhost".parse().unwrap(),
        "http://localhost".parse().unwrap(),
    )
}
#[tokio::test]
async fn barriers_cover_only_work_accepted_before_them() {
    let engine = Engine::manual(ServiceConfig::default());
    let a = queue();
    let b = queue();
    engine.submit(a.core.clone(), SubmitCommand::test_row("covered"));
    let barrier = engine.barrier(&a.core.observation);
    engine.submit(a.core.clone(), SubmitCommand::test_row("later"));
    engine.submit(b.core.clone(), SubmitCommand::test_row("unrelated"));
    let jobs = engine.take_jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].1.len(), 3);
    assert!(engine.take_jobs().is_empty(), "the partition is in flight");
    a.core.observation.finish(jobs[0].1[0].sequence, None, 0, 0);
    assert_eq!(barrier.await.unwrap().delivered(), 1);
    assert_eq!(a.core.observation.len(), 1);
}
#[test]
fn accepting_work_freezes_routing_before_later_login_discovery() {
    let engine = Engine::manual(ServiceConfig::default());
    let queue = queue();
    engine.submit(queue.core.clone(), SubmitCommand::test_row("first"));
    queue.core.login_state.set(
        "token".into(),
        "org".into(),
        "name".into(),
        "http://discovered.example".into(),
        "http://localhost".into(),
    );
    let jobs = engine.take_jobs();
    assert_eq!(jobs[0].0.route.endpoint, "http://localhost/");
    assert_eq!(jobs[0].0.org_name, None);
}
fn http_queue(server: &wiremock::MockServer, max_items: usize) -> LogQueue {
    LogQueue::new(
        LogQueueConfig::builder()
            .sync_flush(true)
            .checked_delivery(true)
            .batch_max_items(max_items)
            .build(),
        LoginState::new(),
        crate::http::build_http_client(std::time::Duration::from_secs(30), None, None).unwrap(),
        32,
        server.uri().parse().unwrap(),
        server.uri().parse().unwrap(),
    )
}

#[tokio::test]
async fn compatible_facades_share_uploads_with_independent_barriers() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let engine = Engine::manual(ServiceConfig::default());
    let a = http_queue(&server, 100);
    let b = http_queue(&server, 100);
    engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
    engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
    let x = engine.barrier(&a.core.observation);
    let y = engine.barrier(&b.core.observation);
    let jobs = engine.take_jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].1.len(), 2);
    for (key, entries) in jobs {
        engine.clone().process_job(key, entries, None).await;
    }
    x.await.unwrap();
    y.await.unwrap();
    let state = engine.intake.lock().unwrap();
    assert!(state.records.is_empty());
    assert!(state.partitions.is_empty());
}

#[tokio::test]
async fn shared_split_failures_report_partial_delivery_to_each_owner() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, Request, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(|request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            if body["rows"].as_array().unwrap().len() > 1 {
                ResponseTemplate::new(413)
            } else if body["rows"][0]["id"] == "a" {
                ResponseTemplate::new(200).set_body_string("{}")
            } else {
                ResponseTemplate::new(400)
            }
        })
        .mount(&server)
        .await;
    let engine = Engine::manual(ServiceConfig::default());
    let a = http_queue(&server, 100);
    let b = http_queue(&server, 100);
    engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
    engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
    let x = engine.barrier(&a.core.observation);
    let y = engine.barrier(&b.core.observation);
    for (key, entries) in engine.take_jobs() {
        engine.clone().process_job(key, entries, None).await;
    }
    assert!(x.await.is_err());
    assert!(y.await.is_err());
    assert_eq!(a.dropped_rows().upload_failed, 0);
    assert_eq!(b.dropped_rows().upload_failed, 1);
}

#[tokio::test]
async fn failure_in_a_different_physical_batch_does_not_fail_a_successful_owner() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, Request, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(|request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            if body["rows"][0]["id"] == "a" {
                ResponseTemplate::new(200).set_body_string("{}")
            } else {
                ResponseTemplate::new(400)
            }
        })
        .mount(&server)
        .await;
    let engine = Engine::manual(ServiceConfig::default());
    let a = http_queue(&server, 1);
    let b = http_queue(&server, 1);
    engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
    engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
    let x = engine.barrier(&a.core.observation);
    let y = engine.barrier(&b.core.observation);
    for (key, entries) in engine.take_jobs() {
        engine.clone().process_job(key, entries, None).await;
    }
    x.await.unwrap();
    assert!(y.await.is_err());
    assert_eq!(a.dropped_rows().upload_failed, 0);
    assert_eq!(b.dropped_rows().upload_failed, 1);
}
#[tokio::test]
async fn a_failed_project_registration_is_attempted_once_per_job() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let engine = Engine::manual(ServiceConfig::default());
    let queue = http_queue(&server, 100);
    for i in 0..20 {
        engine.submit(
            queue.core.clone(),
            SubmitCommand::test_named_span(&format!("row-{i}"), "forbidden"),
        );
    }
    let flush = engine.barrier(&queue.core.observation);
    let jobs = engine.take_jobs();
    assert_eq!(jobs.len(), 1);
    for (key, entries) in jobs {
        engine.clone().process_job(key, entries, None).await;
    }
    assert!(flush.await.is_err());
    assert_eq!(queue.dropped_rows().preparation_failed, 20);
}

#[tokio::test]
async fn rotated_tokens_in_one_org_reuse_the_project_registration() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"project":{"id":"resolved"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(2)
        .mount(&server)
        .await;
    let engine = Engine::manual(ServiceConfig::default());
    let queue = http_queue(&server, 100);
    for token in ["first-token", "rotated-token"] {
        let mut command = SubmitCommand::test_named_span(token, "rotation-project");
        command.credential = Credential::token_value(token.into());
        engine.submit(queue.core.clone(), command);
        let flush = engine.barrier(&queue.core.observation);
        for (key, entries) in engine.take_jobs() {
            engine.clone().process_job(key, entries, None).await;
        }
        flush.await.unwrap();
    }
}

#[tokio::test]
async fn transient_project_registration_failures_are_retried() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/project/register"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"project":{"id":"resolved"}})),
        )
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let engine = Engine::manual(ServiceConfig::default());
    let queue = http_queue(&server, 100);
    engine.submit(
        queue.core.clone(),
        SubmitCommand::test_named_span("row", "flaky-project"),
    );
    let flush = engine.barrier(&queue.core.observation);
    for (key, entries) in engine.take_jobs() {
        engine.clone().process_job(key, entries, None).await;
    }
    assert_eq!(flush.await.unwrap().delivered(), 1);
}

#[test]
fn global_limits_include_buffered_scopes_and_reject_only_incoming_rows() {
    let engine = Engine::manual(ServiceConfig::builder().max_rows(2).build());
    let a = queue();
    let b = queue();
    engine.submit_many(
        a.core.clone(),
        [SubmitCommand::test_row("a1"), SubmitCommand::test_row("a2")],
    );
    engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
    assert_eq!(engine.intake.lock().unwrap().records.len(), 2);
    assert_eq!(a.dropped_rows().queue_full, 0);
    assert_eq!(b.dropped_rows().queue_full, 1);
    assert_eq!(a.core.observation.len(), 2);
    assert_eq!(b.core.observation.len(), 0);
}

#[test]
fn org_budgets_are_shared_across_facades() {
    let engine = Engine::manual(ServiceConfig::builder().max_org_rows(3).build());
    let a = queue();
    let b = queue();
    engine.submit(
        a.core.clone(),
        SubmitCommand::test_scoped_row("a", "p", "org"),
    );
    engine.submit(
        b.core.clone(),
        SubmitCommand::test_scoped_row("same-project", "p", "org"),
    );
    engine.submit(
        b.core.clone(),
        SubmitCommand::test_scoped_row("other-project", "q", "org"),
    );
    engine.submit(
        b.core.clone(),
        SubmitCommand::test_scoped_row("over-org-budget", "r", "org"),
    );
    engine.submit(
        b.core.clone(),
        SubmitCommand::test_scoped_row("other-org", "p", "other"),
    );
    assert_eq!(b.dropped_rows().queue_full, 1);
    assert_eq!(engine.intake.lock().unwrap().records.len(), 4);
}

#[test]
fn racing_producers_share_one_bounded_admission_budget() {
    let engine = Engine::manual(ServiceConfig::builder().max_rows(17).build());
    let queue = queue();
    std::thread::scope(|threads| {
        for _ in 0..8 {
            let engine = engine.clone();
            let core = queue.core.clone();
            threads.spawn(move || {
                for _ in 0..100 {
                    engine.submit(core.clone(), SubmitCommand::test_row("row"));
                }
            });
        }
    });
    assert_eq!(engine.intake.lock().unwrap().records.len(), 17);
    assert_eq!(queue.core.observation.len(), 17);
    assert_eq!(queue.dropped_rows().queue_full, 783);
}

#[tokio::test]
async fn completion_wakes_after_capacity_is_released_and_control_is_unlocked() {
    use futures::task::{waker_ref, ArcWake};
    use std::future::{Future, IntoFuture};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    struct Reenter {
        engine: Arc<Engine>,
        core: Arc<LogQueueCore>,
        woke: AtomicBool,
    }
    impl ArcWake for Reenter {
        fn wake_by_ref(this: &Arc<Self>) {
            assert!(this.engine.intake.try_lock().is_ok());
            assert!(this
                .engine
                .submit(this.core.clone(), SubmitCommand::test_row("next"))
                .submission()
                .is_ok());
            this.woke.store(true, Ordering::SeqCst);
        }
    }
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let queue = http_queue(&server, 1);
    let engine = Engine::manual(ServiceConfig::builder().max_rows(1).build());
    let write = engine.submit(queue.core.clone(), SubmitCommand::test_row("first"));
    let rejected = engine.submit(queue.core.clone(), SubmitCommand::test_row("rejected"));
    assert!(matches!(
        rejected.submission(),
        Err(WriteError::Admission(EnqueueError::Full))
    ));
    assert!(matches!(
        rejected.await,
        Err(WriteError::Admission(EnqueueError::Full))
    ));
    let signal = Arc::new(Reenter {
        engine: engine.clone(),
        core: queue.core.clone(),
        woke: AtomicBool::new(false),
    });
    let waker = waker_ref(&signal);
    let mut completion = Box::pin(write.into_future());
    assert!(matches!(
        completion.as_mut().poll(&mut Context::from_waker(&waker)),
        Poll::Pending
    ));
    let flush = engine.barrier(&queue.core.observation);
    let (key, entries) = engine.take_jobs().pop().unwrap();
    engine.clone().process_job(key, entries, None).await;
    completion.await.unwrap();
    assert!(signal.woke.load(Ordering::SeqCst));
    assert_eq!(flush.await.unwrap().delivered(), 1);
    assert_eq!(engine.intake.lock().unwrap().records.len(), 1);
}

#[test]
fn a_dropped_job_requeues_unsettled_rows_ahead_of_later_ones() {
    let engine = Engine::manual(ServiceConfig::default());
    let a = queue();
    engine.submit_many(
        a.core.clone(),
        [
            SubmitCommand::test_row("delivered"),
            SubmitCommand::test_row("in-flight"),
            SubmitCommand::test_row("unsent"),
        ],
    );
    let (key, entries) = engine.take_jobs().pop().unwrap();
    engine.submit(a.core.clone(), SubmitCommand::test_row("later"));
    let mut job = Job::new(engine.clone(), key, entries, None);
    // Rows 0 and 1 went in one upload; the request holding row 0 succeeded
    // before the job was dropped, while row 1's was still in flight.
    job.report.uploads.push((
        vec![vec![0], vec![1]].into(),
        Arc::new(std::sync::atomic::AtomicUsize::new(1)),
    ));
    // As when the job's runtime shuts down mid-upload.
    drop(job);
    assert_eq!(a.core.observation.len(), 3, "only the settled row retired");
    let jobs = engine.take_jobs();
    assert_eq!(jobs.len(), 1, "the partition is free again");
    let sequences: Vec<_> = jobs[0].1.iter().map(|entry| entry.sequence).collect();
    assert_eq!(sequences, [2, 3, 4]);
}

#[tokio::test]
async fn merged_rows_complete_all_owners_and_count_ancestor_loss_once() {
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(400))
        .expect(1)
        .mount(&server)
        .await;
    let engine = Engine::manual(ServiceConfig::default());
    let root = http_queue(&server, 100);
    let a = root.child();
    let b = root.child();
    engine.submit(a.core.clone(), SubmitCommand::test_span(false));
    engine.submit(b.core.clone(), SubmitCommand::test_span(true));
    let x = engine.barrier(&a.core.observation);
    let y = engine.barrier(&b.core.observation);
    let z = engine.barrier(&root.core.observation);
    let jobs = engine.take_jobs();
    assert_eq!(jobs.len(), 1);
    for (key, entries) in jobs {
        engine.clone().process_job(key, entries, None).await;
    }
    assert!(x.await.is_err());
    assert!(y.await.is_err());
    assert!(z.await.is_err());
    assert_eq!(a.dropped_rows().upload_failed, 1);
    assert_eq!(b.dropped_rows().upload_failed, 1);
    assert_eq!(root.dropped_rows().upload_failed, 1);
}
