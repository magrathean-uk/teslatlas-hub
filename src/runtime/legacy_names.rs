// SPDX-License-Identifier: AGPL-3.0-only

//! Exact historical format identifiers. Their bytes are part of stored digests.

pub const CAR_IDENTITY_V1: &str = "teslatlas-hub/teslamate-car-identity/v1\n";
pub const DIRECT_LOGICAL_PROJECTION_V1: &str =
    "teslatlas-hub/teslamate-direct-logical-projection/v1";
pub const DIRECT_SNAPSHOT_WITH_GEOFENCES_V1: &str =
    "teslatlas-hub/teslamate-direct-snapshot-with-geofences/v1";
pub const DIRECT_SOURCE_EVIDENCE_BINDING_V1: &str =
    "teslatlas-hub/teslamate-direct-source-evidence-binding/v1";
pub const DRIVE_ODOMETER_EVIDENCE_V1: &str = "teslatlas-hub/teslamate-drive-odometer-evidence/v1";
pub const LOGICAL_SNAPSHOT_V1: &str = "teslatlas-hub/teslamate-logical-snapshot/v1";
pub const LOGICAL_STATES_V1: &str = "teslatlas-hub/teslamate-logical-states/v1";
pub const LOGICAL_UPDATES_V1: &str = "teslatlas-hub/teslamate-logical-updates/v1";
pub const PROJECTION_STATE_SEALED_TRANSFER_V1: &str =
    "teslatlas-hub/teslamate-projection-state/sealed-transfer/v1";
pub const PROJECTION_STATE_V1: &str = "teslatlas-hub/teslamate-projection-state/v1";
pub const SOURCE_EVIDENCE_V1: &str = "teslatlas-hub/teslamate-source-evidence/v1";
pub const SOURCE_HISTORY_V2_STANDALONE_POSITIONS: &str =
    "teslatlas-hub/teslamate-source-history/v2-standalone-positions";
pub const STAGED_SOURCE_EVIDENCE_BINDING_V1: &str =
    "teslatlas-hub/teslamate-staged-source-evidence-binding/v1";
pub const STAGE_LOGICAL_CONTENT_V1: &str = "teslatlas-hub/teslamate-stage/logical-content/v1\0";
pub const STATE_TEXT_EVIDENCE_V1: &str = "teslatlas-hub/teslamate-state-text-evidence/v1";
pub const V4_UPDATES_RECEIPT_V1: &str = "teslatlas-teslamate-v4-updates-receipt-v1";

pub const PARITY_PRESERVED_KEY: &str = "allTeslamateFieldsPreserved";
pub const RECOVERY_PREVIOUS_KEY: &str = "teslamateKeyIncluded";

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn frozen_values_hash_to_the_recorded_digests() {
        let expected = [
            (
                PARITY_PRESERVED_KEY,
                "de7e06d146e6278dde5d72b8cb03123b6ad6627796fb1103ddd29560477cf422",
            ),
            (
                RECOVERY_PREVIOUS_KEY,
                "e420d8098f87e76ba789ddb44dc3288c574ecc0f82a6c28952a0a74a8eb56870",
            ),
            (
                CAR_IDENTITY_V1,
                "cf3c0e3eb8746a2bebf52bd0cdc14a2bcb6f7a68383b74204827a8fd44585866",
            ),
            (
                DIRECT_LOGICAL_PROJECTION_V1,
                "2dac0652549b3f7bbe77ac4034773426c7a076c643300aea06ead2b891b43265",
            ),
            (
                DIRECT_SNAPSHOT_WITH_GEOFENCES_V1,
                "4403bd117afd3ab026ffc2f4194cfa1882253d065193f6278f5cd0c4cb0471b1",
            ),
            (
                DIRECT_SOURCE_EVIDENCE_BINDING_V1,
                "fff51138f71b5ec6f710081732ac452aa6df3a759d4fca8798c509a50c414e96",
            ),
            (
                DRIVE_ODOMETER_EVIDENCE_V1,
                "3e70ead523f7a1b77d9f936895212a69b3460d9d84e4b0b86804ccb138a6c3a7",
            ),
            (
                LOGICAL_SNAPSHOT_V1,
                "327dc1f7ef4a8c38d13b9e695944e8b2e0b207281869defa96d3aea69b785663",
            ),
            (
                LOGICAL_STATES_V1,
                "55b6e6678f3bb7e288a92c352c8f221acae50b5ed757c8ef3e195e705b1c94c1",
            ),
            (
                LOGICAL_UPDATES_V1,
                "f49afc6e3321ecf93d7145a428c06895c0a6ad5c96cafd60ee5ad0b308fd664a",
            ),
            (
                PROJECTION_STATE_SEALED_TRANSFER_V1,
                "8b77664266e1bdfe5f9ed467ecc063306928cb3703caae703f3b17fb01b6eb80",
            ),
            (
                PROJECTION_STATE_V1,
                "9217a8bd3ee01ca813b9f90287d24557d0bd6524cdd2e61b9242fba4ae01100a",
            ),
            (
                SOURCE_EVIDENCE_V1,
                "9540aeea5ca0391e80001e2e9659497291548175bac3ba53f6ac7d726cb48f44",
            ),
            (
                SOURCE_HISTORY_V2_STANDALONE_POSITIONS,
                "121bb704cfb6bbb80cb6ae251a0f148541706a551a7d6ec4be46f56ae3919abd",
            ),
            (
                STAGED_SOURCE_EVIDENCE_BINDING_V1,
                "1686c827f97f017c0af47ce31877ce3e02c8524e82fd2a2c0437a434d5329cdf",
            ),
            (
                STAGE_LOGICAL_CONTENT_V1,
                "01665549e080d4256300e0284df2dcff02b8b7163476278cc53f8e91e7473c9a",
            ),
            (
                STATE_TEXT_EVIDENCE_V1,
                "b8f1cca5399cce6fb64f08473d6c04364014bed9e1df87109cc6233b47426df0",
            ),
            (
                V4_UPDATES_RECEIPT_V1,
                "05a53aaadf857306147726cc388a3803b87b5dd4f037db5f5b84fe9272f10cde",
            ),
        ];
        for (value, digest) in expected {
            assert_eq!(hex::encode(Sha256::digest(value.as_bytes())), digest);
        }
    }
}

#[cfg(test)]
mod serialized_names_tests {
    #[test]
    fn persisted_keys_keep_their_serialised_names() {
        let report = crate::source_db_parity::SourceDbParityReport::current();
        let encoded = serde_json::to_value(report).expect("encode persisted report");
        assert_eq!(
            encoded.get(super::PARITY_PRESERVED_KEY),
            Some(&serde_json::Value::Bool(false))
        );
        assert!(encoded.get("allSourceFieldsPreserved").is_none());
    }
}
