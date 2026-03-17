use std::path::Path;

use reqwest::Client;

use crate::error::{BraintrustError, Result};

pub(crate) fn build_http_client(
    timeout: std::time::Duration,
    ca_bundle: Option<&Path>,
) -> Result<Client> {
    let mut builder = Client::builder().timeout(timeout);

    if let Some(ca_bundle) = ca_bundle {
        let pem = std::fs::read(ca_bundle).map_err(|err| {
            BraintrustError::InvalidConfig(format!(
                "failed to read CA bundle {}: {}",
                ca_bundle.display(),
                err
            ))
        })?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem).map_err(|err| {
            BraintrustError::InvalidConfig(format!(
                "failed to parse PEM certificates from CA bundle {}: {}",
                ca_bundle.display(),
                err
            ))
        })?;
        if certs.is_empty() {
            return Err(BraintrustError::InvalidConfig(format!(
                "CA bundle {} did not contain any PEM certificates",
                ca_bundle.display()
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
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;

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

    #[test]
    fn rejects_missing_bundle_path() {
        let path = std::env::temp_dir().join(format!(
            "braintrust-sdk-rust-missing-{}.pem",
            uuid::Uuid::new_v4()
        ));

        let err = build_http_client(Duration::from_secs(1), Some(&path)).unwrap_err();

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err.to_string().contains("failed to read CA bundle"));
    }

    #[test]
    fn rejects_empty_bundle() {
        let path = write_temp_bundle("");

        let err = build_http_client(Duration::from_secs(1), Some(&path)).unwrap_err();
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

        let err = build_http_client(Duration::from_secs(1), Some(&path)).unwrap_err();
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(matches!(err, BraintrustError::InvalidConfig(_)));
        assert!(err.to_string().contains("failed to parse PEM certificates"));
    }

    #[test]
    fn accepts_valid_bundle() {
        let path = write_temp_bundle(VALID_TEST_CERT_PEM);

        let client = build_http_client(Duration::from_secs(1), Some(&path));
        std::fs::remove_file(&path).expect("remove temp bundle");

        assert!(client.is_ok());
    }
}
