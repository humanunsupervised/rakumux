//! Release signatures. A release's `checksums.txt` is signed with minisign
//! (Ed25519, prehashed) by a key built in from `install/release-keys.pub`,
//! with a trusted comment naming the release (`install/scripts/sign-release.sh`).
//! Nothing a release holds is trusted before its signature is: its
//! `checksums.txt` vouches for the packages only once this has checked it.
use anyhow::{bail, Context, Result};
use minisign_verify::{PublicKey, Signature};

/// The release signing keys this build carries (install/AGENTS.md, "Signed
/// releases"): any of them signs a release this build installs.
const RELEASE_KEYS: &str = include_str!("../../../install/release-keys.pub");

/// The signature of a release's `checksums.txt`, uploaded next to it.
pub(super) const SIGNATURE_ASSET: &str = "checksums.txt.minisig";

/// A signature file is four short lines.
pub(super) const MAX_SIGNATURE: usize = 4096;

/// The keys this build accepts releases from.
pub(super) fn release_keys() -> Result<Vec<PublicKey>> {
    parse_keys(RELEASE_KEYS)
}

/// One base64 minisign public key per line (an optional name after it);
/// `#` starts a comment.
pub(super) fn parse_keys(text: &str) -> Result<Vec<PublicKey>> {
    text.lines()
        .filter_map(|line| line.split('#').next()?.split_whitespace().next())
        .map(|key| {
            PublicKey::from_base64(key)
                .map_err(|_| anyhow::anyhow!("invalid release signing key {key}"))
        })
        .collect()
}

/// What the trusted comment of release `tag`'s signature begins with: so a
/// signature can't be moved onto another release.
pub(super) fn trusted_comment(tag: &str) -> String {
    format!("rakumux-release tag:{tag} file:checksums.txt")
}

/// `checksums` is release `version`'s, signed by one of `keys`. Errors say
/// why not; the caller installs nothing then.
pub(super) fn verify(
    checksums: &[u8],
    signature: &[u8],
    version: &semver::Version,
    keys: &[PublicKey],
) -> Result<()> {
    if keys.is_empty() {
        bail!("this build of Rakumux has no release signing keys, so it can't check an update; install it with https://rakumux.com/install.sh");
    }
    let signature = std::str::from_utf8(signature)
        .ok()
        .and_then(|text| Signature::decode(text).ok())
        .context("the release's signature is malformed; update was not installed")?;
    // Rejects a key id that isn't the key's, a legacy (not prehashed)
    // signature, and a trusted comment that isn't signed with the file.
    if !keys
        .iter()
        .any(|key| key.verify(checksums, &signature, false).is_ok())
    {
        bail!("the release isn't signed by a Rakumux release key; update was not installed");
    }
    let expected = trusted_comment(&format!("v{version}"));
    let comment = signature.trusted_comment();
    if comment != expected && !comment.starts_with(&format!("{expected} ")) {
        bail!(
            "the release's signature is for another release; update to {version} was not installed"
        );
    }
    Ok(())
}

/// Throwaway minisign keys for tests: what `minisign -G` and `minisign -S`
/// make, with no secret on disk.
#[cfg(test)]
pub(super) mod testing {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use blake2::{Blake2b512, Digest};
    use ring::rand::{SecureRandom, SystemRandom};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    pub struct TestKey {
        pair: Ed25519KeyPair,
        id: [u8; 8],
    }

    impl TestKey {
        pub fn generate() -> Self {
            let random = SystemRandom::new();
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&random).unwrap();
            let mut id = [0u8; 8];
            random.fill(&mut id).unwrap();
            Self {
                pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
                id,
            }
        }

        /// The public key as `install/release-keys.pub` lists it.
        pub fn public(&self) -> String {
            let mut bytes = b"Ed".to_vec();
            bytes.extend_from_slice(&self.id);
            bytes.extend_from_slice(self.pair.public_key().as_ref());
            STANDARD.encode(bytes)
        }

        /// A prehashed minisign signature file of `data`.
        pub fn sign(&self, data: &[u8], trusted_comment: &str) -> Vec<u8> {
            let hash = Blake2b512::digest(data);
            let signature = self.pair.sign(&hash);
            let mut line = b"ED".to_vec();
            line.extend_from_slice(&self.id);
            line.extend_from_slice(signature.as_ref());
            let mut global = signature.as_ref().to_vec();
            global.extend_from_slice(trusted_comment.as_bytes());
            format!(
                "untrusted comment: test signature\n{}\ntrusted comment: {trusted_comment}\n{}\n",
                STANDARD.encode(line),
                STANDARD.encode(self.pair.sign(&global))
            )
            .into_bytes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TestKey;
    use super::*;

    fn version(text: &str) -> semver::Version {
        semver::Version::parse(text).unwrap()
    }

    fn keys(keys: &[&TestKey]) -> Vec<PublicKey> {
        let text: String = keys.iter().map(|key| key.public() + " name\n").collect();
        parse_keys(&format!("# comment\n\n{text}")).unwrap()
    }

    const CHECKSUMS: &[u8] = b"0123  rakumux-linux-x64.tar.gz\n";

    #[test]
    fn a_release_signed_by_the_main_or_the_backup_key_is_accepted() {
        let (main, backup) = (TestKey::generate(), TestKey::generate());
        let accepted = keys(&[&main, &backup]);
        for key in [&main, &backup] {
            let signature = key.sign(CHECKSUMS, &trusted_comment("v0.1.39"));
            verify(CHECKSUMS, &signature, &version("0.1.39"), &accepted).unwrap();
        }
        // Fields after the release's own (when it was signed) are allowed.
        let signature = main.sign(
            CHECKSUMS,
            "rakumux-release tag:v0.1.39 file:checksums.txt timestamp:1",
        );
        verify(CHECKSUMS, &signature, &version("0.1.39"), &accepted).unwrap();
    }

    #[test]
    fn tampered_checksums_another_key_or_another_release_are_refused() {
        let (main, stranger) = (TestKey::generate(), TestKey::generate());
        let accepted = keys(&[&main]);
        let signature = main.sign(CHECKSUMS, &trusted_comment("v0.1.39"));
        let tampered = b"9999  rakumux-linux-x64.tar.gz\n";
        let error = verify(tampered, &signature, &version("0.1.39"), &accepted).unwrap_err();
        assert!(error.to_string().contains("isn't signed"), "{error}");
        let signature_of_stranger = stranger.sign(CHECKSUMS, &trusted_comment("v0.1.39"));
        assert!(verify(
            CHECKSUMS,
            &signature_of_stranger,
            &version("0.1.39"),
            &accepted
        )
        .is_err());
        // A real signature, replayed onto another release.
        for other in ["0.1.40", "0.1.3"] {
            let error = verify(CHECKSUMS, &signature, &version(other), &accepted).unwrap_err();
            assert!(error.to_string().contains("another release"), "{error}");
        }
        let prefix = main.sign(
            CHECKSUMS,
            "rakumux-release tag:v0.1.39 file:checksums.txt.x",
        );
        assert!(verify(CHECKSUMS, &prefix, &version("0.1.39"), &accepted).is_err());
    }

    #[test]
    fn an_edited_trusted_comment_or_a_malformed_signature_is_refused() {
        let main = TestKey::generate();
        let accepted = keys(&[&main]);
        let signature = main.sign(CHECKSUMS, &trusted_comment("v0.1.39"));
        let text = String::from_utf8(signature).unwrap();
        let moved = text.replace("tag:v0.1.39", "tag:v0.1.40");
        assert!(verify(CHECKSUMS, moved.as_bytes(), &version("0.1.40"), &accepted).is_err());
        for broken in [&b""[..], b"not a signature", &[0xff, 0xfe]] {
            let error = verify(CHECKSUMS, broken, &version("0.1.39"), &accepted).unwrap_err();
            assert!(error.to_string().contains("malformed"), "{error}");
        }
    }

    #[test]
    fn without_release_keys_nothing_is_accepted() {
        let main = TestKey::generate();
        let signature = main.sign(CHECKSUMS, &trusted_comment("v0.1.39"));
        let error = verify(CHECKSUMS, &signature, &version("0.1.39"), &[]).unwrap_err();
        assert!(
            error.to_string().contains("no release signing keys"),
            "{error}"
        );
        assert!(parse_keys("RWQnot-a-key\n").is_err());
        assert!(parse_keys("# only comments\n\n").unwrap().is_empty());
    }

    #[test]
    fn the_built_in_keys_are_readable_and_present() {
        // A build without keys would refuse every update.
        assert!(!release_keys().unwrap().is_empty());
    }
}
