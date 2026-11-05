/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use crate::x509::TrustStore;
use crate::{Error, ca_bundle, ed25519};

fn with_version(bundle: &[u8], version: u32) -> Vec<u8> {
    let mut out = bundle.to_vec();
    out[8..12].copy_from_slice(&version.to_le_bytes());
    out
}

#[cfg(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_PARTITION)]
#[test]
fn installs_bundles_into_the_partition() {
    let dir = std::env::var("DEVKIT_CA_BUNDLE").expect("run inside the dev shell");
    let bundle = std::fs::read(format!("{dir}/ca_bundle.bin")).unwrap();
    let first = TrustStore::default_bundle().unwrap();
    assert!(first.len() > 100);
    let version = ca_bundle::version().unwrap();
    assert_eq!(first.bundle_version(), Some(version));

    assert_eq!(ca_bundle::install_unsigned(&with_version(&bundle, version + 1)), Ok(version + 1));
    assert_eq!(ca_bundle::install_unsigned(&with_version(&bundle, version + 2)), Err(Error::Busy));
    drop(first);

    let key = ed25519::SigningKey::from_seed(&[5; 32]);
    let next = with_version(&bundle, version + 2);
    let mut signed = next.clone();
    signed.extend_from_slice(&key.sign(&next));
    assert_eq!(ca_bundle::install(&next, &key.verifying_key()), Err(Error::Verify));
    let mut forged = signed.clone();
    forged[30] ^= 1;
    assert_eq!(ca_bundle::install(&forged, &key.verifying_key()), Err(Error::Verify));
    assert_eq!(ca_bundle::install(&signed, &key.verifying_key()), Ok(version + 2));
    assert_eq!(ca_bundle::install(&signed, &key.verifying_key()), Err(Error::Verify));
    assert_eq!(ca_bundle::version(), Some(version + 2));
    assert_eq!(TrustStore::default_bundle().unwrap().bundle_version(), Some(version + 2));
    assert_eq!(ca_bundle::install_unsigned(&bundle[..bundle.len() - 1]), Err(Error::Decode));
}

#[cfg(not(CONFIG_DEVKIT_CRYPTO_CA_BUNDLE_PARTITION))]
#[test]
fn install_needs_the_partition() {
    let dir = std::env::var("DEVKIT_CA_BUNDLE").expect("run inside the dev shell");
    let bundle = std::fs::read(format!("{dir}/ca_bundle.bin")).unwrap();
    assert_eq!(ca_bundle::install_unsigned(&with_version(&bundle, 1)), Err(Error::Unsupported));
}
