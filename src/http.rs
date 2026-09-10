use std::ffi::OsString;
use std::path::PathBuf;

use reqwest::Client;

use crate::error::{BraintrustError, Result};

pub(crate) const CUSTOM_CA_BUNDLE_ENV: &str = "BRAINTRUST_CUSTOM_CA_BUNDLE";

#[derive(Clone, Debug)]
pub(crate) enum CaBundleConfig {
    File(PathBuf),
    Pem { contents: Vec<u8>, source: String },
    Environment(OsString),
}

impl CaBundleConfig {
    pub(crate) fn from_environment() -> Option<Self> {
        std::env::var_os(CUSTOM_CA_BUNDLE_ENV).map(Self::Environment)
    }

    pub(crate) fn from_pem(contents: impl AsRef<[u8]>, source: impl Into<String>) -> Self {
        Self::Pem {
            contents: contents.as_ref().to_vec(),
            source: source.into(),
        }
    }

    fn load_pem(&self) -> Result<(Vec<u8>, String)> {
        match self {
            Self::File(path) => std::fs::read(path)
                .map(|contents| (contents, path.display().to_string()))
                .map_err(|err| {
                    BraintrustError::InvalidConfig(format!(
                        "failed to read CA bundle {}: {}",
                        path.display(),
                        err
                    ))
                }),
            Self::Pem { contents, source } => Ok((contents.clone(), source.clone())),
            Self::Environment(value) => value
                .clone()
                .into_string()
                .map(|value| (value.into_bytes(), CUSTOM_CA_BUNDLE_ENV.to_string()))
                .map_err(|_| {
                    BraintrustError::InvalidConfig(format!(
                        "{} must contain valid UTF-8 PEM data",
                        CUSTOM_CA_BUNDLE_ENV
                    ))
                }),
        }
    }
}

pub(crate) fn build_http_client(
    timeout: std::time::Duration,
    ca_bundle: Option<&CaBundleConfig>,
) -> Result<Client> {
    let mut builder = Client::builder().timeout(timeout);

    if let Some(ca_bundle) = ca_bundle {
        let (pem, source) = ca_bundle.load_pem()?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|err| {
            BraintrustError::InvalidConfig(format!(
                "failed to parse PEM certificates from CA bundle {}: {}",
                source, err
            ))
        })?;
        if certs.is_empty() {
            return Err(BraintrustError::InvalidConfig(format!(
                "CA bundle {} did not contain any PEM certificates",
                source
            )));
        }
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
    }

    builder
        .build()
        .map_err(|err| BraintrustError::InvalidConfig(err.to_string()))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::{Arc, Once};
    use std::time::Duration;

    use rcgen::generate_simple_self_signed;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use tokio_rustls::rustls::ServerConfig;
    use tokio_rustls::TlsAcceptor;

    use super::*;

    pub(crate) static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    static CRYPTO_PROVIDER: Once = Once::new();

    pub(crate) const VALID_TEST_CERT_PEM: &str = r#"-----BEGIN CERTIFICATE-----
MIICpDCCAYwCCQDtlc4RX+IuODANBgkqhkiG9w0BAQsFADAUMRIwEAYDVQQDDAls
b2NhbGhvc3QwHhcNMjYwMzE3MTY1MzAyWhcNMjYwMzE4MTY1MzAyWjAUMRIwEAYD
VQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDX
K/y/7AlhzPBkIbiEiCt/l1Qfa99h8FdblOe8BCeJkpoW4Fw10mZnWBgX6peZMF7j
p4rjtIJTWkfl8eNoTPdOkYfmi6B3AwAZzl7VQCMgE0gCFkIrgXrkeLqP+q231UxE
wKgilRG3DWFfELZCQeFtq0jSBcnyWybw+o9SgajaQ+SJg7lbgT6o+8AwHQ54HBo+
VVZJ2CybZvmijQXGiVCMpZ34nxJVW/i6AbsFwp+CLMHOFjrpLuZpv61EnZaGsqsF
RG/VPiNca769Dr8YG4RtPRBKvyDMnUqEDkGwYXrhAVxvI3kKlQq3MHppGCSsjnVl
oqhWm//sE7znMJtuzIf7AgMBAAEwDQYJKoZIhvcNAQELBQADggEBAD1zS7eOkfU2
IzxjW7MAJce5JrAcRWWe3L2ORx+y+PS4uI0ms1FM4AopZ2FxXdbSSXLf5bqC2f2i
qy+8YbVdZacFtFLmnZicCXP86Na5JUYxZERDyqKN4GFwSrfELwLsuv9TWpir+p/H
3XxQ/8/eJdTHOunNtl4BVUefjGp9PVNb6NFvLDkSkNN37KcjNpB9jPVK970uZ5lb
kOx6ulbMXpNH73h5rwzgs6FbVbcAavPJKYGr170rDRxidpfRz3ex+RBQvcfFQeRx
NP64Q8OosOHraKRn7bvST7bXvGFZUp06aIFrlwdmSQPXU/6o4zYNmkR4RVv4VvQ7
cb0bfZ7fHHs=
-----END CERTIFICATE-----
"#;

    pub(crate) fn write_temp_bundle(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "braintrust-sdk-rust-ca-bundle-{}.pem",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, contents).expect("write temp bundle");
        path
    }

    async fn start_tls_server(expected_connections: usize) -> (String, String, JoinHandle<()>) {
        CRYPTO_PROVIDER.call_once(|| {
            tokio_rustls::rustls::crypto::ring::default_provider()
                .install_default()
                .expect("install ring crypto provider");
        });
        let certificate = generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("generate test certificate");
        let certificate_pem = certificate.cert.pem();
        let certificate_der: CertificateDer<'static> = certificate.cert.der().clone();
        let private_key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certificate.key_pair.serialize_der(),
        ));
        let server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate_der], private_key)
            .expect("configure TLS server");
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind TLS test server");
        let address = listener.local_addr().expect("test server address");

        let server = tokio::spawn(async move {
            for _ in 0..expected_connections {
                let (stream, _) = listener.accept().await.expect("accept test connection");
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    continue;
                };
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0_u8; 1024];
                    let read = stream.read(&mut chunk).await.expect("read test request");
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await
                    .expect("write test response");
            }
        });

        (
            format!("https://localhost:{}", address.port()),
            certificate_pem,
            server,
        )
    }

    #[test]
    fn rejects_missing_bundle_path() {
        let path = std::env::temp_dir().join(format!(
            "braintrust-sdk-rust-missing-{}.pem",
            uuid::Uuid::new_v4()
        ));

        let config = CaBundleConfig::File(path);
        let err = build_http_client(Duration::from_secs(1), Some(&config)).unwrap_err();

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err.to_string().contains("failed to read CA bundle"));
    }

    #[test]
    fn rejects_empty_bundle() {
        let path = write_temp_bundle("");

        let config = CaBundleConfig::File(path.clone());
        let err = build_http_client(Duration::from_secs(1), Some(&config)).unwrap_err();
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err
            .to_string()
            .contains("did not contain any PEM certificates"));
    }

    #[test]
    fn rejects_malformed_bundle() {
        let path = write_temp_bundle(
            "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
        );

        let config = CaBundleConfig::File(path.clone());
        let err = build_http_client(Duration::from_secs(1), Some(&config)).unwrap_err();
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err.to_string().contains("failed to parse PEM certificates"));
    }

    #[test]
    fn rejects_malformed_raw_pem_bundle() {
        let config = CaBundleConfig::from_pem(
            "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
            CUSTOM_CA_BUNDLE_ENV,
        );

        let err = build_http_client(Duration::from_secs(1), Some(&config)).unwrap_err();

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err.to_string().contains(CUSTOM_CA_BUNDLE_ENV));
        assert!(err.to_string().contains("failed to parse PEM certificates"));
    }

    #[test]
    fn accepts_valid_bundle() {
        let path = write_temp_bundle(VALID_TEST_CERT_PEM);

        let config = CaBundleConfig::File(path.clone());
        let client = build_http_client(Duration::from_secs(1), Some(&config));
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(client.is_ok());
    }

    #[test]
    fn accepts_multiple_pem_certificates() {
        let bundle = format!("{VALID_TEST_CERT_PEM}{VALID_TEST_CERT_PEM}");
        let config = CaBundleConfig::from_pem(bundle, "test bundle");

        let client = build_http_client(Duration::from_secs(1), Some(&config));

        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn multi_certificate_ca_bundle_enables_private_tls_server() {
        let (url, certificate_pem, server) = start_tls_server(2).await;

        let default_client =
            build_http_client(Duration::from_secs(1), None).expect("default client");
        assert!(default_client.get(&url).send().await.is_err());

        let unrelated_certificate = generate_simple_self_signed(vec!["unrelated.test".to_string()])
            .expect("generate unrelated test certificate");
        let bundle = format!("{}{certificate_pem}", unrelated_certificate.cert.pem());
        let config = CaBundleConfig::from_pem(bundle, "test bundle");
        let custom_ca_client =
            build_http_client(Duration::from_secs(1), Some(&config)).expect("custom CA client");
        let response = custom_ca_client
            .get(&url)
            .send()
            .await
            .expect("custom CA request");
        assert!(response.status().is_success());

        server.await.expect("TLS test server task");
    }

    #[tokio::test]
    async fn environment_ca_bundle_enables_private_tls_server() {
        let _lock = ENV_LOCK.lock().await;
        let original = std::env::var_os(CUSTOM_CA_BUNDLE_ENV);
        let (url, certificate_pem, server) = start_tls_server(1).await;
        std::env::set_var(CUSTOM_CA_BUNDLE_ENV, certificate_pem);

        let config = CaBundleConfig::from_environment().expect("environment CA bundle");
        let client =
            build_http_client(Duration::from_secs(1), Some(&config)).expect("custom CA client");
        let response = client.get(&url).send().await.expect("custom CA request");

        match original {
            Some(value) => std::env::set_var(CUSTOM_CA_BUNDLE_ENV, value),
            None => std::env::remove_var(CUSTOM_CA_BUNDLE_ENV),
        }

        assert!(response.status().is_success());
        server.await.expect("TLS test server task");
    }
}
