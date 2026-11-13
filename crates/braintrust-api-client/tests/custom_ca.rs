use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{self, pki_types::PrivatePkcs8KeyDer};

// Run the SDK transports in a separate process so its trust configuration
// cannot affect other tests.
#[tokio::test(flavor = "multi_thread")]
async fn custom_ca_is_used_for_backend_requests() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()).into(),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let server_url = url.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let url = server_url.clone();
            tokio::spawn(async move {
                let Ok(mut socket) = acceptor.accept(socket).await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() < end + 4 + length {
                        continue;
                    }
                    let first = headers.lines().next().unwrap();
                    let body = if first.contains("/api/apikey/login") {
                        serde_json::json!({"org_info":[{"id":"test-org-id","name":"test-org","api_url":url}]}).to_string()
                    } else if first.contains("/v1/project") {
                        r#"{"objects":[]}"#.into()
                    } else if first.contains("/oauth/token") {
                        r#"{"access_token":"test-token"}"#.into()
                    } else if first.contains("/mcp") {
                        r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26"}}"#
                            .into()
                    } else {
                        "{}".into()
                    };
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    return;
                }
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, cert.cert.pem()).unwrap();
    for mode in ["file", "raw_pem"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "backend_tls_child", "--nocapture"])
            .env("BT_TEST_TLS_URL", &url)
            .env("NO_PROXY", "localhost,127.0.0.1")
            .env("no_proxy", "localhost,127.0.0.1")
            .env_remove("SSL_CERT_FILE")
            .env_remove("SSL_CERT_DIR")
            .env_remove("BRAINTRUST_CUSTOM_CA_BUNDLE");
        if mode == "file" {
            command.env("SSL_CERT_FILE", &ca);
        } else {
            command.env("BRAINTRUST_CUSTOM_CA_BUNDLE", cert.cert.pem());
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "backend transports ({mode}): {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "invalid_bundle_child"])
        .env("BT_TEST_INVALID_CA", "1")
        .env("BRAINTRUST_CUSTOM_CA_BUNDLE", "not a certificate")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.abort();
}

#[test]
fn invalid_bundle_child() {
    if std::env::var_os("BT_TEST_INVALID_CA").is_none() {
        return;
    }
    let error = braintrust_api_client::Client::builder()
        .api_key("test-key")
        .build()
        .unwrap_err();
    assert!(error.to_string().contains("BRAINTRUST_CUSTOM_CA_BUNDLE"));
    assert!(
        braintrust_api_client::backend::build_api_client(
            "https://localhost",
            "https://localhost",
            "test-key",
            Duration::from_secs(5),
        )
        .is_err()
    );
}

#[test]
fn backend_tls_child() {
    let Ok(url) = std::env::var("BT_TEST_TLS_URL") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        use braintrust_api_client::backend::{
            self, BackendClient, auth::OAuthClient, mcp::McpSession,
        };
        let client =
            backend::build_api_client(&url, &url, "test-key", Duration::from_secs(5)).unwrap();
        client.login().await.unwrap();
        let default_client = braintrust_api_client::Client::builder()
            .api_key("test-key")
            .api_url(&url)
            .app_url(&url)
            .build()
            .unwrap();
        default_client.login().await.unwrap();
        client
            .upload_logs3_overflow(
                &{
                    let mut value = braintrust_api_client::transport::Logs3OverflowUpload::new(
                        "PUT",
                        format!("{url}/overflow"),
                        "test-key",
                    );
                    value.headers = None;
                    value.fields = None;
                    value
                },
                b"test".to_vec(),
            )
            .await
            .unwrap();
        let backend =
            BackendClient::new(&url, &url, "test-key", "test-org-id", "test-org", "bt-test")
                .unwrap();
        backend
            .generated()
            .get_project_builder()
            .org_name("test-org")
            .send()
            .await
            .unwrap();
        OAuthClient::new(&url)
            .unwrap()
            .refresh("test-client", "test-refresh")
            .await
            .unwrap();
        let mut mcp = McpSession::new(&url, "test-key", "bt-test", "1").unwrap();
        mcp.initialize().await.unwrap();
        backend::put_signed_url(&format!("{url}/signed"), b"test".to_vec(), None)
            .await
            .unwrap();
    });
}
