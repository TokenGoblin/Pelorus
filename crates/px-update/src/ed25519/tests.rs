//! Ed25519 tests: RFC 8032 §7.1 vectors, negative cases, and the test-only
//! signer used for round trips.

use super::edwards::Point;
use super::field::Fe;
use super::scalar;
use super::{VerifyError, verify};
use crate::sha2::sha512;

pub(crate) fn unhex(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// (a * b) mod L, double-and-add over the bits of b.
fn mul_mod(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let a = scalar::to_limbs(&scalar::reduce(a));
    let mut acc = [0u64; 4];
    for byte in b.iter().rev() {
        for i in (0..8).rev() {
            acc = scalar::add_mod(&acc, &acc);
            if (byte >> i) & 1 == 1 {
                acc = scalar::add_mod(&acc, &a);
            }
        }
    }
    scalar::from_limbs(&acc)
}

fn clamp(h: &[u8]) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
    a
}

/// Test-only RFC 8032 §5.1.6 signer.
pub(crate) fn public_key_of(seed: &[u8; 32]) -> [u8; 32] {
    let a = clamp(&sha512(seed));
    Point::basepoint().unwrap().mul_scalar(&a).compress()
}

pub(crate) fn sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let h = sha512(seed);
    let a = clamp(&h);
    let pk = public_key_of(seed);
    let mut pre = h[32..].to_vec();
    pre.extend_from_slice(msg);
    let r = scalar::reduce(&sha512(&pre));
    let big_r = Point::basepoint().unwrap().mul_scalar(&r).compress();
    let mut kin = big_r.to_vec();
    kin.extend_from_slice(&pk);
    kin.extend_from_slice(msg);
    let k = scalar::reduce(&sha512(&kin));
    let ka = mul_mod(&k, &a);
    let s = scalar::add_mod(&scalar::to_limbs(&r), &scalar::to_limbs(&ka));
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&big_r);
    sig[32..].copy_from_slice(&scalar::from_limbs(&s));
    sig
}

// ---- RFC 8032 section 7.1 -------------------------------------------------

struct Vector {
    sk: &'static str,
    pk: &'static str,
    msg: String,
    sig: &'static str,
}

const MSG_1023: &str = "08b8b2b733424243760fe426a4b54908632110a66c2f6591eabd3345e3e4eb98fa6e264bf09efe12ee50f8f54e9f77b1e355f6c50544e23fb1433ddf73be84d879de7c0046dc4996d9e773f4bc9efe5738829adb26c81b37c93a1b270b20329d658675fc6ea534e0810a4432826bf58c941efb65d57a338bbd2e26640f89ffbc1a858efcb8550ee3a5e1998bd177e93a7363c344fe6b199ee5d02e82d522c4feba15452f80288a821a579116ec6dad2b3b310da903401aa62100ab5d1a36553e06203b33890cc9b832f79ef80560ccb9a39ce767967ed628c6ad573cb116dbefefd75499da96bd68a8a97b928a8bbc103b6621fcde2beca1231d206be6cd9ec7aff6f6c94fcd7204ed3455c68c83f4a41da4af2b74ef5c53f1d8ac70bdcb7ed185ce81bd84359d44254d95629e9855a94a7c1958d1f8ada5d0532ed8a5aa3fb2d17ba70eb6248e594e1a2297acbbb39d502f1a8c6eb6f1ce22b3de1a1f40cc24554119a831a9aad6079cad88425de6bde1a9187ebb6092cf67bf2b13fd65f27088d78b7e883c8759d2c4f5c65adb7553878ad575f9fad878e80a0c9ba63bcbcc2732e69485bbc9c90bfbd62481d9089beccf80cfe2df16a2cf65bd92dd597b0707e0917af48bbb75fed413d238f5555a7a569d80c3414a8d0859dc65a46128bab27af87a71314f318c782b23ebfe808b82b0ce26401d2e22f04d83d1255dc51addd3b75a2b1ae0784504df543af8969be3ea7082ff7fc9888c144da2af58429ec96031dbcad3dad9af0dcbaaaf268cb8fcffead94f3c7ca495e056a9b47acdb751fb73e666c6c655ade8297297d07ad1ba5e43f1bca32301651339e22904cc8c42f58c30c04aafdb038dda0847dd988dcda6f3bfd15c4b4c4525004aa06eeff8ca61783aacec57fb3d1f92b0fe2fd1a85f6724517b65e614ad6808d6f6ee34dff7310fdc82aebfd904b01e1dc54b2927094b2db68d6f903b68401adebf5a7e08d78ff4ef5d63653a65040cf9bfd4aca7984a74d37145986780fc0b16ac451649de6188a7dbdf191f64b5fc5e2ab47b57f7f7276cd419c17a3ca8e1b939ae49e488acba6b965610b5480109c8b17b80e1b7b750dfc7598d5d5011fd2dcc5600a32ef5b52a1ecc820e308aa342721aac0943bf6686b64b2579376504ccc493d97e6aed3fb0f9cd71a43dd497f01f17c0e2cb3797aa2a2f256656168e6c496afc5fb93246f6b1116398a346f1a641f3b041e989f7914f90cc2c7fff357876e506b50d334ba77c225bc307ba537152f3f1610e4eafe595f6d9d90d11faa933a15ef1369546868a7f3a45a96768d40fd9d03412c091c6315cf4fde7cb68606937380db2eaaa707b4c4185c32eddcdd306705e4dc1ffc872eeee475a64dfac86aba41c0618983f8741c5ef68d3a101e8a3b8cac60c905c15fc910840b94c00a0b9d0";

fn vectors() -> Vec<Vector> {
    vec![
        Vector {
            sk: "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            pk: "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            msg: String::new(),
            sig: "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        },
        Vector {
            sk: "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            pk: "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            msg: "72".into(),
            sig: "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        },
        Vector {
            sk: "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            pk: "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            msg: "af82".into(),
            sig: "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        },
        Vector {
            sk: "f5e5767cf153319517630f226876b86c8160cc583bc013744c6bf255f5cc0ee5",
            pk: "278117fc144c72340f67d0f2316e8386ceffbf2b2428c9c51fef7c597f1d426e",
            msg: MSG_1023.into(),
            sig: "0aab4c900501b3e24d7cdf4663326a3a87df5e4843b2cbdb67cbf6e460fec350aa5371b1508f9f4528ecea23c436d94b5e8fcd4f681e30a6ac00a9704a188a03",
        },
        Vector {
            sk: "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
            pk: "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
            msg: "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f".into(),
            sig: "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
        },
    ]
}

#[test]
fn rfc8032_message_1023_is_1023_bytes() {
    assert_eq!(unhex(MSG_1023).len(), 1023);
}

#[test]
fn rfc8032_vectors_verify_and_signer_reproduces_them() {
    for (i, v) in vectors().iter().enumerate() {
        let sk: [u8; 32] = unhex(v.sk).try_into().unwrap();
        let msg = unhex(&v.msg);
        let derived = public_key_of(&sk);
        assert_eq!(hex(&derived), v.pk, "vector {i}: public key");
        assert_eq!(hex(&sign(&sk, &msg)), v.sig, "vector {i}: signer");
        assert_eq!(
            verify(&unhex(v.pk), &msg, &unhex(v.sig)),
            Ok(()),
            "vector {i}: verify"
        );
    }
}

#[test]
fn every_single_bit_flip_of_a_signature_fails() {
    // Vector 2 (one-byte message) keeps this affordable in a debug build.
    let v = &vectors()[1];
    let pk = unhex(v.pk);
    let msg = unhex(&v.msg);
    let sig = unhex(v.sig);
    for bit in 0..512 {
        let mut bad = sig.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        assert!(verify(&pk, &msg, &bad).is_err(), "bit {bit} accepted");
    }
}

#[test]
fn bit_flips_of_message_and_key_fail() {
    let v = &vectors()[1];
    let pk = unhex(v.pk);
    let sig = unhex(v.sig);
    for bit in 0..8 {
        assert!(verify(&pk, &[0x72 ^ (1 << bit)], &sig).is_err());
    }
    for bit in 0..256 {
        let mut bad = pk.clone();
        bad[bit / 8] ^= 1 << (bit % 8);
        assert!(verify(&bad, &[0x72], &sig).is_err(), "key bit {bit}");
    }
}

#[test]
fn wrong_message_and_wrong_key() {
    let v = &vectors();
    let (pk0, sig0) = (unhex(v[0].pk), unhex(v[0].sig));
    assert_eq!(verify(&pk0, b"", &sig0), Ok(()));
    assert_eq!(verify(&pk0, b"x", &sig0), Err(VerifyError::Mismatch));
    assert!(verify(&unhex(v[1].pk), b"", &sig0).is_err());
}

fn order_bytes() -> [u8; 32] {
    scalar::from_limbs(&scalar::L)
}

#[test]
fn s_equal_to_l_is_rejected() {
    let v = &vectors()[0];
    let mut sig = unhex(v.sig);
    sig[32..].copy_from_slice(&order_bytes());
    assert_eq!(
        verify(&unhex(v.pk), b"", &sig),
        Err(VerifyError::NonCanonicalS)
    );
}

#[test]
fn s_plus_l_malleation_is_rejected() {
    // S + L is the same scalar mod L, so a cofactorless equation alone would
    // accept it; the canonicity rule must not. Vector 1's S is small enough
    // that S + L still fits 32 bytes.
    let v = &vectors()[0];
    let sig = unhex(v.sig);
    let s: [u8; 32] = sig[32..].try_into().unwrap();
    let l = scalar::L;
    let sl = scalar::to_limbs(&s);
    let mut out = [0u64; 4];
    let mut carry = false;
    for (o, (x, y)) in out.iter_mut().zip(sl.iter().zip(l.iter())) {
        let (a, c1) = x.overflowing_add(*y);
        let (b, c2) = a.overflowing_add(u64::from(carry));
        *o = b;
        carry = c1 || c2;
    }
    assert!(!carry, "S + L overflowed 256 bits");
    let mut mall = sig.clone();
    mall[32..].copy_from_slice(&scalar::from_limbs(&out));
    assert_eq!(
        verify(&unhex(v.pk), b"", &mall),
        Err(VerifyError::NonCanonicalS)
    );
    // Control: the unmodified signature is fine.
    assert_eq!(verify(&unhex(v.pk), b"", &sig), Ok(()));
}

/// The eight small-order points, compressed.
fn small_order_encodings() -> Vec<[u8; 32]> {
    let mut v = Vec::new();
    // identity
    let mut id = [0u8; 32];
    id[0] = 1;
    v.push(id);
    // order 2: y = -1 = p - 1
    let mut m1 = [0xffu8; 32];
    m1[0] = 0xec;
    m1[31] = 0x7f;
    v.push(m1);
    // order 4: y = 0, both signs
    let z = [0u8; 32];
    v.push(z);
    let mut zs = z;
    zs[31] = 0x80;
    v.push(zs);
    // order 8: y = 0x7a03ac9277fdc74ec6cc392cfa53202a0f67100d760b3cba4fd84d3d706a17c7
    let y8 = unhex("c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a");
    let mut a: [u8; 32] = y8.clone().try_into().unwrap();
    v.push(a);
    a[31] |= 0x80;
    v.push(a);
    // and the negation y -> p - y of the order-8 pair
    let y = Fe::from_bytes(&y8.try_into().unwrap()).neg().to_bytes();
    let mut b = y;
    v.push(b);
    b[31] |= 0x80;
    v.push(b);
    v
}

#[test]
fn small_order_points_are_recognised() {
    for (i, enc) in small_order_encodings().iter().enumerate() {
        let p = Point::decompress(enc).unwrap_or_else(|| panic!("encoding {i} not decodable"));
        assert!(p.is_small_order(), "encoding {i}");
    }
    assert!(!Point::basepoint().unwrap().is_small_order());
    assert!(Point::basepoint().unwrap().is_torsion_free());
}

#[test]
fn small_order_r_is_rejected() {
    let v = &vectors()[0];
    for (i, enc) in small_order_encodings().iter().enumerate() {
        let mut sig = unhex(v.sig);
        sig[..32].copy_from_slice(enc);
        // Some encodings (sign bit on x = 0) are rejected as undecodable
        // before the order check; either way the signature must fail.
        assert!(verify(&unhex(v.pk), b"", &sig).is_err(), "R encoding {i}");
    }
    // The ones that do decode are reported as small order specifically.
    let mut sig = unhex(v.sig);
    sig[..32].copy_from_slice(&small_order_encodings()[0]);
    assert_eq!(
        verify(&unhex(v.pk), b"", &sig),
        Err(VerifyError::SmallOrderR)
    );
}

#[test]
fn small_order_public_key_is_rejected_even_with_a_forged_looking_signature() {
    // With A = identity and R = identity, S = 0 satisfies [S]B = R + [k]A for
    // every message under a naive verifier. Strict verification refuses.
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&small_order_encodings()[0]);
    for enc in small_order_encodings() {
        assert!(verify(&enc, b"anything", &sig).is_err());
    }
    assert_eq!(
        verify(&small_order_encodings()[0], b"anything", &sig),
        Err(VerifyError::SmallOrderPublicKey)
    );
}

#[test]
fn non_canonical_encodings_are_rejected() {
    // y = p (non-canonical zero-ish) and y = p + 1 (non-canonical identity).
    let mut p = [0xffu8; 32];
    p[0] = 0xed;
    p[31] = 0x7f;
    assert!(Point::decompress(&p).is_none());
    let mut p1 = p;
    p1[0] = 0xee;
    assert!(Point::decompress(&p1).is_none());
    // Identity with the sign bit set (x = 0, sign 1).
    let mut id = [0u8; 32];
    id[0] = 1;
    id[31] = 0x80;
    assert!(Point::decompress(&id).is_none());
    // A non-canonical R in an otherwise valid signature.
    let v = &vectors()[0];
    let mut sig = unhex(v.sig);
    sig[..32].copy_from_slice(&p1);
    assert!(verify(&unhex(v.pk), b"", &sig).is_err());
}

#[test]
fn off_curve_key_is_rejected() {
    // Find a y with no matching x deterministically.
    let v = &vectors()[0];
    let sig = unhex(v.sig);
    let mut found = false;
    for b in 2u8..40 {
        let mut y = [0u8; 32];
        y[0] = b;
        if Point::decompress(&y).is_none() {
            assert!(verify(&y, b"", &sig).is_err());
            found = true;
            break;
        }
    }
    assert!(found);
}

#[test]
fn all_zero_signature_is_rejected() {
    let v = &vectors()[0];
    assert!(verify(&unhex(v.pk), b"", &[0u8; 64]).is_err());
    assert!(verify(&[0u8; 32], b"", &[0u8; 64]).is_err());
}

#[test]
fn truncated_and_oversized_inputs_are_rejected() {
    let v = &vectors()[0];
    let pk = unhex(v.pk);
    let sig = unhex(v.sig);
    for n in 0..64 {
        assert_eq!(verify(&pk, b"", &sig[..n]), Err(VerifyError::BadLength));
    }
    for n in 0..32 {
        assert_eq!(verify(&pk[..n], b"", &sig), Err(VerifyError::BadLength));
    }
    let mut long = sig.clone();
    long.push(0);
    assert_eq!(verify(&pk, b"", &long), Err(VerifyError::BadLength));
    let mut longk = pk.clone();
    longk.push(0);
    assert_eq!(verify(&longk, b"", &sig), Err(VerifyError::BadLength));
}

#[test]
fn signer_round_trips_over_varied_messages() {
    let mut seed = [7u8; 32];
    for n in [
        0usize, 1, 2, 31, 32, 63, 64, 111, 112, 113, 127, 128, 129, 300,
    ] {
        seed[0] = n as u8;
        let msg: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
        let pk = public_key_of(&seed);
        let sig = sign(&seed, &msg);
        assert_eq!(verify(&pk, &msg, &sig), Ok(()), "len {n}");
    }
}

#[test]
fn field_and_scalar_basics() {
    let i = Fe::sqrt_m1();
    assert!(i.square().eq(&Fe::ONE.neg()));
    let a = Fe::small(123456789);
    assert!(a.mul(&a.invert()).eq(&Fe::ONE));
    assert_eq!(Fe::ZERO.sub(&Fe::ONE).to_bytes()[0], 0xec);
    // L reduces to zero; L - 1 is canonical and stays.
    assert_eq!(scalar::reduce(&order_bytes()), [0u8; 32]);
    assert!(!scalar::is_canonical(&order_bytes()));
    assert!(scalar::is_canonical(&[0u8; 32]));
    // B has order L.
    assert!(
        Point::basepoint()
            .unwrap()
            .mul_scalar(&order_bytes())
            .is_identity()
    );
}
