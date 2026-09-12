//! Validate compiler feature references against the selected Cargo graph.

pub(crate) fn validate_features(
    features: &[String],
    board: &serde_json::Value,
    metadata: &serde_json::Value,
) -> Result<(), String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("missing Cargo packages")?;
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("missing resolve graph")?;
    let node = nodes
        .iter()
        .find(|n| n["id"] == board["id"])
        .ok_or("missing board node")?;
    for feature in features {
        let Some((dependency, name)) = feature.split_once('/') else {
            if board["features"].get(feature).is_none() {
                return Err(format!("unknown board feature '{feature}'"));
            }
            continue;
        };
        let key = dependency.replace('-', "_");
        let dep = node["deps"]
            .as_array()
            .and_then(|deps| deps.iter().find(|d| d["name"].as_str() == Some(&key)))
            .ok_or_else(|| {
                format!("feature '{feature}' does not name an enabled direct dependency")
            })?;
        let package = packages
            .iter()
            .find(|p| p["id"] == dep["pkg"])
            .ok_or("missing dependency package")?;
        if package["features"].get(name).is_none() {
            return Err(format!("unknown dependency feature '{feature}'"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feature_references_must_be_actual_direct_dependency_features() {
        let board = serde_json::json!({"id":"board", "features":{"stage":[]}});
        let metadata = serde_json::json!({
            "packages":[{"id":"platform", "features":{"riscv64":[]}}],
            "resolve":{"nodes":[{"id":"board", "deps":[{"name":"renamed_platform", "pkg":"platform"}]}]}
        });
        assert!(
            validate_features(
                &["stage".into(), "renamed-platform/riscv64".into()],
                &board,
                &metadata
            )
            .is_ok()
        );
        for feature in [
            "missing",
            "transitive-driver/riscv64",
            "renamed-platform/missing",
        ] {
            assert!(validate_features(&[feature.into()], &board, &metadata).is_err());
        }
    }
}
