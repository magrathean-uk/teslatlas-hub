// SPDX-License-Identifier: AGPL-3.0-only

mod source_identity;

fn main() {
    // The journey-doubles test feature (DEC-116) never reaches a release-profile build,
    // even with debug assertions turned on (DEC-120 b). Cargo sets PROFILE to "release"
    // for the release profile and every profile that inherits from it.
    if std::env::var_os("CARGO_FEATURE_JOURNEY_DOUBLES").is_some()
        && std::env::var("PROFILE").as_deref() == Ok("release")
    {
        panic!(
            "the journey-doubles feature is for test builds only; a release-profile build refuses it"
        );
    }

    const VARIABLE: &str = "TESLATLAS_HUB_SOURCE_COMMIT";
    println!("cargo:rerun-if-env-changed={VARIABLE}");

    let source_commit = match std::env::var(VARIABLE) {
        Ok(value) if source_identity::is_exact_commit(&value) => value,
        Ok(_) => panic!("{VARIABLE} must be exactly 40 lowercase hexadecimal characters"),
        Err(std::env::VarError::NotPresent) => source_identity::UNBOUND_SOURCE_COMMIT.to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("{VARIABLE} must be valid UTF-8")
        }
    };
    println!("cargo:rustc-env={VARIABLE}={source_commit}");
}
