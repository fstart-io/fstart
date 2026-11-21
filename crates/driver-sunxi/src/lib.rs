//! Allwinner sunxi early-boot drivers.

#![no_std]

pub mod a20_ccu;
pub mod a20_dramc;
pub mod ccu_regs;
pub mod d1_ccu;
pub mod d1_dramc;
pub mod h3_ccu;
pub mod h3_dramc;
pub mod mmc;
pub mod pio;
pub mod spi;
pub mod spi_nor;
