use embassy_futures::block_on;
use crate::crypto::{ParseSignature,
                    SignatureVerify,
                    VerifiedFullRead};

use self::compat::Read;

#[test]
fn test_ed25519_verification() {
    use ed25519_dalek::VerifyingKey as EdVerifyingKey;

    let pubkey = read("ed25519.pub").try_into().unwrap();
    let pubkey = EdVerifyingKey::from_bytes(&pubkey).unwrap();
    let mut sig = Read::from(read("dtfs.dtb.ed25519.sig"));
    let sig = block_on(EdVerifyingKey::try_parse_signature(&mut sig))
        .expect("signature parsing error");

    let dtb = read("dtfs.dtb");
    let input = Read::from(dtb.clone());

    let verify = SignatureVerify::new(&pubkey, &sig);

    let mut dest = vec![0u8; dtb.len()];
    let read = VerifiedFullRead::new(&mut dest, input, verify);

    block_on(read.read_and_verify()).expect("signature verification failed");
    assert_eq!(dest, dtb);
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
