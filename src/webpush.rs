//! The Web Push protocol: encrypting a message for one browser (RFC 8291,
//! `aes128gcm` from RFC 8188) and signing our claim to send it (VAPID,
//! RFC 8292). Built on aws-lc-rs, already in the tree, instead of the
//! `web-push` crate and the ~90 crates and OpenSSL it brings.

use aws_lc_rs::{
    aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey},
    agreement::{self, ECDH_P256, UnparsedPublicKey},
    hkdf::{HKDF_SHA256, KeyType, Salt},
    rand::{SecureRandom, SystemRandom},
    signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};

/// The record size advertised in the header. One record always fits a
/// notification (the push services cap payloads at 4 KB).
const RECORD_SIZE: u32 = 4096;

/// Our VAPID key pair, which push services check our requests against, and
/// which browsers were given when they subscribed.
pub struct Vapid {
    key_pair: EcdsaKeyPair,
    /// Base64url, as browsers and the `k=` parameter want it.
    pub public_key: String,
    /// A `mailto:` or `https:` URL push services can reach us at.
    subject: String,
}

#[derive(Debug, PartialEq)]
pub struct BadKey(pub &'static str);

impl Vapid {
    /// From base64url keys in the `web-push` library's format: the 65-byte
    /// uncompressed public point and the 32-byte private scalar.
    pub fn new(public_key: &str, private_key: &str, subject: &str) -> Result<Self, BadKey> {
        let public = B64
            .decode(public_key.trim())
            .map_err(|_| BadKey("public key isn't base64url"))?;
        let private = B64
            .decode(private_key.trim())
            .map_err(|_| BadKey("private key isn't base64url"))?;
        let key_pair = EcdsaKeyPair::from_private_key_and_public_key(
            &ECDSA_P256_SHA256_FIXED_SIGNING,
            &private,
            &public,
        )
        .map_err(|_| BadKey("keys aren't a P-256 key pair"))?;
        Ok(Self {
            public_key: B64.encode(key_pair.public_key().as_ref()),
            key_pair,
            subject: subject.to_owned(),
        })
    }

    /// The `Authorization` header for a request to `endpoint`, valid until
    /// `expires` (epoch seconds; push services refuse more than 24 hours).
    pub fn authorization(&self, endpoint: &str, expires: i64) -> Result<String, BadKey> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| BadKey("endpoint isn't a URL"))?;
        let audience = url.origin().ascii_serialization();
        let header = B64.encode(r#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(
            serde_json::json!({ "aud": audience, "exp": expires, "sub": self.subject }).to_string(),
        );
        let signed = format!("{header}.{claims}");
        let signature = self
            .key_pair
            .sign(&SystemRandom::new(), signed.as_bytes())
            .map_err(|_| BadKey("signing failed"))?;
        Ok(format!(
            "vapid t={signed}.{}, k={}",
            B64.encode(signature.as_ref()),
            self.public_key
        ))
    }
}

/// A browser's subscription keys, as its push subscription gives them.
pub struct Subscriber<'a> {
    /// Base64url P-256 public key.
    pub p256dh: &'a str,
    /// Base64url 16-byte secret.
    pub auth: &'a str,
}

/// `message` encrypted for the subscriber: the request body to send with
/// `Content-Encoding: aes128gcm`.
pub fn encrypt(subscriber: &Subscriber, message: &[u8]) -> Result<Vec<u8>, BadKey> {
    let rng = SystemRandom::new();
    let mut salt = [0; 16];
    rng.fill(&mut salt).map_err(|_| BadKey("no randomness"))?;
    let sender =
        agreement::PrivateKey::generate(&ECDH_P256).map_err(|_| BadKey("no randomness"))?;
    encrypt_with(subscriber, message, &sender, salt)
}

fn encrypt_with(
    subscriber: &Subscriber,
    message: &[u8],
    sender: &agreement::PrivateKey,
    salt: [u8; 16],
) -> Result<Vec<u8>, BadKey> {
    let ua_public = B64
        .decode(subscriber.p256dh.trim())
        .map_err(|_| BadKey("p256dh isn't base64url"))?;
    let auth_secret = B64
        .decode(subscriber.auth.trim())
        .map_err(|_| BadKey("auth isn't base64url"))?;
    let as_public = sender
        .compute_public_key()
        .map_err(|_| BadKey("sender key"))?;
    let as_public = as_public.as_ref();

    // RFC 8291 §3.4: the shared secret, mixed with the browser's auth secret.
    let ecdh_secret = agreement::agree(
        sender,
        UnparsedPublicKey::new(&ECDH_P256, &ua_public),
        BadKey("p256dh isn't a P-256 public key"),
        |secret| Ok(secret.to_vec()),
    )?;
    let key_info = [b"WebPush: info\0".as_slice(), &ua_public, as_public].concat();
    let ikm: [u8; 32] = hkdf(&auth_secret, &ecdh_secret, &key_info);

    // RFC 8188 §2.2–2.3: the content key and nonce.
    let cek: [u8; 16] = hkdf(&salt, &ikm, b"Content-Encoding: aes128gcm\0");
    let nonce: [u8; 12] = hkdf(&salt, &ikm, b"Content-Encoding: nonce\0");

    // One record, so it's the last: its padding delimiter is 2.
    let mut record = [message, &[2]].concat();
    let key =
        LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| BadKey("content key"))?);
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::empty(),
        &mut record,
    )
    .map_err(|_| BadKey("encryption failed"))?;

    // The header: salt, record size, and our public key as the key id.
    let mut body = Vec::with_capacity(16 + 4 + 1 + as_public.len() + record.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

/// HKDF-SHA256 extract-then-expand into `N` bytes.
fn hkdf<const N: usize>(salt: &[u8], secret: &[u8], info: &[u8]) -> [u8; N] {
    struct Len(usize);
    impl KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let mut out = [0; N];
    Salt::new(HKDF_SHA256, salt)
        .extract(secret)
        .expand(&[info], Len(N))
        .and_then(|okm| okm.fill(&mut out))
        .expect("N is well under HKDF-SHA256's limit");
    out
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey as SignaturePublicKey};

    use super::*;

    /// RFC 8291, Appendix A.
    const PLAINTEXT: &str = "When I grow up, I want to be a watermelon";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const AS_PUBLIC: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

    fn subscriber() -> Subscriber<'static> {
        Subscriber {
            p256dh: UA_PUBLIC,
            auth: AUTH,
        }
    }

    #[test]
    fn matches_the_rfc_8291_example() {
        let sender =
            agreement::PrivateKey::from_private_key(&ECDH_P256, &B64.decode(AS_PRIVATE).unwrap())
                .unwrap();
        assert_eq!(
            B64.encode(sender.compute_public_key().unwrap().as_ref()),
            AS_PUBLIC
        );
        let salt = B64.decode(SALT).unwrap().try_into().unwrap();
        let body = encrypt_with(&subscriber(), PLAINTEXT.as_bytes(), &sender, salt).unwrap();
        assert_eq!(B64.encode(body), BODY);
    }

    /// What a browser does with the body: undoes [`encrypt`] with its
    /// private key.
    fn decrypt(body: &[u8]) -> Vec<u8> {
        let salt = &body[..16];
        let key_id_len = body[20] as usize;
        let as_public = &body[21..21 + key_id_len];
        let mut record = body[21 + key_id_len..].to_vec();

        let ua =
            agreement::PrivateKey::from_private_key(&ECDH_P256, &B64.decode(UA_PRIVATE).unwrap())
                .unwrap();
        let ua_public = B64.decode(UA_PUBLIC).unwrap();
        let ecdh = agreement::agree(
            &ua,
            UnparsedPublicKey::new(&ECDH_P256, as_public),
            (),
            |s| Ok(s.to_vec()),
        )
        .unwrap();
        let ikm: [u8; 32] = hkdf(
            &B64.decode(AUTH).unwrap(),
            &ecdh,
            &[b"WebPush: info\0".as_slice(), &ua_public, as_public].concat(),
        );
        let cek: [u8; 16] = hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0");
        let nonce: [u8; 12] = hkdf(salt, &ikm, b"Content-Encoding: nonce\0");
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).unwrap());
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut record,
            )
            .unwrap();
        assert_eq!(plain.last(), Some(&2), "the last record's delimiter");
        plain[..plain.len() - 1].to_vec()
    }

    #[test]
    fn a_browser_can_read_what_was_sent() {
        let body = encrypt(&subscriber(), b"{\"title\":\"Hi\"}").unwrap();
        assert_eq!(decrypt(&body), b"{\"title\":\"Hi\"}");
        // A fresh key and salt every time.
        assert_ne!(
            encrypt(&subscriber(), b"x").unwrap(),
            encrypt(&subscriber(), b"x").unwrap()
        );
    }

    #[test]
    fn signs_a_token_for_the_push_services_origin() {
        // The RFC 8291 sender pair doubles as a VAPID pair.
        let vapid = Vapid::new(AS_PUBLIC, AS_PRIVATE, "mailto:me@example.com").unwrap();
        let header = vapid
            .authorization("https://push.example.net/send/abc?x=1", 1_700_000_000)
            .unwrap();

        let (token, key) = header
            .strip_prefix("vapid t=")
            .unwrap()
            .split_once(", k=")
            .unwrap();
        assert_eq!(key, AS_PUBLIC);
        let (signed, signature) = token.rsplit_once('.').unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(signed.split_once('.').unwrap().1).unwrap())
                .unwrap();
        assert_eq!(
            claims,
            serde_json::json!({ "aud": "https://push.example.net", "exp": 1_700_000_000, "sub": "mailto:me@example.com" })
        );
        SignaturePublicKey::new(&ECDSA_P256_SHA256_FIXED, B64.decode(AS_PUBLIC).unwrap())
            .verify(signed.as_bytes(), &B64.decode(signature).unwrap())
            .expect("the signature checks out with our public key");
    }

    #[test]
    fn turns_away_keys_that_dont_belong_together() {
        assert!(Vapid::new(UA_PUBLIC, AS_PRIVATE, "mailto:me@example.com").is_err());
        assert!(Vapid::new("not base64!", AS_PRIVATE, "mailto:me@example.com").is_err());
    }
}
