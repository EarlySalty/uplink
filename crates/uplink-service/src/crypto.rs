use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use std::fmt;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub struct Secret(Zeroizing<Vec<u8>>);
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([geschützt])")
    }
}
impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
    pub fn matches(&self, candidate: &[u8]) -> bool {
        bool::from(self.0.as_slice().ct_eq(candidate))
    }
    pub fn seal(&self, plaintext: &[u8], binding: &str) -> Result<Vec<u8>, &'static str> {
        let cipher = Aes256Gcm::new_from_slice(&self.0).map_err(|_| "Schlüssel ist ungültig.")?;
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(|_| "Zufallsquelle ist nicht verfügbar.")?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: binding.as_bytes(),
                },
            )
            .map_err(|_| "Verschlüsselung fehlgeschlagen.")?;
        let mut result = Vec::with_capacity(13 + ciphertext.len());
        result.push(2);
        result.extend(nonce);
        result.extend(ciphertext);
        Ok(result)
    }
    pub fn open(&self, ciphertext: &[u8], binding: &str) -> Result<Self, &'static str> {
        if ciphertext.len() < 29 || ciphertext[0] != 2 {
            return Err("Verschlüsseltes Objekt ist ungültig oder benötigt Migration.");
        }
        let cipher = Aes256Gcm::new_from_slice(&self.0).map_err(|_| "Schlüssel ist ungültig.")?;
        cipher
            .decrypt(
                Nonce::from_slice(&ciphertext[1..13]),
                Payload {
                    msg: &ciphertext[13..],
                    aad: binding.as_bytes(),
                },
            )
            .map(Self::new)
            .map_err(|_| "Verschlüsseltes Objekt kann nicht zugeordnet werden.")
    }
}
