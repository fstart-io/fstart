use std::fs;
use std::path::Path;

use fstart_register_gen::{Catalog, load_ir, render, run};

const SOURCE: &str = "
block/Regs:
  items:
    - name: ctrl
      byte_offset: 4
      bit_size: 16
      fieldset: CTRL
    - name: data
      byte_offset: 8
      bit_size: 8
      array: {len: 4, stride: 1}
    - name: status
      byte_offset: 12
      access: Read
    - name: trigger
      byte_offset: 16
      access: Write
fieldset/CTRL:
  bit_size: 16
  fields:
    - name: ENABLE
      bit_offset: 7
      bit_size: 1
    - name: MODE
      bit_offset: 0
      bit_size: 2
      enum: Mode
enum/Mode:
  bit_size: 2
  variants:
    - {name: Quiet, value: 0}
    - {name: Continuous, value: 1}
";

fn catalog() -> Catalog {
    serde_yaml::from_str(
        "source: source.yaml
output: generated.rs
blocks:
  Regs:
    size: 20
    offsets: offsets
    offset_type: u16
",
    )
    .unwrap()
}

fn rejection(source: &str, expected: &str) {
    let error = load_ir(source)
        .and_then(|ir| render(&ir, &catalog()))
        .unwrap_err();
    assert!(error.to_string().contains(expected), "{error:#}");
}

#[test]
fn emits_tock_fields_barrier_aware_overlays_and_offsets() {
    let generated = render(&load_ir(SOURCE).unwrap(), &catalog()).unwrap();
    assert!(generated.contains("ENABLE OFFSET(7) NUMBITS(1) []"));
    assert!(generated.contains("Continuous = 1"));
    assert!(generated.contains("(0x0 => _reserved_0)"));
    assert!(generated.contains("ctrl: fstart_core::mmio::MmioReadWrite<u16, CTRL::Register>"));
    assert!(generated.contains("data: [fstart_core::mmio::MmioReadWrite<u8>; 4]"));
    assert!(generated.contains("status: fstart_core::mmio::MmioReadOnly<u32>"));
    assert!(generated.contains("trigger: fstart_core::mmio::MmioWriteOnly<u32>"));
    assert!(generated.contains("pub const CTRL: u16 = 0x4"));
    assert!(generated.contains("(0x14 => @END)"));
    assert!(!generated.contains("read_volatile"));
    assert_eq!(
        generated,
        render(&load_ir(SOURCE).unwrap(), &catalog()).unwrap()
    );
}

#[test]
fn pci_overlay_reuses_the_shared_header() {
    let source = SOURCE
        .replace("byte_offset: 4", "byte_offset: 64")
        .replace("byte_offset: 8", "byte_offset: 68")
        .replace("byte_offset: 12", "byte_offset: 72")
        .replace("byte_offset: 16", "byte_offset: 76");
    let mut catalog = catalog();
    let binding = catalog.blocks.get_mut("Regs").unwrap();
    binding.pci_type0 = true;
    binding.size = Some(0x1000);
    let generated = render(&load_ir(&source).unwrap(), &catalog).unwrap();
    assert!(generated.contains("fstart_pci::pci_type0_config!"));
    assert!(generated.contains("pub struct Regs"));
    assert!(!generated.contains("vendor_id"));
    assert!(generated.contains("(0x50 => _reserved_end)"));
}

#[test]
fn inherited_fields_use_chiptool_expansion() {
    let source = format!(
        "{SOURCE}\nfieldset/Derived:\n  extends: CTRL\n  bit_size: 16\n  fields:\n    - {{name: EXTRA, bit_offset: 12, bit_size: 1}}\n"
    );
    let ir = load_ir(&source).unwrap();
    assert_eq!(ir.fieldsets["Derived"].fields.len(), 3);
    assert!(render(&ir, &catalog()).unwrap().contains("pub Derived"));
}

#[test]
fn invalid_references_and_inheritance_are_errors_not_panics_or_hangs() {
    rejection(
        &SOURCE.replace("fieldset: CTRL", "fieldset: Missing"),
        "does not exist",
    );
    rejection(
        &SOURCE.replace(
            "bit_size: 16\n  fields:",
            "bit_size: 16\n  extends: Missing\n  fields:",
        ),
        "does not exist",
    );
    rejection(
        &SOURCE.replace(
            "bit_size: 16\n  fields:",
            "bit_size: 16\n  extends: CTRL\n  fields:",
        ),
        "inheritance cycle",
    );
}

#[test]
fn rejects_invalid_widths_fields_enums_and_identifiers() {
    rejection(
        &SOURCE.replace("bit_offset: 7", "bit_offset: 16"),
        "field exceeds",
    );
    rejection(&SOURCE.replace("name: ENABLE", "name: 1BAD"), "identifier");
    rejection(
        &SOURCE.replace("bit_size: 16", "bit_size: 24"),
        "unsupported register width",
    );
    rejection(
        &SOURCE.replace("name: Continuous, value: 1", "name: Continuous, value: 4"),
        "not less",
    );
    rejection(
        &SOURCE.replace(
            "bit_offset: 7",
            "array: {len: 2, stride: 1}\n      bit_offset: 7",
        ),
        "field arrays",
    );
}

#[test]
fn rejects_layout_errors_and_sparse_overlays() {
    rejection(
        &SOURCE.replace("byte_offset: 8", "byte_offset: 5"),
        "overlapping",
    );
    rejection(
        &SOURCE.replace("byte_offset: 4", "byte_offset: 3"),
        "unaligned",
    );
    rejection(
        &SOURCE.replace("byte_offset: 16", "byte_offset: 20"),
        "exceeds overlay",
    );
    rejection(&SOURCE.replace("len: 4", "len: 0"), "empty register array");
    rejection(&SOURCE.replace("stride: 1", "stride: 2"), "sparse arrays");
    rejection(
        &SOURCE.replace("name: trigger", "name: status"),
        "duplicate offset",
    );
}

#[test]
fn sparse_arrays_are_valid_in_offset_only_catalogs() {
    let mut catalog = catalog();
    catalog.blocks.get_mut("Regs").unwrap().size = None;
    assert!(
        render(
            &load_ir(&SOURCE.replace("stride: 1", "stride: 128")).unwrap(),
            &catalog
        )
        .is_ok()
    );
}

#[test]
fn offset_types_and_fieldset_widths_are_checked() {
    let mut catalog = catalog();
    catalog.blocks.get_mut("Regs").unwrap().offset_type = "u8".into();
    let source = SOURCE.replace("byte_offset: 4", "byte_offset: 256");
    assert!(
        render(&load_ir(&source).unwrap(), &catalog)
            .unwrap_err()
            .to_string()
            .contains("does not fit")
    );
    rejection(
        &SOURCE.replacen("bit_size: 16", "bit_size: 8", 1),
        "fieldset width mismatch",
    );
}

fn fixture(root: &Path) {
    fs::create_dir_all(root.join("data/registers")).unwrap();
    fs::write(root.join("source.yaml"), SOURCE).unwrap();
    fs::write(root.join("data/registers/catalogs.yaml"), "catalogs:\n  - source: source.yaml\n    output: generated.rs\n    blocks:\n      Regs: {size: 20}\n").unwrap();
}

#[test]
fn check_detects_missing_and_stale_outputs_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fixture(root);
    assert!(run(root, true).is_err());
    assert!(!root.join("generated.rs").exists());
    run(root, false).unwrap();
    run(root, true).unwrap();
    fs::write(root.join("generated.rs"), "stale").unwrap();
    assert!(run(root, true).is_err());
    assert_eq!(
        fs::read_to_string(root.join("generated.rs")).unwrap(),
        "stale"
    );
    run(root, false).unwrap();
    run(root, true).unwrap();
}

#[test]
fn manifest_rejects_unknown_keys_and_paths_outside_the_repo() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fixture(root);
    let manifest = root.join("data/registers/catalogs.yaml");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, text.replace("size: 20", "szie: 20")).unwrap();
    assert!(
        run(root, false)
            .unwrap_err()
            .to_string()
            .contains("unknown field")
    );
    fs::write(
        &manifest,
        text.replace("output: generated.rs", "output: ../outside.rs"),
    )
    .unwrap();
    assert!(
        run(root, false)
            .unwrap_err()
            .to_string()
            .contains("relative")
    );
}

#[test]
fn committed_registers_are_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    run(root, true).unwrap();
}
