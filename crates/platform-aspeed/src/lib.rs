//! Fixed ASPEED BMC SoC platform flows.
//!
//! The AST2400 and AST2500 ARM cores execute SPI flash in place from the
//! reset vector; [`head`] builds the vector table and hardware strap block
//! that open such an image.

#![no_std]

pub mod head;
