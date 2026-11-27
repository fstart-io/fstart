//! Host-only ACPI baseline check. No firmware or AML methods run on real hardware.

use fstart_acpi::wake::{FacsPrefix, FadtV2Prefix, Rsdp, SdtHeader, XsdtEntry};
use fstart_image_build::acpi::{AcpiBaseline, AcpiOutcome};
use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use zerocopy::FromBytes;

const BASE: u64 = 0x7f00_0000;
const SDT_HEADER: usize = 36;
const FADT_HW_REDUCED_ACPI: u32 = 1 << 20;

struct AcpiImage<'a> {
    bytes: &'a [u8],
    physical_base: u64,
    rsdp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPhase {
    IaslDisassemble,
    IaslCompile,
    AcpiExec,
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().copied().fold(0u8, u8::wrapping_add) == 0
}

fn check_no_overlaps(mut spans: Vec<(u64, u64, String)>) -> Result<(), String> {
    spans.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in spans.windows(2) {
        let (a_start, a_end, a_name) = &pair[0];
        let (b_start, b_end, b_name) = &pair[1];
        if *a_end > *b_start {
            return Err(format!(
                "overlapping ACPI tables: {a_name} ({a_start:#x}..{a_end:#x}) overlaps {b_name} ({b_start:#x}..{b_end:#x})"
            ));
        }
    }
    Ok(())
}

fn table_label(bytes: &[u8]) -> String {
    let raw = bytes.get(..4).unwrap_or(bytes);
    match std::str::from_utf8(raw) {
        Ok(sig) if sig.bytes().all(|b| b.is_ascii_graphic()) => sig.into(),
        _ => format!("{raw:02x?}"),
    }
}

/// Sanitize a validated 4-byte ASCII graphic ACPI signature (such as `"ASF!"`)
/// into a safe filename stem.
fn safe_signature_stem(sig: &str) -> String {
    let mut out = String::with_capacity(sig.len());
    for byte in sig.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'_' {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("_{byte:02X}"));
        }
    }
    out
}

fn fadt_pointer(low32: u32, high64: u64, name: &str) -> Result<u64, String> {
    let low = u64::from(low32);
    if low != 0 && high64 != 0 && low != high64 {
        return Err(format!(
            "FADT {name} mismatch: 32-bit {low:#x} != 64-bit {high64:#x}"
        ));
    }
    Ok(if high64 != 0 { high64 } else { low })
}

impl<'a> AcpiImage<'a> {
    fn tail(&self, address: u64, alignment: u64) -> Result<&'a [u8], String> {
        if !address.is_multiple_of(alignment) {
            return Err(format!("unaligned ACPI address {address:#x}"));
        }
        address
            .checked_sub(self.physical_base)
            .and_then(|offset| usize::try_from(offset).ok())
            .and_then(|offset| self.bytes.get(offset..))
            .ok_or_else(|| format!("ACPI address {address:#x} outside image"))
    }

    fn table(&self, address: u64, facs: bool) -> Result<&'a [u8], String> {
        let tail = self.tail(address, if facs { 64 } else { 4 })?;
        let size = if facs {
            FacsPrefix::ref_from_prefix(tail)
                .map(|(h, _)| h.length.get() as usize)
                .unwrap_or(0)
        } else {
            SdtHeader::ref_from_prefix(tail)
                .map(|(h, _)| h.length.get() as usize)
                .unwrap_or(0)
        };
        let table = tail
            .get(..size)
            .filter(|t| t.len() >= if facs { 64 } else { SDT_HEADER })
            .ok_or_else(|| format!("invalid table length {size} at {address:#x}"))?;
        if facs && !table.starts_with(b"FACS") || !facs && !checksum_ok(table) {
            return Err(format!("invalid ACPI table at {address:#x}"));
        }
        Ok(table)
    }

    fn tables(&self) -> Result<Vec<&'a [u8]>, String> {
        let rsdp_tail = self.tail(self.rsdp, 16)?;
        let (rsdp_hdr, _) = Rsdp::ref_from_prefix(rsdp_tail).map_err(|_| "missing ACPI 2+ RSDP")?;
        if rsdp_hdr.signature != *b"RSD PTR " || rsdp_hdr.revision < 2 {
            return Err("missing ACPI 2+ RSDP".into());
        }
        let rsdp = rsdp_tail
            .get(..rsdp_hdr.length.get() as usize)
            .filter(|r| r.len() >= 36 && checksum_ok(&r[..20]) && checksum_ok(r))
            .ok_or("invalid RSDP length or checksum")?;
        let rsdt_addr = rsdp_hdr.rsdt_addr.get();
        if rsdt_addr != 0 {
            return Err(format!(
                "non-zero RSDP legacy RSDT address {rsdt_addr:#x} is unsupported"
            ));
        }
        let xsdt_addr = rsdp_hdr.xsdt_addr.get();
        let xsdt = self.table(xsdt_addr, false)?;
        let entries =
            <[XsdtEntry]>::ref_from_bytes(&xsdt[SDT_HEADER..]).map_err(|_| "invalid XSDT")?;
        if !xsdt.starts_with(b"XSDT") {
            return Err("invalid XSDT".into());
        }

        let mut addresses = BTreeSet::from([self.rsdp, xsdt_addr]);
        let mut spans = vec![
            (
                self.rsdp,
                self.rsdp
                    .checked_add(rsdp.len() as u64)
                    .ok_or("RSDP address overflow")?,
                "RSDP".into(),
            ),
            (
                xsdt_addr,
                xsdt_addr
                    .checked_add(xsdt.len() as u64)
                    .ok_or("XSDT address overflow")?,
                "XSDT".into(),
            ),
        ];
        let mut tables = vec![xsdt];

        for entry in entries {
            let addr = entry.0.get();
            if !addresses.insert(addr) {
                return Err(format!("duplicate ACPI table address {addr:#x}"));
            }
            let table = self.table(addr, false)?;
            if table.starts_with(b"FACS") {
                return Err("FACS must not appear in XSDT".into());
            }
            if table.starts_with(b"DSDT") {
                return Err("DSDT must not appear in XSDT".into());
            }
            spans.push((
                addr,
                addr.checked_add(table.len() as u64)
                    .ok_or("ACPI table address overflow")?,
                table_label(table),
            ));
            tables.push(table);
        }

        let mut fadts = tables.iter().copied().filter(|t| t.starts_with(b"FACP"));
        let fadt = fadts.next().ok_or("missing FADT")?;
        let (fadt_hdr, _) = FadtV2Prefix::ref_from_prefix(fadt)
            .map_err(|_| format!("pre-2.0 FADT unsupported (length {})", fadt.len()))?;
        if fadts.next().is_some() {
            return Err("duplicate FADT".into());
        }

        let dsdt_addr = fadt_pointer(fadt_hdr.prefix.dsdt.get(), fadt_hdr.x_dsdt.get(), "DSDT")?;
        if dsdt_addr == 0 {
            return Err("FADT DSDT address is zero".into());
        }
        if !addresses.insert(dsdt_addr) {
            return Err(format!("duplicate ACPI table address {dsdt_addr:#x}"));
        }
        let dsdt = self.table(dsdt_addr, false)?;
        if !dsdt.starts_with(b"DSDT") {
            return Err("FADT DSDT pointer does not reference a DSDT".into());
        }
        spans.push((
            dsdt_addr,
            dsdt_addr
                .checked_add(dsdt.len() as u64)
                .ok_or("DSDT address overflow")?,
            "DSDT".into(),
        ));
        tables.push(dsdt);

        let hw_reduced = (fadt_hdr.prefix.flags.get() & FADT_HW_REDUCED_ACPI) != 0;
        let facs_addr = fadt_pointer(
            fadt_hdr.prefix.firmware_ctrl.get(),
            fadt_hdr.prefix.x_firmware_ctrl.get(),
            "FIRMWARE_CTRL",
        )?;
        if facs_addr == 0 {
            if !hw_reduced {
                return Err("non-HW-reduced FADT is missing FIRMWARE_CTRL/X_FIRMWARE_CTRL".into());
            }
        } else {
            if !addresses.insert(facs_addr) {
                return Err(format!("duplicate ACPI table address {facs_addr:#x}"));
            }
            let facs = self.table(facs_addr, true)?;
            spans.push((
                facs_addr,
                facs_addr
                    .checked_add(facs.len() as u64)
                    .ok_or("FACS address overflow")?,
                "FACS".into(),
            ));
            tables.push(facs);
        }

        check_no_overlaps(spans)?;
        Ok(tables)
    }
}

fn contains_ae_status_token(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if &bytes[i..i + 3] == b"AE_" {
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric() && bytes[i - 1] != b'_';
            let after_start = i + 3;
            let mut end = after_start;
            while end < bytes.len() && (bytes[end].is_ascii_uppercase() || bytes[end] == b'_') {
                end += 1;
            }
            let after_ok = end > after_start + 1
                && (end == bytes.len()
                    || (!bytes[end].is_ascii_alphanumeric() && bytes[end] != b'_'));
            if before_ok && after_ok {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn has_tool_error(phase: ToolPhase, output: &str) -> bool {
    output.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Error ")
            || trimmed.starts_with("Error:")
            || trimmed.starts_with("Firmware Error")
            || trimmed.starts_with("Could not ")
            || line.contains("ACPI Error:")
            || line.contains("ACPI BIOS Error")
            || line.contains("ACPI Exception:")
        {
            return true;
        }
        // Match ACPICA AE_* status diagnostics without tripping on OEM IDs in
        // `ACPI: <SIG> ...` table banners or AML identifiers in warning source
        // excerpts.
        if !trimmed.starts_with("ACPI: ") && contains_ae_status_token(line) {
            match phase {
                ToolPhase::IaslCompile => {
                    line.contains("Error") || line.contains("Exception") || line.contains("Abort")
                }
                ToolPhase::IaslDisassemble | ToolPhase::AcpiExec => {
                    line.contains("Error")
                        || line.contains("Exception")
                        || line.contains("Failed")
                        || line.contains("Failure")
                        || line.contains("Could not")
                        || line.contains("Status")
                        || line.contains("Abort")
                        || line.contains(", AE_")
                }
            }
        } else {
            false
        }
    })
}

fn parse_iasl_warnings(output: &str) -> usize {
    for line in output.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with("Compilation successful.")
            || trimmed.starts_with("Compilation complete.")
        {
            for part in trimmed.split(',') {
                let mut words = part.split_whitespace();
                if let (Some(count), Some(label)) = (words.next(), words.next())
                    && label.starts_with("Warning")
                    && let Ok(n) = count.parse::<usize>()
                {
                    return n;
                }
            }
        }
    }
    0
}

fn check_decoded_data_table(dsl_path: &Path) -> Result<(), String> {
    let dsl = fs::read_to_string(dsl_path)
        .map_err(|e| format!("read decoded data table {}: {e}", dsl_path.display()))?;
    if !dsl.contains("ACPI Data Table [") {
        return Err(format!(
            "decoded data table {} is missing ACPICA data-table header",
            dsl_path.display()
        ));
    }
    for (idx, line) in dsl.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("****") || line.contains("[Unknown ") {
            return Err(format!(
                "data table decoder error in {}:{} ({})",
                dsl_path.display(),
                idx + 1,
                trimmed
            ));
        }
    }
    Ok(())
}

fn normalize_acpi_segment(seg: &str) -> String {
    let mut out = seg.to_ascii_uppercase();
    while out.len() < 4 {
        out.push('_');
    }
    out
}

fn normalize_acpi_path(path: &str) -> String {
    path.trim()
        .trim_start_matches('\\')
        .split('.')
        .filter(|s| !s.is_empty())
        .map(normalize_acpi_segment)
        .collect::<Vec<_>>()
        .join(".")
}

/// Extract declared `External (<path>, ...)` targets from a disassembled `.dsl`.
fn extract_dsl_externals(dsl: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in dsl.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix("External (")
            .or_else(|| trimmed.strip_prefix("External("))
            && let Some(target) = rest.split(',').next()
        {
            let normalized = normalize_acpi_path(target);
            if !normalized.is_empty() {
                out.push(normalized);
            }
        }
    }
    out
}

/// Parse `acpiexec -b namespace` output into canonical 4-char-padded ACPI paths.
fn parse_acpiexec_namespace(log: &str) -> Option<BTreeSet<String>> {
    let mut lines = log.lines();
    lines.find(|line| line.trim() == "ACPI Namespace (from Namespace Root):")?;
    let mut stack: Vec<String> = Vec::new();
    let mut paths = BTreeSet::new();
    for line in lines {
        let trimmed = line.trim_start();
        let Some((depth_str, rest)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(depth) = depth_str.parse::<usize>() else {
            continue;
        };
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        let seg = normalize_acpi_segment(name);
        if depth <= stack.len() {
            stack.truncate(depth);
        }
        stack.push(seg);
        paths.insert(stack.join("."));
    }
    Some(paths)
}

fn run_tool(
    dir: &Path,
    tool: &str,
    args: &[&str],
    log: &str,
    phase: ToolPhase,
) -> Result<String, String> {
    let log_path = dir.join(log);
    let log_file = fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let mut child = Command::new(tool)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(log_file.try_clone().map_err(|e| e.to_string())?)
        .stdout(log_file)
        .spawn()
        .map_err(|e| format!("start {tool} (ACPICA tools): {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("wait for {tool}: {e}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{tool} timed out; see {}", log_path.display()));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = fs::read_to_string(&log_path).map_err(|e| e.to_string())?;
    if !status.success() || has_tool_error(phase, &output) {
        let excerpt: Vec<&str> = output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let tail = if excerpt.len() > 12 {
            &excerpt[excerpt.len() - 12..]
        } else {
            &excerpt[..]
        };
        let detail = if tail.is_empty() {
            String::new()
        } else {
            format!("\n{}", tail.join("\n"))
        };
        return Err(format!(
            "{tool} {args:?} failed ({status}); see {}{detail}",
            log_path.display()
        ));
    }
    Ok(output)
}

fn check_acpica_tools() -> Result<(), String> {
    for tool in ["iasl", "acpiexec"] {
        let status = Command::new(tool)
            .arg("-v")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| {
                format!("required ACPICA tool `{tool}` unavailable (install acpica-tools): {e}")
            })?;
        if !status.success() {
            return Err(format!(
                "ACPICA tool `{tool} -v` exited with {status} (install acpica-tools)"
            ));
        }
    }
    Ok(())
}

fn validate(dir: &Path, baseline: &AcpiBaseline) -> Result<(), String> {
    fs::write(dir.join("image.bin"), &baseline.bytes).map_err(|e| e.to_string())?;
    let image = AcpiImage {
        bytes: &baseline.bytes,
        physical_base: baseline.physical_base,
        rsdp: baseline.rsdp,
    };
    let mut dsdt = None;
    let mut aml = Vec::new();
    let mut data = Vec::new();
    let mut acpiexec_data = Vec::new();
    for (index, table) in image.tables()?.into_iter().enumerate() {
        let sig = std::str::from_utf8(&table[..4]).map_err(|_| "invalid ACPI signature")?;
        if !sig.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(format!("invalid ACPI signature {sig:?}"));
        }
        let stem = safe_signature_stem(sig);
        let file = format!("{stem}-{index:02}.aml");
        fs::write(dir.join(&file), table).map_err(|e| e.to_string())?;
        match sig {
            "DSDT" => dsdt = Some(file),
            "SSDT" | "PSDT" => aml.push(file),
            // FACS has neither an SDT header nor any AML to disassemble.
            "FACS" => (),
            // XSDT is decoded by iasl -d; acpiexec builds its own XSDT from the
            // passed table files.
            "XSDT" => data.push(file),
            _ => {
                acpiexec_data.push(file.clone());
                data.push(file);
            }
        }
    }
    let dsdt = dsdt.ok_or("no DSDT to validate")?;
    let namespace: Vec<&str> = core::iter::once(dsdt.as_str())
        .chain(aml.iter().map(String::as_str))
        .collect();

    // 1. Disassemble every AML table (DSDT + any SSDTs/PSDTs), passing all
    //    other namespace tables via `-e` for cross-table symbol resolution.
    for file in &namespace {
        let stem = file.strip_suffix(".aml").unwrap_or(file);
        let mut args = Vec::new();
        for other in &namespace {
            if other != file {
                args.push("-e");
                args.push(other);
            }
        }
        args.push("-d");
        args.push(file);
        run_tool(
            dir,
            "iasl",
            &args,
            &format!("iasl-disasm-{stem}.log"),
            ToolPhase::IaslDisassemble,
        )?;
    }

    // 2. Recompile every disassembled AML table with `-we` (warnings as errors)
    //    so ASL syntax, scopes, and resource descriptors are checked cleanly.
    for file in &namespace {
        let stem = file.strip_suffix(".aml").unwrap_or(file);
        let dsl = format!("{stem}.dsl");
        let prefix = format!("recompiled-{stem}");
        let log = format!("iasl-recompile-{stem}.log");
        let output = run_tool(
            dir,
            "iasl",
            &["-we", "-p", &prefix, &dsl],
            &log,
            ToolPhase::IaslCompile,
        )?;
        let warnings = parse_iasl_warnings(&output);
        if warnings > 0 {
            return Err(format!(
                "iasl reported {warnings} warning(s) for {dsl}; see {}",
                dir.join(&log).display()
            ));
        }
    }

    // 3. Disassemble every fixed/data table (FADT, MADT, MCFG, DMAR, IVRS, ...)
    //    and inspect the decoded `.dsl` for ACPICA decoder errors.
    for file in &data {
        let stem = file.strip_suffix(".aml").unwrap_or(file);
        run_tool(
            dir,
            "iasl",
            &["-d", file],
            &format!("iasl-{file}.log"),
            ToolPhase::IaslDisassemble,
        )?;
        check_decoded_data_table(&dir.join(format!("{stem}.dsl")))?;
    }

    // 4. Load the AML namespace along with the real FADT and data tables in
    //    acpiexec (skipping hardware _INI/_STA and allocator diagnostics), then
    //    verify the namespace loaded completely and every disassembled External
    //    resolves against it.
    let mut args = vec!["-l", "-di", "-dt", "-b", "namespace"];
    args.extend(namespace.iter().copied());
    args.extend(acpiexec_data.iter().map(String::as_str));
    let exec_log = run_tool(dir, "acpiexec", &args, "acpiexec.log", ToolPhase::AcpiExec)?;
    let expected_loaded = format!(
        "ACPI: {} ACPI AML tables successfully acquired and loaded",
        namespace.len()
    );
    if !exec_log.contains(&expected_loaded)
        || !exec_log.contains("Final data object initialization:")
    {
        return Err(format!(
            "acpiexec did not confirm loading {} AML table(s); see {}",
            namespace.len(),
            dir.join("acpiexec.log").display()
        ));
    }
    let loaded_paths = parse_acpiexec_namespace(&exec_log).ok_or_else(|| {
        format!(
            "acpiexec did not dump namespace; see {}",
            dir.join("acpiexec.log").display()
        )
    })?;
    for file in &namespace {
        let stem = file.strip_suffix(".aml").unwrap_or(file);
        let dsl_path = dir.join(format!("{stem}.dsl"));
        let dsl = fs::read_to_string(&dsl_path)
            .map_err(|e| format!("read {}: {e}", dsl_path.display()))?;
        for external in extract_dsl_externals(&dsl) {
            if !loaded_paths.contains(&external) {
                return Err(format!(
                    "unresolved AML External reference `{external}` in {}",
                    dsl_path.display()
                ));
            }
        }
    }

    Ok(())
}

fn scenario_dir_name(scenario: &str) -> Result<&str, String> {
    if scenario.is_empty()
        || !scenario
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(format!("invalid ACPI scenario name {scenario:?}"));
    }
    Ok(scenario)
}

fn validate_scenarios(images: &[AcpiBaseline]) -> Result<(), String> {
    if images.is_empty() {
        return Err("platform emitted zero ACPI baseline images".into());
    }
    let mut seen = BTreeSet::new();
    for baseline in images {
        let name = scenario_dir_name(&baseline.scenario)?;
        if !seen.insert(name) {
            return Err(format!("duplicate ACPI scenario name {name:?}"));
        }
    }
    Ok(())
}

fn prepare_board_dir(root: &Path, board: &str) -> Result<std::path::PathBuf, String> {
    let dir = root.join("target/fstart-acpi").join(board);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("clean {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir)
}

enum BoardAcpiResult {
    Passed(usize),
    Skipped,
    Failed,
}

fn check_one_board(
    root: &Path,
    manifests: &[crate::board_manifest::BoardManifest],
    name: &str,
) -> BoardAcpiResult {
    let result = (|| {
        let board = crate::board_manifest::find_in(manifests, name)?;
        match crate::host_plan::acpi_baselines(root, &board, BASE)? {
            AcpiOutcome::NotApplicable { reason } => {
                println!("SKIP {name}: {reason}");
                Ok::<_, String>(None)
            }
            AcpiOutcome::Images { images } => {
                validate_scenarios(&images)?;
                // Serialize checks that share `target/fstart-acpi/<board>` so
                // repeated board IDs or variants cannot wipe each other's dir.
                let board_lock =
                    crate::path_lock(&root.join("target/fstart-acpi").join(&board.board));
                let _board_guard = board_lock.lock().unwrap_or_else(|e| e.into_inner());
                let board_dir = prepare_board_dir(root, &board.board)?;
                let multi = images.len() > 1;
                let mut scenario_errors = Vec::new();
                for baseline in &images {
                    let dir = board_dir.join(&baseline.scenario);
                    fs::create_dir_all(&dir)
                        .map_err(|e| format!("create {}: {e}", dir.display()))?;
                    match validate(&dir, baseline) {
                        Ok(()) => {
                            if multi {
                                println!("PASS {name} ({}): {}", baseline.scenario, dir.display());
                            } else {
                                println!("PASS {name}: {}", dir.display());
                            }
                        }
                        Err(e) => {
                            scenario_errors.push(format!(
                                "{} ({}): {e}",
                                baseline.scenario,
                                dir.display()
                            ));
                        }
                    }
                }
                if !scenario_errors.is_empty() {
                    return Err(scenario_errors.join("; "));
                }
                Ok(Some(images.len()))
            }
        }
    })();
    match result {
        Ok(Some(count)) => BoardAcpiResult::Passed(count),
        Ok(None) => BoardAcpiResult::Skipped,
        Err(err) => {
            eprintln!("FAIL {name}: {err}");
            BoardAcpiResult::Failed
        }
    }
}

/// Generate and validate baseline ACPI images for the selected boards/variants,
/// or every discovered board and variant when `boards` is empty.
pub fn check(boards: &[String]) -> Result<(), String> {
    check_acpica_tools()?;
    let root = crate::build_board::workspace_root()?;
    let manifests = crate::board_manifest::discover(&root)?;
    let selected: Vec<String> = if boards.is_empty() {
        let mut all: Vec<String> = manifests
            .iter()
            .flat_map(|m| std::iter::once(m.board.clone()).chain(m.variants.keys().cloned()))
            .collect();
        all.sort();
        all
    } else {
        boards.to_vec()
    };

    let mut outcomes: Vec<Option<BoardAcpiResult>> = (0..selected.len()).map(|_| None).collect();
    if let Some((first, rest)) = selected.split_first() {
        outcomes[0] = Some(check_one_board(&root, &manifests, first));
        let workers = crate::default_parallelism().min(rest.len());
        if workers <= 1 {
            for (idx, name) in rest.iter().enumerate() {
                outcomes[idx + 1] = Some(check_one_board(&root, &manifests, name));
            }
        } else {
            let next = std::sync::atomic::AtomicUsize::new(0);
            let slots: Vec<std::sync::Mutex<Option<BoardAcpiResult>>> = (0..rest.len())
                .map(|_| std::sync::Mutex::new(None))
                .collect();
            std::thread::scope(|scope| {
                for _ in 0..workers {
                    scope.spawn(|| {
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            if i >= rest.len() {
                                break;
                            }
                            let outcome = check_one_board(&root, &manifests, &rest[i]);
                            *slots[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
                        }
                    });
                }
            });
            for (idx, slot) in slots.into_iter().enumerate() {
                outcomes[idx + 1] = slot.into_inner().unwrap_or_else(|e| e.into_inner());
            }
        }
    }

    let mut passed_boards = 0usize;
    let mut passed_images = 0usize;
    let mut skipped_boards = 0usize;
    let mut failed = Vec::new();
    for (name, outcome) in selected.iter().zip(outcomes) {
        match outcome.expect("every selected board was checked") {
            BoardAcpiResult::Passed(count) => {
                passed_boards += 1;
                passed_images += count;
            }
            BoardAcpiResult::Skipped => skipped_boards += 1,
            BoardAcpiResult::Failed => failed.push(name.as_str()),
        }
    }

    println!(
        "ACPI check summary: {passed_images} image(s) passed across {passed_boards} board(s), {skipped_boards} skipped, {} failed",
        failed.len()
    );
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("ACPI check failed: {}", failed.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use byteorder::{ByteOrder, LittleEndian as Le};
    use fstart_acpi::platform::{FadtConfig, assemble};

    fn sample_fadt() -> FadtConfig {
        FadtConfig {
            pm1a_evt_blk: 0x400,
            pm1a_cnt_blk: 0x404,
            pm_tmr_blk: 0x408,
            gpe0_blk: 0x420,
            gpe0_blk_len: 16,
            sci_int: 9,
            ..FadtConfig::default()
        }
    }

    fn sample_image() -> Vec<u8> {
        assemble(BASE, &sample_fadt(), &[], &[], &[])
    }

    fn image(bytes: &[u8]) -> AcpiImage<'_> {
        AcpiImage {
            bytes,
            physical_base: BASE,
            rsdp: BASE,
        }
    }

    fn fix_checksum(table: &mut [u8]) {
        table[9] = 0;
        table[9] = 0u8.wrapping_sub(table.iter().copied().fold(0u8, u8::wrapping_add));
    }

    fn fix_rsdp_checksums(rsdp: &mut [u8]) {
        rsdp[8] = 0;
        rsdp[8] = 0u8.wrapping_sub(rsdp[..20].iter().copied().fold(0u8, u8::wrapping_add));
        rsdp[32] = 0;
        rsdp[32] = 0u8.wrapping_sub(rsdp.iter().copied().fold(0u8, u8::wrapping_add));
    }

    fn compile_asl(dir: &Path, name: &str, asl: &str) -> Vec<u8> {
        let src = dir.join(format!("{name}.asl"));
        fs::write(&src, asl).unwrap();
        let prefix = dir.join(name);
        let output = Command::new("iasl")
            .args(["-oe", "-p"])
            .arg(&prefix)
            .arg(&src)
            .output()
            .expect("run iasl");
        assert!(
            output.status.success(),
            "iasl failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read(dir.join(format!("{name}.aml"))).unwrap()
    }

    fn assemble_with_dsdt_and_extra(dsdt: &[u8], extra: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = assemble(BASE, &sample_fadt(), &[], &[], extra);
        // Append the compiled DSDT at the end (16-byte aligned) and repoint FADT.
        let tables = image(&bytes).tables().unwrap();
        let fadt_off = tables[1].as_ptr() as usize - bytes.as_ptr() as usize;
        let fadt_len = Le::read_u32(&bytes[fadt_off + 4..fadt_off + 8]) as usize;
        let aligned = bytes.len().next_multiple_of(16);
        bytes.resize(aligned, 0);
        let new_dsdt_addr = BASE + bytes.len() as u64;
        bytes.extend_from_slice(dsdt);
        let fadt = &mut bytes[fadt_off..fadt_off + fadt_len];
        Le::write_u32(&mut fadt[40..44], new_dsdt_addr as u32);
        Le::write_u64(&mut fadt[140..148], new_dsdt_addr);
        fix_checksum(fadt);
        bytes
    }

    #[test]
    fn follows_the_assembled_tables_and_rejects_invalid_images() {
        let mut bytes = sample_image();
        let tables = image(&bytes).tables().unwrap();
        assert_eq!(
            tables.iter().map(|t| &t[..4]).collect::<Vec<_>>(),
            [b"XSDT", b"FACP", b"DSDT", b"FACS"]
        );
        let fadt = tables[1].as_ptr() as usize - bytes.as_ptr() as usize;
        let dsdt = tables[2].as_ptr() as usize - bytes.as_ptr() as usize;
        assert!(image(&bytes[..dsdt + 8]).tables().is_err());

        bytes[dsdt + 9] ^= 1;
        assert!(
            image(&bytes)
                .tables()
                .unwrap_err()
                .contains("invalid ACPI table")
        );
        bytes[dsdt + 9] ^= 1;

        let len = Le::read_u32(&bytes[fadt + 4..fadt + 8]) as usize;
        let table = &mut bytes[fadt..fadt + len];
        Le::write_u32(&mut table[40..44], 0);
        Le::write_u64(&mut table[140..148], BASE - 16);
        fix_checksum(table);
        assert!(
            image(&bytes)
                .tables()
                .unwrap_err()
                .contains("outside image")
        );
    }

    #[test]
    fn rejects_fadt_32_vs_64_pointer_mismatch() {
        let mut bytes = sample_image();
        let tables = image(&bytes).tables().unwrap();
        let fadt = tables[1].as_ptr() as usize - bytes.as_ptr() as usize;
        let len = Le::read_u32(&bytes[fadt + 4..fadt + 8]) as usize;
        let table = &mut bytes[fadt..fadt + len];
        Le::write_u32(&mut table[40..44], (BASE + 0x100) as u32);
        fix_checksum(table);
        let err = image(&bytes).tables().unwrap_err();
        assert!(err.contains("FADT DSDT mismatch"), "{err}");
    }

    #[test]
    fn rejects_nonzero_rsdt_pointer_in_rsdp() {
        let mut bytes = sample_image();
        let rsdp_len = Le::read_u32(&bytes[20..24]) as usize;
        Le::write_u32(&mut bytes[16..20], 0xdead_beef);
        fix_rsdp_checksums(&mut bytes[..rsdp_len]);
        let err = image(&bytes).tables().unwrap_err();
        assert!(err.contains("legacy RSDT address"), "{err}");
    }

    #[test]
    fn rejects_facs_and_dsdt_inside_xsdt() {
        // Place a checksummed FACS-signature table in XSDT.
        let mut bogus_facs = vec![0u8; 64];
        bogus_facs[..4].copy_from_slice(b"FACS");
        Le::write_u32(&mut bogus_facs[4..8], 64);
        fix_checksum(&mut bogus_facs);
        let bytes = assemble(
            BASE,
            &FadtConfig {
                pm1a_evt_blk: 0x400,
                ..FadtConfig::default()
            },
            &[],
            &[],
            &[bogus_facs],
        );
        let err = image(&bytes).tables().unwrap_err();
        assert!(err.contains("FACS must not appear in XSDT"), "{err}");

        // Place a checksummed DSDT-signature table in XSDT.
        let mut bogus_dsdt = vec![0u8; SDT_HEADER];
        bogus_dsdt[..4].copy_from_slice(b"DSDT");
        Le::write_u32(&mut bogus_dsdt[4..8], SDT_HEADER as u32);
        fix_checksum(&mut bogus_dsdt);
        let bytes = assemble(
            BASE,
            &FadtConfig {
                pm1a_evt_blk: 0x400,
                ..FadtConfig::default()
            },
            &[],
            &[],
            &[bogus_dsdt],
        );
        let err = image(&bytes).tables().unwrap_err();
        assert!(err.contains("DSDT must not appear in XSDT"), "{err}");
    }

    #[test]
    fn enforces_facs_presence_for_non_hw_reduced_fadt() {
        let mut bytes = sample_image();
        let tables = image(&bytes).tables().unwrap();
        let fadt_off = tables[1].as_ptr() as usize - bytes.as_ptr() as usize;
        let fadt_len = Le::read_u32(&bytes[fadt_off + 4..fadt_off + 8]) as usize;

        // Zero both FACS pointers on non-HW-reduced FADT -> rejected.
        let fadt = &mut bytes[fadt_off..fadt_off + fadt_len];
        Le::write_u32(&mut fadt[36..40], 0);
        Le::write_u64(&mut fadt[132..140], 0);
        fix_checksum(fadt);
        let err = image(&bytes).tables().unwrap_err();
        assert!(err.contains("missing FIRMWARE_CTRL"), "{err}");

        // Set HW_REDUCED_ACPI -> zero FACS is permitted.
        let fadt = &mut bytes[fadt_off..fadt_off + fadt_len];
        let flags = Le::read_u32(&fadt[112..116]) | FADT_HW_REDUCED_ACPI;
        Le::write_u32(&mut fadt[112..116], flags);
        fix_checksum(fadt);
        let tables = image(&bytes).tables().unwrap();
        assert_eq!(
            tables.iter().map(|t| &t[..4]).collect::<Vec<_>>(),
            [b"XSDT", b"FACP", b"DSDT"]
        );
    }

    #[test]
    fn rejects_duplicate_dsdt_and_overlapping_tables() {
        let bytes = sample_image();
        let tables = image(&bytes).tables().unwrap();
        let xsdt = tables[0].as_ptr() as usize - bytes.as_ptr() as usize;
        let fadt = tables[1].as_ptr() as usize - bytes.as_ptr() as usize;
        let fadt_len = Le::read_u32(&bytes[fadt + 4..fadt + 8]) as usize;

        // Point DSDT at the XSDT address -> rejected as duplicate address.
        {
            let mut dup = bytes.clone();
            let table = &mut dup[fadt..fadt + fadt_len];
            let xsdt_addr = BASE + xsdt as u64;
            Le::write_u32(&mut table[40..44], xsdt_addr as u32);
            Le::write_u64(&mut table[140..148], xsdt_addr);
            fix_checksum(table);
            let err = image(&dup).tables().unwrap_err();
            assert!(err.contains("duplicate ACPI table address"), "{err}");
        }

        let err = check_no_overlaps(vec![
            (BASE, BASE + 0x40, "RSDP".into()),
            (BASE + 0x30, BASE + 0x80, "XSDT".into()),
        ])
        .unwrap_err();
        assert!(err.contains("overlapping ACPI tables"), "{err}");
    }

    #[test]
    fn sanitizes_non_alphanumeric_signatures_and_validates_scenarios() {
        assert_eq!(safe_signature_stem("FACP"), "FACP");
        assert_eq!(safe_signature_stem("ASF!"), "ASF_21");

        assert!(scenario_dir_name("baseline").is_ok());
        assert!(scenario_dir_name("rome").is_ok());
        assert!(scenario_dir_name("../escape").is_err());
        assert!(scenario_dir_name("").is_err());

        let mk = |name: &str| AcpiBaseline {
            scenario: name.into(),
            physical_base: BASE,
            rsdp: BASE,
            bytes: Vec::new(),
        };
        assert!(validate_scenarios(&[]).is_err());
        assert!(validate_scenarios(&[mk("rome"), mk("milan")]).is_ok());
        assert!(
            validate_scenarios(&[mk("rome"), mk("rome")])
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn tool_error_matching_avoids_oem_and_aml_false_positives() {
        assert!(!has_tool_error(
            ToolPhase::IaslCompile,
            "ACPI: DSDT 0x0 00100 (v02 AE_OEM FSTARTFW 00000001 FST0 00000001)\n\
             DSDT.dsl   12: Device (AE_0)\n\
             Warning 3144 - Method Local is set but never used\n\
             Compilation successful. 0 Errors, 1 Warnings, 0 Remarks\n"
        ));
        assert_eq!(
            parse_iasl_warnings("Compilation successful. 0 Errors, 11 Warnings, 5 Remarks\n"),
            11
        );
        assert!(has_tool_error(
            ToolPhase::AcpiExec,
            "ACPI Error: AE_NOT_FOUND, During name lookup\n"
        ));
        assert!(has_tool_error(
            ToolPhase::AcpiExec,
            "Could not load table\n"
        ));
        assert!(has_tool_error(
            ToolPhase::IaslCompile,
            "Error    6126 - syntax error\n"
        ));
    }

    #[test]
    fn rejects_checksum_valid_malformed_dmar_end_to_end() {
        let scratch = tempfile::tempdir().unwrap();
        let dsdt = compile_asl(
            scratch.path(),
            "dsdt_min",
            r#"
DefinitionBlock ("", "DSDT", 2, "FSTART", "FSTARTFW", 1) {
    Scope (\_SB) {
        Name (OKAY, 1)
    }
}
"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let mut dmar = vec![0u8; 72];
        dmar[..4].copy_from_slice(b"DMAR");
        Le::write_u32(&mut dmar[4..8], 72);
        dmar[8] = 1; // revision
        dmar[10..16].copy_from_slice(b"FSTART");
        dmar[16..24].copy_from_slice(b"FSTARTFW");
        dmar[36] = 35; // host address width
        // Subtable at offset 48: invalid DMAR subtable type 0x00FE, length 24.
        Le::write_u16(&mut dmar[48..50], 0x00fe);
        Le::write_u16(&mut dmar[50..52], 24);
        fix_checksum(&mut dmar);

        let bytes = assemble_with_dsdt_and_extra(&dsdt, &[dmar]);
        let baseline = AcpiBaseline {
            scenario: "bad-dmar".into(),
            physical_base: BASE,
            rsdp: BASE,
            bytes,
        };
        let err = validate(dir.path(), &baseline).unwrap_err();
        assert!(err.contains("data table decoder error"), "{err}");
    }

    #[test]
    fn validates_dsdt_ssdt_cross_reference_and_rejects_unresolved_external() {
        let scratch = tempfile::tempdir().unwrap();

        // 1. Valid DSDT + SSDT where SSDT calls \_SB.FOO.VAL() defined in DSDT.
        let dsdt_ok = compile_asl(
            scratch.path(),
            "dsdt_ok",
            r#"
DefinitionBlock ("", "DSDT", 2, "FSTART", "TEST", 1) {
    Scope (\_SB) {
        Device (FOO) {
            Name (_HID, "PNP0C02")
            Method (VAL, 0) { Return (0x42) }
        }
    }
}
"#,
        );
        let ssdt_ok = compile_asl(
            scratch.path(),
            "ssdt_ok",
            r#"
DefinitionBlock ("", "SSDT", 2, "FSTART", "TEST", 1) {
    External (\_SB.FOO.VAL, MethodObj)
    Scope (\_SB) {
        Method (CALL, 0) { Return (\_SB.FOO.VAL ()) }
    }
}
"#,
        );
        let valid_dir = tempfile::tempdir().unwrap();
        let valid_bytes = assemble_with_dsdt_and_extra(&dsdt_ok, &[ssdt_ok]);
        let valid = AcpiBaseline {
            scenario: "valid-ssdt".into(),
            physical_base: BASE,
            rsdp: BASE,
            bytes: valid_bytes,
        };
        validate(valid_dir.path(), &valid).expect("valid DSDT+SSDT must pass");

        // 2. DSDT calling missing \_SB.NOPE() -> must fail as unresolved External.
        let dsdt_bad = compile_asl(
            scratch.path(),
            "dsdt_bad",
            r#"
DefinitionBlock ("", "DSDT", 2, "FSTART", "TEST", 1) {
    External (\_SB.NOPE, MethodObj)
    Scope (\_SB) {
        Method (BAD, 0) { \_SB.NOPE () }
    }
}
"#,
        );
        let bad_dir = tempfile::tempdir().unwrap();
        let bad_bytes = assemble_with_dsdt_and_extra(&dsdt_bad, &[]);
        let bad = AcpiBaseline {
            scenario: "bad-ext".into(),
            physical_base: BASE,
            rsdp: BASE,
            bytes: bad_bytes,
        };
        let err = validate(bad_dir.path(), &bad).unwrap_err();
        assert!(err.contains("unresolved AML External reference"), "{err}");
    }

    #[test]
    fn rejects_iasl_warnings_as_errors() {
        let scratch = tempfile::tempdir().unwrap();
        let dsdt_warn = compile_asl(
            scratch.path(),
            "dsdt_warn",
            r#"
DefinitionBlock ("", "DSDT", 2, "FSTART", "TEST", 1) {
    Scope (\_SB) {
        Method (WARN, 0) {
            Local0 = 1
        }
    }
}
"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let bytes = assemble_with_dsdt_and_extra(&dsdt_warn, &[]);
        let baseline = AcpiBaseline {
            scenario: "warn-dsdt".into(),
            physical_base: BASE,
            rsdp: BASE,
            bytes,
        };
        let err = validate(dir.path(), &baseline).unwrap_err();
        assert!(err.contains("Local0") || err.contains("warning"), "{err}");
    }
}
