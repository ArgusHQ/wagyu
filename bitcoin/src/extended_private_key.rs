use model::crypto::{checksum, hash160};
use crate::private_key::BitcoinPrivateKey;
use crate::extended_public_key::BitcoinExtendedPublicKey;
use crate::network::Network;

use base58::{FromBase58, ToBase58};
use byteorder::{BigEndian, ByteOrder, ReadBytesExt};
use hmac::{Hmac, Mac};
use secp256k1::{Secp256k1, SecretKey, PublicKey};
use sha2::Sha512;

use std::{fmt, fmt::Display};
use std::io::Cursor;
use std::str::FromStr;

type HmacSha512 = Hmac<Sha512>;

/// Represents a Bitcoin Extended Private Key
//#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct BitcoinExtendedPrivateKey {
    /// The BitcoinPrivateKey
    pub private_key: BitcoinPrivateKey,

    /// The chain code corresponding to this extended private key.
    pub chain_code: [u8; 32],

    /// The network this extended private key can be used on.
    pub network: Network,

    /// 0x00 for master nodes, 0x01 for level-1 derived keys, ....
    pub depth: u8,

    /// The first 32 bits of the key identifier (hash160(ECDSA_public_key))
    pub parent_fingerprint: [u8; 4],

    /// This is ser32(i) for i in xi = xpar/i, with xi the key being serialized. (0x00000000 if master key)
    pub child_number: u32,
}

impl BitcoinExtendedPrivateKey {
    /// Generates new extended private key
    pub fn new(seed: &[u8]) -> Self {
        BitcoinExtendedPrivateKey::generate_master(seed)
    }

    /// Generates new master extended private key
    fn generate_master(seed: &[u8]) -> Self {
        let mut mac = HmacSha512::new_varkey(b"Bitcoin seed").expect("Error generating hmac");
        mac.input(seed);
        let result = mac.result().code();
        let (private_key, chain_code) = BitcoinExtendedPrivateKey::derive_private_key_and_chain_code(&result);
        Self {
            private_key,
            chain_code,
            network: Network::Mainnet,
            depth: 0,
            parent_fingerprint: [0; 4],
            child_number: 0x00000000,
        }
    }

    /// Generates the child extended private key at child_number from the current extended private key
    pub fn ckd_priv(&self, child_number: u32) -> Self {
        let mut mac = HmacSha512::new_varkey(
            &self.chain_code).expect("error generating hmac from chain code");
        let public_key_serialized = &PublicKey::from_secret_key(
            &Secp256k1::new(), &self.private_key.secret_key).serialize()[..];

        // Check whether i ≥ 2^31 (whether the child is a hardened key).
        // If so (hardened child): let I = HMAC-SHA512(Key = cpar, Data = 0x00 || ser256(kpar) || ser32(i)). (Note: The 0x00 pads the private key to make it 33 bytes long.)
        // If not (normal child): let I = HMAC-SHA512(Key = cpar, Data = serP(point(kpar)) || ser32(i)).
        if child_number >= 2_u32.pow(31) {
            let mut private_key_bytes = [0u8; 33];
            private_key_bytes[1..33].copy_from_slice(&self.private_key.secret_key[..]);
            mac.input(&private_key_bytes[..]);
        } else {
            mac.input(public_key_serialized);
        }

        let mut child_num_big_endian = [0u8; 4];
        BigEndian::write_u32(&mut child_num_big_endian, child_number);
        mac.input(&child_num_big_endian);

        let result = mac.result().code();

        let (mut private_key, chain_code) = BitcoinExtendedPrivateKey::derive_private_key_and_chain_code(&result);
        private_key.secret_key.add_assign(&Secp256k1::new(), &self.private_key.secret_key).expect("error add assign");

        let mut parent_fingerprint = [0u8; 4];
        parent_fingerprint.copy_from_slice(&hash160(public_key_serialized)[0..4]);

        Self {
            private_key,
            chain_code,
            network: self.network,
            depth: self.depth + 1,
            parent_fingerprint,
            child_number,

        }
    }

    /// Generates the extended public key associated with the current extended private key
    pub fn to_xpub(&self) -> BitcoinExtendedPublicKey {
        BitcoinExtendedPublicKey::from_private(&self)
    }

    /// Generates extended private key from Secp256k1 secret key, chain code, and network
    pub fn derive_private_key_and_chain_code(result: &[u8]) -> (BitcoinPrivateKey, [u8; 32]) {
        let private_key = BitcoinPrivateKey::from_secret_key(
            SecretKey::from_slice(&Secp256k1::without_caps(), &result[0..32]).expect("error generating secret key"),
            &Network::Mainnet,
            true,
        );

        let mut chain_code = [0u8; 32];
        chain_code[0..32].copy_from_slice(&result[32..]);

        return (private_key, chain_code);
    }
}

//impl Default for BitcoinExtendedPrivateKey {
//    /// Returns a randomly-generated mainnet Bitcoin private key.
//    fn default() -> Self {
//        Self::new(generate_random_seed)
//    }
//}

impl FromStr for BitcoinExtendedPrivateKey {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, &'static str> {
        let data = s.from_base58().expect("Error decoding base58 extended private key string");
        if data.len() != 82 {
            return Err("Invalid extended private key string length");
        }

        let network = if &data[0..4] == [0x04u8, 0x88, 0xAD, 0xE4] {
            Network::Mainnet
        } else if &data[0..4] == [0x04u8, 0x35, 0x83, 0x94] {
            Network::Testnet
        } else {
            return Err("Invalid network version");
        };

        let depth = data[4] as u8;

        let mut parent_fingerprint = [0u8; 4];
        parent_fingerprint.copy_from_slice(&data[5..9]);

        let child_number: u32 = Cursor::new(&data[9..13]).read_u32::<BigEndian>().unwrap();

        let mut chain_code = [0u8; 32];
        chain_code.copy_from_slice(&data[13..45]);

        let secp = Secp256k1::new();
        let private_key = BitcoinPrivateKey::from_secret_key(
            SecretKey::from_slice(&secp, &data[46..78]).expect("Error decoding secret key string"),
            &network,
            true);

        let expected = &data[78..82];
        let checksum = &checksum(&data[0..78])[0..4];

        match *expected == *checksum {
            true => Ok(Self {
                private_key,
                chain_code,
                network,
                depth,
                parent_fingerprint,
                child_number
            }),
            false => Err("Invalid extended private key")
        }
    }
}

impl Display for BitcoinExtendedPrivateKey {
    /// BIP32 serialization format: https://github.com/bitcoin/bips/blob/master/bip-0032.mediawiki#serialization-format
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let mut result = [0u8; 82];
        result[0..4].copy_from_slice(&match self.network {
            Network::Mainnet => [0x04, 0x88, 0xAD, 0xE4],
            Network::Testnet => [0x04, 0x35, 0x83, 0x94],
        }[..]);
        result[4] = self.depth as u8;
        result[5..9].copy_from_slice(&self.parent_fingerprint[..]);

        BigEndian::write_u32(&mut result[9..13], u32::from(self.child_number));

        result[13..45].copy_from_slice(&self.chain_code[..]);
        result[45] = 0;
        result[46..78].copy_from_slice(&self.private_key.secret_key[..]);

        let checksum = &checksum(&result[0..78])[0..4];
        result[78..82].copy_from_slice(&checksum);

        fmt.write_str(&result.to_base58())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use hex;

    fn test_from_str(
        expected_secret_key: &str,
        expected_chain_code: &str,
        expected_depth: u8,
        expected_parent_fingerprint: &str,
        expected_child_number: u32,
        expected_xpriv_serialized: &str
    ) {
        let xpriv = BitcoinExtendedPrivateKey::from_str(&expected_xpriv_serialized).expect("error generating xpriv object");
        assert_eq!(expected_secret_key, xpriv.private_key.secret_key.to_string());
        assert_eq!(expected_chain_code, hex::encode(xpriv.chain_code));
        assert_eq!(expected_depth, xpriv.depth);
        assert_eq!(expected_parent_fingerprint, hex::encode(xpriv.parent_fingerprint));
        assert_eq!(expected_child_number, xpriv.child_number);
        assert_eq!(expected_xpriv_serialized, xpriv.to_string());
    }

    fn test_new(
        expected_secret_key: &str,
        expected_chain_code: &str,
        expected_parent_fingerprint: &str,
        expected_xpriv_serialized: &str,
        seed: &str,
    ) {
        let seed_bytes = hex::decode(seed).expect("error decoding hex seed");
        let xpriv = BitcoinExtendedPrivateKey::new(&seed_bytes);
        assert_eq!(expected_secret_key, xpriv.private_key.secret_key.to_string());
        assert_eq!(expected_chain_code, hex::encode(xpriv.chain_code));
        assert_eq!(0, xpriv.depth);
        assert_eq!(expected_parent_fingerprint, hex::encode(xpriv.parent_fingerprint));
        assert_eq!(0, xpriv.child_number);
        assert_eq!(expected_xpriv_serialized, xpriv.to_string());
    }

    fn test_to_xpub(expected_xpub_serialized: &str, xpriv: &BitcoinExtendedPrivateKey) {
        let xpub = xpriv.to_xpub();
        assert_eq!(expected_xpub_serialized, xpub.to_string());
    }

    fn test_ckd_priv(
        expected_secret_key: &str,
        expected_chain_code: &str,
        expected_parent_fingerprint: &str,
        expected_xpriv_serialized: &str,
        expected_xpub_serialized: &str,
        parent_xpriv: &BitcoinExtendedPrivateKey,
        child_number: u32,
    ) -> BitcoinExtendedPrivateKey {
        let child_xpriv = parent_xpriv.ckd_priv(child_number);
        assert_eq!(expected_secret_key, child_xpriv.private_key.secret_key.to_string());
        assert_eq!(expected_chain_code, hex::encode(child_xpriv.chain_code));
        assert_eq!(expected_parent_fingerprint, hex::encode(child_xpriv.parent_fingerprint));
        assert_eq!(expected_xpriv_serialized, child_xpriv.to_string());
        assert_eq!(expected_xpub_serialized, child_xpriv.to_xpub().to_string());
        assert_eq!(child_number, child_xpriv.child_number);

        child_xpriv
    }

    /// Test vectors from https://en.bitcoin.it/wiki/BIP_0032_TestVectors
    mod bip32_default {
        use super::*;

        // (depth, master_seed, secret_key, chain_code, parent_fingerprint, xpriv, xpub)
        const KEYPAIR_TREE_HARDENED: [(&str, &str, &str, &str, &str, &str, &str); 2] = [
            (
                "0x00",
                "000102030405060708090a0b0c0d0e0f",
                "e8f32e723decf4051aefac8e2c93c9c5b214313817cdb01a1494b917c8436b35",
                "873dff81c02f525623fd1fe5167eac3a55a049de3d314bb42ee227ffed37d508",
                "00000000",
                "xprv9s21ZrQH143K3QTDL4LXw2F7HEK3wJUD2nW2nRk4stbPy6cq3jPPqjiChkVvvNKmPGJxWUtg6LnF5kejMRNNU3TGtRBeJgk33yuGBxrMPHi",
                "xpub661MyMwAqRbcFtXgS5sYJABqqG9YLmC4Q1Rdap9gSE8NqtwybGhePY2gZ29ESFjqJoCu1Rupje8YtGqsefD265TMg7usUDFdp6W1EGMcet8"
            ),
            (
                "0x01",
                "000102030405060708090a0b0c0d0e0f",
                "edb2e14f9ee77d26dd93b4ecede8d16ed408ce149b6cd80b0715a2d911a0afea",
                "47fdacbd0f1097043b78c63c20c34ef4ed9a111d980047ad16282c7ae6236141",
                "3442193e",
                "xprv9uHRZZhk6KAJC1avXpDAp4MDc3sQKNxDiPvvkX8Br5ngLNv1TxvUxt4cV1rGL5hj6KCesnDYUhd7oWgT11eZG7XnxHrnYeSvkzY7d2bhkJ7",
                "xpub68Gmy5EdvgibQVfPdqkBBCHxA5htiqg55crXYuXoQRKfDBFA1WEjWgP6LHhwBZeNK1VTsfTFUHCdrfp1bgwQ9xv5ski8PX9rL2dZXvgGDnw"
            )
        ];
        // (depth, master_seed, secret_key, chain_code, parent_fingerprint, xpriv, xpub)
        const KEYPAIR_TREE_NORMAL: [(&str, &str, &str, &str, &str, &str, &str); 2] = [
            (
                "0x00",
                "fffcf9f6f3f0edeae7e4e1dedbd8d5d2cfccc9c6c3c0bdbab7b4b1aeaba8a5a29f9c999693908d8a8784817e7b7875726f6c696663605d5a5754514e4b484542",
                "4b03d6fc340455b363f51020ad3ecca4f0850280cf436c70c727923f6db46c3e",
                "60499f801b896d83179a4374aeb7822aaeaceaa0db1f85ee3e904c4defbd9689",
                "00000000",
                "xprv9s21ZrQH143K31xYSDQpPDxsXRTUcvj2iNHm5NUtrGiGG5e2DtALGdso3pGz6ssrdK4PFmM8NSpSBHNqPqm55Qn3LqFtT2emdEXVYsCzC2U",
                "xpub661MyMwAqRbcFW31YEwpkMuc5THy2PSt5bDMsktWQcFF8syAmRUapSCGu8ED9W6oDMSgv6Zz8idoc4a6mr8BDzTJY47LJhkJ8UB7WEGuduB"
            ),
            (
                "0x01",
                "fffcf9f6f3f0edeae7e4e1dedbd8d5d2cfccc9c6c3c0bdbab7b4b1aeaba8a5a29f9c999693908d8a8784817e7b7875726f6c696663605d5a5754514e4b484542",
                "abe74a98f6c7eabee0428f53798f0ab8aa1bd37873999041703c742f15ac7e1e",
                "f0909affaa7ee7abe5dd4e100598d4dc53cd709d5a5c2cac40e7412f232f7c9c",
                "bd16bee5",
                "xprv9vHkqa6EV4sPZHYqZznhT2NPtPCjKuDKGY38FBWLvgaDx45zo9WQRUT3dKYnjwih2yJD9mkrocEZXo1ex8G81dwSM1fwqWpWkeS3v86pgKt",
                "xpub69H7F5d8KSRgmmdJg2KhpAK8SR3DjMwAdkxj3ZuxV27CprR9LgpeyGmXUbC6wb7ERfvrnKZjXoUmmDznezpbZb7ap6r1D3tgFxHmwMkQTPH"
            )
        ];

        #[test]
        fn test_from_str_hardened() {
            let (
                _,
                _,
                secret_key,
                chain_code,
                parent_fingerprint,
                xpriv,
                _
            ) = KEYPAIR_TREE_HARDENED[0];
            test_from_str(
                secret_key,
                chain_code,
                0,
                parent_fingerprint,
                0,
                xpriv
            );
        }

        #[test]
        fn test_from_str_normal() {
            let (
                _,
                _,
                secret_key,
                chain_code,
                parent_fingerprint,
                xpriv,
                _
            ) = KEYPAIR_TREE_HARDENED[0];
            test_from_str(
                secret_key,
                chain_code,
                0,
                parent_fingerprint,
                0,
                xpriv
            );
        }

        #[test]
        fn test_new_hardended() {
            let (_,
                seed,
                secret_key,
                chain_code,
                parent_fingerprint,
                xpriv,
                _
            ) = KEYPAIR_TREE_HARDENED[0];
            test_new(
                secret_key,
                chain_code,
                parent_fingerprint,
                xpriv,
                seed
            );
        }

        #[test]
        fn test_new_normal() {
            let (
                _,
                seed,
                secret_key,
                chain_code,
                parent_fingerprint,
                xpriv,
                _
            ) = KEYPAIR_TREE_NORMAL[0];
            test_new(
                secret_key,
                chain_code,
                parent_fingerprint,
                xpriv,
                seed
            );
        }


        #[test]
        fn test_to_xpub_hardened() {
            let (_, seed, _, _, _, _, extended_public_key) = KEYPAIR_TREE_HARDENED[0];
            let seed_bytes = hex::decode(seed).unwrap();
            let xpriv = BitcoinExtendedPrivateKey::new(&seed_bytes);
            test_to_xpub(extended_public_key, &xpriv);
        }

        #[test]
        fn test_to_xpub_normal() {
            let (_, seed, _, _, _, _, extended_public_key) = KEYPAIR_TREE_NORMAL[0];
            let seed_bytes = hex::decode(seed).unwrap();
            let xpriv = BitcoinExtendedPrivateKey::new(&seed_bytes);
            test_to_xpub(extended_public_key, &xpriv);
        }

        #[test]
        fn test_ckd_priv_hardened() {
            let (_, seed, _, _, _, _, _) = KEYPAIR_TREE_HARDENED[0];
            let seed_bytes = hex::decode(seed).unwrap();
            let mut parent_xpriv = BitcoinExtendedPrivateKey::new(&seed_bytes);
            for (i,
                (
                    _,
                    _,
                    secret_key,
                    chain_code,
                    parent_fingerprint,
                    xpriv,
                    xpub
                )
            ) in KEYPAIR_TREE_HARDENED[1..].iter_mut().enumerate() {
                parent_xpriv = test_ckd_priv(
                    secret_key,
                    chain_code,
                    parent_fingerprint,
                    xpriv,
                    xpub,
                    &parent_xpriv,
                    2_u32.pow(31) + (i as u32)
                );
            }
        }

        #[test]
        fn test_ckd_priv_normal() {
            let (_, seed, _, _, _, _, _) = KEYPAIR_TREE_NORMAL[0];
            let seed_bytes = hex::decode(seed).unwrap();
            let mut parent_xpriv = BitcoinExtendedPrivateKey::new(&seed_bytes);
            for (i,
                (
                    _,
                    _,
                    secret_key,
                    chain_code,
                    parent_fingerprint,
                    xpriv,
                    xpub
                )
            ) in KEYPAIR_TREE_NORMAL[1..].iter_mut().enumerate() {
                parent_xpriv = test_ckd_priv(
                    secret_key,
                    chain_code,
                    parent_fingerprint,
                    xpriv,
                    xpub,
                    &parent_xpriv,
                    i as u32
                );
            }
        }
    }
}