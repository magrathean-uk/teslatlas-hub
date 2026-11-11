// SPDX-License-Identifier: AGPL-3.0-only

//! Who may change the issuer a Hub trusts (design 2.2, review finding 3).
//!
//! The control channel is reachable by any process of the Hub's own user, so
//! a trust document arriving on it proves nothing by itself. Once a Hub
//! trusts an issuer, its key id is pinned outside the data directory, and a
//! new trust document (a new epoch) is accepted only when the pinned issuer
//! endorsed it: `window issuer-trust` signs the document with the issuer key.
//! Anything else, the first trust of a fresh Hub included, is a bootstrap and
//! needs the owner's confirmation in the logged-in session, which another
//! process of the same user cannot give.

use std::{fmt, path::Path, process::Command, sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};

use super::admission::OperatorRefusal;
use crate::collection::window::{
    AnchorStore, TrustDocument, WindowController,
    grant::key_id_of,
    state::WindowStore,
};

/// Format of a trust document endorsed by its issuer.
pub const ENDORSED_FORMAT: &str = "window-trust-endorsed/1";
/// Domain of the endorsement signature.
const ENDORSEMENT_DOMAIN: &[u8] = b"teslatlas-window-trust-endorsement/1\n";

pub const OWNER_CONFIRMATION_REFUSED: &str = "owner_confirmation_refused";
pub const TRUST_ENDORSEMENT_INVALID: &str = "trust_endorsement_invalid";
pub const ISSUER_KEY_UNREADABLE: &str = "issuer_key_unreadable";

/// How long the owner has to answer the confirmation.
pub const CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(120);

/// A trust document with the issuer's signature over it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndorsedTrust {
    pub format: String,
    pub document: TrustDocument,
    /// Base64 Ed25519 signature by the document's own key.
    pub endorsement: String,
}

fn endorsement_message(document: &TrustDocument) -> Result<Vec<u8>, OperatorRefusal> {
    let mut message = ENDORSEMENT_DOMAIN.to_vec();
    message.extend(document.to_bytes().map_err(OperatorRefusal::window)?);
    Ok(message)
}

fn unreadable() -> OperatorRefusal {
    OperatorRefusal::new(ISSUER_KEY_UNREADABLE, "the issuer key file cannot be used")
}

/// Reads the issuer's private key file (hex seed, 0600, a regular file).
fn load_issuer_key(path: &Path) -> Result<SigningKey, OperatorRefusal> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| unreadable())?;
    if !metadata.is_file() || metadata.mode() & 0o177 != 0 {
        return Err(unreadable());
    }
    let text = zeroize::Zeroizing::new(std::fs::read_to_string(path).map_err(|_| unreadable())?);
    let seed: zeroize::Zeroizing<[u8; 32]> = zeroize::Zeroizing::new(
        hex::decode(text.trim())
            .map_err(|_| unreadable())?
            .try_into()
            .map_err(|_| unreadable())?,
    );
    Ok(SigningKey::from_bytes(&seed))
}

/// The endorsed trust file for `document`, signed with the issuer key at
/// `key_file`, which must be the document's own key.
pub fn endorse(document: &TrustDocument, key_file: &Path) -> Result<Vec<u8>, OperatorRefusal> {
    let key = load_issuer_key(key_file)?;
    if key_id_of(&key.verifying_key()) != document.issuer_key_id {
        return Err(unreadable());
    }
    let signature = key.sign(&endorsement_message(document)?);
    let endorsed = EndorsedTrust {
        format: ENDORSED_FORMAT.to_owned(),
        document: document.clone(),
        endorsement: STANDARD.encode(signature.to_bytes()),
    };
    let mut bytes = serde_json::to_vec_pretty(&endorsed)
        .map_err(|_| OperatorRefusal::new("window_io", "the trust document could not be written"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// A trust file as `window trust` reads it: a plain document (a bootstrap) or
/// an endorsed one.
pub fn parse_trust(bytes: &[u8]) -> Result<(TrustDocument, Option<Vec<u8>>), OperatorRefusal> {
    if let Ok(endorsed) = serde_json::from_slice::<EndorsedTrust>(bytes) {
        if endorsed.format != ENDORSED_FORMAT {
            return Err(OperatorRefusal::new(TRUST_ENDORSEMENT_INVALID, "the trust file format is unknown"));
        }
        endorsed.document.verifying_key().map_err(OperatorRefusal::window)?;
        let signature = STANDARD
            .decode(endorsed.endorsement.as_bytes())
            .map_err(|_| OperatorRefusal::new(TRUST_ENDORSEMENT_INVALID, "the endorsement is not valid base64"))?;
        return Ok((endorsed.document, Some(signature)));
    }
    let document = TrustDocument::parse(bytes).map_err(OperatorRefusal::window)?;
    Ok((document, None))
}

/// Whether `signature` is the issuer `pinned_key_id`'s endorsement of
/// `document`.
pub fn endorsed_by(document: &TrustDocument, signature: &[u8], pinned_key_id: &str) -> bool {
    if document.issuer_key_id != pinned_key_id {
        return false;
    }
    let (Ok(key), Ok(signature), Ok(message)) = (
        document.verifying_key(),
        Signature::from_slice(signature),
        endorsement_message(document),
    ) else {
        return false;
    };
    key.verify_strict(&message, &signature).is_ok()
}

/// The owner's answer to "trust this issuer?", asked in the logged-in session.
pub trait OwnerConfirmation: Send + Sync {
    /// Blocks until the owner answers; `false` on a decline, a timeout or no
    /// session to ask in.
    fn confirm_issuer(&self, key_id: &str, trust_epoch: u64, replacing: Option<&str>) -> bool;
}

/// Refuses every bootstrap: for hosts with no session to ask in.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoConfirmation;

impl OwnerConfirmation for NoConfirmation {
    fn confirm_issuer(&self, _: &str, _: u64, _: Option<&str>) -> bool {
        false
    }
}

/// A macOS dialog in the owner's session (`osascript`). Answering it takes
/// the owner's click; another process of the same user cannot press its
/// button without the Accessibility permission.
#[derive(Debug, Default, Clone, Copy)]
pub struct DialogConfirmation;

impl DialogConfirmation {
    fn script(key_id: &str, trust_epoch: u64, replacing: Option<&str>) -> String {
        // Key ids are `ed25519-sha256-` and hex: nothing to escape.
        let clean = |text: &str| -> String {
            text.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect()
        };
        let replacing = replacing
            .map(|old| format!(" It replaces the trusted issuer {}.", clean(old)))
            .unwrap_or_default();
        format!(
            "display dialog \"Trust window issuer {} (trust epoch {trust_epoch}) for this Hub?{replacing} Choose Trust only if you started this.\" buttons {{\"Don't Trust\", \"Trust\"}} default button \"Don't Trust\" with title \"Teslatlas Hub\" with icon caution giving up after {}",
            clean(key_id),
            CONFIRMATION_TIMEOUT.as_secs()
        )
    }
}

impl OwnerConfirmation for DialogConfirmation {
    fn confirm_issuer(&self, key_id: &str, trust_epoch: u64, replacing: Option<&str>) -> bool {
        if !cfg!(target_vendor = "apple") {
            return false;
        }
        let Ok(output) = Command::new("/usr/bin/osascript")
            .args(["-e", &Self::script(key_id, trust_epoch, replacing)])
            .stdin(std::process::Stdio::null())
            .output()
        else {
            return false;
        };
        let answer = String::from_utf8_lossy(&output.stdout);
        output.status.success()
            && answer.contains("button returned:Trust")
            && !answer.contains("gave up:true")
    }
}

/// The pinned issuer and the owner's confirmation, consulted before the
/// controller is asked to trust a document.
#[derive(Clone)]
pub struct TrustAuthority {
    pin: Arc<dyn AnchorStore>,
    pin_name: String,
    confirmation: Arc<dyn OwnerConfirmation>,
}

impl fmt::Debug for TrustAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustAuthority").field("pin_name", &self.pin_name).finish_non_exhaustive()
    }
}

impl TrustAuthority {
    pub fn new(pin: Arc<dyn AnchorStore>, host_id: &str, confirmation: Arc<dyn OwnerConfirmation>) -> Self {
        Self {
            pin,
            pin_name: format!("window-issuer-key-{host_id}"),
            confirmation,
        }
    }

    /// The pinned key id, else the key the window store trusts now.
    fn pinned(&self, window_dir: &Path) -> Option<String> {
        if let Ok(Some(pinned)) = self.pin.read(&self.pin_name) {
            return Some(pinned);
        }
        WindowStore::new(window_dir)
            .load()
            .ok()
            .and_then(|loaded| loaded.trust)
            .map(|record| record.document.issuer_key_id)
    }

    /// Trusts the document in `bytes` if the pinned issuer endorsed it or the
    /// owner confirms it, then pins its key.
    pub fn trust(
        &self,
        controller: &WindowController,
        window_dir: &Path,
        bytes: &[u8],
    ) -> Result<(), OperatorRefusal> {
        let (document, endorsement) = parse_trust(bytes)?;
        let status = controller.status();
        if !status.faulted && status.phase != crate::collection::window::Phase::Closed {
            return Err(OperatorRefusal::window(crate::collection::window::Refusal::WindowActive));
        }
        let pinned = self.pinned(window_dir);
        let endorsed = match (&pinned, &endorsement) {
            (Some(pinned), Some(signature)) => endorsed_by(&document, signature, pinned),
            _ => false,
        };
        if !endorsed
            && !self.confirmation.confirm_issuer(
                &document.issuer_key_id,
                document.trust_epoch,
                pinned.as_deref().filter(|old| *old != document.issuer_key_id),
            )
        {
            return Err(OperatorRefusal::new(
                OWNER_CONFIRMATION_REFUSED,
                "a new issuer needs the owner's confirmation on this Mac; a new epoch of the trusted issuer needs its endorsed trust file",
            ));
        }
        controller.trust(&document).map_err(OperatorRefusal::window)?;
        if self.pin.write(&self.pin_name, &document.issuer_key_id).is_err() {
            // The window store holds the same key id, which `pinned` reads.
            tracing::warn!("the issuer pin could not be written");
        }
        Ok(())
    }
}
