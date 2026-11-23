// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure-Rust RSA-SHA256 PKCS#7/CMS detached signer and self-signed X.509 v3
//! DER certificate builder for UEFI FMP capsules.
//!
//! Runs identically on host CLIs (`fbuild`) and browser WASM (`tools/image-web`)
//! with no external `openssl` dependency.

use crabefi_pkcs7::cms::SignedData;
use crabefi_pkcs7::der::{Tlv, tag};
use crabefi_pkcs7::oid;
use crabefi_pkcs7::x509::Certificate;
use crypto_bigint::modular::runtime_mod::{DynResidue, DynResidueParams};
use crypto_bigint::{Encoding, U2048, U4096};

/// `rsaEncryption` OID (`1.2.840.113549.1.1.1`).
const OID_RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];

/// `commonName` OID (`2.5.4.3`).
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];

/// RFC 8017 §9.2 `DigestInfo` prefix for SHA-256 (19 bytes).
const SHA256_DIGEST_INFO_PREFIX: &[u8] = &[
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

/// Development/CI-only RSA-2048 key for QEMU A/B capsule testing.
///
/// NEVER use this key for production firmware! Production builds must supply
/// their own offline signing key and embed its certificate in RO.
pub const DEV_CAPSULE_RSA2048_PEM: &str = "\
-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEA1q/xsM0lLTkVztKkW398qKhY/cnwk7lfh5c+IAgYSfG0e/XG
x3blEXc/uYCHu0PMTB06MFsPvgNN5VF/k+u/gyaK7ZZaOsx/ITNHdm63Ntzjs56/
+6vo7gcl4jdjpOYENR8oZ6hPBBQkBX8vKFBSnxqq2DQEyTqd6a83CSPimC3lbv2t
XdooWhy5adFsUa6l22G5QWX0GXGfccM1t/UYfSO7BMHmaq1ZPibqBuCh2+caH49P
4/WWhn+ZrebuC/VXmUzBUemrNSLGfuMGUAdAsCWZyLDZTxml4jiV3tcUhcbQGZR+
K5G7wORvmOHxAIaNHm3V0SgBRAwx7oeCRqywGQIDAQABAoIBAFb7mGgy5gIpo91V
WQQ9ZguLkerZu+COddcg/ynJPdYP2Tl0PiKo2Gy4LRiCq/vCn26jVOUsr/cjrZbg
61jKZqDDp/xVM5+pJUUVWrnwZ8kRvCwVgOF+KHTwbnf2SygPjdrcEenVtpYHdwTH
IBssWB1gl6HbiajiWALLW/gXEwLmryJbxCBpDZ+lTK2qtS/I8oWRxBXWLEt4ctJs
XxB3HIezynWxYppw0ga6XMWYWz9uXGiwrOFw01ikd2ccjtIEcDvVkkrcPicpPwFI
l2JJZn5VGgVSEd65ZJZBg4yL1oR8Lv+ulZz4vLysbHKrjmwrC1A65vkYgG6D00wP
45/FLZUCgYEA+48KQhhjnhW4ICJpJ3w4EGUKadkT5+INIAZODtF1sYcucUmoAn6c
+XvZHmp+DoiBSt2dJlPLTVNN7NWdur0fR/a+5VmuEVatSq+NAoml7QWgeQ0TTR1z
vKWb3Hdt0vX3hyWyfSzNl+8NEGdyo7dDJh/hI/JZcHDuzpH46bZMRWcCgYEA2npB
7nJF63PKEXIkfk8uRc10iSHwIalfQApZxDwoN8KAIzNMVGWV/ElaRB/+keaj6NDO
j9qixdNuTEYqdxu0+uT0SPe8Z+yZOzCvn05mGe2bTwGNEbSv9EASh9kfx9vt1Ttd
rVCTvpMYcrSt0FmST176TW3vG+W1wf/nf+Bcbn8CgYBkbLcoqbnjG0KVOYeHJtTu
3pdoqCuFL74TodC5o677fz7wJPp4fHlIM8P6m4LVZHNRIuWgrmUD6pS6TpEYQnXh
ccTFpKaT72lg3nJJqA/gbpWgnbKt32sbzXVKbzcdt+noPwlhZWhyFhG+Eb43U2rw
Jlvn0NEZzUvYlXesPYV3UwKBgDYCLRROCXzYJebEKcNTvaOh06Im0p7vqM1DAcfh
+ys1mMma386pQjeYxoi6xAq7Ul+2ZqnnKX/v8X9HhS3mmom/WahEHb4vHf4k3fVq
wxC7zT3tqzrqSOj/gdU1VobollW1FEZI+g0Nfei1Q5aS3fljCEtl1ss1sQXhRhA1
nYPvAoGBAKmk89a4n8+NVtgJAiEW4ZsVG++Jx2D9lhULzhc4T+5Hbd6c3eX/8G3/
hHfYK1ubgLvJYqjcK+yp9H7qnbkkzbUO5uGiMzRFmR8iSGYq6fkyS79hqW1+8n5O
rBtpGsT/FpNUH7/JyKjOQ0tKLTx8oTLu25o0EoITQmsRGT9JZC7p
-----END RSA PRIVATE KEY-----";

/// Parsed RSA private key (up to 4096 bits) for pure-Rust PKCS#1 v1.5 SHA-256 signing.
#[derive(Clone)]
pub struct RsaPrivateKey {
    modulus: Vec<u8>,
    public_exponent: Vec<u8>,
    private_exponent: Vec<u8>,
}

impl RsaPrivateKey {
    /// Parse an RSA private key from PEM (`PRIVATE KEY` PKCS#8 or `RSA PRIVATE KEY` PKCS#1).
    pub fn from_pem(pem: &str) -> Result<Self, String> {
        let (label, der) =
            pem_rfc7468::decode_vec(pem.as_bytes()).map_err(|e| format!("invalid RSA PEM: {e}"))?;
        match label {
            "RSA PRIVATE KEY" => Self::from_pkcs1_der(&der),
            "PRIVATE KEY" => Self::from_pkcs8_der(&der),
            other => Err(format!("unsupported RSA private key PEM label '{other}'")),
        }
    }

    /// Parse a PKCS#8 `PrivateKeyInfo` DER blob wrapping a PKCS#1 `RSAPrivateKey`.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, String> {
        let mut fields = Tlv::parse(der)
            .and_then(|tlv| tlv.contents(tag::SEQUENCE))
            .map_err(|_| "invalid PKCS#8 PrivateKeyInfo DER")?;
        let _version = fields
            .read()
            .and_then(Tlv::unsigned_integer)
            .map_err(|_| "invalid PKCS#8 version")?;
        let mut alg = fields
            .read_tag(tag::SEQUENCE)
            .and_then(|tlv| tlv.contents(tag::SEQUENCE))
            .map_err(|_| "invalid PKCS#8 AlgorithmIdentifier")?;
        let alg_oid = alg
            .read()
            .and_then(Tlv::oid)
            .map_err(|_| "invalid PKCS#8 algorithm OID")?;
        if alg_oid != OID_RSA_ENCRYPTION {
            return Err("PKCS#8 key is not rsaEncryption".into());
        }
        let pkcs1 = fields
            .read()
            .and_then(Tlv::octet_string)
            .map_err(|_| "invalid PKCS#8 privateKey octet string")?;
        Self::from_pkcs1_der(pkcs1)
    }

    /// Parse a PKCS#1 `RSAPrivateKey` DER blob.
    pub fn from_pkcs1_der(der: &[u8]) -> Result<Self, String> {
        let mut fields = Tlv::parse(der)
            .and_then(|tlv| tlv.contents(tag::SEQUENCE))
            .map_err(|_| "invalid PKCS#1 RSAPrivateKey DER")?;
        let _version = fields
            .read()
            .and_then(Tlv::unsigned_integer)
            .map_err(|_| "invalid PKCS#1 version")?;
        let modulus = trim_leading_zeros(
            fields
                .read()
                .and_then(Tlv::unsigned_integer)
                .map_err(|_| "invalid RSA modulus")?,
        );
        let public_exponent = trim_leading_zeros(
            fields
                .read()
                .and_then(Tlv::unsigned_integer)
                .map_err(|_| "invalid RSA publicExponent")?,
        );
        let private_exponent = trim_leading_zeros(
            fields
                .read()
                .and_then(Tlv::unsigned_integer)
                .map_err(|_| "invalid RSA privateExponent")?,
        );

        let bits = modulus_bits(&modulus);
        if !(1024..=4096).contains(&bits) {
            return Err(format!(
                "unsupported RSA modulus length: {bits} bits (expected 1024..=4096)"
            ));
        }
        if modulus.last().copied().unwrap_or(0) & 1 == 0 {
            return Err("RSA modulus must be odd".into());
        }
        if public_exponent.is_empty()
            || (public_exponent.len() == 1 && public_exponent[0] < 3)
            || public_exponent.last().copied().unwrap_or(0) & 1 == 0
        {
            return Err("RSA public exponent must be an odd integer >= 3".into());
        }
        if private_exponent.is_empty() || private_exponent.len() > modulus.len() {
            return Err("RSA private exponent out of range".into());
        }

        Ok(Self {
            modulus,
            public_exponent,
            private_exponent,
        })
    }

    /// Return the modulus length in bytes.
    pub fn modulus_len(&self) -> usize {
        self.modulus.len()
    }

    /// Compute an RSASSA-PKCS1-v1_5 SHA-256 signature over `message`.
    pub fn sign_pkcs1v15_sha256(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        let digest = fstart_crypto::digest::hash_sha256(message);
        self.sign_pkcs1v15_sha256_digest(&digest)
    }

    /// Compute an RSASSA-PKCS1-v1_5 signature over a precomputed 32-byte SHA-256 digest.
    pub fn sign_pkcs1v15_sha256_digest(&self, digest: &[u8; 32]) -> Result<Vec<u8>, String> {
        let k = self.modulus.len();
        let t_len = SHA256_DIGEST_INFO_PREFIX.len() + digest.len();
        if k < t_len + 11 {
            return Err("RSA modulus too small for PKCS#1 v1.5 SHA-256".into());
        }
        let ps_len = k - t_len - 3;
        let mut em = Vec::with_capacity(k);
        em.push(0x00);
        em.push(0x01);
        em.resize(2 + ps_len, 0xff);
        em.push(0x00);
        em.extend_from_slice(SHA256_DIGEST_INFO_PREFIX);
        em.extend_from_slice(digest);
        debug_assert_eq!(em.len(), k);

        let sig = if k <= 256 {
            let n = u2048_from_be(&self.modulus);
            let d = u2048_from_be(&self.private_exponent);
            let m = u2048_from_be(&em);
            let params = DynResidueParams::new(&n);
            let s = DynResidue::new(&m, params).pow(&d).retrieve();
            let be = s.to_be_bytes();
            be[256 - k..].to_vec()
        } else if k <= 512 {
            let n = u4096_from_be(&self.modulus);
            let d = u4096_from_be(&self.private_exponent);
            let m = u4096_from_be(&em);
            let params = DynResidueParams::new(&n);
            let s = DynResidue::new(&m, params).pow(&d).retrieve();
            let be = s.to_be_bytes();
            be[512 - k..].to_vec()
        } else {
            return Err("RSA modulus exceeds 4096 bits".into());
        };

        Ok(sig)
    }

    /// Encode this key's PKCS#1 `RSAPublicKey` DER (`SEQUENCE { INTEGER n, INTEGER e }`).
    pub fn public_key_pkcs1_der(&self) -> Vec<u8> {
        let mut inner = Vec::new();
        inner.extend_from_slice(&der_unsigned_int(&self.modulus));
        inner.extend_from_slice(&der_unsigned_int(&self.public_exponent));
        der_seq(&inner)
    }

    /// Build a self-signed X.509 v3 DER certificate (`sha256WithRSAEncryption`)
    /// accepted by CrabEFI's capsule trust store and `crabefi_pkcs7`.
    pub fn self_signed_x509_der(
        &self,
        common_name: &str,
        serial: &[u8],
    ) -> Result<Vec<u8>, String> {
        if serial.is_empty() || serial.iter().all(|&b| b == 0) {
            return Err("X.509 serial number must be non-zero".into());
        }
        let sig_alg = der_alg_id(oid::SHA256_WITH_RSA_ENCRYPTION);
        let name = der_cn_name(common_name);
        let validity = {
            let mut v = Vec::new();
            v.extend_from_slice(&der_tlv(tag::UTC_TIME, b"200101000000Z"));
            v.extend_from_slice(&der_tlv(tag::GENERALIZED_TIME, b"20991231235959Z"));
            der_seq(&v)
        };
        let pubkey_pkcs1 = self.public_key_pkcs1_der();
        let spki = {
            let mut s = Vec::new();
            s.extend_from_slice(&der_alg_id(OID_RSA_ENCRYPTION));
            s.extend_from_slice(&der_bit_string(&pubkey_pkcs1, 0));
            der_seq(&s)
        };

        // Extensions: BasicConstraints(CA=TRUE), KeyUsage(digitalSignature|keyCertSign), SKI.
        let basic_constraints_ext = der_extension(
            oid::BASIC_CONSTRAINTS,
            true,
            &der_seq(&der_tlv(tag::BOOLEAN, &[0xff])),
        );
        // KeyUsage bit 0 = digitalSignature (0x80), bit 5 = keyCertSign (0x04), 2 unused bits.
        let key_usage_ext = der_extension(oid::KEY_USAGE, true, &der_bit_string(&[0x84], 2));
        let ski_hash = fstart_crypto::digest::hash_sha256(&pubkey_pkcs1);
        let ski_ext = der_extension(
            oid::SUBJECT_KEY_IDENTIFIER,
            false,
            &der_tlv(tag::OCTET_STRING, &ski_hash[..20]),
        );
        let mut exts = Vec::new();
        exts.extend_from_slice(&basic_constraints_ext);
        exts.extend_from_slice(&key_usage_ext);
        exts.extend_from_slice(&ski_ext);
        let exts_field = der_tlv(tag::context_constructed(3), &der_seq(&exts));

        let mut tbs_body = Vec::new();
        // version [0] EXPLICIT INTEGER v3(2)
        tbs_body.extend_from_slice(&der_tlv(
            tag::context_constructed(0),
            &der_unsigned_int(&[2]),
        ));
        tbs_body.extend_from_slice(&der_unsigned_int(serial));
        tbs_body.extend_from_slice(&sig_alg);
        tbs_body.extend_from_slice(&name); // issuer
        tbs_body.extend_from_slice(&validity);
        tbs_body.extend_from_slice(&name); // subject
        tbs_body.extend_from_slice(&spki);
        tbs_body.extend_from_slice(&exts_field);
        let tbs = der_seq(&tbs_body);

        let sig = self.sign_pkcs1v15_sha256(&tbs)?;
        let mut cert_body = Vec::new();
        cert_body.extend_from_slice(&tbs);
        cert_body.extend_from_slice(&sig_alg);
        cert_body.extend_from_slice(&der_bit_string(&sig, 0));
        Ok(der_seq(&cert_body))
    }

    /// Build a detached RFC 5652 CMS `ContentInfo(SignedData)` DER signature
    /// over `content` using SHA-256 and `sha256WithRSAEncryption`.
    pub fn sign_pkcs7_detached_sha256(
        &self,
        cert_der: &[u8],
        content: &[u8],
    ) -> Result<Vec<u8>, String> {
        let cert = Certificate::parse(cert_der)
            .map_err(|_| "invalid X.509 DER certificate for PKCS#7 signing")?;
        let rsa_pub = cert
            .rsa_public_key()
            .map_err(|_| "X.509 certificate does not contain a valid RSA public key")?;
        if trim_leading_zeros(rsa_pub.modulus) != self.modulus
            || trim_leading_zeros(rsa_pub.public_exponent) != self.public_exponent
        {
            return Err("RSA signing key does not match the provided X.509 certificate".into());
        }
        let serial_tlv = extract_cert_serial_tlv(cert.tbs)?;
        let content_digest = fstart_crypto::digest::hash_sha256(content);

        // SignedAttributes in canonical DER SET OF order:
        // contentType (1.2.840.113549.1.9.3) then messageDigest (1.2.840.113549.1.9.4).
        let attr_content_type = {
            let mut a = Vec::new();
            a.extend_from_slice(&der_tlv(tag::OID, oid::CONTENT_TYPE));
            a.extend_from_slice(&der_set(&der_tlv(tag::OID, oid::DATA)));
            der_seq(&a)
        };
        let attr_message_digest = {
            let mut a = Vec::new();
            a.extend_from_slice(&der_tlv(tag::OID, oid::MESSAGE_DIGEST));
            a.extend_from_slice(&der_set(&der_tlv(tag::OCTET_STRING, &content_digest)));
            der_seq(&a)
        };
        let mut attrs_inner = Vec::new();
        if attr_content_type <= attr_message_digest {
            attrs_inner.extend_from_slice(&attr_content_type);
            attrs_inner.extend_from_slice(&attr_message_digest);
        } else {
            attrs_inner.extend_from_slice(&attr_message_digest);
            attrs_inner.extend_from_slice(&attr_content_type);
        }

        // RFC 5652 §5.4: sign over the SET OF encoding (tag 0x31), emit with [0] IMPLICIT (0xA0).
        let attrs_for_signing = der_set(&attrs_inner);
        let sig = self.sign_pkcs1v15_sha256(&attrs_for_signing)?;
        let attrs_implicit = der_tlv(tag::context_constructed(0), &attrs_inner);

        let issuer_and_serial = {
            let mut ias = Vec::new();
            ias.extend_from_slice(cert.issuer);
            ias.extend_from_slice(serial_tlv);
            der_seq(&ias)
        };

        let sha256_alg = der_alg_id(oid::SHA256);
        let sig_alg = der_alg_id(oid::SHA256_WITH_RSA_ENCRYPTION);

        let signer_info = {
            let mut si = Vec::new();
            si.extend_from_slice(&der_unsigned_int(&[1])); // version = 1
            si.extend_from_slice(&issuer_and_serial);
            si.extend_from_slice(&sha256_alg);
            si.extend_from_slice(&attrs_implicit);
            si.extend_from_slice(&sig_alg);
            si.extend_from_slice(&der_tlv(tag::OCTET_STRING, &sig));
            der_seq(&si)
        };

        let encap_content_info = der_seq(&der_tlv(tag::OID, oid::DATA));
        let signed_data = {
            let mut sd = Vec::new();
            sd.extend_from_slice(&der_unsigned_int(&[1])); // version = 1
            sd.extend_from_slice(&der_set(&sha256_alg)); // digestAlgorithms
            sd.extend_from_slice(&encap_content_info); // detached id-data
            sd.extend_from_slice(&der_tlv(tag::context_constructed(0), cert_der)); // certificates
            sd.extend_from_slice(&der_set(&signer_info)); // signerInfos
            der_seq(&sd)
        };

        let mut content_info = Vec::new();
        content_info.extend_from_slice(&der_tlv(tag::OID, oid::SIGNED_DATA));
        content_info.extend_from_slice(&der_tlv(tag::context_constructed(0), &signed_data));
        let cms_der = der_seq(&content_info);

        // Self-verify the emitted CMS SignedData with crabefi_pkcs7 before returning.
        let parsed = SignedData::parse(&cms_der)
            .map_err(|_| "self-verification failed: produced CMS SignedData did not parse")?;
        let signer = parsed
            .signers()
            .next()
            .ok_or("self-verification failed: missing SignerInfo")?
            .map_err(|_| "self-verification failed: malformed SignerInfo")?;
        let attrs = signer
            .signed_attributes
            .ok_or("self-verification failed: missing SignedAttributes")?;
        if attrs.message_digest().ok() != Some(content_digest.as_slice()) {
            return Err("self-verification failed: messageDigest mismatch".into());
        }
        let attrs_digest = attrs.digest();
        if !rsa_pub.verify_pkcs1v15_sha256::<128>(signer.signature, &attrs_digest) {
            return Err("self-verification failed: RSA signature verification rejected".into());
        }

        Ok(cms_der)
    }
}

/// Extract the raw DER `INTEGER` TLV bytes for `TBSCertificate.serialNumber`
/// so CMS `IssuerAndSerialNumber` preserves the exact wire encoding even for
/// non-minimal or high-bit serial numbers.
fn extract_cert_serial_tlv(tbs_der: &[u8]) -> Result<&[u8], String> {
    let mut fields = Tlv::parse(tbs_der)
        .and_then(|tlv| tlv.contents(tag::SEQUENCE))
        .map_err(|_| "invalid X.509 TBSCertificate DER")?;
    let first = fields.read().map_err(|_| "empty X.509 TBSCertificate")?;
    let serial = if first.tag == tag::context_constructed(0) {
        fields
            .read_tag(tag::INTEGER)
            .map_err(|_| "missing X.509 serialNumber INTEGER")?
    } else if first.tag == tag::INTEGER {
        first
    } else {
        return Err("unexpected first field in X.509 TBSCertificate".into());
    };
    Ok(serial.encoded)
}

/// Return the deterministic dev/CI RSA-2048 signing key and its self-signed X.509 DER certificate.
pub fn dev_capsule_key_and_cert() -> Result<(RsaPrivateKey, Vec<u8>), String> {
    let key = RsaPrivateKey::from_pem(DEV_CAPSULE_RSA2048_PEM)?;
    let cert = key.self_signed_x509_der("fstart dev capsule test key", &[1])?;
    Ok((key, cert))
}

fn trim_leading_zeros(bytes: &[u8]) -> Vec<u8> {
    let first_nz = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    bytes[first_nz..].to_vec()
}

fn modulus_bits(modulus: &[u8]) -> usize {
    match modulus.first() {
        Some(&first) => (modulus.len() - 1) * 8 + (8 - first.leading_zeros() as usize),
        None => 0,
    }
}

fn u2048_from_be(bytes: &[u8]) -> U2048 {
    let mut padded = [0u8; 256];
    padded[256 - bytes.len()..].copy_from_slice(bytes);
    U2048::from_be_slice(&padded)
}

fn u4096_from_be(bytes: &[u8]) -> U4096 {
    let mut padded = [0u8; 512];
    padded[512 - bytes.len()..].copy_from_slice(bytes);
    U4096::from_be_slice(&padded)
}

fn der_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let be = len.to_be_bytes();
        let start = be.iter().position(|&b| b != 0).unwrap_or(be.len() - 1);
        let count = be.len() - start;
        out.push(0x80 | count as u8);
        out.extend_from_slice(&be[start..]);
    }
}

fn der_tlv(tag_byte: u8, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 5 + value.len());
    out.push(tag_byte);
    der_len(value.len(), &mut out);
    out.extend_from_slice(value);
    out
}

fn der_seq(body: &[u8]) -> Vec<u8> {
    der_tlv(tag::SEQUENCE, body)
}

fn der_set(body: &[u8]) -> Vec<u8> {
    der_tlv(tag::SET, body)
}

fn der_unsigned_int(magnitude: &[u8]) -> Vec<u8> {
    let trimmed = trim_leading_zeros(magnitude);
    if trimmed.is_empty() {
        return der_tlv(tag::INTEGER, &[0]);
    }
    if trimmed[0] & 0x80 != 0 {
        let mut buf = Vec::with_capacity(trimmed.len() + 1);
        buf.push(0);
        buf.extend_from_slice(&trimmed);
        der_tlv(tag::INTEGER, &buf)
    } else {
        der_tlv(tag::INTEGER, &trimmed)
    }
}

fn der_bit_string(bytes: &[u8], unused_bits: u8) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + bytes.len());
    buf.push(unused_bits);
    buf.extend_from_slice(bytes);
    der_tlv(tag::BIT_STRING, &buf)
}

fn der_alg_id(alg_oid: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&der_tlv(tag::OID, alg_oid));
    body.extend_from_slice(&der_tlv(tag::NULL, &[]));
    der_seq(&body)
}

fn der_cn_name(common_name: &str) -> Vec<u8> {
    let mut atv = Vec::new();
    atv.extend_from_slice(&der_tlv(tag::OID, OID_COMMON_NAME));
    atv.extend_from_slice(&der_tlv(0x0c, common_name.as_bytes())); // UTF8String
    der_seq(&der_set(&der_seq(&atv)))
}

fn der_extension(ext_oid: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&der_tlv(tag::OID, ext_oid));
    if critical {
        body.extend_from_slice(&der_tlv(tag::BOOLEAN, &[0xff]));
    }
    body.extend_from_slice(&der_tlv(tag::OCTET_STRING, value));
    der_seq(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabefi_pkcs7::cms::{SignedData, SignerIdentifier};

    #[test]
    fn self_signed_cert_and_detached_pkcs7_verify_with_crabefi_pkcs7() {
        let (key, cert_der) = dev_capsule_key_and_cert().unwrap();
        let cert = Certificate::parse(&cert_der).unwrap();
        let rsa_pub = cert.rsa_public_key().unwrap();
        assert!(rsa_pub.is_well_formed());
        assert_eq!(rsa_pub.modulus_bits(), 2048);

        // Verify self-signature on the X.509 certificate itself.
        let tbs_digest = fstart_crypto::digest::hash_sha256(cert.tbs);
        assert!(rsa_pub.verify_pkcs1v15_sha256::<64>(cert.signature, &tbs_digest));

        // Verify extensions and validity window parse cleanly.
        assert!(cert.validity().is_ok());
        let bc = cert.basic_constraints().unwrap().unwrap();
        assert!(bc.ca);
        let ku = cert.key_usage().unwrap().unwrap();
        assert!(ku.digital_signature());
        assert!(ku.key_cert_sign());
        assert!(cert.subject_key_identifier().unwrap().is_some());

        // Sign a detached payload and verify with crabefi_pkcs7::cms::SignedData.
        let payload = b"fstart UEFI FMP capsule test payload + monotonic count";
        let pkcs7_der = key.sign_pkcs7_detached_sha256(&cert_der, payload).unwrap();
        let signed = SignedData::parse(&pkcs7_der).unwrap();
        assert_eq!(signed.encapsulated_content.content_type, oid::DATA);
        assert!(signed.encapsulated_content.content.is_none());

        let certs: Vec<_> = signed.certificates().collect();
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].encoded, cert_der.as_slice());

        let signers: Vec<_> = signed.signers().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(signers.len(), 1);
        let signer = signers[0];
        match signer.identifier {
            SignerIdentifier::IssuerAndSerialNumber { issuer, serial } => {
                assert_eq!(issuer, cert.issuer);
                assert_eq!(serial, cert.serial);
            }
            other => panic!("unexpected signer id {other:?}"),
        }
        let attrs = signer.signed_attributes.unwrap();
        assert_eq!(attrs.content_type().unwrap(), oid::DATA);
        assert_eq!(
            attrs.message_digest().unwrap(),
            fstart_crypto::digest::hash_sha256(payload).as_slice()
        );
        let attrs_digest = attrs.digest();
        assert!(rsa_pub.verify_pkcs1v15_sha256::<64>(signer.signature, &attrs_digest));

        // Tampering with the signature must fail verification.
        let mut bad_sig = signer.signature.to_vec();
        bad_sig[0] ^= 1;
        assert!(!rsa_pub.verify_pkcs1v15_sha256::<64>(&bad_sig, &attrs_digest));
    }

    #[test]
    fn pkcs8_der_wrapping_parses_identically_to_pkcs1_pem() {
        let (key, _) = dev_capsule_key_and_cert().unwrap();
        let (_, pkcs1_der) = pem_rfc7468::decode_vec(DEV_CAPSULE_RSA2048_PEM.as_bytes()).unwrap();
        let mut pkcs8_body = Vec::new();
        pkcs8_body.extend_from_slice(&der_unsigned_int(&[0]));
        pkcs8_body.extend_from_slice(&der_alg_id(OID_RSA_ENCRYPTION));
        pkcs8_body.extend_from_slice(&der_tlv(tag::OCTET_STRING, &pkcs1_der));
        let pkcs8_der = der_seq(&pkcs8_body);

        let from_pkcs8 = RsaPrivateKey::from_pkcs8_der(&pkcs8_der).unwrap();
        let sig1 = key.sign_pkcs1v15_sha256(b"hello").unwrap();
        let sig2 = from_pkcs8.sign_pkcs1v15_sha256(b"hello").unwrap();
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn rejects_weak_public_exponent_one() {
        let (key, _) = dev_capsule_key_and_cert().unwrap();
        // Rebuild a PKCS#1 RSAPrivateKey DER sequence with e = 1.
        let mut body = Vec::new();
        body.extend_from_slice(&der_unsigned_int(&[0])); // version
        body.extend_from_slice(&der_unsigned_int(&key.modulus)); // modulus
        body.extend_from_slice(&der_unsigned_int(&[1])); // e = 1 (insecure!)
        body.extend_from_slice(&der_unsigned_int(&[1])); // d
        let der = der_seq(&body);
        let Err(err) = RsaPrivateKey::from_pkcs1_der(&der) else {
            panic!("expected e=1 key to be rejected");
        };
        assert!(
            err.contains("public exponent"),
            "unexpected error for e=1: {err}"
        );
    }

    #[test]
    fn rejects_mismatched_cert_and_signing_key() {
        let (mut key, cert_der) = dev_capsule_key_and_cert().unwrap();
        // Mutate the key's modulus so it no longer matches cert_der.
        key.modulus[0] ^= 0x01;
        let err = key
            .sign_pkcs7_detached_sha256(&cert_der, b"payload")
            .unwrap_err();
        assert!(
            err.contains("does not match"),
            "unexpected error for cert/key mismatch: {err}"
        );
    }

    #[test]
    fn dev_capsule_cert_matches_qemu_q35_ab_fixture() {
        let (_, cert_der) = dev_capsule_key_and_cert().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../boards/qemu/q35-ab/dev-capsule-cert.der");
        if std::env::var_os("UPDATE_DEV_CERT").is_some() {
            std::fs::write(&fixture, &cert_der).unwrap();
        }
        let committed = std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", fixture.display()));
        assert_eq!(
            committed, cert_der,
            "boards/qemu/q35-ab/dev-capsule-cert.der is out of sync with dev_capsule_key_and_cert()"
        );
    }
}
