//! Position-independent SMM images: a linked handler with one entry stub
//! per CPU. The options are plan data, also where the image is not built
//! (a bundle assembled in a browser); the builder needs the host's
//! toolchain.

#[cfg(not(target_arch = "wasm32"))]
mod build;
#[cfg(not(target_arch = "wasm32"))]
pub use build::*;

#[derive(Debug, Clone, Copy)]
pub struct ImageOptions {
    pub entry_count: u16,
    pub stack_size: u32,
    pub coreboot_module_args: bool,
    pub coreboot_header: bool,
}
