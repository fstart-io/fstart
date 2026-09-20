//! Build-script shim for fstart-platform-intel.
//!
//! Forwards build-selected inputs to firmware compilation: the ramstage SMM
//! image and the SMBIOS release date shared by RTC and Type 0 metadata.

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fstart_intel_has_smm_image)");
    println!(
        "cargo:rustc-check-cfg=cfg(fstart_stage_env, values(\"car\", \"postcar\", \"ram\", \"smm\"))"
    );

    println!("cargo:rerun-if-env-changed=FSTART_SMM_IMAGE");
    if let Ok(smm_image) = env::var("FSTART_SMM_IMAGE") {
        println!("cargo:rerun-if-changed={smm_image}");
        println!("cargo:rustc-env=FSTART_SMM_IMAGE={smm_image}");
        println!("cargo:rustc-cfg=fstart_intel_has_smm_image");
    }

    println!("cargo:rerun-if-env-changed=FSTART_SMBIOS_DATE");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    let date = env::var("FSTART_SMBIOS_DATE").unwrap_or_else(|_| current_smbios_date());
    println!("cargo:rustc-env=FSTART_SMBIOS_DATE={date}");
}

fn current_smbios_date() -> String {
    let timestamp = env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        });
    let (year, month, day) = civil_from_days((timestamp / 86_400) as i64);
    format!("{month:02}/{day:02}/{year:04}")
}

// Howard Hinnant's civil-from-days transform, with day zero at 1970-01-01.
fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}
