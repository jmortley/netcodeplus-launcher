//! Compiled-in trust root: the single Ed25519 public key that every update
//! manifest must verify against.
//!
//! The matching **private** key never touches a build server or this repo. It
//! lives encrypted on disk, and its passphrase is derived from a YubiKey
//! challenge-response credential (see the project's signing ceremony notes), so
//! signing requires physical possession of a provisioned YubiKey. Rotating this
//! key is a breaking change: every already-installed launcher trusts only the
//! value baked in here, so a rotation requires shipping a new launcher build.
//!
//! This is the minisign public key in raw base64 form (the value `rsign`/
//! `minisign` print after `-P`, i.e. the second line of `netcodeplus.pub`
//! without the `untrusted comment:` header). [`minisign_verify::PublicKey::
//! from_base64`] parses exactly this form.

use minisign_verify::PublicKey;

/// The launcher's root signing public key (minisign / Ed25519, base64).
///
/// Generated 2026-05-31 via the YubiKey-backed signing ceremony.
pub const PUBLIC_KEY_B64: &str = "RWSBsJd2OGt1NABcQTevaMe6jyptpP+DaGtAVVJwXSG2rkw7UIruxj/y";

/// Parse the compiled-in trust-root public key.
///
/// Infallible in practice: the input is a compile-time constant validated by
/// [`trust_root_parses`], so a failure here means the constant was edited to an
/// invalid value and is caught by that test before shipping.
///
/// Consumed by the update-verification command (F1, not yet wired) which passes
/// it to [`ncp_manifest::Manifest::load_and_verify`].
#[allow(dead_code)] // wired in by F1 (fetch_and_verify_manifest)
pub fn public_key() -> PublicKey {
    PublicKey::from_base64(PUBLIC_KEY_B64)
        .expect("compiled-in trust-root public key must be valid base64 minisign key")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiled-in key must parse as a minisign public key, so a bad edit
    /// to the constant fails CI rather than the field.
    #[test]
    fn trust_root_parses() {
        let _ = public_key();
    }
}
