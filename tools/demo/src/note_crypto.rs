//! Note encryption, format v2 (`docs/NOTE_ENCRYPTION.md`).
//!
//! ```text
//! note      = 0x02 || ephemeral_public_key (32) || ciphertext (64) || tag (16)
//! shared    = X25519(ephemeral_secret, recipient_key)
//! key||nonce = HKDF-SHA256(shared, salt = E || R, info, 44 bytes)
//! plaintext = amount (32 BE) || blinding (32 BE)
//! aad       = 0x02 || E
//! ```

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use hkdf::Hkdf;
use sha2_10::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

pub const VERSION: u8 = 0x02;
pub const NOTE_LEN: usize = 113;
const INFO: &[u8] = b"veil/note/v2/chacha20poly1305";

/// What a note opens to: the amount and blinding behind a commitment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    pub amount: [u8; 32],
    pub blinding: [u8; 32],
}

#[derive(Debug, thiserror::Error)]
pub enum NoteError {
    #[error("the shared secret is all zeros (a low-order key)")]
    WeakKey,
    #[error("encryption failed")]
    Encrypt,
}

fn cipher(
    shared: &[u8; 32],
    ephemeral: &[u8; 32],
    recipient: &[u8; 32],
) -> (ChaCha20Poly1305, [u8; 12]) {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral);
    salt[32..].copy_from_slice(recipient);
    let mut okm = [0u8; 44];
    // 44 bytes is far below HKDF-SHA256's limit, so expand cannot fail.
    let _ = Hkdf::<Sha256>::new(Some(&salt), shared).expand(INFO, &mut okm);
    let mut key = [0u8; 32];
    key.copy_from_slice(&okm[..32]);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&okm[32..]);
    (ChaCha20Poly1305::new(&Key::from(key)), nonce)
}

/// Encrypts `opened` to `recipient`, using `ephemeral` as the one-time key.
/// Callers pass a fresh random secret for every note.
pub fn encrypt(
    recipient: &[u8; 32],
    ephemeral: StaticSecret,
    opened: &Opened,
) -> Result<Vec<u8>, NoteError> {
    let ephemeral_public = PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&PublicKey::from(*recipient));
    if !shared.was_contributory() {
        return Err(NoteError::WeakKey);
    }
    let (cipher, nonce) = cipher(shared.as_bytes(), &ephemeral_public, recipient);
    let mut plaintext = [0u8; 64];
    plaintext[..32].copy_from_slice(&opened.amount);
    plaintext[32..].copy_from_slice(&opened.blinding);
    let mut aad = [0u8; 33];
    aad[0] = VERSION;
    aad[1..].copy_from_slice(&ephemeral_public);
    let sealed = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| NoteError::Encrypt)?;
    let mut note = Vec::with_capacity(NOTE_LEN);
    note.push(VERSION);
    note.extend_from_slice(&ephemeral_public);
    note.extend_from_slice(&sealed);
    Ok(note)
}

/// Opens `note` with the recipient's secret, or `None` if it is not a v2 note
/// for this recipient. Never panics on hostile input.
pub fn decrypt(secret: &StaticSecret, note: &[u8]) -> Option<Opened> {
    if note.len() != NOTE_LEN || note.first() != Some(&VERSION) {
        return None;
    }
    let ephemeral: [u8; 32] = note.get(1..33)?.try_into().ok()?;
    let recipient = PublicKey::from(secret).to_bytes();
    let shared = secret.diffie_hellman(&PublicKey::from(ephemeral));
    if !shared.was_contributory() {
        return None;
    }
    let (cipher, nonce) = cipher(shared.as_bytes(), &ephemeral, &recipient);
    let aad = note.get(..33)?;
    let plaintext = cipher
        .decrypt(
            &Nonce::from(nonce),
            Payload {
                msg: note.get(33..)?,
                aad,
            },
        )
        .ok()?;
    let amount: [u8; 32] = plaintext.get(..32)?.try_into().ok()?;
    let blinding: [u8; 32] = plaintext.get(32..)?.try_into().ok()?;
    Some(Opened { amount, blinding })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(byte: u8) -> StaticSecret {
        StaticSecret::from([byte; 32])
    }

    fn opened() -> Opened {
        let mut amount = [0u8; 32];
        amount[31] = 40;
        Opened {
            amount,
            blinding: [7u8; 32],
        }
    }

    #[test]
    fn a_note_opens_for_its_recipient_only() {
        let recipient = secret(1);
        let recipient_key = PublicKey::from(&recipient).to_bytes();
        let note = encrypt(&recipient_key, secret(2), &opened()).expect("encrypts");
        assert_eq!(note.len(), NOTE_LEN);
        assert_eq!(note[0], VERSION);
        assert_eq!(decrypt(&recipient, &note), Some(opened()));
        assert_eq!(decrypt(&secret(3), &note), None);
    }

    #[test]
    fn any_change_to_a_note_is_rejected() {
        let recipient = secret(1);
        let recipient_key = PublicKey::from(&recipient).to_bytes();
        let note = encrypt(&recipient_key, secret(2), &opened()).expect("encrypts");
        for index in 0..note.len() {
            let mut tampered = note.clone();
            tampered[index] ^= 1;
            assert_eq!(decrypt(&recipient, &tampered), None, "byte {index}");
        }
        assert_eq!(decrypt(&recipient, &note[..NOTE_LEN - 1]), None);
        assert_eq!(decrypt(&recipient, &[]), None);
    }

    #[test]
    fn the_137_byte_notes_already_on_chain_are_skipped() {
        let mut legacy = vec![0x01];
        legacy.extend([0xab; 136]);
        assert_eq!(decrypt(&secret(1), &legacy), None);
    }

    #[test]
    fn a_low_order_recipient_key_is_refused() {
        assert!(matches!(
            encrypt(&[0u8; 32], secret(2), &opened()),
            Err(NoteError::WeakKey)
        ));
    }

    /// Pins the format: the same keys and plaintext always give these bytes,
    /// so another implementation can check itself against them.
    #[test]
    fn test_vector() {
        let recipient_key = PublicKey::from(&secret(1)).to_bytes();
        let note = encrypt(&recipient_key, secret(2), &opened()).expect("encrypts");
        assert_eq!(
            hex::encode(&note[..33]),
            "02ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59"
        );
        assert_eq!(decrypt(&secret(1), &note), Some(opened()));
    }
}
