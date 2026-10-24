//! Certificate inspection, wildcard matching, and chain checks.
//!
//! Uploaded PEM is parsed here before it is encrypted at rest. Matching is
//! pure so the panel can tell which certificate a new hostname will use.

use p256::elliptic_curve::sec1::ToSec1Point;
use p256::pkcs8::DecodePrivateKey;
use p256::SecretKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey as DecodeRsaPkcs8;
use rsa::traits::PublicKeyParts;
use rsa::{RsaPrivateKey, RsaPublicKey};
use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::GeneralName;
use x509_parser::pem::Pem;
use x509_parser::prelude::FromDer;
use x509_parser::public_key::PublicKey;

use crate::Error;

/// Facts shown on the certificate page and stored beside the ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateDetails {
    pub common_name: Option<String>,
    pub sans: Vec<String>,
    pub issuer: String,
    pub subject: String,
    pub not_before_unix: i64,
    pub not_after_unix: i64,
}

/// A stored certificate reduced to the names it can serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostMatch {
    pub name: String,
    pub common_name: Option<String>,
    pub sans: Vec<String>,
}

/// `*.domain.com` matches one DNS label and nothing else.
pub fn certificate_matches_host(host: &str, names: &[impl AsRef<str>]) -> bool {
    let Ok(host) = normalize_host(host) else {
        return false;
    };
    names.iter().any(|name| name_matches(&host, name.as_ref()))
}

/// First certificate whose SAN (or CN, when no SAN is present) matches `host`.
pub fn matching_certificate<'a>(
    host: &str,
    certificates: &'a [HostMatch],
) -> Option<&'a HostMatch> {
    certificates.iter().find(|certificate| {
        let names: Vec<&str> = match_names(certificate);
        certificate_matches_host(host, &names)
    })
}

/// Parse the leaf, check the private key, and verify any chain that was supplied.
pub fn validate_key_and_chain(
    cert_pem: &str,
    key_pem: &str,
    chain_pem: Option<&str>,
) -> Result<CertificateDetails, Error> {
    if cert_pem.len() > 256 * 1024 || key_pem.len() > 64 * 1024 {
        return Err(Error::Validation(
            "certificate or key is larger than the allowed size".into(),
        ));
    }
    if chain_pem.is_some_and(|chain| chain.len() > 256 * 1024) {
        return Err(Error::Validation("certificate chain is too large".into()));
    }
    if key_pem.contains("ENCRYPTED") {
        return Err(Error::Validation(
            "encrypted private keys are not supported; upload PKCS#8 or PKCS#1 PEM".into(),
        ));
    }
    let mut ders = pem_certificates(cert_pem)?;
    if let Some(chain) = chain_pem.filter(|value| !value.trim().is_empty()) {
        ders.extend(pem_certificates(chain)?);
    }
    if ders.is_empty() {
        return Err(Error::Validation(
            "no CERTIFICATE block found in the PEM".into(),
        ));
    }
    verify_links(&ders)?;
    let details = describe(&ders[0])?;
    ensure_key_matches(key_pem, &ders[0])?;
    Ok(details)
}

fn match_names(certificate: &HostMatch) -> Vec<&str> {
    if certificate.sans.is_empty() {
        certificate.common_name.as_deref().into_iter().collect()
    } else {
        certificate.sans.iter().map(String::as_str).collect()
    }
}

fn normalize_host(host: &str) -> Result<String, Error> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 {
        return Err(Error::Validation("invalid host".into()));
    }
    Ok(host)
}

fn name_matches(host: &str, pattern: &str) -> bool {
    let Ok(pattern) = normalize_host(pattern) else {
        return false;
    };
    if let Some(suffix) = pattern.strip_prefix("*.") {
        let Some((label, rest)) = host.split_once('.') else {
            return false;
        };
        return rest == suffix && !label.is_empty() && !label.contains('.') && label != "*";
    }
    host == pattern
}

fn pem_certificates(pem: &str) -> Result<Vec<Vec<u8>>, Error> {
    let mut ders = Vec::new();
    for block in Pem::iter_from_buffer(pem.as_bytes()) {
        let block = block.map_err(|_| Error::Validation("could not read PEM".into()))?;
        if block.label == "CERTIFICATE" {
            ders.push(block.contents);
        }
    }
    Ok(ders)
}

/// Facts of one DER certificate, such as the one a server presents in a handshake.
pub fn describe(der: &[u8]) -> Result<CertificateDetails, Error> {
    let cert = parse_cert(der)?;
    let common_name = cert
        .subject()
        .iter_common_name()
        .next()
        .and_then(|name| name.as_str().ok())
        .map(str::to_owned);
    let mut sans = Vec::new();
    if let Some(extension) = cert
        .subject_alternative_name()
        .map_err(|_| Error::Validation("invalid subject alternative name".into()))?
    {
        for name in &extension.value.general_names {
            if let GeneralName::DNSName(dns) = name {
                sans.push((*dns).to_owned());
            }
        }
    }
    let validity = cert.validity();
    Ok(CertificateDetails {
        common_name,
        sans,
        issuer: cert.issuer().to_string(),
        subject: cert.subject().to_string(),
        not_before_unix: validity.not_before.timestamp(),
        not_after_unix: validity.not_after.timestamp(),
    })
}

fn verify_links(ders: &[Vec<u8>]) -> Result<(), Error> {
    if ders.len() == 1 {
        let leaf = parse_cert(&ders[0])?;
        if leaf.issuer().to_string() == leaf.subject().to_string() {
            leaf.verify_signature(None).map_err(|_| {
                Error::Validation("self-signed certificate signature is invalid".into())
            })?;
        }
        return Ok(());
    }
    for index in 0..ders.len() - 1 {
        let signed = parse_cert(&ders[index])?;
        let signer = parse_cert(&ders[index + 1])?;
        if signed.issuer().to_string() != signer.subject().to_string() {
            return Err(Error::Validation("certificate chain is incomplete".into()));
        }
        signed
            .verify_signature(Some(signer.public_key()))
            .map_err(|_| Error::Validation("certificate chain signature check failed".into()))?;
    }
    let last = parse_cert(ders.last().expect("chain has a last certificate"))?;
    if last.issuer().to_string() == last.subject().to_string() {
        last.verify_signature(None)
            .map_err(|_| Error::Validation("root certificate signature is invalid".into()))?;
    }
    Ok(())
}

fn parse_cert(der: &[u8]) -> Result<X509Certificate<'_>, Error> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|_| Error::Validation("invalid X.509 certificate".into()))?;
    Ok(cert)
}

fn ensure_key_matches(key_pem: &str, der: &[u8]) -> Result<(), Error> {
    let cert = parse_cert(der)?;
    let public = cert
        .public_key()
        .parsed()
        .map_err(|_| Error::Validation("could not read the certificate public key".into()))?;
    let matches = match public {
        PublicKey::RSA(rsa) => rsa_matches(key_pem, rsa.modulus, rsa.exponent)?,
        PublicKey::EC(point) => ec_matches(key_pem, point.data())?,
        _ => {
            return Err(Error::Validation(
                "only RSA and P-256 certificates are supported".into(),
            ))
        }
    };
    if matches {
        Ok(())
    } else {
        Err(Error::Validation(
            "private key does not match the certificate".into(),
        ))
    }
}

fn rsa_matches(key_pem: &str, modulus: &[u8], exponent: &[u8]) -> Result<bool, Error> {
    let key = <RsaPrivateKey as DecodeRsaPkcs8>::from_pkcs8_pem(key_pem)
        .or_else(|_| <RsaPrivateKey as DecodeRsaPrivateKey>::from_pkcs1_pem(key_pem));
    let Ok(key) = key else {
        return Ok(false);
    };
    let public = RsaPublicKey::from(&key);
    Ok(unsigned_eq(&public.n().to_bytes_be(), modulus)
        && unsigned_eq(&public.e().to_bytes_be(), exponent))
}

fn ec_matches(key_pem: &str, point: &[u8]) -> Result<bool, Error> {
    if point.len() != 65 {
        return Err(Error::Validation(
            "only P-256 and RSA certificates are supported".into(),
        ));
    }
    let key = <SecretKey as DecodePrivateKey>::from_pkcs8_pem(key_pem)
        .or_else(|_| SecretKey::from_sec1_pem(key_pem));
    let Ok(key) = key else {
        return Ok(false);
    };
    let encoded = key.public_key().to_sec1_point(false);
    Ok(encoded.as_bytes() == point)
}

fn unsigned_eq(left: &[u8], right: &[u8]) -> bool {
    strip_zeros(left) == strip_zeros(right)
}

fn strip_zeros(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|byte| *byte != 0).unwrap_or(0);
    if start >= bytes.len() {
        &[]
    } else {
        &bytes[start..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{
        BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
        SanType,
    };

    #[test]
    fn wildcard_matches_one_label_only() {
        let names = ["*.domain.com", "api.domain.com"];
        assert!(certificate_matches_host("a.domain.com", &names));
        assert!(certificate_matches_host("API.domain.com", &names));
        assert!(!certificate_matches_host("domain.com", &names));
        assert!(!certificate_matches_host("a.b.domain.com", &names));
        assert!(certificate_matches_host("api.domain.com", &names));
    }

    #[test]
    fn san_takes_priority_over_common_name() {
        let certificate = HostMatch {
            name: "uploaded".into(),
            common_name: Some("other.example".into()),
            sans: vec!["app.example".into()],
        };
        assert!(matching_certificate("app.example", std::slice::from_ref(&certificate)).is_some());
        assert!(
            matching_certificate("other.example", std::slice::from_ref(&certificate)).is_none()
        );
    }

    #[test]
    fn common_name_is_used_when_there_is_no_san() {
        let certificate = HostMatch {
            name: "legacy".into(),
            common_name: Some("legacy.example".into()),
            sans: Vec::new(),
        };
        assert!(matching_certificate("legacy.example", &[certificate]).is_some());
    }

    #[test]
    fn generated_certificate_matches_its_key_and_wildcard_san() {
        let (cert_pem, key_pem) = self_signed(&["example.com".into(), "*.example.com".into()]);
        let details = validate_key_and_chain(&cert_pem, &key_pem, None).unwrap();
        assert!(details.sans.iter().any(|name| name == "*.example.com"));
        assert!(certificate_matches_host("www.example.com", &details.sans));
        assert!(!certificate_matches_host(
            "example.com.evil.test",
            &details.sans
        ));
        let (_other_cert, other_key) = self_signed(&["example.com".into()]);
        let error = validate_key_and_chain(&cert_pem, &other_key, None).unwrap_err();
        assert_eq!(
            error,
            Error::Validation("private key does not match the certificate".into())
        );
    }

    #[test]
    fn chain_must_link_and_verify() {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.distinguished_name = dn("Rudder Test CA");
        let ca = ca_params.self_signed(&ca_key).unwrap();

        let leaf_key = KeyPair::generate().unwrap();
        let mut leaf_params = CertificateParams::new(vec!["app.example.com".into()]).unwrap();
        leaf_params.distinguished_name = dn("app.example.com");
        let issuer = Issuer::from_params(&ca_params, &ca_key);
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();
        let details =
            validate_key_and_chain(&leaf.pem(), &leaf_key.serialize_pem(), Some(&ca.pem()))
                .unwrap();
        assert_eq!(details.sans, vec!["app.example.com".to_string()]);

        let other_key = KeyPair::generate().unwrap();
        let mut other_params = CertificateParams::default();
        other_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        other_params.distinguished_name = dn("Other CA");
        let other = other_params.self_signed(&other_key).unwrap();
        assert!(
            validate_key_and_chain(&leaf.pem(), &leaf_key.serialize_pem(), Some(&other.pem()))
                .is_err()
        );
    }

    fn self_signed(names: &[String]) -> (String, String) {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(names.to_vec()).unwrap();
        params.distinguished_name = dn(names.first().map(String::as_str).unwrap_or("example.com"));
        if let Some(first) = names.first() {
            params.subject_alt_names = names
                .iter()
                .map(|name| SanType::DnsName(name.clone().try_into().unwrap()))
                .collect();
            let _ = first;
        }
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    fn dn(common_name: &str) -> DistinguishedName {
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, common_name);
        name
    }
}
