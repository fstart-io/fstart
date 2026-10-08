//! Execute the emitted brightness methods using ACPICA's simulated regions.
//! This validates AML units/fallbacks, not physical panel or interrupt delivery.
extern crate std;

use super::{IntelGm965, IntelGm965Config};
use crate::IntelNorthbridgeDriver;
use fstart_acpi::device::AcpiDevice;
use fstart_acpi_macros::acpi_dsl;
use std::{fs, process::Command};

#[test]
fn brightness_requests_scale_mailbox_and_pwm_and_report_live_state() {
    static CONFIG: IntelGm965Config = IntelGm965Config::new();
    let mut north = IntelGm965::new_from_config(&CONFIG).unwrap();
    assert!(north.supports_s3_replay());
    north.set_s3_enabled(true);
    let mut aml = north.dsdt_aml(&CONFIG);
    aml.extend_from_slice(&acpi_dsl! {
        Scope("\\_SB_.PCI0.GFX0.LCD0") {
            Method("TST0", 0, Serialized) {
                // Missing OpRegion must still allow legacy PWM control.
                ASLS = 0u32;
                BCLM = 1552u32;
                BCLV = 1552u32;
                _BCM(50u32);
                If (BCLV != 776u32) { Return(1u32); }
                If (BCLM != 1552u32) { Return(2u32); }
                Local0 = _BQC();
                If (Local0 != 50u32) { Return(3u32); }
                _BCM(100u32);
                If (BCLV != 1552u32) { Return(4u32); }
                _BCM(0u32);
                If (BCLV != 0u32) { Return(5u32); }
                // No mailbox support is another immediate fallback.
                ASLS = 0x100000u32;
                MBOX = 0u32;
                _BCM(25u32);
                If (BCLV != 388u32) { Return(6u32); }
                // Driver not ready: publish the 0..255 request then fall back.
                MBOX = 4u32;
                ARDY = 0u32;
                _BCM(50u32);
                If (BCLP != 0x8000007fu32) { Return(7u32); }
                If (BCLV != 776u32) { Return(8u32); }
                // An unacknowledged interrupt takes the bounded timeout path.
                ARDY = 1u32;
                _BCM(75u32);
                If (BCLP != 0x800000bfu32) { Return(9u32); }
                If (ASLE != 1u32) { Return(10u32); }
                If (BCLV != 1164u32) { Return(11u32); }
                // Query reads hardware, rather than returning a stale cache.
                BCLV = 388u32;
                Local0 = _BQC();
                If (Local0 != 25u32) { Return(12u32); }
                ARDY = 0u32;
                _BCM(200u32);
                If (BCLP != 0x800000ffu32) { Return(13u32); }
                If (BCLV != 1552u32) { Return(14u32); }
                // No period: avoid division by zero and preserve hardware.
                BCLM = 0u32;
                BCLV = 77u32;
                _BCM(40u32);
                If (BCLV != 77u32) { Return(15u32); }
                Local0 = _BQC();
                If (Local0 != 40u32) { Return(16u32); }
                // Hotkeys must saturate even for non-step cached levels.
                _BCM(5u32);
                DECB();
                Local0 = _BQC();
                If (Local0 != 0u32) { Return(17u32); }
                _BCM(95u32);
                INCB();
                Local0 = _BQC();
                If (Local0 != 100u32) { Return(18u32); }
                Return(0u32);
            }
        }
    });
    let dir = std::env::temp_dir().join(std::format!(
        "fstart-gm965-brightness-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("dsdt.aml"),
        fstart_acpi::platform::build_dsdt(&aml),
    )
    .unwrap();
    let result = Command::new("acpiexec")
        .current_dir(&dir)
        .args([
            "-dt",
            "-fv",
            "0",
            "-b",
            "execute \\_S3;execute \\_SB.PCI0.GFX0.LCD0.TST0",
            "dsdt.aml",
        ])
        .output()
        .unwrap();
    let text = std::format!(
        "{}{}",
        std::string::String::from_utf8_lossy(&result.stdout),
        std::string::String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.status.success() && !text.contains("AE_") && !text.contains("ACPI Error"),
        "{text}"
    );
    assert!(
        text.contains("[Package] Contains 4 Elements")
            && text.contains("[Integer] = 0000000000000005"),
        "S3 missing: {text}"
    );
    assert!(
        text.rsplit("[Integer] = ")
            .next()
            .is_some_and(|value| value.starts_with("0000000000000000")),
        "brightness test failed: {text}"
    );
    fs::remove_dir_all(dir).unwrap();
}
