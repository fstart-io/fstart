//! Host ACPI baseline transport, shared by platform adapters and `fbuild check-acpi`.
//!
//! The generated runner prints a single JSON [`AcpiEnvelope`] line to stdout;
//! any diagnostics emitted while building baselines must go to stderr.

use serde::{Deserialize, Serialize};

/// Version of the transport between `fbuild`'s generated host ACPI runner and
/// a platform's [`HostAcpi`] adapter.
pub const ACPI_SCHEMA: u32 = 1;

/// One assembled ACPI table image produced by a platform's production builders
/// from deterministic baseline hardware facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpiBaseline {
    /// Short scenario label (e.g. `"baseline"`, `"rome"`, `"milan"`), used as
    /// the artifact subdirectory under `target/fstart-acpi/<board>/`.
    pub scenario: String,
    /// Physical base address the image was assembled at.
    pub physical_base: u64,
    /// Physical address of the ACPI 2+ RSDP inside the image.
    pub rsdp: u64,
    /// Raw contiguous table image bytes starting at `physical_base`.
    #[serde(with = "hex_bytes")]
    pub bytes: Vec<u8>,
}

/// Outcome returned by a platform's [`HostAcpi`] adapter for a board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpiOutcome {
    /// This board or platform does not generate firmware ACPI tables (for
    /// example device-tree-only boards or QEMU `fw_cfg` machines).
    NotApplicable { reason: String },
    /// One or more assembled baseline images to validate with ACPICA.
    Images { images: Vec<AcpiBaseline> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
enum WireOutcome {
    NotApplicable { reason: String },
    Images { images: Vec<AcpiBaseline> },
    Error { message: String },
}

/// Structured stdout envelope emitted by the host ACPI runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpiEnvelope {
    pub schema: u32,
    #[serde(flatten)]
    outcome: WireOutcome,
}

impl AcpiEnvelope {
    pub fn into_outcome(self) -> Result<AcpiOutcome, String> {
        if self.schema != ACPI_SCHEMA {
            return Err(format!(
                "platform ACPI schema {} does not match fbuild's {ACPI_SCHEMA}; rebuild the host runner",
                self.schema
            ));
        }
        match self.outcome {
            WireOutcome::NotApplicable { reason } => Ok(AcpiOutcome::NotApplicable { reason }),
            WireOutcome::Images { images } => Ok(AcpiOutcome::Images { images }),
            WireOutcome::Error { message } => Err(message),
        }
    }
}

/// Serialize `build(base)` as one [`AcpiEnvelope`] JSON document.
pub fn emit_envelope(base: u64, build: impl FnOnce(u64) -> Result<AcpiOutcome, String>) -> String {
    let outcome = match build(base) {
        Ok(AcpiOutcome::NotApplicable { reason }) => WireOutcome::NotApplicable { reason },
        Ok(AcpiOutcome::Images { images }) => WireOutcome::Images { images },
        Err(message) => WireOutcome::Error { message },
    };
    let envelope = AcpiEnvelope {
        schema: ACPI_SCHEMA,
        outcome,
    };
    serde_json::to_string(&envelope).unwrap_or_else(|e| {
        serde_json::json!({
            "schema": ACPI_SCHEMA,
            "outcome": "error",
            "message": format!("serialize ACPI baseline: {e}"),
        })
        .to_string()
    })
}

/// The contract `fbuild`'s host ACPI runner calls: every platform's `Plan<B>`
/// implements this for its board type `B`. Non-ACPI platforms return
/// [`AcpiOutcome::NotApplicable`]; ACPI platforms run their production table
/// builders with deterministic baseline hardware facts.
pub trait HostAcpi<B> {
    fn acpi_baselines(base: u64) -> Result<AcpiOutcome, String>;

    fn emit_acpi(base: u64) {
        std::println!("{}", emit_envelope(base, Self::acpi_baselines));
    }
}

mod hex_bytes {
    use serde::{Deserializer, Serializer, de};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        serializer.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s: String = serde::Deserialize::deserialize(deserializer)?;
        let raw = s.as_bytes();
        let (pairs, remainder) = raw.as_chunks::<2>();
        if !remainder.is_empty() {
            return Err(de::Error::custom("odd hex length"));
        }
        let nibble = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        };
        let mut out = Vec::with_capacity(pairs.len());
        for &[hi_ch, lo_ch] in pairs {
            let hi = nibble(hi_ch).ok_or_else(|| de::Error::custom("invalid hex digit"))?;
            let lo = nibble(lo_ch).ok_or_else(|| de::Error::custom("invalid hex digit"))?;
            out.push((hi << 4) | lo);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips_images_not_applicable_and_errors() {
        let json = emit_envelope(0x7f00_0000, |base| {
            Ok(AcpiOutcome::Images {
                images: vec![AcpiBaseline {
                    scenario: "baseline".into(),
                    physical_base: base,
                    rsdp: base,
                    bytes: vec![0x00, 0xab, 0xff],
                }],
            })
        });
        let outcome = serde_json::from_str::<AcpiEnvelope>(&json)
            .unwrap()
            .into_outcome()
            .unwrap();
        assert_eq!(
            outcome,
            AcpiOutcome::Images {
                images: vec![AcpiBaseline {
                    scenario: "baseline".into(),
                    physical_base: 0x7f00_0000,
                    rsdp: 0x7f00_0000,
                    bytes: vec![0x00, 0xab, 0xff],
                }],
            }
        );

        let na_json = emit_envelope(0, |_| {
            Ok(AcpiOutcome::NotApplicable {
                reason: "device-tree platform".into(),
            })
        });
        assert_eq!(
            serde_json::from_str::<AcpiEnvelope>(&na_json)
                .unwrap()
                .into_outcome()
                .unwrap(),
            AcpiOutcome::NotApplicable {
                reason: "device-tree platform".into(),
            }
        );

        let err_json = emit_envelope(0, |_| Err("bad config".into()));
        assert_eq!(
            serde_json::from_str::<AcpiEnvelope>(&err_json)
                .unwrap()
                .into_outcome()
                .unwrap_err(),
            "bad config"
        );

        let stale = AcpiEnvelope {
            schema: ACPI_SCHEMA + 1,
            outcome: WireOutcome::NotApplicable {
                reason: "old".into(),
            },
        };
        assert!(stale.into_outcome().unwrap_err().contains("does not match"));
    }
}
