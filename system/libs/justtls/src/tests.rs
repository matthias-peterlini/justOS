//! Known-answer tests (RFCs, NIST) and checks against OpenSSL's output.

use crate::*;
use alloc::vec::Vec;

fn hex(s: &str) -> Vec<u8> {
    let s: alloc::string::String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

#[test]
fn sha2() {
    assert_eq!(sha2::Sha256::digest(b"abc").to_vec(), hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    assert_eq!(
        sha2::sha384(b"abc").to_vec(),
        hex("cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7")
    );
    assert_eq!(
        sha2::sha512(b"abc").to_vec(),
        hex("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")
    );
    // Many blocks, fed in odd pieces.
    let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    let mut h = sha2::Sha256::new();
    for c in data.chunks(7) {
        h.update(c);
    }
    assert_eq!(h.finish(), sha2::Sha256::digest(&data));
}

#[test]
fn hmac_hkdf() {
    assert_eq!(
        hash::hmac(hash::Alg::Sha256, b"Jefe", b"what do ya want for nothing?"),
        hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
    );
    let prk = hash::hkdf_extract(hash::Alg::Sha256, &hex("000102030405060708090a0b0c"), &[0x0b; 22]);
    let okm = hash::hkdf_expand(hash::Alg::Sha256, &prk, &hex("f0f1f2f3f4f5f6f7f8f9"), 42);
    assert_eq!(okm, hex("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"));
}

#[test]
fn poly1305_and_chacha() {
    let key: [u8; 32] = hex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b").try_into().unwrap();
    let mut p = chacha::Poly1305::new(&key);
    p.update(b"Cryptographic Forum Research Group");
    assert_eq!(p.finish().to_vec(), hex("a8061dc1305136c6c22b8baf0c0127a9"));

    let key: [u8; 32] = hex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f").try_into().unwrap();
    let nonce: [u8; 12] = hex("070000004041424344454647").try_into().unwrap();
    let aad = hex("50515253c0c1c2c3c4c5c6c7");
    let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    let mut data = pt.to_vec();
    chacha::seal(&key, &nonce, &aad, &mut data);
    assert_eq!(data[..16].to_vec(), hex("d31a8d34648e60db7b86afbc53ef7ec2"));
    assert_eq!(data[data.len() - 16..].to_vec(), hex("1ae10b594f09e26a7e902ecbd0600691"));
    assert!(chacha::open(&key, &nonce, &aad, &mut data));
    assert_eq!(&data[..], &pt[..]);
    let mut bad = data.clone();
    chacha::seal(&key, &nonce, &aad, &mut bad);
    bad[3] ^= 1;
    assert!(!chacha::open(&key, &nonce, &aad, &mut bad));
}

#[test]
fn aes_gcm() {
    let g = aes::Gcm::new(&[0u8; 16]);
    let mut d = Vec::new();
    g.seal(&[0; 12], &[], &mut d);
    assert_eq!(d, hex("58e2fccefa7e3061367f1d57a4e7455a"));
    let mut d = alloc::vec![0u8; 16];
    g.seal(&[0; 12], &[], &mut d);
    assert_eq!(d, hex("0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"));
    assert!(g.open(&[0; 12], &[], &mut d));
    assert_eq!(d, alloc::vec![0u8; 16]);
    let g = aes::Gcm::new(&[0u8; 32]);
    let mut d = Vec::new();
    g.seal(&[0; 12], &[], &mut d);
    assert_eq!(d, hex("530f8afbc74536b9a963b4f1c4cb738b"));
    // NIST test case 4 (AES-128, with associated data).
    let g = aes::Gcm::new(&hex("feffe9928665731c6d6a8f9467308308"));
    let nonce: [u8; 12] = hex("cafebabefacedbaddecaf888").try_into().unwrap();
    let mut d = hex(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
    );
    g.seal(&nonce, &hex("feedfacedeadbeeffeedfacedeadbeefabaddad2"), &mut d);
    assert_eq!(
        d,
        hex("42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e0915bc94fbc3221a5db94fae95ae7121a47")
    );
}

#[test]
fn x25519_vectors() {
    let k: [u8; 32] = hex("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4").try_into().unwrap();
    let u: [u8; 32] = hex("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c").try_into().unwrap();
    assert_eq!(x25519::x25519(&k, &u).to_vec(), hex("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"));
    // Alice and Bob (RFC 7748 6.1).
    let a: [u8; 32] = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a").try_into().unwrap();
    let b: [u8; 32] = hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb").try_into().unwrap();
    assert_eq!(x25519::public(&a).to_vec(), hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"));
    let s = hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
    assert_eq!(x25519::x25519(&a, &x25519::public(&b)).to_vec(), s);
}

#[test]
fn ecdh_p256_agrees() {
    let (ka, pa) = ec::keypair(ec::Curve::P256, &[7u8; 48]);
    let (kb, pb) = ec::keypair(ec::Curve::P256, &[9u8; 48]);
    assert_eq!(ec::shared(ec::Curve::P256, &ka, &pb), ec::shared(ec::Curve::P256, &kb, &pa));
    let (ka, pa) = ec::keypair(ec::Curve::P384, &[3u8; 48]);
    let (kb, pb) = ec::keypair(ec::Curve::P384, &[5u8; 48]);
    assert_eq!(ec::shared(ec::Curve::P384, &ka, &pb), ec::shared(ec::Curve::P384, &kb, &pa));
}

static CA: &[u8] = include_bytes!("../tests/data/ca.der");
static INT: &[u8] = include_bytes!("../tests/data/int.der");
static LEAF: &[u8] = include_bytes!("../tests/data/leaf.der");
static RLEAF: &[u8] = include_bytes!("../tests/data/rleaf.der");
static MSG: &[u8] = include_bytes!("../tests/data/msg.bin");

#[test]
fn signatures_from_openssl() {
    let leaf = x509::parse(LEAF).unwrap();
    let int = x509::parse(INT).unwrap();
    let ca = x509::parse(CA).unwrap();
    let lk = x509::parse_spki(leaf.spki).unwrap();
    let ik = x509::parse_spki(int.spki).unwrap();
    let ck = x509::parse_spki(ca.spki).unwrap();
    let ecdsa = include_bytes!("../tests/data/msg.ecdsa.sig");
    let pkcs1 = include_bytes!("../tests/data/msg.pkcs1.sig");
    let pss = include_bytes!("../tests/data/msg.pss.sig");
    let p384 = include_bytes!("../tests/data/msg.p384.sig");
    use hash::Alg::*;
    use x509::SigAlg::*;
    assert!(x509::verify(&lk, Ecdsa(Sha256), MSG, ecdsa));
    assert!(!x509::verify(&lk, Ecdsa(Sha256), b"something else", ecdsa));
    assert!(x509::verify(&ik, RsaPkcs1(Sha256), MSG, pkcs1));
    assert!(!x509::verify(&ik, RsaPkcs1(Sha384), MSG, pkcs1));
    assert!(x509::verify(&ik, RsaPss(Sha384), MSG, pss));
    assert!(x509::verify(&ck, Ecdsa(Sha384), MSG, p384));
}

fn roots() -> Vec<x509::Root> {
    let ca = x509::parse(CA).unwrap();
    alloc::vec![x509::Root { subject: ca.subject.to_vec(), key: x509::parse_spki(ca.spki).unwrap() }]
}

#[test]
fn chains() {
    let now = 1_800_000_000; // 2027
    let r = roots();
    let chain = alloc::vec![LEAF.to_vec(), INT.to_vec()];
    assert!(x509::verify_chain(&chain, "localhost", now, &r).is_ok());
    assert!(x509::verify_chain(&chain, "www.arc.test", now, &r).is_ok());
    assert!(x509::verify_chain(&chain, "127.0.0.1", now, &r).is_ok());
    assert!(x509::verify_chain(&chain, "arc.test", now, &r).is_err());
    assert!(x509::verify_chain(&chain, "a.b.arc.test", now, &r).is_err());
    assert!(x509::verify_chain(&chain, "example.com", now, &r).is_err());
    // Without the intermediate there is no path.
    assert!(x509::verify_chain(&[LEAF.to_vec()], "localhost", now, &r).is_err());
    // Expired (year 2100).
    assert!(x509::verify_chain(&chain, "localhost", 4_102_444_800, &r).is_err());
    // A PSS-signed leaf.
    assert!(x509::verify_chain(&[RLEAF.to_vec(), INT.to_vec()], "localhost", now, &r).is_ok());
    // A tampered leaf.
    let mut bad = LEAF.to_vec();
    let n = bad.len();
    bad[n - 10] ^= 1;
    assert!(x509::verify_chain(&[bad, INT.to_vec()], "localhost", now, &r).is_err());
}

#[test]
fn drbg_differs() {
    let mut d = Drbg::new(&[1; 32]);
    let (mut a, mut b) = ([0u8; 40], [0u8; 40]);
    d.fill(&mut a);
    d.fill(&mut b);
    assert_ne!(a, b);
}

#[test]
fn mozilla_roots_parse() {
    let roots = x509::parse_roots(include_bytes!("../../../../rootfs/system/certs.bin"));
    assert!(roots.len() >= 100, "{} roots", roots.len());
}

#[test]
fn ed25519_rfc8032() {
    let cases = [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
    ];
    for (seed, public, msg, sig) in cases {
        let seed: [u8; 32] = hex(seed).try_into().unwrap();
        let public: [u8; 32] = hex(public).try_into().unwrap();
        let (msg, sig) = (hex(msg), hex(sig));
        assert_eq!(ed25519::public_key(&seed), public);
        assert_eq!(ed25519::sign(&seed, &msg).to_vec(), sig);
        assert!(ed25519::verify(&public, &msg, &sig));
        let mut bad = sig.clone();
        bad[5] ^= 1;
        assert!(!ed25519::verify(&public, &msg, &bad));
        assert!(!ed25519::verify(&public, b"altro", &sig));
        // S + L is the same signature in a non-canonical form: refused.
        let mut s = sig.clone();
        let l = hex("edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010");
        let mut carry = 0u16;
        for i in 0..32 {
            let t = s[32 + i] as u16 + l[i] as u16 + carry;
            s[32 + i] = t as u8;
            carry = t >> 8;
        }
        assert!(!ed25519::verify(&public, &msg, &s));
    }
    // A longer message, signed and checked here.
    let seed = [7u8; 32];
    let msg: Vec<u8> = (0..3000u32).map(|i| (i * 31) as u8).collect();
    let sig = ed25519::sign(&seed, &msg);
    assert!(ed25519::verify(&ed25519::public_key(&seed), &msg, &sig));
}
