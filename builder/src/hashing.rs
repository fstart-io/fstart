use digest::Digest as _;
use sha2::Sha512;
use sha3::Sha3_256;

use fstart_fs::{config, crypto::{AssociatedAlgo as _, double}};
use crate::dtfs::DtfsDigest;

pub fn hash(msg: &[u8]) -> Vec<DtfsDigest> {
    config::Digest::new().chain_update(msg).into_dtfs()
}

trait IntoDtfs {
    fn into_dtfs(self) -> Vec<DtfsDigest>;
}

#[allow(dead_code)]
impl<D1, D2> IntoDtfs for double::Digest<D1, D2>
where
    D1: IntoDtfs,
    D2: IntoDtfs,
{
    fn into_dtfs(self) -> Vec<DtfsDigest> {
        let mut vec = self.0.into_dtfs();
        vec.append(&mut self.1.into_dtfs());
        vec
    }
}

#[allow(dead_code)]
impl IntoDtfs for Sha512 {
    fn into_dtfs(self) -> Vec<DtfsDigest> {
        let digest = Vec::from(self.finalize().as_slice());
        vec![DtfsDigest { algo: Self::algo(), digest }]
    }
}

#[allow(dead_code)]
impl IntoDtfs for Sha3_256 {
    fn into_dtfs(self) -> Vec<DtfsDigest> {
        let digest = Vec::from(self.finalize().as_slice());
        vec![DtfsDigest { algo: Self::algo(), digest }]
    }
}
