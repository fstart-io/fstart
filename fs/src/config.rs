use sha2::Sha512;
use sha3::Sha3_256;
use ed25519_dalek::{SigningKey as EdSigningKey, VerifyingKey as EdVerifyingKey};
use ml_dsa::{SigningKey as MlSigningKey, VerifyingKey as MlVerifyingKey, MlDsa44};
use crate::crypto::double;

pub type Digest = double::Digest<Sha512, Sha3_256>;

pub type Signer = double::SigningKey<EdSigningKey, MlSigningKey<MlDsa44>>;
pub type Verifier = double::VerifyingKey<EdVerifyingKey, MlVerifyingKey<MlDsa44>>;
