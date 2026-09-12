//! Opt-in all-board build-lock prototype, with selected compilation-graph proof.

use crate::selection::Selection;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path, process::Command};

fn normalized(graph: &Value) -> Result<Value, String> {
    if graph["version"] != 1 {
        return Err("unsupported Cargo unit-graph version".into());
    }
    let units = graph["units"].as_array().ok_or("missing units")?;
    let bases = units
        .iter()
        .map(|unit| {
            let mut base = unit.clone();
            let object = base.as_object_mut().ok_or("invalid unit")?;
            object.remove("pkg_id");
            object.remove("dependencies");
            let path = Path::new(
                base["target"]["src_path"]
                    .as_str()
                    .ok_or("missing source")?,
            )
            .canonicalize()
            .map_err(|e| e.to_string())?;
            base["target"]["src_path"] = json!(path);
            serde_json::to_string(&base).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    if bases.iter().collect::<BTreeSet<_>>().len() != bases.len() {
        return Err("duplicate compiler unit after canonical source normalization".into());
    }
    let mut result = Vec::new();
    for (i, unit) in units.iter().enumerate() {
        let mut deps = Vec::new();
        for dependency in unit["dependencies"]
            .as_array()
            .ok_or("missing dependencies")?
        {
            let mut dep = dependency.clone();
            let index = dep["index"].as_u64().ok_or("invalid unit dependency")? as usize;
            dep["index"] = json!(bases.get(index).ok_or("unit dependency out of range")?);
            deps.push(serde_json::to_string(&dep).map_err(|e| e.to_string())?);
        }
        deps.sort();
        result.push(
            serde_json::to_string(&json!({"unit":bases[i], "deps":deps}))
                .map_err(|e| e.to_string())?,
        );
    }
    result.sort();
    let mut roots = graph["roots"]
        .as_array()
        .ok_or("missing roots")?
        .iter()
        .map(|v| {
            bases
                .get(v.as_u64().ok_or("invalid root")? as usize)
                .cloned()
                .ok_or("root out of range")
        })
        .collect::<Result<Vec<_>, _>>()?;
    roots.sort();
    Ok(json!({"roots":roots, "units":result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_agreement_ignores_workspace_aliases_and_order_but_checks_edges_and_features() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ide_lock.rs");
        let leaf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs");
        let original = json!({"version":1,"roots":[0],"units":[
            {"pkg_id":"selected-root", "target":{"src_path":source}, "features":["a"],
             "dependencies":[{"index":1,"extern_crate_name":"leaf","noprelude":false}]},
            {"pkg_id":"selected-leaf", "target":{"src_path":leaf}, "features":[], "dependencies":[]}
        ]});
        let mut reordered = original.clone();
        reordered["units"].as_array_mut().unwrap().swap(0, 1);
        reordered["roots"] = json!([1]);
        reordered["units"][1]["pkg_id"] = json!("inventory-root");
        reordered["units"][1]["target"]["src_path"] =
            json!(source.parent().unwrap().join("../src/ide_lock.rs"));
        reordered["units"][1]["dependencies"][0]["index"] = json!(0);
        assert_eq!(
            normalized(&original).unwrap(),
            normalized(&reordered).unwrap()
        );
        let mut changed = reordered.clone();
        changed["units"][1]["features"] = json!(["b"]);
        assert_ne!(
            normalized(&original).unwrap(),
            normalized(&changed).unwrap()
        );
        changed = reordered;
        changed["units"][1]["dependencies"][0]["extern_crate_name"] = json!("other");
        assert_ne!(
            normalized(&original).unwrap(),
            normalized(&changed).unwrap()
        );
    }
}

pub(crate) fn audit(root: &Path, selection: &Selection) -> Result<Value, String> {
    let boards = crate::board_manifest::discover(root)?;
    let directory = crate::build_board::prepare_inventory_workspace(root, &boards)?;
    let manifest = directory.join("Cargo.toml");
    // Seeded from the existing root lock; Cargo may add the excluded boards.
    // This never promotes the prototype lock or modifies any source lock.
    let metadata = Command::new("cargo")
        .current_dir(root)
        .args(["metadata", "--format-version=1", "--manifest-path"])
        .arg(&manifest)
        .output()
        .map_err(|e| e.to_string())?;
    if !metadata.status.success() {
        return Err(String::from_utf8_lossy(&metadata.stderr).into_owned());
    }
    let metadata: Value = serde_json::from_slice(&metadata.stdout).map_err(|e| e.to_string())?;
    let mut command = selection.command(false);
    // Replace only the manifest; all target/profile/feature/cfg choices stay
    // identical to the build/check invocation used to produce the editor view.
    let args: Vec<_> = command.get_args().map(|s| s.to_owned()).collect();
    let mut args = args;
    let at = args
        .iter()
        .position(|s| s == "--manifest-path")
        .ok_or("missing selected manifest argument")?;
    args[at + 1] = manifest.into_os_string();
    command = Command::new("cargo");
    selection.apply_environment(&mut command);
    let result = command
        .current_dir(root)
        .args(args)
        .args(["-Zunstable-options", "--unit-graph"])
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned());
    }
    let graph: Value = serde_json::from_slice(&result.stdout).map_err(|e| e.to_string())?;
    let selected: Value = serde_json::from_slice(
        &fs::read(selection.directory.join("ide-unit-graph.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let same_graph = normalized(&graph)? == normalized(&selected)?;
    let old_pins = crate::ide::external_pins(
        &fs::read_to_string(selection.manifest.with_file_name("Cargo.lock"))
            .map_err(|e| e.to_string())?,
    )?;
    let new_pins = crate::ide::external_pins(
        &fs::read_to_string(directory.join("Cargo.lock")).map_err(|e| e.to_string())?,
    )?;
    let same_pins = old_pins.is_subset(&new_pins);
    let report = json!({
        "inventory_boards":boards.len(), "workspace_members":metadata["workspace_members"].as_array().map(Vec::len),
        "lock":directory.join("Cargo.lock"), "selected_unit_graph_agrees":same_graph,
        "selected_external_pins_agree":same_pins, "promoted":false
    });
    fs::write(
        directory.join("report.json"),
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    if !same_graph || !same_pins {
        return Err(format!(
            "all-board lock prototype disagrees with selected build; see {}",
            directory.join("report.json").display()
        ));
    }
    Ok(report)
}
