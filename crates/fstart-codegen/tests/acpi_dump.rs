//! ACPI DSDT dry-run: generate AML from the foxconn-d41s board config
//! and write it to a file that can be disassembled with `iasl -d`.
//!
//! Run with:
//!   cargo test --package fstart-codegen --test acpi_dump -- --nocapture
//!
//! Then disassemble:
//!   nix-shell -p acpica-tools --run "iasl -d /tmp/fstart-dsdt.aml"
//!   cat /tmp/fstart-dsdt.dsl

use fstart_acpi::device::AcpiDevice;
use fstart_driver_intel_ich7::{IntelIch7, IntelIch7Config};
use fstart_driver_intel_pineview::{IntelPineview, IntelPineviewConfig};
use fstart_services::device::Device;

/// Build a minimal DSDT AML table wrapping the given body bytes.
///
/// Produces a valid ACPI table with signature "DSDT", proper length,
/// and correct checksum — enough for `iasl -d` to disassemble.
fn wrap_in_dsdt(body: &[u8]) -> Vec<u8> {
    let total_len = (36 + body.len()) as u32;
    let mut buf = Vec::with_capacity(total_len as usize);

    // SDT header (36 bytes)
    buf.extend_from_slice(b"DSDT"); // Signature
    buf.extend_from_slice(&total_len.to_le_bytes()); // Length
    buf.push(2); // Revision
    buf.push(0); // Checksum (patched below)
    buf.extend_from_slice(b"FSTART"); // OEM ID (6 bytes)
    buf.extend_from_slice(b"D41SACPI"); // OEM Table ID (8 bytes)
    buf.extend_from_slice(&1u32.to_le_bytes()); // OEM Revision
    buf.extend_from_slice(b"FSTA"); // Creator ID
    buf.extend_from_slice(&1u32.to_le_bytes()); // Creator Revision

    // Body
    buf.extend_from_slice(body);

    // Fix checksum (byte 9): entire table must sum to 0.
    let sum: u8 = buf.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    buf[9] = sum.wrapping_neg();

    buf
}

#[test]
fn dump_foxconn_d41s_dsdt() {
    // Parse configs directly — we know the exact structure from the board RON.
    let ich7_ron = r#"(
        rcba: 0xFED1C000,
        pirq_routing: (0x0A, 0x0A, 0x0A, 0x0A, 0x80, 0x80, 0x80, 0x80),
        gpe0_en: 0x00000400,
        lpc_decode: (0, 0, 0, 0),
        hda: Some((verbs: [(
            vendor_id: 0x10ec0662,
            subsystem_id: 0x105b0d55,
            pins: [
                ( nid: 0x14, device: LineOut, conn: Jack, loc: External, geo: Rear,
                  connector: StereoMono18, color: Green, misc: 0xC, group: 1, seq: 0 ),
                ( nid: 0x15, nc: Some(0) ),
            ],
        )])),
        sata: Some(( mode: Ahci, ports: 0x3 )),
        usb: Some(( ehci: true, uhci: (true, true, true, true) )),
        pata: false,
        ecam_base: 0xE0000000,
        smbus_base: 0x0400,
        gpio: (pins: [
            ( pin: 0 ),
            ( pin: 6 ),
        ]),
        acpi_name: Some("LPCB"),
        c3_latency: 85,
        power_on_after_fail: 0,
    )"#;

    let pv_ron = r#"(
        mchbar: 0xFED10000,
        dmibar: 0xFED18000,
        epbar: 0xFED19000,
        ecam_base: 0xE0000000,
        acpi_name: Some("MCHC"),
    )"#;

    let ich7_cfg: IntelIch7Config = ron::from_str(ich7_ron).expect("ICH7 config parse");
    let pv_cfg: IntelPineviewConfig = ron::from_str(pv_ron).expect("PV config parse");

    let ich7 = IntelIch7::new(&ich7_cfg).expect("ICH7 new");
    let pineview = IntelPineview::new(&pv_cfg).expect("PV new");

    let pv_aml = pineview.dsdt_aml(&pv_cfg);
    let ich7_aml = ich7.dsdt_aml(&ich7_cfg);

    // Concatenate all AML fragments.
    let mut body = Vec::new();
    body.extend_from_slice(&pv_aml);
    body.extend_from_slice(&ich7_aml);

    let dsdt = wrap_in_dsdt(&body);

    // Write binary AML.
    let out_path = "/tmp/fstart-dsdt.aml";
    std::fs::write(out_path, &dsdt).unwrap();

    println!();
    println!("=== fstart DSDT dump ===");
    println!("  Pineview AML: {} bytes", pv_aml.len());
    println!("  ICH7 AML:     {} bytes", ich7_aml.len());
    println!("  Total body:   {} bytes", body.len());
    println!("  DSDT table:   {} bytes", dsdt.len());
    println!("  Written to:   {out_path}");
    println!();
    println!("Disassemble with:");
    println!("  nix-shell -p acpica-tools --run \"iasl -d {out_path}\"");
    println!("  cat /tmp/fstart-dsdt.dsl");

    // Copy coreboot reference DSDT if available (from a prior coreboot D41S build).
    let cb_asl = std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
        .join("src/coreboot/build/dsdt.asl");
    let cb_dsl = std::path::Path::new(&std::env::var("HOME").unwrap_or_default())
        .join("src/coreboot/build/dsdt.dsl");
    if cb_asl.exists() {
        std::fs::copy(&cb_asl, "/tmp/coreboot-d41s-dsdt.asl").ok();
        println!("  Coreboot reference ASL: /tmp/coreboot-d41s-dsdt.asl");
    }
    if cb_dsl.exists() {
        std::fs::copy(&cb_dsl, "/tmp/coreboot-d41s-dsdt.dsl").ok();
        println!("  Coreboot reference DSL: /tmp/coreboot-d41s-dsdt.dsl");
    }
    println!();
    println!("Compare with:");
    println!("  diff -u /tmp/coreboot-d41s-dsdt.dsl /tmp/fstart-dsdt.dsl | head -100");

    // Verify checksum.
    let sum: u8 = dsdt.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    assert_eq!(sum, 0, "DSDT checksum failed");

    // Verify it's large enough to contain real content.
    assert!(dsdt.len() > 500, "DSDT too small: {} bytes", dsdt.len());
}
