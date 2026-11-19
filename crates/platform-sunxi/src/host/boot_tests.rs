//! The boot tests of the H3 boards QEMU's orangepi-pc machine runs, eGON SD
//! boot: what they print, from the lines the stages log (`crate::messages`).

use crate::facts::BoardFacts;
use crate::messages::{INVALID_MEMORY_POLICY, LINUX_FIRST_STEP, MIB_DRAM};
use fstart_image_build::{
    boot_test::{Asset, BootTest, GUEST_BOOTED},
    build_plan::FfsAssembly,
};
extern crate std;
use std::{format, vec, vec::Vec};

/// The DRAM of QEMU's orangepi-pc machine, which the run recipe asks for.
pub(super) const ORANGEPI_PC_DRAM_MIB: u64 = 1024;

pub(super) fn orangepi_pc(facts: &BoardFacts) -> Vec<BootTest> {
    let mut tests = vec![
        BootTest::new("halt", "halt")
            .expect(format!("{ORANGEPI_PC_DRAM_MIB} {MIB_DRAM}"))
            // The selection must halt rather than look for the absent Linux
            // files.
            .reject(LINUX_FIRST_STEP)
            .reject(INVALID_MEMORY_POLICY),
    ];
    if let Some(dtb) = Asset::device_tree(facts.dtb) {
        tests.push(
            BootTest::new("linux", "linux")
                .input(FfsAssembly::KERNEL, Asset::Armv7Linux)
                .input(FfsAssembly::DTB, dtb)
                .expect(GUEST_BOOTED),
        );
    }
    tests
}
