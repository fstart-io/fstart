//! The boot tests of the boards QEMU emulates: the H3 on the orangepi-pc
//! machine, eGON SD boot.

use crate::facts::{SunxiBoardFacts, SunxiSoc};
use crate::messages::{INVALID_MEMORY_POLICY, LINUX_FIRST_STEP, MIB_DRAM};
use fstart_core::QemuMachine;
use fstart_image_build::boot_test::{Asset, BootTest, BootTests, GUEST_BOOTED, RunInput};
extern crate std;
use std::{format, vec, vec::Vec};

/// QEMU's orangepi-pc machine has 1 GiB of DRAM.
const ORANGEPI_PC_DRAM_MIB: u64 = 1024;

impl<B: SunxiBoardFacts> BootTests for super::Plan<B> {
    fn boot_tests() -> Vec<BootTest> {
        let facts = B::FACTS;
        if !matches!(
            (facts.soc, facts.qemu_machine),
            (SunxiSoc::H3, Some(QemuMachine::OrangePiPc))
        ) {
            return Vec::new();
        }
        let mut tests = vec![
            BootTest::new("halt", "halt")
                .expect(format!("{ORANGEPI_PC_DRAM_MIB} {MIB_DRAM}"))
                // The selection must halt rather than look for the
                // absent Linux files.
                .reject(LINUX_FIRST_STEP)
                .reject(INVALID_MEMORY_POLICY),
        ];
        if let Some(dtb) = Asset::device_tree(facts.dtb) {
            tests.push(
                BootTest::new("linux", "linux")
                    .input(RunInput::Kernel(Asset::Armv7Linux))
                    .input(RunInput::PayloadFile(facts.dtb.into(), dtb))
                    .expect(GUEST_BOOTED),
            );
        }
        tests
    }
}
