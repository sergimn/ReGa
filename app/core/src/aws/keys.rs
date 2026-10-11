//! The RSA key pair of a machine. AWS encrypts the Windows Administrator
//! password with its public half; the app decrypts it with the private half.

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize, Pkcs1PrivateDecryptingKey, PrivateDecryptingKey};
use aws_lc_rs::signature::KeyPair as _;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use crate::provider::{ProviderError, ProviderResult};

const PEM_BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
const PEM_END: &str = "-----END PRIVATE KEY-----";

pub struct MachineKey {
    pkcs8: Vec<u8>,
}

fn key_error(what: &str) -> ProviderError {
    ProviderError::Cloud(format!("machine key: {what}"))
}

impl MachineKey {
    /// A new 2048-bit key, the size EC2 supports for Windows passwords.
    pub fn generate() -> ProviderResult<Self> {
        let pair =
            KeyPair::generate(KeySize::Rsa2048).map_err(|_| key_error("generation failed"))?;
        let pkcs8 = pair
            .as_der()
            .map_err(|_| key_error("export failed"))?
            .as_ref()
            .to_vec();
        Ok(Self { pkcs8 })
    }

    pub fn from_pem(pem: &str) -> ProviderResult<Self> {
        let body: String = pem
            .trim()
            .strip_prefix(PEM_BEGIN)
            .and_then(|s| s.strip_suffix(PEM_END))
            .ok_or_else(|| key_error("not a PEM private key"))?
            .split_whitespace()
            .collect();
        let pkcs8 = STANDARD
            .decode(body)
            .map_err(|_| key_error("not a PEM private key"))?;
        KeyPair::from_pkcs8(&pkcs8).map_err(|_| key_error("not an RSA key"))?;
        Ok(Self { pkcs8 })
    }

    pub fn to_pem(&self) -> String {
        let b64 = STANDARD.encode(&self.pkcs8);
        let mut pem = String::from(PEM_BEGIN);
        for line in b64.as_bytes().chunks(64) {
            pem.push('\n');
            pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        }
        pem.push('\n');
        pem.push_str(PEM_END);
        pem.push('\n');
        pem
    }

    /// The public half in OpenSSH form (`ssh-rsa AAAA… comment`).
    pub fn openssh_public(&self, comment: &str) -> String {
        let pair = KeyPair::from_pkcs8(&self.pkcs8).expect("checked when created");
        let public = pair.public_key();
        let mut blob = Vec::new();
        ssh_string(&mut blob, b"ssh-rsa");
        ssh_mpint(
            &mut blob,
            public.exponent().big_endian_without_leading_zero(),
        );
        ssh_mpint(
            &mut blob,
            public.modulus().big_endian_without_leading_zero(),
        );
        format!("ssh-rsa {} {comment}", STANDARD.encode(blob))
    }

    /// Decrypts EC2's `password_data` (base64, RSA PKCS#1 v1.5).
    pub fn decrypt_password(&self, password_data: &str) -> ProviderResult<String> {
        let cipher = STANDARD
            .decode(password_data.split_whitespace().collect::<String>())
            .map_err(|_| key_error("password data isn't base64"))?;
        let private = PrivateDecryptingKey::from_pkcs8(&self.pkcs8)
            .map_err(|_| key_error("not an RSA key"))?;
        let private =
            Pkcs1PrivateDecryptingKey::new(private).map_err(|_| key_error("not an RSA key"))?;
        let mut plain = vec![0; private.min_output_size()];
        let plain = private
            .decrypt(&cipher, &mut plain)
            .map_err(|_| key_error("the password wasn't made for this key"))?;
        String::from_utf8(plain.to_vec()).map_err(|_| key_error("the password isn't text"))
    }
}

fn ssh_string(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
}

/// SSH `mpint`: big-endian, with a zero byte in front when the top bit is
/// set so the number stays positive.
fn ssh_mpint(out: &mut Vec<u8>, magnitude: &[u8]) {
    if magnitude.first().is_some_and(|b| b & 0x80 != 0) {
        let mut padded = vec![0];
        padded.extend_from_slice(magnitude);
        ssh_string(out, &padded);
    } else {
        ssh_string(out, magnitude);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::rsa::Pkcs1PublicEncryptingKey;

    fn encrypt_for(key: &MachineKey, text: &str) -> String {
        let private = PrivateDecryptingKey::from_pkcs8(&key.pkcs8).unwrap();
        let public = Pkcs1PublicEncryptingKey::new(private.public_key()).unwrap();
        let mut out = vec![0; public.ciphertext_size()];
        let cipher = public.encrypt(text.as_bytes(), &mut out).unwrap();
        STANDARD.encode(cipher)
    }

    #[test]
    fn decrypts_a_password_encrypted_with_its_public_key() {
        let key = MachineKey::generate().unwrap();
        let data = encrypt_for(&key, "S3cret!pass");
        assert_eq!(key.decrypt_password(&data).unwrap(), "S3cret!pass");

        let other = MachineKey::generate().unwrap();
        assert!(other.decrypt_password(&data).is_err());
    }

    #[test]
    fn pem_round_trips() {
        let key = MachineKey::generate().unwrap();
        let pem = key.to_pem();
        assert!(pem.starts_with(PEM_BEGIN));
        assert!(pem.lines().all(|l| l.len() <= 64));
        let back = MachineKey::from_pem(&pem).unwrap();
        assert_eq!(back.pkcs8, key.pkcs8);
        assert!(MachineKey::from_pem("nope").is_err());
    }

    #[test]
    fn openssh_public_key_has_the_rsa_layout() {
        let key = MachineKey::generate().unwrap();
        let line = key.openssh_public("rega-m");
        let mut parts = line.split(' ');
        assert_eq!(parts.next(), Some("ssh-rsa"));
        let blob = STANDARD.decode(parts.next().unwrap()).unwrap();
        assert_eq!(parts.next(), Some("rega-m"));
        // string "ssh-rsa", then mpint e = 65537.
        assert_eq!(&blob[..11], b"\0\0\0\x07ssh-rsa");
        assert_eq!(&blob[11..18], &[0, 0, 0, 3, 1, 0, 1]);
        // mpint n: 2048 bits with the top bit set, so 257 bytes with a zero.
        assert_eq!(&blob[18..23], &[0, 0, 1, 1, 0]);
        assert_eq!(blob.len(), 23 + 256);
    }

    #[test]
    fn mpint_pads_only_when_the_top_bit_is_set() {
        let mut out = Vec::new();
        ssh_mpint(&mut out, &[0x7f]);
        ssh_mpint(&mut out, &[0x80]);
        assert_eq!(out, [0, 0, 0, 1, 0x7f, 0, 0, 0, 2, 0, 0x80]);
    }
}
