use embedded_io_async::Read;
use ml_dsa::{VerifyingKey, MlDsaParams, Signature, EncodedSignature};
use super::ParseSignature;

impl<P: MlDsaParams> ParseSignature<Signature<P>> for VerifyingKey<P> {
    async fn try_parse_signature<R: Read>(read: &mut R) -> Option<Signature<P>> {
        let mut encoded = EncodedSignature::<P>::default();
        read.read_exact(&mut encoded).await.ok()?;
        Signature::decode(&encoded)
    }
}
