//! Secure Boot in a new image's settings store, for CrabEFI.
//!
//! CrabEFI keeps Secure Boot in its variable store: the key databases (PK,
//! KEK, db, dbx: EFI signature lists) and its `SecureBootEnable` preference.
//! It trusts what the store holds at boot, so an image can start with them.
//! Without any, CrabEFI enrolls its default keys on first boot and leaves
//! Secure Boot off; an image that starts with Secure Boot on but no keys
//! would wait for a PK instead, so it gets those defaults written out.

use crabefi_efi_types::authentication::{
    EfiSignatureList, EfiVariableAuthentication2, SignatureIterator, SignatureListIterator,
    validate_signature_database,
};
use crabefi_efi_types::secure_boot::{
    EFI_CERT_TYPE_PKCS7_GUID, EFI_CERT_X509_GUID, EFI_GLOBAL_VARIABLE_GUID,
    EFI_IMAGE_SECURITY_DATABASE_GUID, defaults,
};
use fstart_options::varstore::Variable;

/// Secure Boot as a new image starts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecureBoot {
    /// Whether Secure Boot is on: CrabEFI's `SecureBootEnable`.
    pub enforce: bool,
    /// The key databases; `None` takes CrabEFI's defaults.
    pub keys: Option<Keys>,
}

/// Key databases, each from files as tools hand them out: a DER or PEM
/// X.509 certificate (a PEM file may hold several), an EFI signature list
/// (`.esl`), or a signed update of one (`.auth`, its signature list).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keys {
    /// The Platform Key: one certificate.
    pub pk: Vec<u8>,
    pub kek: Vec<Vec<u8>>,
    pub db: Vec<Vec<u8>>,
    pub dbx: Vec<Vec<u8>>,
}

/// Non-volatile, boot-service and runtime access, time-based authenticated
/// writes: the key databases' attributes.
const KEY_ATTRIBUTES: u32 = Variable::ATTRIBUTES | 0x20;

impl SecureBoot {
    /// The variables the store starts with: none when the image boots as
    /// CrabEFI would without them (default keys, Secure Boot off).
    pub fn variables(&self) -> Result<Vec<Variable<'static>>, String> {
        let databases = match &self.keys {
            None if !self.enforce => return Ok(Vec::new()),
            None => [
                ("PK", signature_lists(&[defaults::PK])),
                ("KEK", signature_lists(defaults::KEK)),
                ("db", signature_lists(defaults::DB)),
                ("dbx", Vec::new()),
            ],
            Some(keys) => {
                let pk = database("PK", std::slice::from_ref(&keys.pk), true)?;
                let certificates = SignatureListIterator::new(&pk)
                    .flat_map(|(list, bytes)| SignatureIterator::new(list, bytes))
                    .count();
                if certificates != 1 {
                    return Err(format!(
                        "Secure Boot PK: holds {certificates} certificates, not one"
                    ));
                }
                [
                    ("PK", pk),
                    ("KEK", database("KEK", &keys.kek, true)?),
                    ("db", database("db", &keys.db, true)?),
                    // Revocations need not be keys CrabEFI could verify with.
                    ("dbx", database("dbx", &keys.dbx, false)?),
                ]
            }
        };
        let mut variables: Vec<_> = (databases.into_iter())
            .filter(|(_, data)| !data.is_empty())
            .map(|(name, data)| Variable {
                guid: match name {
                    "PK" | "KEK" => EFI_GLOBAL_VARIABLE_GUID,
                    _ => EFI_IMAGE_SECURITY_DATABASE_GUID,
                },
                name,
                attributes: KEY_ATTRIBUTES,
                data,
            })
            .collect();
        variables.push(Variable {
            guid: EFI_GLOBAL_VARIABLE_GUID,
            name: "SecureBootEnable",
            attributes: Variable::ATTRIBUTES,
            data: vec![u8::from(self.enforce)],
        });
        Ok(variables)
    }

    /// What the image starts with, for its notes.
    pub fn describe(&self) -> String {
        let keys = match &self.keys {
            None if !self.enforce => return "Secure Boot off, CrabEFI's default keys".into(),
            None => "CrabEFI's default keys",
            Some(_) => "own keys",
        };
        let on = if self.enforce { "on" } else { "off" };
        format!("Secure Boot {on}, {keys}")
    }
}

/// The signature lists of `certificates`, each with its owner.
fn signature_lists(certificates: &[(&[u8], [u8; 16])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (der, owner) in certificates {
        let at = out.len();
        out.resize(at + EfiSignatureList::x509_size(der.len()), 0);
        EfiSignatureList::write_x509(&mut out[at..], owner, der).expect("sized above");
    }
    out
}

/// Database `name` from `files`; with `verified`, every certificate in it is
/// one CrabEFI can verify with.
fn database(name: &str, files: &[Vec<u8>], verified: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let lists = signature_database(file).map_err(|why| {
            let which = if files.len() > 1 {
                format!(" file {}", index + 1)
            } else {
                String::new()
            };
            format!("Secure Boot {name}{which}: {why}")
        })?;
        if verified {
            for (list, bytes) in SignatureListIterator::new(&lists) {
                if list.signature_type != EFI_CERT_X509_GUID {
                    continue;
                }
                for (_, der) in SignatureIterator::new(list, bytes) {
                    usable(der).map_err(|why| format!("Secure Boot {name}: {why}"))?;
                }
            }
        }
        out.extend_from_slice(&lists);
    }
    Ok(out)
}

/// A key file as signature lists: what [`Keys`] takes.
fn signature_database(file: &[u8]) -> Result<Vec<u8>, String> {
    if validate_signature_database(file) {
        return Ok(file.to_vec());
    }
    if let Some(auth) = EfiVariableAuthentication2::from_bytes(file)
        && auth.auth_info.cert_type == EFI_CERT_TYPE_PKCS7_GUID
        && let Some(lists) = auth.variable_data(file)
        && validate_signature_database(lists)
    {
        return Ok(lists.to_vec());
    }
    let certificates = match std::str::from_utf8(file) {
        Ok(text) if text.contains("-----BEGIN") => pem_certificates(text)?,
        _ => vec![file.to_vec()],
    };
    let mut lists = Vec::new();
    for der in &certificates {
        crabefi_pkcs7::x509::Certificate::parse(der).map_err(|_| {
            "not an X.509 certificate (DER or PEM), a signature list or a signed one".to_string()
        })?;
        lists.extend(signature_lists(&[(der, defaults::CRABEFI_OWNER_GUID)]));
    }
    Ok(lists)
}

/// The certificates of PEM `text`, in order.
fn pem_certificates(text: &str) -> Result<Vec<Vec<u8>>, String> {
    const END: &str = "-----END CERTIFICATE-----";
    let mut certificates = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN CERTIFICATE-----") {
        let end = rest[start..]
            .find(END)
            .map(|end| start + end + END.len())
            .ok_or("a PEM certificate does not end")?;
        let (_, der) = pem_rfc7468::decode_vec(&rest.as_bytes()[start..end])
            .map_err(|why| format!("PEM certificate: {why}"))?;
        certificates.push(der);
        rest = &rest[end..];
    }
    if certificates.is_empty() {
        return Err("a PEM file without a certificate".into());
    }
    Ok(certificates)
}

/// Whether CrabEFI can verify with certificate `der`: signed with
/// sha256WithRSAEncryption and holding an RSA key of at most 4096 bits, as
/// CrabEFI's verifier (and its build-time keys) require. A PK it cannot
/// verify with could never be updated.
fn usable(der: &[u8]) -> Result<(), String> {
    use crabefi_pkcs7::{oid, x509};
    let certificate = x509::Certificate::parse(der).map_err(|_| "a certificate is not X.509")?;
    let sha256_rsa = x509::AlgorithmIdentifier::parse(certificate.signature_algorithm)
        .is_ok_and(|algorithm| algorithm.is(oid::SHA256_WITH_RSA_ENCRYPTION));
    if !sha256_rsa {
        return Err("a certificate is not signed with sha256WithRSAEncryption".into());
    }
    let key = certificate
        .rsa_public_key()
        .map_err(|_| "a certificate holds no RSA key")?;
    if !key.is_well_formed() || key.modulus_bits() > 4096 {
        return Err("a certificate's RSA key is larger than 4096 bits or malformed".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS_KEK: &[u8] = defaults::MICROSOFT_KEK_CA_2011;

    fn pem(der: &[u8]) -> Vec<u8> {
        pem_rfc7468::encode_string("CERTIFICATE", pem_rfc7468::LineEnding::LF, der)
            .unwrap()
            .into_bytes()
    }

    fn named<'a>(variables: &'a [Variable], name: &str) -> Option<&'a Variable<'a>> {
        variables.iter().find(|variable| variable.name == name)
    }

    #[test]
    fn off_with_the_defaults_leaves_the_store_to_crabefi() {
        let none = SecureBoot {
            enforce: false,
            keys: None,
        };
        assert_eq!(none.variables().unwrap(), []);
    }

    #[test]
    fn on_with_the_defaults_writes_crabefis_defaults() {
        let on = SecureBoot {
            enforce: true,
            keys: None,
        };
        let variables = on.variables().unwrap();
        let names: Vec<_> = variables.iter().map(|v| v.name).collect();
        assert_eq!(names, ["PK", "KEK", "db", "SecureBootEnable"]);
        let db = named(&variables, "db").unwrap();
        assert_eq!(db.guid, EFI_IMAGE_SECURITY_DATABASE_GUID);
        assert_eq!(db.attributes, 0x27);
        assert_eq!(SignatureListIterator::new(&db.data).count(), 2);
        assert_eq!(named(&variables, "SecureBootEnable").unwrap().data, [1]);
        // Every default is a key CrabEFI verifies with.
        for (der, _) in core::iter::once(&defaults::PK)
            .chain(defaults::KEK)
            .chain(defaults::DB)
        {
            usable(der).unwrap();
        }
    }

    #[test]
    fn own_keys_come_as_der_pem_signature_lists_or_signed_lists() {
        let esl = signature_lists(&[(defaults::MICROSOFT_UEFI_CA_2011, [7; 16])]);
        // A signed update: timestamp, WIN_CERTIFICATE_UEFI_GUID, then its lists.
        let mut auth = vec![0u8; 16];
        auth.extend_from_slice(&24u32.to_le_bytes());
        auth.extend_from_slice(&0x0200u16.to_le_bytes());
        auth.extend_from_slice(&0x0ef1u16.to_le_bytes());
        auth.extend_from_slice(&EFI_CERT_TYPE_PKCS7_GUID);
        auth.extend_from_slice(&esl);
        let both = [pem(MS_KEK), pem(defaults::MICROSOFT_UEFI_CA_2011)].concat();
        let own = SecureBoot {
            enforce: false,
            keys: Some(Keys {
                pk: pem(MS_KEK),
                kek: vec![MS_KEK.to_vec()],
                db: vec![esl.clone(), both],
                dbx: vec![auth],
            }),
        };
        let variables = own.variables().unwrap();
        let pk = named(&variables, "PK").unwrap();
        assert_eq!(pk.guid, EFI_GLOBAL_VARIABLE_GUID);
        let (list, bytes) = SignatureListIterator::new(&pk.data).next().unwrap();
        let (owner, der) = SignatureIterator::new(list, bytes).next().unwrap();
        assert_eq!((owner, der), (defaults::CRABEFI_OWNER_GUID, MS_KEK));
        assert_eq!(
            SignatureListIterator::new(&named(&variables, "db").unwrap().data).count(),
            3
        );
        assert_eq!(named(&variables, "dbx").unwrap().data, esl);
        assert_eq!(named(&variables, "SecureBootEnable").unwrap().data, [0]);
    }

    #[test]
    fn keys_crabefi_cannot_use_are_refused() {
        let keys = |pk: Vec<u8>| SecureBoot {
            enforce: true,
            keys: Some(Keys {
                pk,
                ..Default::default()
            }),
        };
        assert!(keys(b"not a key".to_vec()).variables().is_err());
        assert!(keys(Vec::new()).variables().is_err());
        // Two certificates are not a PK.
        let two = [pem(MS_KEK), pem(MS_KEK)].concat();
        assert!(keys(two).variables().unwrap_err().contains("not one"));
        // A certificate signed otherwise than sha256WithRSAEncryption.
        let mut sha1 = MS_KEK.to_vec();
        let oid = crabefi_pkcs7::oid::SHA256_WITH_RSA_ENCRYPTION;
        let at = sha1.windows(oid.len()).rposition(|w| w == oid).unwrap();
        sha1[at + oid.len() - 1] = 5;
        assert!(keys(sha1).variables().is_err());
    }
}
