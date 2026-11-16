//! WASM bindings over `Bundle` for the web UI (`www/`) and other browser callers.
use fstart_image_build::bundle::{Bundle, Supplied};
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct ImageAssembler {
    bundle: Bundle,
    inputs: BTreeMap<String, Vec<u8>>,
    donor: Option<Vec<u8>>,
    settings: BTreeMap<String, String>,
}

#[wasm_bindgen]
impl ImageAssembler {
    #[wasm_bindgen(constructor)]
    pub fn new(compressed_bundle: &[u8]) -> Result<ImageAssembler, JsValue> {
        Ok(Self {
            bundle: Bundle::decode(compressed_bundle).map_err(error)?,
            inputs: BTreeMap::new(),
            donor: None,
            settings: BTreeMap::new(),
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
        options
            .seed(&[(key.clone(), value.clone())])
            .map_err(error)?;
        self.settings.insert(key, value);
        Ok(())
    }

    /// Forget the donor, inputs and settings, so the next assembly takes
    /// only what is set after this.
    pub fn clear(&mut self) {
        self.inputs.clear();
        self.donor = None;
        self.settings.clear();
    }

    #[wasm_bindgen(js_name = setDonor)]
    pub fn set_donor(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.donor = Some(self.bundle.donor(bytes).map_err(error)?);
        Ok(())
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
    /// supplies randomness for a fresh ephemeral Ed25519 key.
    pub fn assemble(&self, signing_key: Option<Vec<u8>>) -> Result<js_sys::Map, JsValue> {
        let supplied = Supplied {
            donor: self.donor.as_deref(),
            inputs: self.inputs.clone(),
            signing_key: signing_key.as_deref(),
            settings: self.settings.clone().into_iter().collect(),
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
