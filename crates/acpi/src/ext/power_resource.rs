//! AML PowerResource construct.
//!
//! [`PowerResource`] declares a power resource referenced by `_PR0`/`_PR2`
//! packages from devices that can be powered down.
//!
//! Note: implemented here because `acpi_tables` 0.2.x emits the extended
//! opcode prefix *after* the PowerResource opcode (`84 5B ...`), which every
//! AML interpreter rejects; the correct encoding is `5B 84`. The upstream
//! rust-vmm/acpi_tables crate is archived (read-only since 2026-07) and has
//! no PR for this, so the corrected implementation lives here.

extern crate alloc;

use alloc::vec::Vec;

use acpi_tables::aml::Path;
use acpi_tables::{Aml, AmlSink};

/// AML opcode constants.
const EXT_OP_PREFIX: u8 = 0x5B;
const POWER_RESOURCE_OP: u8 = 0x84;

fn create_pkg_length(len: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(4);

    // PkgLength is inclusive and includes the length bytes.
    let length_length = if len < (2usize.pow(6) - 1) {
        1
    } else if len < (2usize.pow(12) - 2) {
        2
    } else if len < (2usize.pow(20) - 3) {
        3
    } else {
        4
    };

    let length = len + length_length;

    match length_length {
        1 => result.push(length as u8),
        2 => {
            result.push((1u8 << 6) | (length & 0xf) as u8);
            result.push((length >> 4) as u8)
        }
        3 => {
            result.push((2u8 << 6) | (length & 0xf) as u8);
            result.push((length >> 4) as u8);
            result.push((length >> 12) as u8);
        }
        _ => {
            result.push((3u8 << 6) | (length & 0xf) as u8);
            result.push((length >> 4) as u8);
            result.push((length >> 12) as u8);
            result.push((length >> 20) as u8);
        }
    }

    result
}

/// AML `PowerResource` object.
///
/// Encoding: `ExtOpPrefix PowerResourceOp PkgLength NameString SystemLevel
/// ResourceOrder ObjectList`.
pub struct PowerResource<'a> {
    name: Path,
    level: u8,
    order: u16,
    children: Vec<&'a dyn Aml>,
}

impl<'a> PowerResource<'a> {
    /// Create a PowerResource with the given name (4-char NameSeg), system
    /// level and resource order.
    pub fn new(name: Path, level: u8, order: u16, children: Vec<&'a dyn Aml>) -> Self {
        Self {
            name,
            level,
            order,
            children,
        }
    }
}

impl Aml for PowerResource<'_> {
    fn to_aml_bytes(&self, sink: &mut dyn AmlSink) {
        let mut body = Vec::new();
        self.name.to_aml_bytes(&mut body);
        body.push(self.level);
        body.extend(self.order.to_le_bytes());
        for child in &self.children {
            child.to_aml_bytes(&mut body);
        }

        let mut out = Vec::new();
        out.push(EXT_OP_PREFIX);
        out.push(POWER_RESOURCE_OP);
        out.extend(create_pkg_length(body.len()));
        out.extend_from_slice(&body);

        sink.vec(&out);
    }
}
