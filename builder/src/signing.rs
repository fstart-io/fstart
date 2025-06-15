use std::io;
use rand_core::OsRng;
use signature::Signer as _;
use ed25519_dalek::{SigningKey as EdSigningKey, SecretKey,
                    Signature as EdSignature};
use ml_dsa::{SigningKey as MlSigningKey, EncodedSigningKey,
             Signature as MlSignature, MlDsaParams};
use fstart_fs::{config, crypto::double};

pub type Result<O> = core::result::Result<O, Error>;

#[allow(dead_code)] // inner values are only used for debugging
#[derive(Debug)]
pub enum Error {
    IoError(io::Error),
    SignatureError(signature::Error),
}

pub fn sign(msg: &[u8]) -> Result<Vec<u8>> {
    let signer = match config::Signer::from_file() {
        Ok(signer) => signer,
        #[cfg(not(test))]
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // TODO: Alert user about automatically generated keys!
            config::Signer::create_file().map_err(Error::IoError)?;
            config::Signer::from_file().map_err(Error::IoError)?
        },
        Err(err) => return Err(Error::IoError(err))
    };
    Ok(signer.try_sign(msg).map_err(Error::SignatureError)?.into_vec())
}

trait FileBased: Sized {
    fn from_file() -> io::Result<Self>;
    fn create_file() -> io::Result<()>;
}

#[allow(dead_code)]
impl<S1: FileBased, S2: FileBased> FileBased for double::SigningKey<S1, S2> {
    fn from_file() -> io::Result<Self> {
        Ok(double::SigningKey(S1::from_file()?, S2::from_file()?))
    }
    fn create_file() -> io::Result<()> {
        S1::create_file()?;
        S2::create_file()?;
        Ok(())
    }
}

#[allow(dead_code)]
impl FileBased for EdSigningKey {
    fn from_file() -> io::Result<Self> {
        let mut secret = SecretKey::default();
        read_key(&mut secret, "ed25519")?;
        Ok(EdSigningKey::from_bytes(&secret))
    }
    fn create_file() -> io::Result<()> {
        let secret = EdSigningKey::generate(&mut OsRng);
        write_key(secret.verifying_key().as_bytes(), "ed25519.pub")?;
        write_key(secret.as_bytes(), "ed25519.secret")
    }
}

#[allow(dead_code)]
impl<P: MlDsaParams + Named> FileBased for MlSigningKey<P> {
    fn from_file() -> io::Result<Self> {
        let mut enc = EncodedSigningKey::<P>::default();
        read_key(&mut enc, P::name())?;
        Ok(MlSigningKey::<P>::decode(&enc))
    }
    fn create_file() -> io::Result<()> {
        use ml_dsa::KeyGen;
        let kp = P::key_gen(&mut OsRng);
        let name = String::from(P::name());
        write_key(&kp.verifying_key().encode(), &(name.clone() + ".pub"))?;
        write_key(&kp.signing_key().encode(), &(name + ".secret"))
    }
}

trait Named { fn name() -> &'static str; }
impl Named for ml_dsa::MlDsa44 { fn name() -> &'static str { "ml_dsa44" } }
impl Named for ml_dsa::MlDsa65 { fn name() -> &'static str { "ml_dsa65" } }
impl Named for ml_dsa::MlDsa87 { fn name() -> &'static str { "ml_dsa87" } }

trait IntoVec {
    fn into_vec(self) -> Vec<u8>;
}

#[allow(dead_code)]
impl<S1: IntoVec, S2: IntoVec> IntoVec for (S1, S2) {
    fn into_vec(self) -> Vec<u8> {
        let mut vec = self.0.into_vec();
        vec.append(&mut self.1.into_vec());
        vec
    }
}

#[allow(dead_code)]
impl IntoVec for EdSignature {
    fn into_vec(self) -> Vec<u8> {
        self.to_vec()
    }
}

#[allow(dead_code)]
impl<P: MlDsaParams> IntoVec for MlSignature<P> {
    fn into_vec(self) -> Vec<u8> {
        Vec::from(&self.encode()[..])
    }
}

fn read_key(buf: &mut [u8], prefix: &str) -> io::Result<()> {
    use std::fs::File;
    use std::io::Read;

    let mut path = key_path();
    path.push(String::from(prefix) + ".secret");

    let mut f = File::open(path)?;
    f.read_exact(buf)
}

fn write_key(buf: &[u8], name: &str) -> io::Result<()> {
    use std::fs::File;
    use std::io::Write;

    let mut path = key_path();
    path.push(String::from(name));

    let mut f = File::create(path)?;
    f.write_all(buf)?;
    f.sync_all()
}

fn key_path() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    #[cfg(not(test))]
    path.push("../.secrets");
    #[cfg(test)]
    path.push("test-data");
    path
}
