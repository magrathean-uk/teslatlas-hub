// SPDX-License-Identifier: AGPL-3.0-only

mod source_identity;

fn main() {
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
