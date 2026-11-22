use braintrust_api_client::{Client, types};
use futures_util::StreamExt;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

#[tokio::test]
async fn invocation_delivers_events_before_the_response_finishes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (release, wait) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let header_end = loop {
            let mut buffer = [0; 2048];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(position) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
        assert!(headers.starts_with("post /v1/function/function/invoke "));
        assert!(headers.contains("authorization: bearer key\r\n"));
        assert!(headers.contains("accept: text/event-stream\r\n"));
        assert!(!headers.contains("x-bt-org-name"));
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        while request.len() < header_end + length {
            let mut buffer = [0; 2048];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request[header_end..]).unwrap(),
            json!({"stream":true})
        );
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        // Split an event across HTTP chunks and split a multi-byte UTF-8 character.
        let first = "id: 1\r\nevent: text_delta\r\ndata: h\u{00e9}".as_bytes();
        for chunk in [
            &first[..first.len() - 1],
            &first[first.len() - 1..],
            b"\r\n\r\n",
        ] {
            socket
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await
                .unwrap();
            socket.write_all(chunk).await.unwrap();
            socket.write_all(b"\r\n").await.unwrap();
        }
        wait.await.unwrap();
        let last = b"event: done\ndata: \n\n";
        socket
            .write_all(format!("{:x}\r\n", last.len()).as_bytes())
            .await
            .unwrap();
        socket.write_all(last).await.unwrap();
        socket.write_all(b"\r\n0\r\n\r\n").await.unwrap();
    });
    let client = Client::builder()
        .api_key("key")
        .org_name("org")
        .api_url(format!("http://{address}"))
        .build()
        .unwrap();
    let mut events = client
        .post_function_id_invoke_sse_builder("function")
        .stream(true)
        .send()
        .await
        .unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first.event, "text_delta");
    assert_eq!(first.data, "h\u{00e9}");
    assert_eq!(first.id.as_deref(), Some("1"));
    release.send(()).unwrap();
    let done = events.next().await.unwrap().unwrap();
    assert_eq!(done.event, "done");
    assert!(done.data.is_empty());
    assert!(events.next().await.is_none());
    server.await.unwrap();
}

#[tokio::test]
async fn proxy_methods_support_both_json_and_sse() {
    let server = MockServer::start().await;
    let client = Client::builder()
        .api_key("key")
        .api_url(server.uri())
        .build()
        .unwrap();
    for route in ["auto", "chat/completions", "completions"] {
        Mock::given(method("POST"))
            .and(path(format!("/v1/proxy/{route}")))
            .and(header("authorization", "Bearer key"))
            .and(header("accept", "text/event-stream"))
            .and(body_json(json!({"stream":true})))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "data: {\"delta\":\"hello\"}\n\ndata: [DONE]\n\n",
                "text/event-stream",
            ))
            .expect(1)
            .mount(&server)
            .await;
    }
    let mut streams = vec![
        client.proxyauto_sse(json!({"stream":true})).await.unwrap(),
        client
            .proxychat_completions_sse(json!({"stream":true}))
            .await
            .unwrap(),
        client
            .proxycompletions_sse(json!({"stream":true}))
            .await
            .unwrap(),
    ];
    for stream in &mut streams {
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event.event, "message");
        assert_eq!(event.data, "{\"delta\":\"hello\"}");
    }
    Mock::given(method("POST"))
        .and(path("/v1/proxy/chat/completions"))
        .and(header("accept", "application/json"))
        .and(body_json(json!({"stream":false})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices":[]})))
        .expect(1)
        .mount(&server)
        .await;
    let response = client
        .proxychat_completions(json!({"stream":false}))
        .await
        .unwrap();
    assert_eq!(response, json!({"choices":[]}));
}

#[tokio::test]
async fn eval_streams_named_events_and_preserves_http_errors() {
    let server = MockServer::start().await;
    let client = Client::builder()
        .api_key("key")
        .api_url(server.uri())
        .org_name("org")
        .build()
        .unwrap();
    Mock::given(method("POST")).and(path("/v1/eval"))
        .and(header("accept", "text/event-stream"))
        .and(header("authorization", "Bearer key"))
        .and(|request: &Request| !request.headers.contains_key("x-bt-org-name"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_raw("event: start\ndata: {\"experiment_id\":\"id\"}\n\nevent: summary\ndata: {\"scores\":{}}\n\nevent: done\ndata: \n\n", "text/event-stream"))
        .expect(1).mount(&server).await;
    let request: types::RunEval = serde_json::from_value(json!({
        "project_id":"project", "data":{"data":[]}, "task":{"function_id":"task"}, "scores":[], "stream":true
    })).unwrap();
    let events: Vec<_> = client
        .eval_launch_sse(request)
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(
        events
            .into_iter()
            .map(|event| event.unwrap().event)
            .collect::<Vec<_>>(),
        ["start", "summary", "done"]
    );
    Mock::given(method("POST"))
        .and(path("/v1/function/function/invoke"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "5")
                .set_body_string("slow down"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let error = match client.post_function_id_invoke_sse("function", None).await {
        Ok(_) => panic!("expected HTTP error"),
        Err(error) => error,
    };
    let response = error.api().unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(response.headers["retry-after"], "5");
    assert_eq!(response.raw_body, b"slow down");
}
