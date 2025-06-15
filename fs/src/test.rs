use hex_lit::hex;
use embassy_futures::block_on;
use sha2::{Sha256, Digest};
use ed25519_dalek::{VerifyingKey as EdVerifyingKey, Signature as EdSignature};
use crate::crypto::{double,
                    ParseSignature,
                    SignatureVerify, HashVerify,
                    VerifiedFullRead};

use self::compat::Read;

type MlSignature = ml_dsa::Signature<ml_dsa::MlDsa44>;
type MlVerifyingKey = ml_dsa::VerifyingKey<ml_dsa::MlDsa44>;

type DoubleSignature = (EdSignature, MlSignature);
type DoubleVerifyingKey = double::VerifyingKey<EdVerifyingKey, MlVerifyingKey>;

const DATA: [u8; 1234] = [0xa5u8; 1234];

#[test]
fn test_sha256_verification() {
    let hash = &hex!(
        "cb0bbb4bc2d3be60bbde9dde593dc69537b447292f4a71e555eac1c68004a272")
        .into();

    let input = Read::from(&DATA[..]);
    let verify = HashVerify::new(Sha256::new(), hash);

    let mut dest = vec![0u8; DATA.len()];
    let read = VerifiedFullRead::new(&mut dest, input, verify);

    block_on(read.read_and_verify()).expect("hash verification failed");
    assert_eq!(dest, DATA);
}

#[test]
fn test_ed25519_verification() {
    let (pubkey, sig) = read_ed25519("dtfs.dtb".into());
    test_dtfs(&pubkey, &sig);
}

#[test]
fn test_double_verification() {
    let (pubkey, sig) = read_double("dtfs.dtb".into());
    test_dtfs(&pubkey, &sig);
}

#[test]
fn test_ed25519_ml_dsa44_verification() {
    let (ed_pub, ed_sig) = read_ed25519("dtfs.dtb".into());
    let (ml_pub, ml_sig) = read_ml_dsa44("dtfs.dtb".into());
    test_dtfs(&double::VerifyingKey(ed_pub, ml_pub), &(ed_sig, ml_sig));
}

fn test_dtfs<V, S>(pubkey: &V, sig: &S)
where
    V: signature::Verifier<S>,
{
    let dtb = read("dtfs.dtb");
    let input = Read::from(dtb.clone());

    let verify = SignatureVerify::new(pubkey, sig);

    let mut dest = vec![0u8; dtb.len()];
    let read = VerifiedFullRead::new(&mut dest, input, verify);

    block_on(read.read_and_verify()).expect("signature verification failed");
    assert_eq!(dest, dtb);
}

fn read_double(stem: String) -> (DoubleVerifyingKey, DoubleSignature) {
    let pubkey = double::VerifyingKey(read_ed_pub(), read_ml_pub());
    let mut sig = Read::from(read(&(stem + ".ed25519+ml_dsa44.sig")));
    let sig = block_on(DoubleVerifyingKey::try_parse_signature(&mut sig))
                                            .expect("signature parsing error");
    (pubkey, sig)
}

fn read_ed25519(stem: String) -> (EdVerifyingKey, EdSignature) {
    let mut sig = Read::from(read(&(stem + ".ed25519.sig")));
    let sig = block_on(EdVerifyingKey::try_parse_signature(&mut sig))
                                        .expect("signature parsing error");
    (read_ed_pub(), sig)
}

fn read_ed_pub() -> EdVerifyingKey {
    let pubkey = read("ed25519.pub").try_into().unwrap();
    EdVerifyingKey::from_bytes(&pubkey).unwrap()
}

fn read_ml_dsa44(stem: String) -> (MlVerifyingKey, MlSignature) {
    let mut sig = Read::from(read(&(stem + ".ml_dsa44.sig")));
    let sig = block_on(MlVerifyingKey::try_parse_signature(&mut sig))
                                        .expect("signature parsing error");
    (read_ml_pub(), sig)
}

fn read_ml_pub() -> MlVerifyingKey {
    let pubkey = read("ml_dsa44.pub").as_slice().try_into().unwrap();
    MlVerifyingKey::decode(&pubkey)
}

fn read(file: &str) -> Vec<u8> {
    use std::io::Read as _;

    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("test-data");
    path.push(file);

    let mut v = Vec::new();
    let mut f = std::fs::File::open(path).unwrap();
    f.read_to_end(&mut v).unwrap();
    v
}

mod compat {
    use embedded_io_async::{Read as EIORead, ErrorType};
    use std::collections::VecDeque;
    use std::io::Read as StdRead;
    use crate::{Error, Result};

    pub struct Read<R>(R);

    impl From<&[u8]> for Read<VecDeque<u8>> {
        fn from(value: &[u8]) -> Self { Read(VecDeque::from(Vec::from(value))) }
    }

    impl From<Vec<u8>> for Read<VecDeque<u8>> {
        fn from(value: Vec<u8>) -> Self { Read(VecDeque::from(value)) }
    }

    impl<R: StdRead> EIORead for Read<R> {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            let len = usize::min(buf.len(), 7);
            self.0.read(&mut buf[..len]).map_err(|_| Error::Other)
        }
    }

    impl<R> ErrorType for Read<R> {
        type Error = Error;
    }
}
