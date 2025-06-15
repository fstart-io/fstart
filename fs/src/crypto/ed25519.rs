use embedded_io_async::Read;
use ed25519_dalek::{VerifyingKey, Signature};
use super::ParseSignature;

impl ParseSignature<Signature> for VerifyingKey {
    async fn try_parse_signature<R: Read>(read: &mut R) -> Option<Signature> {
        let mut buf = [0u8; Signature::BYTE_SIZE];
        read.read_exact(&mut buf).await.ok()?;
        Signature::try_from(buf).ok()
    }
}
