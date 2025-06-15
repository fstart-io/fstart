use core::ops::Add;
use embedded_io_async::Read;
use generic_array::ArrayLength;
use digest::{Update, HashMarker, OutputSizeUser,
             Output, FixedOutput, Reset, FixedOutputReset};
use signature::{Error, Result, Signer, Verifier};
use super::ParseSignature;

#[derive(Default)]
pub struct Digest<D1, D2>(pub D1, pub D2);

impl<D1: FixedOutput, D2: FixedOutput> FixedOutput for Digest<D1, D2>
where
    D1::OutputSize: Add<D2::OutputSize>,
    <D1::OutputSize as Add<D2::OutputSize>>::Output: ArrayLength<u8>,
{
    fn finalize_into(self, out: &mut Output<Self>) {
        let (d1, d2) = out.split_at_mut(D1::output_size());
        self.0.finalize_into(Output::<D1>::from_mut_slice(d1));
        self.1.finalize_into(Output::<D2>::from_mut_slice(d2));
    }
}

impl<D1, D2> FixedOutputReset for Digest<D1, D2>
where
    D1: FixedOutputReset,
    D2: FixedOutputReset,
    D1::OutputSize: Add<D2::OutputSize>,
    <D1::OutputSize as Add<D2::OutputSize>>::Output: ArrayLength<u8>,
{
    fn finalize_into_reset(&mut self, out: &mut Output<Self>) {
        let (d1, d2) = out.split_at_mut(D1::output_size());
        self.0.finalize_into_reset(Output::<D1>::from_mut_slice(d1));
        self.1.finalize_into_reset(Output::<D2>::from_mut_slice(d2));
    }
}

impl<D1: Reset, D2: Reset> Reset for Digest<D1, D2> {
    fn reset(&mut self) {
        self.0.reset();
        self.1.reset();
    }
}

impl<D1, D2> OutputSizeUser for Digest<D1, D2>
where
    D1: OutputSizeUser,
    D2: OutputSizeUser,
    D1::OutputSize: Add<D2::OutputSize>,
    <D1::OutputSize as Add<D2::OutputSize>>::Output: ArrayLength<u8>,
{
    type OutputSize = <D1::OutputSize as Add<D2::OutputSize>>::Output;
}

impl<D1: Update, D2: Update> Update for Digest<D1, D2> {
    fn update(&mut self, data: &[u8]) {
        self.0.update(data);
        self.1.update(data);
    }
}

impl<D1, D2> HashMarker for Digest<D1, D2> { }

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
