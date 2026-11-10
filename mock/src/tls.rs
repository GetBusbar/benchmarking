// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// HTTPS ON THE MOCK, KEPT LIGHT (engine v1.2). Some gateways pin a real cloud endpoint over HTTPS
// (Bedrock runtime hosts, googleapis.com, the AWS SDK's default endpoints). With the box's DNS
// answering every name with the mock's address (dns.rs), those connections arrive here, on :443.
//
// - A certificate authority generated at start-up, for this run only. Its certificate is served at
//   `GET /__mock/ca` on the plain listener; the engine adds it to the gateway's trust bundle. The
//   private keys never leave this process.
// - One ECDSA P-256 key for every leaf. A leaf certificate is issued from the run's authority the
//   first time a name (SNI) is seen and cached, so a name costs one signature per run.
// - TLS 1.3 only, rustls with the ring provider. Stateless session tickets so a reconnecting client
//   resumes instead of paying a full handshake, and keep-alive as on the plain listener, so a pooled
//   gateway pays the handshake about once per connection it keeps.
// - Routing and handlers are the plain listener's, unchanged: the same `handle`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::{CertifiedKey, SigningKey};

/// The name a client that sends no SNI (it connected to a bare IP) is issued a certificate for.
const NO_SNI_NAME: &str = "localhost";

/// The run's certificate authority and the leaf certificates it has issued so far.
pub struct RunCa {
    issuer: Issuer<'static, KeyPair>,
    ca_der: CertificateDer<'static>,
    ca_pem: String,
    leaf_key: KeyPair,
    leaf_signer: Arc<dyn SigningKey>,
    issued: RwLock<HashMap<String, Arc<CertifiedKey>>>,
}

impl std::fmt::Debug for RunCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunCa").finish_non_exhaustive()
    }
}

impl RunCa {
    /// A fresh authority and leaf key, generated now.
    pub fn generate() -> Result<Self, String> {
        let ca_key =
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| format!("ca key: {e}"))?;
        let mut params = CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "On the Bench mock run CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        params.not_before = rcgen::date_time_ymd(2025, 1, 1);
        params.not_after = rcgen::date_time_ymd(2035, 1, 1);
        let ca_cert = params
            .self_signed(&ca_key)
            .map_err(|e| format!("ca certificate: {e}"))?;
        let leaf_key =
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| format!("leaf key: {e}"))?;
        let leaf_signer = rustls::crypto::ring::sign::any_ecdsa_type(&PrivateKeyDer::Pkcs8(
            PrivatePkcs8KeyDer::from(leaf_key.serialize_der()),
        ))
        .map_err(|e| format!("leaf signer: {e}"))?;
        Ok(RunCa {
            ca_der: ca_cert.der().clone(),
            ca_pem: ca_cert.pem(),
            issuer: Issuer::new(params, ca_key),
            leaf_key,
            leaf_signer,
            issued: RwLock::new(HashMap::new()),
        })
    }

    /// The authority's certificate, PEM. What a gateway must trust.
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    /// The certificate for `name`, issued on first sight and cached for the run.
    fn certified_for(&self, name: &str) -> Option<Arc<CertifiedKey>> {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if let Some(ck) = self.issued.read().ok()?.get(&name) {
            return Some(ck.clone());
        }
        let mut params = CertificateParams::new(vec![name.clone()]).ok()?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, name.as_str());
        params.is_ca = IsCa::NoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        params.not_before = rcgen::date_time_ymd(2025, 1, 1);
        params.not_after = rcgen::date_time_ymd(2035, 1, 1);
        let leaf = match params.signed_by(&self.leaf_key, &self.issuer) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("mock tls: could not issue a certificate for {name:?}: {e}");
                return None;
            }
        };
        let ck = Arc::new(CertifiedKey::new(
            vec![leaf.der().clone(), self.ca_der.clone()],
            self.leaf_signer.clone(),
        ));
        let mut issued = self.issued.write().ok()?;
        // A concurrent first sight of the same name may have won the race; keep whichever landed.
        Some(issued.entry(name).or_insert(ck).clone())
    }
}

impl ResolvesServerCert for RunCa {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.certified_for(hello.server_name().unwrap_or(NO_SNI_NAME))
    }
}

/// The listener's TLS configuration: TLS 1.3 only, ring, per-SNI leaves from `ca`, tickets on.
pub fn server_config(ca: Arc<RunCa>) -> Result<rustls::ServerConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| format!("tls versions: {e}"))?
        .with_no_client_auth()
        .with_cert_resolver(ca);
    // Both, because hyper's auto builder serves either; a client that asks for h2 gets it, as it
    // would from the real provider.
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    cfg.ticketer =
        rustls::crypto::ring::Ticketer::new().map_err(|e| format!("tls ticketer: {e}"))?;
    Ok(cfg)
}
