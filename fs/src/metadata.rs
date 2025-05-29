use zerocopy::{AsBytes, FromBytes, FromZeroes};

#[derive(PartialEq, PartialOrd, Default, Eq, Debug, Clone)]
pub struct FlashAddress(pub u32);
#[derive(PartialEq, PartialOrd, Default, Eq, Debug, Clone)]
pub struct MappedAddress(pub u64);

#[derive(PartialEq, PartialOrd, Default, Eq, Debug, Clone)]
pub struct MemoryMap {
    pub flash_address: FlashAddress,
    pub mapped_address: MappedAddress,
    pub size: u32,
}

impl MemoryMap {
    pub fn is_mapped(&self, base: MappedAddress, size: u32) -> bool {
        let begin = base.0;
        let end = base.0 + u64::from(size);

        if begin < self.mapped_address.0 {
            return false;
        }
        if end > self.mapped_address.0 + u64::from(self.size) {
            return false;
        }
        true
    }
}

#[derive(PartialEq, PartialOrd, Eq, Debug, Clone)]
pub enum BoardCategory {
    Client,
    Embedded,
    Server,
    Other,
}

impl BoardCategory {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Embedded => "embedded",
            Self::Server => "server",
            Self::Other => "other",
        }
    }
}

#[derive(PartialEq, PartialOrd, Eq, Debug, Clone)]
pub enum MediumType {
    SpiFlash,
    Mmc,
    Other,
}

impl MediumType {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SpiFlash => "spi-flash",
            Self::Mmc => "mmc",
            Self::Other => "other",
        }
    }
}

#[derive(PartialEq, PartialOrd, Eq, Debug, Clone)]
pub enum HashAlgo {
    //    Sha256,
    //    Sha384,
    Sha512,
    // TODO are these good targets??
    //    SlhDsaShake128s,
    //    SlhDsaShake196s,
    SlhDsaShake256s,
}

impl HashAlgo {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Sha512 => "sha512",
            Self::SlhDsaShake256s => "slh_dsa_shake_256s",
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CompressionAlgo {
    Lz4,
    Lzma,
    Zstd,
}

impl CompressionAlgo {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Lz4 => "lz4",
            Self::Lzma => "lzma",
            Self::Zstd => "zstd",
        }
    }
}

#[derive(Default, AsBytes, FromBytes, FromZeroes)]
#[repr(C)]
pub struct DtfsHeader {
    magic: [u8; 16],
    dtfs_offset: u32,
    signatures_offset: u32,
    _reserved: [u8; 8],
}

impl DtfsHeader {
    pub const DTFS_MAGIC: &'static [u8; 16] = b"FSTART_DTFS\0\0\0\0\0";

    pub fn new(signatures_offset: u32) -> Self {
        let dtfs_offset = size_of::<DtfsHeader>().try_into().unwrap();
        assert!(dtfs_offset < signatures_offset);

        Self {
            magic:              *Self::DTFS_MAGIC,
            dtfs_offset:        dtfs_offset,
            signatures_offset:  signatures_offset,
            ..Default::default()
        }
    }
}
