mod support;

use support::MockServer;

use braintrust_sdk_rust::{BraintrustClient, SpanComponents, SpanLog, SpanObjectType};
use serde_json::json;
use std::time::{Duration, Instant};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

const LOGS3_DELAY: Duration = Duration::from_millis(500);

async fn flush_rows(rows: usize) -> (Vec<usize>, Duration) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("{}")
                .set_delay(LOGS3_DELAY),
        )
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");

    for i in 0..rows {
        let exported = SpanComponents {
            object_type: SpanObjectType::ProjectLogs,
            object_id: Some("proj-id".to_string()),
            compute_object_metadata_args: None,
            row_id: Some(format!("row-{i}")),
            span_id: Some(format!("span-{i}")),
            root_span_id: Some(format!("root-{i}")),
            span_parents: None,
            propagated_event: None,
        }
        .to_str();
        client
            .update_span_with_credentials(
                "token",
                "org-id",
                &exported,
                SpanLog::builder()
                    .output(json!({ "i": i }))
                    .build()
                    .expect("build"),
            )
            .expect("update");
    }

    let started = Instant::now();
    client.flush().await.expect("flush");
    let elapsed = started.elapsed();

    let rows_per_request = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
        .map(|request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
            body["rows"].as_array().expect("rows").len()
        })
        .collect();
    (rows_per_request, elapsed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flush_sends_full_batches_with_bounded_concurrency() {
    // 1000 rows = 10 batches of 100. With at most 8 requests in flight, the
    // flush takes two round trips rather than one per batch.
    let (rows_per_request, elapsed) = flush_rows(1000).await;
    assert_eq!(rows_per_request.iter().sum::<usize>(), 1000);
    // A background flush may claim some rows first, adding one partial batch.
    assert!(
        rows_per_request.len() <= 11,
        "rows should be sent in batches of 100, got {rows_per_request:?}"
    );
    assert!(
        elapsed >= LOGS3_DELAY * 2,
        "at most 8 requests should be in flight, flush took {elapsed:?}"
    );
    assert!(
        elapsed < LOGS3_DELAY * 5,
        "batches should be sent concurrently, flush took {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_project_does_not_block_other_projects() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .and(body_string_contains("throttled-project"))
        .respond_with(ResponseTemplate::new(429))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/logs3"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .with_priority(2)
        .mount(&server)
        .await;

    let client = BraintrustClient::builder()
        .skip_login(true)
        .api_url(server.uri())
        .app_url(server.uri())
        .build()
        .await
        .expect("client");

    let destinations = (0..25)
        .map(|i| ("healthy-project", i))
        .chain(std::iter::once(("throttled-project", 25)));
    for (project, i) in destinations {
        let exported = SpanComponents {
            object_type: SpanObjectType::ProjectLogs,
            object_id: Some(project.to_string()),
            compute_object_metadata_args: None,
            row_id: Some(format!("row-{i}")),
            span_id: Some(format!("span-{i}")),
            root_span_id: Some(format!("root-{i}")),
            span_parents: None,
            propagated_event: None,
        }
        .to_str();
        client
            .update_span_with_credentials(
                "token",
                "org-id",
                &exported,
                SpanLog::builder()
                    .output(json!({ "i": i }))
                    .build()
                    .expect("build"),
            )
            .expect("update");
    }
    let _ = client.flush().await;

    let mut healthy_rows_accepted = 0;
    for request in server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/logs3")
    {
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json");
        let rows = body["rows"].as_array().expect("rows");
        let projects: std::collections::HashSet<_> =
            rows.iter().map(|row| row["project_id"].clone()).collect();
        assert_eq!(projects.len(), 1, "a request mixed projects: {projects:?}");
        if !String::from_utf8_lossy(&request.body).contains("throttled-project") {
            healthy_rows_accepted += rows.len();
        }
    }
    assert_eq!(healthy_rows_accepted, 25);
}
