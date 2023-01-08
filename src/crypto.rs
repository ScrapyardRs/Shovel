use openssl::sha::sha1;
use rand::rngs::OsRng;

use rsa::pkcs1v15::VerifyingKey;
use rsa::signature::digest::FixedOutput;
use rsa::signature::{Signature, Verifier};
use rsa::{BigUint, PaddingScheme, PublicKeyParts, RsaPublicKey};
use sha2::digest::HashMarker;

pub type MCPrivateKey = rsa::RsaPrivateKey;
pub type MCPublicKey = RsaPublicKey;
pub type Padding = PaddingScheme;

pub fn new_key() -> rsa::errors::Result<MCPrivateKey> {
    let mut rng = OsRng;
    rsa::RsaPrivateKey::new(&mut rng, 1024)
}

#[derive(Debug)]
pub enum CapturedRsaError {
    RsaError(rsa::errors::Error),
    RsaDerError(rsa_der::Error),
    SigError(rsa::signature::Error),
}

impl std::error::Error for CapturedRsaError {}

impl std::fmt::Display for CapturedRsaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RsaError(rsa_err) => rsa_err.fmt(f),
            Self::RsaDerError(rsa_der_err) => rsa_der_err.fmt(f),
            Self::SigError(sig_err) => sig_err.fmt(f),
        }
    }
}

impl From<rsa::errors::Error> for CapturedRsaError {
    fn from(err: rsa::errors::Error) -> Self {
        Self::RsaError(err)
    }
}

impl From<rsa_der::Error> for CapturedRsaError {
    fn from(err: rsa_der::Error) -> Self {
        Self::RsaDerError(err)
    }
}

impl From<rsa::signature::Error> for CapturedRsaError {
    fn from(err: rsa::signature::Error) -> Self {
        Self::SigError(err)
    }
}

pub fn key_from_der(der: &[u8]) -> Result<MCPublicKey, CapturedRsaError> {
    let (n, e) =
        rsa_der::public_key_from_der(der).map_err(|err| CapturedRsaError::RsaDerError(err))?;
    RsaPublicKey::new(BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e))
        .map_err(|err| CapturedRsaError::RsaError(err))
}

pub fn private_key_to_der(key: &MCPrivateKey) -> Vec<u8> {
    let pub_key = RsaPublicKey::from(key);
    rsa_der::public_key_to_der(&pub_key.n().to_bytes_be(), &pub_key.e().to_bytes_be())
}

pub fn verify_signature<Hash: FixedOutput + HashMarker + Default>(
    verify_key: VerifyingKey<Hash>,
    signature: &[u8],
    message: &[u8],
) -> Result<(), CapturedRsaError> {
    verify_key.verify(message, &Signature::from_bytes(signature)?)?;
    Ok(())
}

pub fn sha1_message(bytes: &[u8]) -> [u8; 20] {
    sha1(bytes)
}
