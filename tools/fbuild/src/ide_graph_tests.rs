use super::*;
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn compiler_selection_and_cargo_units_preserve_sources_host_cfgs_and_generated_data() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("fstart ide projection {}", std::process::id())));
    fs::create_dir_all(&scratch.0).unwrap();
    let root = scratch.0.canonicalize().unwrap();
    let actual_root = crate::build_board::workspace_root_pub().unwrap();
    let board = crate::board_manifest::find(&actual_root, "qemu-riscv64").unwrap();
    let plan = fstart_platform_qemu::host::resolve(
        fstart_platform_qemu::facts::VirtMachine::Riscv64,
        fstart_image_build::plan::BuildSelection {
            payload: Some("halt".into()),
        },
    )
    .unwrap();
    let selection = Selection::prepare_unit(
        &root,
        &board,
        &plan.units[0],
        root.join("selection"),
        &serde_json::to_string(&plan).unwrap(),
        Default::default(),
        &[],
        true,
    )
    .unwrap();
    let build = selection.command(true);
    let check = selection.check_command();
    let args: Vec<_> = build.get_args().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(&args[2..], &check[3..]);
    let encoded = &selection.environment()["CARGO_ENCODED_RUSTFLAGS"];
    assert_eq!(encoded.split('\u{1f}').collect::<Vec<_>>(), selection.flags);
    assert!(
        selection
            .flags
            .last()
            .unwrap()
            .contains("fstart ide projection ")
    );

    for name in ["app", "macros"] {
        fs::create_dir_all(root.join(name).join("src")).unwrap();
        fs::write(root.join(name).join("src/lib.rs"), "").unwrap();
        fs::write(root.join(name).join("Cargo.toml"), "").unwrap();
    }
    std::os::unix::fs::symlink(root.join("app"), root.join("app-alias")).unwrap();
    let packages: Vec<_> = [("app", "app-alias"), ("macros", "macros")]
        .into_iter()
        .map(|(id, dir)| {
            json!({
                "id":id, "name":id, "version":"0.1.0-rc.1+build", "authors":[],
                "manifest_path":root.join(dir).join("Cargo.toml")
            })
        })
        .collect();
    let macro_target = json!({"name":"macros", "kind":["proc-macro"], "src_path":root.join("macros/src/lib.rs"), "edition":"2024"});
    let mut graph = json!({"units":[
        {"pkg_id":"app", "mode":"check", "platform":"riscv64gc-unknown-none-elf", "features":["chosen"],
         "profile":{"debug_assertions":false},
         "target":{"name":"app", "kind":["lib"], "src_path":root.join("app-alias/src/lib.rs"), "edition":"2024"},
         "dependencies":[{"index":1,"extern_crate_name":"renamed_macro"},{"index":2,"extern_crate_name":"build_script_build"}]},
        {"pkg_id":"macros", "mode":"build", "platform":null, "features":[],
         "profile":{"debug_assertions":false}, "target":macro_target, "dependencies":[]},
        {"pkg_id":"app", "mode":"run-custom-build"}
    ]});
    let out = selection
        .directory
        .join("cargo/riscv64gc-unknown-none-elf/release/build/app/out");
    fs::create_dir_all(&out).unwrap();
    let dylib = root.join("macro.so");
    fs::write(&dylib, "not executed by this projection test").unwrap();
    let messages = vec![
        json!({"reason":"build-script-executed", "package_id":"app", "out_dir":out,
            "cfgs":["from_build_script"], "env":[["GENERATED_VALUE","42"]]}),
        json!({"reason":"compiler-artifact", "package_id":"macros", "target":macro_target,
            "features":[], "filenames":[dylib]}),
    ];
    let metadata = json!({"packages":packages});
    let output = project(
        &root,
        &selection,
        &metadata,
        &graph,
        &messages,
        "/toolchain",
        "x86_64-unknown-linux-gnu",
    )
    .unwrap();
    let app = &output["crates"][0];
    let host = &output["crates"][1];
    assert_eq!(output["crates"].as_array().unwrap().len(), 2);
    assert_eq!(app["root_module"], json!(root.join("app/src/lib.rs")));
    assert_eq!(app["deps"], json!([{"crate":1,"name":"renamed_macro"}]));
    assert!(
        app["cfg"]
            .as_array()
            .unwrap()
            .contains(&json!("fstart_payload=\"halt\""))
    );
    assert!(
        app["cfg"]
            .as_array()
            .unwrap()
            .contains(&json!("feature=\"chosen\""))
    );
    assert!(
        app["cfg"]
            .as_array()
            .unwrap()
            .contains(&json!("from_build_script"))
    );
    assert_eq!(host["cfg"], json!([]));
    assert_eq!(host["target"], "x86_64-unknown-linux-gnu");
    assert_eq!(host["proc_macro_dylib_path"], json!(dylib));
    assert_eq!(app["env"]["GENERATED_VALUE"], "42");
    assert_eq!(app["env"]["CARGO_PKG_VERSION_PRE"], "rc.1");
    assert!(
        app["source"]["include_dirs"]
            .as_array()
            .unwrap()
            .contains(&json!(out))
    );
    let mut ambiguous = messages.clone();
    ambiguous.push(messages[0].clone());
    assert!(
        project(
            &root,
            &selection,
            &metadata,
            &graph,
            &ambiguous,
            "/toolchain",
            "host"
        )
        .unwrap_err()
        .contains("ambiguous")
    );
    graph["units"][0]["dependencies"][0]["index"] = json!(999);
    assert!(
        project(
            &root,
            &selection,
            &metadata,
            &graph,
            &messages,
            "/toolchain",
            "host"
        )
        .unwrap_err()
        .contains("out of range")
    );
}
