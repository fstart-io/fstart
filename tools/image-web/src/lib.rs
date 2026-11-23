//! WASM bindings over `Bundle` for the web UI (`www/`) and other browser callers.
use fstart_image_build::bundle::{AssemblyMode, Bundle, CapsuleSigning, Supplied};
use fstart_image_build::options::Settings;
use fstart_image_build::pkcs7_sign::{RsaPrivateKey, decode_cert_pem_or_der};
use fstart_image_build::secure_boot::{Keys, SecureBoot};
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

struct StoredCapsuleSigning {
    rsa_key: Vec<u8>,
    cert: Vec<u8>,
    monotonic_count: u64,
    lowest_supported_version: Option<u32>,
}

#[wasm_bindgen]
pub struct ImageAssembler {
    bundle: Bundle,
    inputs: BTreeMap<String, Vec<u8>>,
    donor: Option<Vec<u8>>,
    intel_flash: Option<Vec<u8>>,
    settings: BTreeMap<String, String>,
    secure_boot: Option<SecureBoot>,
    update_only: bool,
    fw_version: Option<u32>,
    security_version: Option<u64>,
    capsule_signing: Option<StoredCapsuleSigning>,
}

#[wasm_bindgen]
impl ImageAssembler {
    #[wasm_bindgen(constructor)]
    pub fn new(compressed_bundle: &[u8]) -> Result<ImageAssembler, JsValue> {
        Ok(Self {
            bundle: Bundle::decode(compressed_bundle).map_err(error)?,
            inputs: BTreeMap::new(),
            donor: None,
            intel_flash: None,
            settings: BTreeMap::new(),
            secure_boot: None,
            update_only: false,
            fw_version: None,
            security_version: None,
            capsule_signing: None,
        })
    }

    /// JSON manifest, including required inputs, stage binaries and hashes.
    pub fn manifest(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.bundle.manifest).map_err(error)
    }

    /// JSON of the options the build offers (`fbuild options`): their
    /// keys, forms, legal values and the board's values, and the store.
    pub fn options(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.bundle.manifest.plan.options).map_err(error)
    }

    /// Whether update-only assembly still requires the vendor donor image.
    #[wasm_bindgen(js_name = updateRequiresDonor)]
    pub fn update_requires_donor(&self) -> bool {
        self.bundle.manifest.update_requires_donor()
    }

    /// Default FFS `security_version` as an exact decimal string (avoids JS Number precision loss).
    #[wasm_bindgen(js_name = defaultSecurityVersion)]
    pub fn default_security_version(&self) -> Option<String> {
        self.bundle
            .manifest
            .plan
            .ffs_step()
            .map(|(_, ffs)| ffs.security_version.to_string())
    }

    /// Start the image's settings store with `key` = `value` (a number,
    /// `true`/`false` or a choice's label), as `fbuild assemble --setting`
    /// does; an empty `value` clears it.
    #[wasm_bindgen(js_name = setSetting)]
    pub fn set_setting(&mut self, key: String, value: String) -> Result<(), JsValue> {
        if value.is_empty() {
            self.settings.remove(&key);
            return Ok(());
        }
        let options = &self.bundle.manifest.plan.options;
        let setting = Settings {
            options: vec![(key.clone(), value.clone())],
            secure_boot: None,
        };
        options.seed(&setting).map_err(error)?;
        self.settings.insert(key, value);
        Ok(())
    }

    /// Start the image with Secure Boot `enforce`d or not. Without a `pk` it
    /// holds CrabEFI's default keys; with one, the given databases: each an
    /// array of files (a DER or PEM certificate, a signature list or a
    /// signed one). As `fbuild assemble --secure-boot` does.
    #[wasm_bindgen(js_name = setSecureBoot)]
    pub fn set_secure_boot(
        &mut self,
        enforce: bool,
        pk: Option<Vec<u8>>,
        kek: js_sys::Array,
        db: js_sys::Array,
        dbx: js_sys::Array,
    ) -> Result<(), JsValue> {
        let files = |array: js_sys::Array| -> Vec<Vec<u8>> {
            (array.iter())
                .map(|file| js_sys::Uint8Array::new(&file).to_vec())
                .collect()
        };
        let secure_boot = SecureBoot {
            enforce,
            keys: pk.map(|pk| Keys {
                pk,
                kek: files(kek),
                db: files(db),
                dbx: files(dbx),
            }),
        };
        let setting = Settings {
            options: Vec::new(),
            secure_boot: Some(secure_boot.clone()),
        };
        self.bundle
            .manifest
            .plan
            .options
            .seed(&setting)
            .map_err(error)?;
        self.secure_boot = Some(secure_boot);
        Ok(())
    }

    /// Choose full ROM assembly (`false`) or update-only RW slot assembly (`true`).
    #[wasm_bindgen(js_name = setUpdateOnly)]
    pub fn set_update_only(&mut self, update_only: bool) -> Result<(), JsValue> {
        if update_only && self.bundle.manifest.plan.update.is_none() {
            return Err(error("this build does not support update-only assembly"));
        }
        self.update_only = update_only;
        Ok(())
    }

    /// Override the signed FFS `Root` firmware version (`fw_version`).
    #[wasm_bindgen(js_name = setFirmwareVersion)]
    pub fn set_firmware_version(&mut self, version: Option<u32>) {
        self.fw_version = version;
    }

    /// Override the signed FFS `Root` rollback security version (decimal `u64` string).
    #[wasm_bindgen(js_name = setSecurityVersion)]
    pub fn set_security_version(&mut self, version: Option<String>) -> Result<(), JsValue> {
        self.security_version = match version {
            Some(text) if !text.trim().is_empty() => Some(
                text.trim()
                    .parse::<u64>()
                    .map_err(|_| error("security_version must be an unsigned 64-bit integer"))?,
            ),
            _ => None,
        };
        Ok(())
    }

    /// Configure RSA-SHA256 PKCS#7 signing for a UEFI 2.10 FMP `.cap` output.
    #[wasm_bindgen(js_name = setCapsuleSigning)]
    pub fn set_capsule_signing(
        &mut self,
        rsa_key: Vec<u8>,
        cert: Vec<u8>,
        monotonic_count: String,
        lowest_supported_version: Option<u32>,
    ) -> Result<(), JsValue> {
        if self.bundle.manifest.plan.update.is_none() {
            return Err(error(
                "this build does not declare a firmware update contract for capsule signing",
            ));
        }
        let monotonic_count = monotonic_count
            .trim()
            .parse::<u64>()
            .map_err(|_| error("monotonic_count must be an unsigned 64-bit integer"))?;
        let key = RsaPrivateKey::from_pem_or_der(&rsa_key).map_err(error)?;
        let cert_der = decode_cert_pem_or_der(&cert).map_err(error)?;
        key.verify_matches_cert(&cert_der).map_err(error)?;
        self.capsule_signing = Some(StoredCapsuleSigning {
            rsa_key,
            cert: cert_der,
            monotonic_count,
            lowest_supported_version,
        });
        Ok(())
    }

    /// Forget the donor, inputs, settings and update options, so the next
    /// assembly takes only what is set after this.
    pub fn clear(&mut self) {
        self.inputs.clear();
        self.donor = None;
        self.intel_flash = None;
        self.settings.clear();
        self.secure_boot = None;
        self.update_only = false;
        self.fw_version = None;
        self.security_version = None;
        self.capsule_signing = None;
    }

    #[wasm_bindgen(js_name = setDonor)]
    pub fn set_donor(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.donor = Some(self.bundle.donor(bytes).map_err(error)?);
        Ok(())
    }

    #[wasm_bindgen(js_name = inspectIntelFlash)]
    pub fn inspect_intel_flash(&self, bytes: &[u8]) -> Result<String, JsValue> {
        let regions = self.bundle.inspect_intel_flash(bytes).map_err(error)?;
        serde_json::to_string(&regions).map_err(error)
    }

    #[wasm_bindgen(js_name = setIntelFlash)]
    pub fn set_intel_flash(&mut self, bytes: Vec<u8>) {
        self.intel_flash = Some(bytes);
    }

    #[wasm_bindgen(js_name = setInput)]
    pub fn set_input(&mut self, name: String, bytes: &[u8]) -> Result<(), JsValue> {
        let input = self
            .bundle
            .manifest
            .inputs
            .iter()
            .find(|input| input.name == name)
            .ok_or_else(|| error(format!("unknown input {name}")))?;
        if bytes.is_empty() || bytes.len() as u64 > input.capacity {
            return Err(error("input is empty or exceeds capacity"));
        }
        self.inputs.insert(name, bytes.to_vec());
        Ok(())
    }

    /// Read a compiled binary using its logical path from the manifest.
    pub fn binary(&self, path: &str) -> Result<Vec<u8>, JsValue> {
        use fstart_image_build::files::Files;
        self.bundle
            .files
            .read(std::path::Path::new(path))
            .map_err(error)
    }

    /// Returns Map<filename, Uint8Array>. With no key, browser Web Crypto
    /// supplies randomness for a fresh ephemeral Ed25519 key (Full ROM mode
    /// without capsule signing only).
    pub fn assemble(&mut self, signing_key: Option<Vec<u8>>) -> Result<js_sys::Map, JsValue> {
        let capsule_signing = self.capsule_signing.take();
        let supplied = Supplied {
            donor: self.donor.as_deref(),
            intel_flash: self.intel_flash.as_deref(),
            inputs: self.inputs.clone(),
            signing_key: signing_key.as_deref(),
            settings: Settings {
                options: self.settings.clone().into_iter().collect(),
                secure_boot: self.secure_boot.clone(),
            },
            mode: if self.update_only {
                AssemblyMode::UpdateOnly
            } else {
                AssemblyMode::Full
            },
            fw_version: self.fw_version,
            security_version: self.security_version,
            capsule_signing: capsule_signing.as_ref().map(|cap| CapsuleSigning {
                rsa_key: &cap.rsa_key,
                cert: &cap.cert,
                monotonic_count: cap.monotonic_count,
                lowest_supported_version: cap.lowest_supported_version,
            }),
        };
        let outputs = self.bundle.assemble(&supplied).map_err(error)?;
        let map = js_sys::Map::new();
        for (name, bytes) in outputs {
            map.set(
                &JsValue::from_str(&name),
                &js_sys::Uint8Array::from(bytes.as_slice()),
            );
        }
        Ok(map)
    }
}

fn error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}
