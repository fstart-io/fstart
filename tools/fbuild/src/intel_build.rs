//! Execute the three fixed Intel compiler units, after their SMM producer.
use crate::{
    board_manifest::BoardManifest,
    intel_layout::IntelStage,
    resolved_intel::ResolvedIntel,
    selection::{Selection, StageInput},
};
use object::read::elf::ProgramHeader;
use object::{Object, ObjectSection, ObjectSymbol};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

pub(crate) fn producer(
    root: &Path,
    board: &BoardManifest,
    layout: &ResolvedIntel,
    release: bool,
) -> Result<BTreeMap<String, String>, String> {
    let config = layout.assembler_config(&board.board)?;
    let artifacts = crate::build_board::build_smm_artifacts(root, board, release, &config)?
        .ok_or("Intel ramstage requires an SMM image")?;
    let mut env = BTreeMap::from([(
        "FSTART_SMM_IMAGE".into(),
        artifacts.image_path.display().to_string(),
    )]);
    if let Some(header) = artifacts.header_path {
        env.insert(
            "FSTART_SMM_COREBOOT_HEADER".into(),
            header.display().to_string(),
        );
    }
    Ok(env)
}

pub(crate) fn selection(
    root: &Path,
    board: &BoardManifest,
    layout: &ResolvedIntel,
    role: IntelStage,
    release: bool,
    producer: &BTreeMap<String, String>,
) -> Result<Selection, String> {
    let row = layout
        .stages
        .iter()
        .find(|row| row.role == role)
        .ok_or("missing fixed Intel stage")?;
    let mut env = if role == IntelStage::Ramstage {
        producer.clone()
    } else {
        BTreeMap::new()
    };
    if role == IntelStage::Ramstage && !env.contains_key("FSTART_SMM_IMAGE") {
        return Err("missing generated SMM image".into());
    }
    let mut hash = Sha256::new();
    for (name, path) in &env {
        hash.update(name);
        hash.update(fs::read(path).map_err(|e| format!("producer {path}: {e}"))?);
    }
    let directory = layout
        .artifact_dir(root, &board.board, release)?
        .join(role.name())
        .join(format!("{:x}", hash.finalize()));
    if role == IntelStage::Ramstage {
        env.insert("FSTART_INTEL_MAX_CPUS".into(), layout.max_cpus.to_string());
    }
    Selection::prepare_stage(
        root,
        board,
        StageInput {
            target: &layout.target,
            entry: "x86_64",
            env: role.environment(),
            payload: &row.payload,
            features: &row.features,
            build_std: "core,alloc",
            directory,
            linker_script: crate::linker::resolved_intel(&layout.reservations, role, true)?,
            resolved_json: layout.json()?,
            producer_environment: env,
        },
        release,
    )
}

fn execute(
    root: &Path,
    board: &BoardManifest,
    layout: &ResolvedIntel,
    release: bool,
    checking: bool,
) -> Result<crate::build_board::BuildResult, String> {
    let producer = producer(root, board, layout, release)?;
    let mut stages = Vec::new();
    for row in &layout.stages {
        let selection = selection(root, board, layout, row.role, release, &producer)?;
        eprintln!(
            "[fstart] Intel {}: {}",
            row.role.name(),
            selection.directory.display()
        );
        let output = selection
            .command(!checking)
            .current_dir(root)
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!("Intel {} compiler failed", row.role.name()));
        }
        if checking {
            continue;
        }
        let artifacts = output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(serde_json::from_slice::<serde_json::Value>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        let executables: Vec<_> = artifacts
            .iter()
            .filter(|v| {
                v["reason"] == "compiler-artifact"
                    && v["target"]["name"].as_str() == board.stage_bin.as_deref()
            })
            .filter_map(|v| v["executable"].as_str())
            .collect();
        let [elf] = executables.as_slice() else {
            return Err("expected exactly one Intel stage executable".into());
        };
        let elf = PathBuf::from(elf);
        validate_elf(
            &fs::read(&elf).map_err(|e| e.to_string())?,
            &layout.reservations,
            row.role,
        )?;
        let flat = selection.directory.join("stage.bin");
        crate::build_board::write_flat_binary(&elf, &flat)?;
        let reservation = layout.reservations.stage(row.role);
        let capacity = if row.role == IntelStage::Bootblock {
            reservation.image.size
        } else {
            reservation.load_window()?.size
        };
        let size = fs::metadata(&flat).map_err(|e| e.to_string())?.len();
        if size > capacity || (row.role == IntelStage::Bootblock && size != capacity) {
            return Err("Intel initialized image differs from its reservation".into());
        }
        stages.push(fstart_image_build::StageBinary {
            name: row.role.name().into(),
            path: elf,
            run_path: flat,
            load_addr: reservation.image.base,
        });
    }
    Ok(crate::build_board::BuildResult { stages })
}
pub fn check(
    root: &Path,
    board: &BoardManifest,
    layout: &ResolvedIntel,
    release: bool,
) -> Result<(), String> {
    execute(root, board, layout, release, true).map(|_| ())
}
pub fn build(
    root: &Path,
    board: &BoardManifest,
    layout: &ResolvedIntel,
    release: bool,
) -> Result<crate::build_board::BuildResult, String> {
    execute(root, board, layout, release, false)
}

pub(crate) fn validate_elf(
    bytes: &[u8],
    layout: &crate::intel_layout::IntelReservations,
    role: IntelStage,
) -> Result<(), String> {
    let file = object::File::parse(bytes).map_err(|e| e.to_string())?;
    if file.architecture() != object::Architecture::X86_64 || !file.is_little_endian() {
        return Err("Intel ELF architecture/endianness mismatch".into());
    }
    let object::File::Elf64(elf) = &file else {
        return Err("Intel ELF must be ELF64".into());
    };
    let reservation = layout.stage(role);
    let mut loads = 0;
    for segment in elf
        .elf_program_headers()
        .iter()
        .filter(|s| s.p_type(elf.endian()) == object::elf::PT_LOAD)
    {
        let paddr = segment.p_paddr(elf.endian());
        let vaddr = segment.p_vaddr(elf.endian());
        let filesz = segment.p_filesz(elf.endian());
        let memsz = segment.p_memsz(elf.endian());
        if memsz == 0 {
            continue;
        }
        loads += 1;
        let stored = filesz == 0
            || reservation.image.contains(paddr, filesz)
            || (role != IntelStage::Bootblock && reservation.writable.contains(paddr, filesz));
        let runtime =
            reservation.image.contains(vaddr, memsz) || reservation.writable.contains(vaddr, memsz);
        if filesz > memsz
            || !stored
            || !runtime
            || (role != IntelStage::Bootblock && paddr != vaddr)
        {
            return Err("Intel PT_LOAD exceeds physical/virtual reservations".into());
        }
    }
    if loads == 0 {
        return Err("Intel ELF has no load segments".into());
    }
    let entry = if role == IntelStage::Bootblock {
        0xfffffff0
    } else {
        reservation.image.base
    };
    if file.entry() != entry {
        return Err(format!(
            "Intel entry {:#x} differs from {entry:#x}",
            file.entry()
        ));
    }
    let descriptor = file
        .section_by_name(".fstart.layout")
        .ok_or("missing Intel descriptor")?;
    if descriptor.data().map_err(|e| e.to_string())? != layout.descriptor(role)?.as_bytes()
        || !reservation
            .image
            .contains(descriptor.address(), descriptor.size())
    {
        return Err("Intel descriptor differs from resolved layout".into());
    }
    let object::SectionFlags::Elf { sh_flags } = descriptor.flags() else {
        return Err("non-ELF descriptor".into());
    };
    if descriptor.address() % 8 != 0
        || descriptor.file_range().is_none()
        || sh_flags & u64::from(object::elf::SHF_ALLOC) == 0
        || sh_flags & u64::from(object::elf::SHF_WRITE) != 0
    {
        return Err("Intel descriptor is not loaded aligned read-only data".into());
    }
    let mut symbols = vec![
        ("_fstart_layout_start", descriptor.address()),
        (
            "_fstart_layout_end",
            descriptor.address() + descriptor.size(),
        ),
        ("_stack_bottom", reservation.stack_span().base),
        ("_stack_top", reservation.stack_span().end()?),
    ];
    if let Some(heap) = reservation.heap_span() {
        symbols.push(("_FSTART_HEAP", heap.base));
    }
    for (name, address) in symbols {
        if !file
            .symbols()
            .any(|s| s.name() == Ok(name) && s.address() == address)
        {
            return Err(format!("Intel symbol {name} differs from reservation"));
        }
    }
    Ok(())
}
