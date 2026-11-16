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
    // After the rename (DEC-160) the earlier spelling is read when the current
    // name is unset, for one release; before it the two names are the same.
    let earlier = VARIABLE.replace("TESATLAS", "TESLATLAS"); // rename:keep
    println!("cargo:rerun-if-env-changed={earlier}");
    let value = match std::env::var(VARIABLE) {
        Err(std::env::VarError::NotPresent) => std::env::var(&earlier),
        other => other,
    };

    let source_commit = match value {
        Ok(value) if source_identity::is_exact_commit(&value) => value,
        Ok(_) => panic!("{VARIABLE} must be exactly 40 lowercase hexadecimal characters"),
        Err(std::env::VarError::NotPresent) => source_identity::UNBOUND_SOURCE_COMMIT.to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!("{VARIABLE} must be valid UTF-8")
        }
    };
    println!("cargo:rustc-env={VARIABLE}={source_commit}");
}
