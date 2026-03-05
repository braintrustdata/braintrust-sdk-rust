use braintrust_sdk_rust::eval::*;
use braintrust_sdk_rust::*;

// Note: Full integration tests require Braintrust credentials.
// These tests verify type correctness and basic functionality.
// To run integration tests, set up credentials and remove #[ignore]

#[tokio::test]
#[ignore] // Requires Braintrust credentials
async fn test_basic_eval_flow() {
    let client = BraintrustClient::builder()
        .api_key("test-key")
        .build()
        .await
        .expect("Failed to create client");

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
