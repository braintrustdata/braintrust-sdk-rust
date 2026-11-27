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
    let engine = Engine::manual();
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
    let engine = Engine::manual();
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
    let engine = Engine::manual();
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
        engine.clone().process_job(key, entries).await;
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
    let engine = Engine::manual();
    let a = http_queue(&server, 100);
    let b = http_queue(&server, 100);
    engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
    engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
    let x = engine.barrier(&a.core.observation);
    let y = engine.barrier(&b.core.observation);
    for (key, entries) in engine.take_jobs() {
        engine.clone().process_job(key, entries).await;
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
    let engine = Engine::manual();
    let a = http_queue(&server, 1);
    let b = http_queue(&server, 1);
    engine.submit(a.core.clone(), SubmitCommand::test_row("a"));
    engine.submit(b.core.clone(), SubmitCommand::test_row("b"));
    let x = engine.barrier(&a.core.observation);
    let y = engine.barrier(&b.core.observation);
    for (key, entries) in engine.take_jobs() {
        engine.clone().process_job(key, entries).await;
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
    let engine = Engine::manual();
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
        engine.clone().process_job(key, entries).await;
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
    let engine = Engine::manual();
    let queue = http_queue(&server, 100);
    for token in ["first-token", "rotated-token"] {
        let mut command = SubmitCommand::test_named_span(token, "rotation-project");
        command.credential = Credential::token_value(token.into());
        engine.submit(queue.core.clone(), command);
        let flush = engine.barrier(&queue.core.observation);
        for (key, entries) in engine.take_jobs() {
            engine.clone().process_job(key, entries).await;
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
    let engine = Engine::manual();
    let queue = http_queue(&server, 100);
    engine.submit(
        queue.core.clone(),
        SubmitCommand::test_named_span("row", "flaky-project"),
    );
    let flush = engine.barrier(&queue.core.observation);
    for (key, entries) in engine.take_jobs() {
        engine.clone().process_job(key, entries).await;
    }
    assert_eq!(flush.await.unwrap().delivered(), 1);
}

#[test]
fn project_name_span_reserves_its_row_before_a_raw_update() {
    let queue = queue();
    let engine = Engine::manual();
    let first = engine.submit(
        queue.core.clone(),
        SubmitCommand::test_named_span("row", "name"),
    );
    engine.submit(
        queue.core.clone(),
        SubmitCommand::test_scoped_row("row", "id", "org"),
    );
    engine.submit(
        queue.core.clone(),
        SubmitCommand::test_scoped_row("independent", "other", "org"),
    );

    let jobs = engine.take_jobs();
    let (name_key, before) = jobs
        .into_iter()
        .find(|(_, rows)| rows[0].sequence == 1)
        .unwrap();
    assert_eq!(before.len(), 1);
    assert!(
        engine.take_jobs().is_empty(),
        "the raw update still waits for the span"
    );
    drop(Job::new(engine.clone(), name_key, before));
    drop(first);
    let after = engine.take_jobs();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].1[0].sequence, 2);
}

#[tokio::test]
async fn completed_batch_retires_while_another_batch_is_waiting() {
    use wiremock::{
        matchers::{body_string_contains, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_string_contains("held"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(10)))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .with_priority(2)
        .mount(&server)
        .await;

    let a = http_queue(&server, 1);
    let b = http_queue(&server, 1);
    let engine = Engine::manual();
    engine.submit(a.core.clone(), SubmitCommand::test_row("quick"));
    engine.submit(b.core.clone(), SubmitCommand::test_row("held"));
    let barrier = engine.barrier(&a.core.observation);
    let (key, entries) = engine.take_jobs().pop().unwrap();
    let job = tokio::spawn(engine.clone().process_job(key, entries));
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), barrier)
            .await
            .unwrap()
            .unwrap()
            .delivered(),
        1
    );
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn a_timed_out_owner_is_removed_before_a_shared_batch_uploads() {
    use crate::TokenProvider;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    struct Gated {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    #[async_trait::async_trait]
    impl TokenProvider for Gated {
        async fn token(&self) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            self.started.notify_one();
            self.release.notified().await;
            Ok("token".into())
        }
    }

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let a = http_queue(&server, 10);
    let b = http_queue(&server, 10);
    let engine = Engine::manual();
    let provider = Arc::new(Gated {
        started: Default::default(),
        release: Default::default(),
    });
    for (queue, id) in [(&a, "expired"), (&b, "active")] {
        let mut row = SubmitCommand::test_row(id);
        row.credential = Credential::Provider(provider.clone());
        engine.submit(queue.core.clone(), row);
    }
    let (key, entries) = engine.take_jobs().pop().unwrap();
    assert_eq!(entries.len(), 2);
    let job = tokio::spawn(engine.clone().process_job(key, entries));
    provider.started.notified().await;
    engine.expire(&a.core.observation);
    provider.release.notify_one();
    job.await.unwrap();

    let uploads = server.received_requests().await.unwrap();
    let upload = uploads
        .iter()
        .find(|request| request.url.path() == "/logs3")
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&upload.body).unwrap();
    assert_eq!(body["rows"].as_array().unwrap().len(), 1);
    assert_eq!(body["rows"][0]["id"], "active");
}

#[test]
fn distinct_known_projects_can_reuse_the_same_row_id() {
    let queue = queue();
    let engine = Engine::manual();
    for project in ["a", "b"] {
        engine.submit(
            queue.core.clone(),
            SubmitCommand::test_scoped_row("same", project, "org"),
        );
    }
    assert_eq!(engine.take_jobs().len(), 2);
}
