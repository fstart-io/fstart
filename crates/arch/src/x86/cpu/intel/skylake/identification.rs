// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native client CPU identity shared by core initialization and the DDR4 PHY.

use crate::x86::mp::{CpuIdMatch, CpuVendor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Skylake,
    KabyLake,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Package {
    UltUlx,
    DesktopHalo,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stepping {
    SklB0,
    SklC0,
    SklD0,
    SklP0,
    SklQ0,
    SklM0,
    SklR0,
    KblG0,
    KblA0,
    KblH0,
    KblY0,
    KblB0,
    KblN0,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identification {
    pub family: Family,
    pub package: Package,
    pub stepping: Stepping,
    pub signature: u32,
}

use Family::{KabyLake, Skylake};
use Package::{DesktopHalo, UltUlx};
use Stepping::*;

const SUPPORTED: [Identification; 13] = [
    Identification {
        signature: 0x406e1,
        family: Skylake,
        package: UltUlx,
        stepping: SklB0,
    },
    Identification {
        signature: 0x406e2,
        family: Skylake,
        package: UltUlx,
        stepping: SklC0,
    },
    Identification {
        signature: 0x406e3,
        family: Skylake,
        package: UltUlx,
        stepping: SklD0,
    },
    Identification {
        signature: 0x406e8,
        family: KabyLake,
        package: UltUlx,
        stepping: KblG0,
    },
    Identification {
        signature: 0x506e0,
        family: Skylake,
        package: DesktopHalo,
        stepping: SklP0,
    },
    Identification {
        signature: 0x506e1,
        family: Skylake,
        package: DesktopHalo,
        stepping: SklQ0,
    },
    Identification {
        signature: 0x506e2,
        family: Skylake,
        package: DesktopHalo,
        stepping: SklM0,
    },
    Identification {
        signature: 0x506e3,
        family: Skylake,
        package: DesktopHalo,
        stepping: SklR0,
    },
    Identification {
        signature: 0x506e8,
        family: KabyLake,
        package: DesktopHalo,
        stepping: KblA0,
    },
    Identification {
        signature: 0x806e9,
        family: KabyLake,
        package: UltUlx,
        stepping: KblH0,
    },
    Identification {
        signature: 0x806ea,
        family: KabyLake,
        package: UltUlx,
        stepping: KblY0,
    },
    Identification {
        signature: 0x906e9,
        family: KabyLake,
        package: DesktopHalo,
        stepping: KblB0,
    },
    Identification {
        signature: 0x906ea,
        family: KabyLake,
        package: DesktopHalo,
        stepping: KblN0,
    },
];
const SIGNATURE_MASK: u32 = 0x0fff_0fff;

pub fn identify(signature: u32) -> Option<Identification> {
    SUPPORTED
        .iter()
        .find(|entry| entry.signature == signature & SIGNATURE_MASK)
        .map(|entry| Identification {
            signature,
            ..*entry
        })
}

const fn matches() -> [CpuIdMatch; SUPPORTED.len()] {
    let mut result = [CpuIdMatch {
        vendor: CpuVendor::Intel,
        signature: 0,
        mask: SIGNATURE_MASK,
    }; SUPPORTED.len()];
    let mut index = 0;
    while index < SUPPORTED.len() {
        result[index].signature = SUPPORTED[index].signature;
        index += 1;
    }
    result
}
pub static CPU_IDS: &[CpuIdMatch] = &matches();
