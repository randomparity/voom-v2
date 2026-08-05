use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::CertificateDer;
use rustls::{ClientConfig, RootCertStore};
use time::{Duration, OffsetDateTime};

pub struct TestCertificate {
    pub ca_der: CertificateDer<'static>,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
}

pub fn valid_localhost(directory: &Path) -> Result<TestCertificate, Box<dyn Error>> {
    localhost_certificate(directory, false)
}

pub fn expired_localhost(directory: &Path) -> Result<TestCertificate, Box<dyn Error>> {
    localhost_certificate(directory, true)
}

pub fn rustls_client(
    ca_der: CertificateDer<'static>,
    alpn_protocols: Vec<Vec<u8>>,
) -> Result<Arc<ClientConfig>, Box<dyn Error>> {
    let mut roots = RootCertStore::empty();
    roots.add(ca_der)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn_protocols;
    Ok(Arc::new(config))
}

fn localhost_certificate(
    directory: &Path,
    expired: bool,
) -> Result<TestCertificate, Box<dyn Error>> {
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate()?;
    let ca_cert = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::new(ca_params, ca_key);

    let mut server_params = CertificateParams::new(vec!["localhost".to_owned()])?;
    server_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    if expired {
        server_params.not_before = OffsetDateTime::UNIX_EPOCH;
        server_params.not_after = OffsetDateTime::UNIX_EPOCH + Duration::days(1);
    } else {
        server_params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
        server_params.not_after = OffsetDateTime::now_utc() + Duration::days(1);
    }
    let server_key = KeyPair::generate()?;
    let server_cert = server_params.signed_by(&server_key, &issuer)?;
    let cert_path = directory.join("server.pem");
    let key_path = directory.join("server.key");
    std::fs::write(&cert_path, server_cert.pem())?;
    std::fs::write(&key_path, server_key.serialize_pem())?;

    Ok(TestCertificate {
        ca_der: ca_cert.der().clone(),
        cert_path,
        key_path,
    })
}
