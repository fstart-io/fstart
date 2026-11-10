//! Tock backend for chiptool's register IR. Access transports stay in firmware.

use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use chiptool::ir::{Access, Array, BitOffset, Block, BlockItemInner, IR};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub catalogs: Vec<Catalog>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub source: PathBuf,
    pub output: PathBuf,
    #[serde(default)]
    pub blocks: BTreeMap<String, Binding>,
}

/// Code-generation choices, separate from chiptool's hardware descriptions.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Emit an overlay with this byte extent. No overlay when absent.
    pub size: Option<u32>,
    #[serde(default)]
    pub pci_type0: bool,
    #[serde(default)]
    pub crate_private: bool,
    /// Emit offsets in a module, retaining the legacy integer offset API.
    pub offsets: Option<String>,
    #[serde(default = "default_offset_type")]
    pub offset_type: String,
}

fn default_offset_type() -> String {
    "u32".into()
}

fn identifier(name: &str) -> Result<()> {
    syn::parse_str::<syn::Ident>(name)
        .with_context(|| format!("invalid Rust identifier {name}"))?;
    ensure!(
        !name.starts_with("r#"),
        "raw identifiers are not supported: {name}"
    );
    Ok(())
}

fn width(bits: u32) -> Result<()> {
    ensure!(
        matches!(bits, 8 | 16 | 32 | 64),
        "unsupported register width {bits}"
    );
    Ok(())
}

fn docs(out: &mut String, description: &Option<String>, indent: usize) {
    if let Some(description) = description {
        for line in description.lines() {
            writeln!(out, "{}/// {line}", " ".repeat(indent)).unwrap();
        }
    }
}

// The pinned chiptool expansion assumes an acyclic inheritance graph.
fn check_inheritance(parents: BTreeMap<&str, Option<&str>>) -> Result<()> {
    for name in parents.keys() {
        let mut seen = HashSet::new();
        let mut at = *name;
        while let Some(parent) = parents[at] {
            ensure!(seen.insert(at), "inheritance cycle at {at}");
            at = parent;
        }
    }
    Ok(())
}

/// Parse and expand standard chiptool YAML rather than maintaining another schema.
pub fn load_ir(text: &str) -> Result<IR> {
    let mut ir: IR = serde_yaml::from_str(text)?;
    // Validate references before ExpandExtends, whose missing-parent lookup
    // assumes that the input has already been checked.
    for enumm in ir.enums.values() {
        ensure!((1..64).contains(&enumm.bit_size), "unsupported enum width");
    }
    let errors = chiptool::validate::validate(
        &ir,
        chiptool::validate::Options {
            // Legacy catalogs contain alternate/RAW views and deliberately
            // overlapping timing fields. Preserve them, do not "fix" hardware.
            allow_register_overlap: true,
            allow_field_overlap: true,
            allow_enum_dup_value: false,
            allow_unused_enums: false,
            // Some fields are used with raw transport reads, not an overlay.
            allow_unused_fieldsets: true,
        },
    );
    ensure!(errors.is_empty(), "{}", errors.join("\n"));
    check_inheritance(
        ir.blocks
            .iter()
            .map(|(name, block)| (name.as_str(), block.extends.as_deref()))
            .collect(),
    )?;
    check_inheritance(
        ir.fieldsets
            .iter()
            .map(|(name, fs)| (name.as_str(), fs.extends.as_deref()))
            .collect(),
    )?;
    chiptool::transform::expand_extends::ExpandExtends {}.run(&mut ir)?;
    Ok(ir)
}

fn emit_fields(out: &mut String, ir: &IR) -> Result<()> {
    for (name, fs) in ir.fieldsets.iter().collect::<BTreeMap<_, _>>() {
        identifier(name)?;
        width(fs.bit_size)?;
        writeln!(out, "tock_registers::register_bitfields![u{},", fs.bit_size)?;
        docs(out, &fs.description, 4);
        writeln!(out, "    pub {name} [")?;
        let mut names = HashSet::new();
        for field in &fs.fields {
            identifier(&field.name)?;
            ensure!(
                names.insert(&field.name),
                "{name}: duplicate field {}",
                field.name
            );
            ensure!(
                field.array.is_none(),
                "{name}: field arrays are not supported"
            );
            let BitOffset::Regular(offset) = field.bit_offset else {
                bail!("{name}: split bitfields are not supported");
            };
            ensure!(
                field.bit_size > 0
                    && offset
                        .checked_add(field.bit_size)
                        .is_some_and(|end| end <= fs.bit_size),
                "{name}.{}: field exceeds register width",
                field.name
            );
            docs(out, &field.description, 8);
            write!(
                out,
                "        {} OFFSET({offset}) NUMBITS({}) [",
                field.name, field.bit_size
            )?;
            if let Some(enumm) = &field.enumm {
                writeln!(out)?;
                let enumm = &ir.enums[enumm];
                let mut variants = HashSet::new();
                for variant in &enumm.variants {
                    identifier(&variant.name)?;
                    ensure!(
                        variants.insert(&variant.name),
                        "{name}: duplicate enum variant {}",
                        variant.name
                    );
                    docs(out, &variant.description, 12);
                    writeln!(out, "            {} = {},", variant.name, variant.value)?;
                }
                write!(out, "        ")?;
            }
            writeln!(out, "],")?;
        }
        writeln!(out, "    ]\n];")?;
    }
    Ok(())
}

fn emit_block(
    out: &mut String,
    name: &str,
    block: &Block,
    binding: &Binding,
    ir: &IR,
) -> Result<()> {
    identifier(name)?;
    ensure!(
        binding.size.is_some() || !binding.pci_type0,
        "{name}: PCI header requires an overlay"
    );
    let offset_max = match binding.offset_type.as_str() {
        "u8" => u8::MAX as u64,
        "u16" => u16::MAX as u64,
        "u32" | "usize" => u32::MAX as u64,
        "u64" => u64::MAX,
        other => bail!("unsupported offset type {other}"),
    };
    if let Some(module) = &binding.offsets {
        identifier(module)?;
        docs(out, &block.description, 0);
        writeln!(out, "#[allow(dead_code)] pub mod {module} {{")?;
        let mut names = HashSet::new();
        for item in &block.items {
            let constant = item.name.to_ascii_uppercase();
            identifier(&constant)?;
            ensure!(
                names.insert(constant.clone()),
                "{name}: duplicate offset {constant}"
            );
            ensure!(
                u64::from(item.byte_offset) <= offset_max,
                "{name}.{constant}: offset does not fit {}",
                binding.offset_type
            );
            docs(out, &item.description, 4);
            writeln!(
                out,
                "    pub const {constant}: {} = {:#x};",
                binding.offset_type, item.byte_offset
            )?;
            if let Some(Array::Regular(array)) = &item.array {
                ensure!(
                    u64::from(array.stride) <= offset_max,
                    "{name}.{constant}: stride does not fit offset type"
                );
                for suffix in ["STRIDE", "COUNT"] {
                    ensure!(
                        names.insert(format!("{constant}_{suffix}")),
                        "{name}: duplicate array constant"
                    );
                }
                writeln!(
                    out,
                    "    pub const {constant}_STRIDE: {} = {:#x};",
                    binding.offset_type, array.stride
                )?;
                writeln!(
                    out,
                    "    pub const {constant}_COUNT: usize = {};",
                    array.len
                )?;
            }
        }
        writeln!(out, "}}")?;
    }

    let mut items = block.items.iter().collect::<Vec<_>>();
    items.sort_by_key(|item| item.byte_offset);
    let mut cursor = if binding.pci_type0 { 0x40 } else { 0 };
    if binding.size.is_some() {
        if binding.pci_type0 {
            writeln!(out, "fstart_pci::pci_type0_config! {{")?;
        } else {
            writeln!(out, "tock_registers::register_structs! {{")?;
        }
        docs(out, &block.description, 4);
        let vis = if binding.crate_private {
            "pub(crate)"
        } else {
            "pub"
        };
        // pci_type0_config's grammar includes `struct`, register_structs' does not.
        let keyword = if binding.pci_type0 { "struct " } else { "" };
        writeln!(out, "    {vis} {keyword}{name} {{")?;
    }
    let mut names = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        identifier(&item.name)?;
        ensure!(
            names.insert(&item.name),
            "{name}: duplicate register {}",
            item.name
        );
        let BlockItemInner::Register(reg) = &item.inner else {
            bail!("{name}: nested blocks are not supported by the Tock backend");
        };
        width(reg.bit_size)?;
        if let Some(fieldset) = &reg.fieldset {
            ensure!(
                ir.fieldsets[fieldset].bit_size == reg.bit_size,
                "{name}.{}: fieldset width mismatch",
                item.name
            );
        }
        let bytes = reg.bit_size / 8;
        let count = match &item.array {
            None => 1,
            Some(Array::Regular(array)) => {
                ensure!(array.len > 0, "{name}: empty register array");
                ensure!(array.stride >= bytes, "{name}: overlapping array elements");
                ensure!(
                    array
                        .stride
                        .checked_mul(array.len - 1)
                        .and_then(|last| item.byte_offset.checked_add(last))
                        .and_then(|last| last.checked_add(bytes))
                        .is_some(),
                    "{name}: register array extent overflow"
                );
                if binding.size.is_some() {
                    ensure!(
                        array.stride == bytes,
                        "{name}: sparse arrays cannot be overlaid"
                    );
                }
                array.len
            }
            Some(Array::Cursed(_)) => bail!("{name}: irregular arrays are not supported"),
        };
        if let Some(size) = binding.size {
            ensure!(
                item.byte_offset % bytes == 0,
                "{name}.{}: unaligned overlay register",
                item.name
            );
            ensure!(
                item.byte_offset >= cursor,
                "{name}.{}: overlapping overlay registers",
                item.name
            );
            if item.byte_offset > cursor {
                writeln!(out, "        ({cursor:#x} => _reserved_{index}),")?;
            }
            let end = bytes
                .checked_mul(count)
                .and_then(|len| item.byte_offset.checked_add(len));
            cursor = end.context("overlay extent overflow")?;
            ensure!(
                cursor <= size,
                "{name}.{}: register exceeds overlay size",
                item.name
            );
            let access = match reg.access {
                Access::ReadWrite => "MmioReadWrite",
                Access::Read => "MmioReadOnly",
                Access::Write => "MmioWriteOnly",
            };
            let fieldset = reg
                .fieldset
                .as_ref()
                .map(|fs| format!(", {fs}::Register"))
                .unwrap_or_default();
            let ty = format!("fstart_core::mmio::{access}<u{}{fieldset}>", reg.bit_size);
            let ty = if item.array.is_some() {
                format!("[{ty}; {count}]")
            } else {
                ty
            };
            docs(out, &item.description, 8);
            writeln!(
                out,
                "        ({:#x} => pub {}: {ty}),",
                item.byte_offset, item.name
            )?;
        }
    }
    if let Some(size) = binding.size {
        if cursor < size {
            writeln!(out, "        ({cursor:#x} => _reserved_end),")?;
        }
        writeln!(out, "        ({size:#x} => @END),\n    }}\n}}")?;
    }
    Ok(())
}

pub fn render(ir: &IR, catalog: &Catalog) -> Result<String> {
    let mut out = String::new();
    emit_fields(&mut out, ir)?;
    ensure!(
        catalog.blocks.len() == ir.blocks.len(),
        "every block must have exactly one binding"
    );
    for (name, binding) in &catalog.blocks {
        let block = ir
            .blocks
            .get(name)
            .with_context(|| format!("unknown block {name}"))?;
        emit_block(&mut out, name, block, binding, ir)?;
    }
    syn::parse_file(&out).context("generated Rust is invalid")?;
    // Use the repository's pinned rustfmt so regeneration also passes cargo fmt.
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("run rustfmt")?;
    child.stdin.take().unwrap().write_all(out.as_bytes())?;
    let formatted = child.wait_with_output()?;
    ensure!(
        formatted.status.success(),
        "rustfmt failed: {}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    Ok(format!(
        "// Generated by cargo register-gen from {}. DO NOT EDIT.\n// Regenerate: cargo register-gen\n\n{}",
        catalog.source.display(),
        String::from_utf8(formatted.stdout)?
    ))
}

/// Generate committed sources, or check them without modifying any file.
pub fn run(root: &Path, check: bool) -> Result<()> {
    let manifest: Manifest = serde_yaml::from_str(&fs::read_to_string(
        root.join("data/registers/catalogs.yaml"),
    )?)?;
    let mut outputs = HashSet::new();
    let mut stale = Vec::new();
    for catalog in &manifest.catalogs {
        for path in [&catalog.source, &catalog.output] {
            ensure!(
                path.components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "catalog paths must stay relative to the repository"
            );
        }
        ensure!(
            outputs.insert(&catalog.output),
            "duplicate output {}",
            catalog.output.display()
        );
        let text = fs::read_to_string(root.join(&catalog.source))?;
        let ir = load_ir(&text).with_context(|| catalog.source.display().to_string())?;
        let generated =
            render(&ir, catalog).with_context(|| catalog.source.display().to_string())?;
        let path = root.join(&catalog.output);
        if fs::read_to_string(&path).ok().as_deref() == Some(&generated) {
            continue;
        }
        if check {
            stale.push(catalog.output.display().to_string());
        } else {
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, generated)?;
            println!("generated {}", catalog.output.display());
        }
    }
    ensure!(
        stale.is_empty(),
        "stale generated registers; run cargo register-gen:\n{}",
        stale.join("\n")
    );
    Ok(())
}
