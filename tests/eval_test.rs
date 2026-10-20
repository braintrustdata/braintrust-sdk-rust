use braintrust_sdk_rust::eval::Dataset;
use braintrust_sdk_rust::eval::*;
use braintrust_sdk_rust::*;

async fn recording_client() -> (BraintrustClient, wiremock::MockServer) {
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    for (endpoint, response) in [
        (
            "/api/apikey/login",
            json!({"org_info":[{"id":"org","name":"org"}]}),
        ),
        ("/api/project/register", json!({"project":{"id":"project"}})),
        ("/logs3", json!({})),
    ] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let client = BraintrustClient::builder()
        .app_url(server.uri())
        .api_url(server.uri())
        .default_project("project")
        .blocking_login(true)
        .api_key("test-key")
        .build()
        .await
        .expect("Failed to create client");

    (client, server)
}

#[tokio::test]
async fn test_basic_eval_flow() {
    let (client, server) = recording_client().await;
    let evaluator = Evaluator::new(client);

    let cases = vec![
        Case {
            input: "hello".to_string(),
            expected: Some("HELLO".to_string()),
            ..Default::default()
        },
        Case {
            input: "world".to_string(),
            expected: Some("WORLD".to_string()),
            ..Default::default()
        },
    ];

    let task = SimpleFnTask::new(|input: &String| Ok(input.to_uppercase()));
    let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

    let result = evaluator
        .run(
            EvalOpts::builder()
                .name("test-basic-eval".to_string())
                .dataset(cases.into())
                .task(Box::new(task))
                .scorers(scorers)
                .quiet(true)
                .build(),
        )
        .await
        .expect("Evaluation failed");

    assert_eq!(result.total_cases, 2);
    assert_eq!(result.successful_cases, 2);
    assert!(result
        .results
        .iter()
        .all(|case| case.span_id.is_some() && case.errors.is_empty()));
    let requests = server.received_requests().await.unwrap();
    assert!(requests
        .iter()
        .any(|request| request.url.path() == "/logs3"));
}

#[test]
fn test_eval_types_compile() {
    // Test that all the types and builders work correctly
    let cases = vec![Case {
        input: "test".to_string(),
        expected: Some("TEST".to_string()),
        ..Default::default()
    }];

    let _dataset: Box<dyn Dataset<String, String>> = cases.into();
    let _task: Box<dyn Task<String, String>> =
        Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase())));
    let _scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

    // Verify Score creation
    let score = Score::new("test_score", 0.95);
    assert_eq!(score.name, "test_score");
    assert_eq!(score.score, 0.95);
}

#[test]
fn test_case_default() {
    let case: Case<String, String> = Case {
        input: "hello".to_string(),
        ..Default::default()
    };

    assert_eq!(case.input, "hello");
    assert_eq!(case.expected, None);
    assert_eq!(case.metadata, None);
    assert_eq!(case.tags, None);
}

#[tokio::test]
async fn test_scorers() {
    // Test built-in scorers work correctly

    // ExactMatch
    let scorer = ExactMatch::new();
    let args = ScorerArgs {
        input: &"test".to_string(),
        output: &"TEST".to_string(),
        expected: Some(&"TEST".to_string()),
        metadata: None,
    };
    let scores = scorer.score(args).await.unwrap();
    assert_eq!(scores[0].score, 1.0);

    // Levenshtein
    let scorer = LevenshteinScorer::new();
    let args = ScorerArgs {
        input: &"test".to_string(),
        output: &"hello".to_string(),
        expected: Some(&"hallo".to_string()),
        metadata: None,
    };
    let scores = scorer.score(args).await.unwrap();
    assert!(scores[0].score > 0.7); // ~0.8 similarity

    // NumericDiff
    let scorer = NumericDiffScorer::new();
    let args = ScorerArgs {
        input: &"test".to_string(),
        output: &10.0,
        expected: Some(&8.0),
        metadata: None,
    };
    let scores = scorer.score(args).await.unwrap();
    assert_eq!(scores[0].score, 2.0); // absolute difference
}

#[test]
fn test_eval_opts_builder() {
    // Verify the builder pattern works
    let cases = vec![Case {
        input: "test".to_string(),
        expected: Some("TEST".to_string()),
        ..Default::default()
    }];

    let dataset: Box<dyn Dataset<String, String>> = cases.into();
    let task: Box<dyn Task<String, String>> =
        Box::new(SimpleFnTask::new(|input: &String| Ok(input.to_uppercase())));
    let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![Box::new(ExactMatch::new())];

    let _opts = EvalOpts::builder()
        .name("test-eval".to_string())
        .dataset(dataset)
        .task(task)
        .scorers(scorers)
        .max_concurrency(10)
        .quiet(true)
        .build();
}

#[tokio::test]
async fn recorded_scorer_failure_keeps_valid_scores_and_finishes_the_case() {
    let (client, server) = recording_client().await;
    let evaluator = Evaluator::new(client);
    let scorers: Vec<Box<dyn Scorer<String, String>>> = vec![
        Box::new(ExactMatch::new()),
        Box::new(SyncFnScorer::new("broken", |_| {
            anyhow::bail!("scoring failed")
        })),
    ];
    let result = evaluator
        .run(
            EvalOpts::builder()
                .name("failed-scorer".into())
                .dataset(
                    vec![Case {
                        input: "a".to_owned(),
                        expected: Some("A".to_owned()),
                        id: Some("source-case".into()),
                        ..Default::default()
                    }]
                    .into(),
                )
                .task(Box::new(SimpleFnTask::new(|input: &String| {
                    Ok(input.to_uppercase())
                })))
                .scorers(scorers)
                .quiet(true)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(result.successful_cases, 0);
    assert_eq!(result.failed_cases, 1);
    assert_eq!(result.score_stats[0].name, "exact_match");
    assert_eq!(result.score_stats[0].mean, 1.0);
    let case = &result.results[0];
    assert_eq!(case.case_id.as_deref(), Some("source-case"));
    assert!(matches!(&case.errors[0], CaseError::Scorer {name,..} if name == "broken"));
    let rows: Vec<serde_json::Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/logs3")
        .flat_map(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()["rows"]
                .as_array()
                .unwrap()
                .clone()
        })
        .collect();
    assert!(rows
        .iter()
        .any(|row| row["error"][0]["stage"] == "scorer" && row["error"][0]["name"] == "broken"));
    assert!(rows
        .iter()
        .any(|row| row["span_id"].as_str() == case.span_id.as_deref()
            && row["metrics"]["end"].is_number()));
}
