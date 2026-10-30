//! Certificates for the local rakumux.com, made afresh for each run (no key
//! is kept anywhere): a CA, which the account service trusts through
//! `RAKUMUX_CLOUD_CA`, and the server's certificate for 127.0.0.1 it signs.
//! ECDSA P-256 with SHA-256, encoded by hand as the few DER shapes X.509
//! needs.

use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

/// What the server and the client need.
pub struct TestCerts {
    /// The CA's certificate, PEM: what `RAKUMUX_CLOUD_CA` names.
    pub ca_pem: String,
    /// The server's chain (its certificate, then the CA's), DER.
    pub chain: Vec<Vec<u8>>,
    /// The server's private key, PKCS #8 DER.
    pub key: Vec<u8>,
}

/// `ecdsa-with-SHA256`, as an AlgorithmIdentifier's contents.
const ECDSA_SHA256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const PRIME256V1: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];
const SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

impl TestCerts {
    pub fn new() -> Self {
        let rng = SystemRandom::new();
        let keypair = |pkcs8: &[u8]| {
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8, &rng).unwrap()
        };
        let ca_pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let ca_key = keypair(ca_pkcs8.as_ref());
        let server_pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let server_key = keypair(server_pkcs8.as_ref());
        const CA_NAME: &str = "Rakumux e2e test CA";
        let ca = certificate(
            1,
            CA_NAME,
            CA_NAME,
            ca_key.public_key().as_ref(),
            vec![
                extension(BASIC_CONSTRAINTS, true, sequence(&[vec![0x01, 0x01, 0xff]])),
                // keyCertSign, cRLSign.
                extension(KEY_USAGE, true, vec![0x03, 0x02, 0x01, 0x06]),
            ],
            &ca_key,
            &rng,
        );
        let server = certificate(
            2,
            CA_NAME,
            "127.0.0.1",
            server_key.public_key().as_ref(),
            vec![
                extension(BASIC_CONSTRAINTS, true, sequence(&[])),
                // digitalSignature.
                extension(KEY_USAGE, true, vec![0x03, 0x02, 0x07, 0x80]),
                extension(
                    EXTENDED_KEY_USAGE,
                    false,
                    sequence(&[tlv(0x06, SERVER_AUTH)]),
                ),
                extension(
                    SUBJECT_ALT_NAME,
                    false,
                    sequence(&[tlv(0x87, &[127, 0, 0, 1]), tlv(0x82, b"localhost")]),
                ),
            ],
            &ca_key,
            &rng,
        );
        Self {
            ca_pem: pem("CERTIFICATE", &ca),
            chain: vec![server, ca],
            key: server_pkcs8.as_ref().to_vec(),
        }
    }
}

/// A DER element: `tag`, its length, `content`.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let length = content.len();
    match length {
        0..=0x7f => out.push(length as u8),
        0x80..=0xff => out.extend([0x81, length as u8]),
        _ => out.extend([0x82, (length >> 8) as u8, length as u8]),
    }
    out.extend_from_slice(content);
    out
}

fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

/// A Name of one common name.
fn name(common: &str) -> Vec<u8> {
    let attribute = sequence(&[tlv(0x06, COMMON_NAME), tlv(0x0c, common.as_bytes())]);
    sequence(&[tlv(0x31, &attribute)])
}

fn extension(oid: &[u8], critical: bool, value: Vec<u8>) -> Vec<u8> {
    let mut parts = vec![tlv(0x06, oid)];
    if critical {
        parts.push(vec![0x01, 0x01, 0xff]);
    }
    parts.push(tlv(0x04, &value));
    sequence(&parts)
}

fn bits(bytes: &[u8]) -> Vec<u8> {
    let mut content = vec![0];
    content.extend_from_slice(bytes);
    tlv(0x03, &content)
}

/// An X.509 v3 certificate for `public` (an uncompressed P-256 point),
/// valid from 2020 to 2049, signed by `signer`.
fn certificate(
    serial: u8,
    issuer: &str,
    subject: &str,
    public: &[u8],
    extensions: Vec<Vec<u8>>,
    signer: &EcdsaKeyPair,
    rng: &SystemRandom,
) -> Vec<u8> {
    let algorithm = tlv(0x30, ECDSA_SHA256);
    let key_info = sequence(&[
        sequence(&[EC_PUBLIC_KEY.to_vec(), PRIME256V1.to_vec()]),
        bits(public),
    ]);
    let tbs = sequence(&[
        tlv(0xa0, &tlv(0x02, &[2])),
        tlv(0x02, &[serial]),
        algorithm.clone(),
        name(issuer),
        sequence(&[tlv(0x17, b"200101000000Z"), tlv(0x17, b"491231235959Z")]),
        name(subject),
        key_info,
        tlv(0xa3, &sequence(&extensions)),
    ]);
    let signature = signer.sign(rng, &tbs).unwrap();
    sequence(&[tbs, algorithm, bits(signature.as_ref())])
}

fn pem(label: &str, der: &[u8]) -> String {
    use base64::Engine;
    let text = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in text.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}
