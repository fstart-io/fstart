//! A library declares the selection axes it actually consumes. Its vocabulary
//! is intrinsic to that crate, not copied from each board's platform schema.
//! Packages without a declaration receive no board-dependent profile flags.
use fstart_image_build::build_plan::CompilationUnit;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) type PackageSchemas = BTreeMap<String, BTreeMap<String, Vec<String>>>;

pub(crate) fn packages(metadata: &serde_json::Value) -> Result<PackageSchemas, String> {
    let mut schemas = PackageSchemas::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or("missing Cargo packages")?
    {
        let Some(schema) = package["metadata"]["fstart"].get("compiler-cfg") else {
            continue;
        };
        let name = package["name"].as_str().ok_or("missing package name")?;
        let schema = serde_json::from_value(schema.clone())
            .map_err(|e| format!("{name} compiler-cfg: {e}"))?;
        if schemas.insert(name.into(), schema).is_some() {
            return Err(format!("ambiguous compiler-cfg package {name}"));
        }
    }
    Ok(schemas)
}

pub(crate) fn flags(
    unit: &CompilationUnit,
    schemas: &PackageSchemas,
) -> Result<BTreeMap<String, Vec<String>>, String> {
    schemas
        .iter()
        .map(|(package, schema)| {
            let mut flags = Vec::new();
            for (key, allowed) in schema {
                let value = match key.as_str() {
                    "fstart_entry" => &unit.entry,
                    "fstart_stage_env" => &unit.environment,
                    "fstart_payload" => &unit.payload,
                    _ => return Err(format!("{package} declares unknown compiler cfg {key}")),
                };
                // This is the vocabulary checked in this library's source,
                // not a platform selector schema. A library may simply take
                // its default branch for another platform's selected value.
                // The platform's complete selection is validated separately.
                if allowed.is_empty()
                    || allowed.iter().collect::<BTreeSet<_>>().len() != allowed.len()
                {
                    return Err(format!("{package}: invalid {key} source vocabulary"));
                }
                flags.extend([
                    "--cfg".into(),
                    format!("{key}={}", serde_json::to_string(value).unwrap()),
                ]);
                flags.push(format!(
                    "--check-cfg=cfg({key},values({}))",
                    allowed
                        .iter()
                        .map(|v| serde_json::to_string(v).unwrap())
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
            Ok((package.clone(), flags))
        })
        .collect()
}
