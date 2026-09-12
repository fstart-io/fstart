//! Translate Cargo's actual nightly unit graph, not a hand-maintained feature
//! graph. Build-script outputs and proc-macro paths come from a real Cargo check.

use crate::selection::Selection;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

#[cfg(test)]
#[path = "ide_graph_tests.rs"]
mod tests;

fn rustc(root: &Path, args: &[&str]) -> Result<String, String> {
    let result = Command::new("rustc")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).into_owned());
    }
    String::from_utf8(result.stdout).map_err(|e| e.to_string())
}

pub(crate) fn generate(
    root: &Path,
    selection: &Selection,
    metadata: &Value,
) -> Result<Value, String> {
    let mut graph_command = selection.command(false);
    let graph_output = graph_command
        .current_dir(root)
        .args(["-Zunstable-options", "--unit-graph"])
        .output()
        .map_err(|e| e.to_string())?;
    if !graph_output.status.success() {
        return Err(String::from_utf8_lossy(&graph_output.stderr).into_owned());
    }
    let graph: Value = serde_json::from_slice(&graph_output.stdout).map_err(|e| e.to_string())?;
    if graph["version"] != 1 {
        return Err("unsupported Cargo unit-graph version".into());
    }
    eprintln!("[fstart] checking editor selection for build scripts and proc macros");
    let output = selection
        .command(false)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        // Ordinary source errors must not prevent opening an editor. The
        // projection below still requires all needed build-script and macro
        // data, rather than silently inventing defaults for failed producers.
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        eprintln!(
            "[fstart] compiler diagnostics remain; preparing the editor from available build data"
        );
    }
    let messages: Vec<Value> = output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    fs::write(
        selection.directory.join("ide-unit-graph.json"),
        &graph_output.stdout,
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        selection.directory.join("ide-cargo-messages.jsonl"),
        &output.stdout,
    )
    .map_err(|e| e.to_string())?;
    let sysroot = rustc(root, &["--print", "sysroot"])?;
    let version = rustc(root, &["-vV"])?;
    let host = version
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .ok_or("rustc did not report host")?;
    project(
        root,
        selection,
        metadata,
        &graph,
        &messages,
        sysroot.trim(),
        host,
    )
}

fn project(
    root: &Path,
    selection: &Selection,
    metadata: &Value,
    graph: &Value,
    messages: &[Value],
    sysroot: &str,
    host: &str,
) -> Result<Value, String> {
    let units = graph["units"].as_array().ok_or("missing Cargo units")?;
    // rust-analyzer supplies sysroot source crates itself. Cargo still remains
    // responsible for checking the actual -Zbuild-std units above.
    let indices: BTreeMap<usize, usize> = units
        .iter()
        .enumerate()
        .filter(|(_, u)| u["is_std"] != true && u["mode"] != "run-custom-build")
        .enumerate()
        .map(|(new, (old, _))| (old, new))
        .collect();
    let packages = metadata["packages"]
        .as_array()
        .ok_or("missing Cargo packages")?;
    let mut crates = Vec::new();
    for old in indices.keys() {
        let unit = &units[*old];
        let package = packages
            .iter()
            .find(|p| p["id"] == unit["pkg_id"])
            .ok_or("unit package missing from metadata")?;
        let src = Path::new(
            unit["target"]["src_path"]
                .as_str()
                .ok_or("unit missing source")?,
        )
        .canonicalize()
        .map_err(|e| e.to_string())?;
        let manifest = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("missing manifest")?,
        );
        let directory = manifest.parent().ok_or("manifest missing parent")?;
        let canonical = directory.canonicalize().map_err(|e| e.to_string())?;
        let platform = unit["platform"].as_str();
        let mut cfg = Vec::new();
        if platform.is_some() {
            cfg.extend(selection.cfgs.iter().cloned());
        }
        for feature in unit["features"].as_array().ok_or("missing unit features")? {
            cfg.push(format!("feature={}", feature));
        }
        if unit["profile"]["debug_assertions"] == true {
            cfg.push("debug_assertions".into());
        }
        let mut env = BTreeMap::from([
            (
                "CARGO_MANIFEST_DIR".to_owned(),
                directory.display().to_string(),
            ),
            (
                "CARGO_MANIFEST_PATH".to_owned(),
                manifest.display().to_string(),
            ),
            (
                "CARGO_CRATE_NAME".to_owned(),
                unit["target"]["name"]
                    .as_str()
                    .ok_or("missing crate name")?
                    .replace('-', "_"),
            ),
        ]);
        for (variable, field) in [
            ("CARGO_PKG_NAME", "name"),
            ("CARGO_PKG_VERSION", "version"),
            ("CARGO_PKG_DESCRIPTION", "description"),
            ("CARGO_PKG_HOMEPAGE", "homepage"),
            ("CARGO_PKG_LICENSE", "license"),
            ("CARGO_PKG_REPOSITORY", "repository"),
        ] {
            env.insert(
                variable.into(),
                package[field].as_str().unwrap_or("").into(),
            );
        }
        let version = package["version"]
            .as_str()
            .ok_or("missing package version")?;
        let without_build = version.split('+').next().unwrap_or(version);
        let (numbers, pre) = without_build.split_once('-').unwrap_or((without_build, ""));
        for (key, value) in [
            "CARGO_PKG_VERSION_MAJOR",
            "CARGO_PKG_VERSION_MINOR",
            "CARGO_PKG_VERSION_PATCH",
        ]
        .into_iter()
        .zip(numbers.split('.'))
        {
            env.insert(key.into(), value.into());
        }
        env.insert("CARGO_PKG_VERSION_PRE".into(), pre.into());
        env.insert(
            "CARGO_PKG_AUTHORS".into(),
            package["authors"]
                .as_array()
                .ok_or("missing authors")?
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(":"),
        );
        if unit["target"]["kind"]
            .as_array()
            .is_some_and(|k| k.iter().any(|v| v == "bin"))
        {
            env.insert(
                "CARGO_BIN_NAME".into(),
                unit["target"]["name"]
                    .as_str()
                    .ok_or("missing bin name")?
                    .into(),
            );
        }
        let mut include = BTreeSet::from([canonical.clone()]);
        let outputs: Vec<_> = messages
            .iter()
            .filter(|m| m["reason"] == "build-script-executed" && m["package_id"] == unit["pkg_id"])
            .collect();
        for output in &outputs {
            let out = Path::new(output["out_dir"].as_str().ok_or("missing OUT_DIR")?);
            include.insert(out.canonicalize().map_err(|e| e.to_string())?);
        }
        let consumes_output = unit["dependencies"]
            .as_array()
            .ok_or("missing unit deps")?
            .iter()
            .any(|dep| {
                dep["index"]
                    .as_u64()
                    .and_then(|i| units.get(i as usize))
                    .is_some_and(|u| {
                        u["mode"] == "run-custom-build" && u["pkg_id"] == unit["pkg_id"]
                    })
            });
        let applicable: Vec<_> = outputs
            .iter()
            .filter(|o| {
                if !consumes_output {
                    return false;
                }
                let out = Path::new(o["out_dir"].as_str().unwrap_or(""));
                let target_output = platform
                    .is_some_and(|p| out.starts_with(selection.directory.join("cargo").join(p)));
                if platform.is_some() {
                    target_output
                } else {
                    out.starts_with(selection.directory.join("cargo").join("release"))
                        || out.starts_with(selection.directory.join("cargo").join("debug"))
                }
            })
            .collect();
        if consumes_output && applicable.is_empty() {
            return Err(format!(
                "missing build-script output for {}",
                unit["pkg_id"]
            ));
        }
        if applicable.len() > 1 {
            return Err(format!(
                "ambiguous build-script outputs for {}",
                unit["pkg_id"]
            ));
        }
        if let Some(output) = applicable.first() {
            cfg.extend(
                output["cfgs"]
                    .as_array()
                    .ok_or("missing build-script cfgs")?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned),
            );
            env.insert(
                "OUT_DIR".into(),
                output["out_dir"].as_str().ok_or("missing OUT_DIR")?.into(),
            );
            for pair in output["env"].as_array().ok_or("missing build-script env")? {
                env.insert(
                    pair[0].as_str().ok_or("invalid env key")?.into(),
                    pair[1].as_str().ok_or("invalid env value")?.into(),
                );
            }
        }
        let mut deps = Vec::new();
        for dep in unit["dependencies"].as_array().ok_or("missing unit deps")? {
            let old = usize::try_from(dep["index"].as_u64().ok_or("invalid dependency index")?)
                .map_err(|e| e.to_string())?;
            units.get(old).ok_or("dependency index out of range")?;
            if let Some(index) = indices.get(&old) {
                deps.push(json!({"crate":index, "name":dep["extern_crate_name"]}));
            }
        }
        let proc_macro = unit["target"]["kind"]
            .as_array()
            .is_some_and(|k| k.iter().any(|v| v == "proc-macro"));
        let mut krate = json!({
            "display_name": unit["target"]["name"], "root_module": src, "edition": unit["target"]["edition"],
            "version": package["version"], "is_workspace_member": canonical.starts_with(root),
            "deps":deps, "cfg":cfg, "target":platform.unwrap_or(host), "env":env,
            "source":{"include_dirs":include, "exclude_dirs":[]}, "is_proc_macro":proc_macro
        });
        if proc_macro {
            let artifact = messages
                .iter()
                .find(|m| {
                    m["reason"] == "compiler-artifact"
                        && m["package_id"] == unit["pkg_id"]
                        && m["target"] == unit["target"]
                        && m["features"] == unit["features"]
                })
                .ok_or("missing proc-macro artifact")?;
            let dylib = artifact["filenames"]
                .as_array()
                .ok_or("missing proc-macro paths")?
                .iter()
                .filter_map(Value::as_str)
                .find(|p| p.ends_with(".so") || p.ends_with(".dylib") || p.ends_with(".dll"))
                .ok_or("missing proc-macro dylib")?;
            krate["proc_macro_dylib_path"] =
                json!(Path::new(dylib).canonicalize().map_err(|e| e.to_string())?);
        }
        crates.push(krate);
    }
    Ok(
        json!({"sysroot":sysroot, "sysroot_src":Path::new(sysroot).join("lib/rustlib/src/rust/library"), "crates":crates}),
    )
}
