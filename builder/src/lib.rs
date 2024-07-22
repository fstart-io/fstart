/*++

Licensed under the Apache-2.0 license.

File Name:

lib.rs

Abstract:

File contains exports for fstart Library.

--*/

use std::io::Error;
use std::process::Command;

fn dtb_from_dts(dts_path: &str) -> Result<Vec<u8>, Error> {
    let output = Command::new("dtc")
        .args(["-I", "dts"])
        .arg(dts_path)
        .args(["-O", "dtb"])
        .arg("-Wno-unit_address_vs_reg")
        .output()?;

    if !output.status.success() {
        let msg = format!("dtc failed: {:?}", String::from_utf8(output.stderr));
        return Err(Error::new(std::io::ErrorKind::InvalidInput, msg));
    }
    Ok(output.stdout)
}

#[test]
fn build_image_with_1_raw_bin() {
    let dts_path = concat!(env!("CARGO_MANIFEST_DIR"), "/test-data/raw_bin_test.dts");

    let dtb = dtb_from_dts(dts_path).unwrap();
    let _parsed_fdt = fdt::Fdt::new(dtb.as_slice()).unwrap();
}

#[test]
fn build_image_with_1_raw_bin_fail() {
    let dts_path = concat!(env!("CARGO_MANIFEST_DIR"), "/FILE_DOES_NOT_EXIST");

    let result = dtb_from_dts(dts_path);
    assert!(result.is_err());
}
