//! Headless browser API; the website is a separate consumer of these bindings.
use fstart_image_build::bundle::Bundle;
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct ImageAssembler {
    bundle: Bundle,
    inputs: BTreeMap<String, Vec<u8>>,
    donor: Option<Vec<u8>>,
}

#[wasm_bindgen]
impl ImageAssembler {
    #[wasm_bindgen(constructor)]
    pub fn new(compressed_bundle: &[u8]) -> Result<ImageAssembler, JsValue> {
        Ok(Self {
            bundle: Bundle::decode(compressed_bundle).map_err(error)?,
            inputs: BTreeMap::new(),
            donor: None,
        })
    }

    /// JSON manifest, including required inputs, stage binaries and hashes.
    pub fn manifest(&self) -> Result<String, JsValue> {
        serde_json::to_string(&self.bundle.manifest).map_err(error)
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
        let outputs = self
            .bundle
            .assemble(self.donor.as_deref(), &self.inputs, signing_key.as_deref())
            .map_err(error)?;
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
