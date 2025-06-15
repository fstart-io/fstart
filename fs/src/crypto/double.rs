use embedded_io_async::Read;
use signature::{Error, Result, Signer, Verifier};
use super::ParseSignature;

pub struct VerifyingKey<V1, V2>(pub V1, pub V2);

impl<V1, S1, V2, S2> Verifier<(S1, S2)> for VerifyingKey<V1, V2>
where
    V1: Verifier<S1>,
    V2: Verifier<S2>,
{
    fn verify(&self, msg: &[u8], signature: &(S1, S2)) -> Result<()> {
        let r1 = self.0.verify(msg, &signature.0);
        let r2 = self.1.verify(msg, &signature.1);
        if r1.is_err() || r2.is_err() {
            Err(Error::new())
        } else {
            Ok(())
        }
    }
}

impl<V1, V2, S1, S2> ParseSignature<(S1, S2)> for VerifyingKey<V1, V2>
where
    V1: ParseSignature<S1>,
    V2: ParseSignature<S2>,
{
    async fn try_parse_signature<R: Read>(read: &mut R) -> Option<(S1, S2)> {
        Some((V1::try_parse_signature(read).await?,
              V2::try_parse_signature(read).await?))
    }
}

pub struct SigningKey<S1, S2>(pub S1, pub S2);

impl<S1, Sig1, S2, Sig2> Signer<(Sig1, Sig2)> for SigningKey<S1, S2>
where
    S1: Signer<Sig1>,
    S2: Signer<Sig2>,
{
    fn try_sign(&self, msg: &[u8]) -> Result<(Sig1, Sig2)> {
        Ok((self.0.try_sign(msg)?, self.1.try_sign(msg)?))
    }
}
