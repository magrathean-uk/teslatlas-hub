// SPDX-License-Identifier: AGPL-3.0-only

//! Environment variables after the rename (DEC-160): the current name is read
//! first, then the earlier spelling, for one release. A set current name
//! always wins, even when it is empty.

use std::ffi::{OsStr, OsString};

/// `std::env::var_os` with the earlier-spelling fallback.
pub fn var_os(name: impl AsRef<OsStr>) -> Option<OsString> {
    var_os_from(name.as_ref(), |key| std::env::var_os(key))
}

/// `std::env::var` with the earlier-spelling fallback.
pub fn var(name: impl AsRef<OsStr>) -> Result<String, std::env::VarError> {
    match var_os(name) {
        Some(value) => value.into_string().map_err(std::env::VarError::NotUnicode),
        None => Err(std::env::VarError::NotPresent),
    }
}

/// The lookup with an injected environment, for tests.
pub fn var_os_from(name: &OsStr, lookup: impl Fn(&OsStr) -> Option<OsString>) -> Option<OsString> {
    if let Some(value) = lookup(name) {
        return Some(value);
    }
    let earlier = super::spelling::earlier(name.to_str()?)?;
    lookup(OsStr::new(&earlier))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn lookup<'a>(values: &'a HashMap<&'a str, &'a str>) -> impl Fn(&OsStr) -> Option<OsString> + 'a {
        |key| values.get(key.to_str()?).map(OsString::from)
    }

    #[test]
    fn the_current_name_wins_and_the_earlier_one_is_a_fallback() {
        let earlier = "TESLATLAS_HUB_DEVELOPMENT"; // rename:keep
        let only_earlier = HashMap::from([(earlier, "1")]);
        assert_eq!(var_os_from(OsStr::new("TESATLAS_HUB_DEVELOPMENT"), lookup(&only_earlier)), Some("1".into()));
        let both = HashMap::from([(earlier, "1"), ("TESATLAS_HUB_DEVELOPMENT", "")]);
        assert_eq!(var_os_from(OsStr::new("TESATLAS_HUB_DEVELOPMENT"), lookup(&both)), Some("".into()));
        let neither = HashMap::new();
        assert_eq!(var_os_from(OsStr::new("TESATLAS_HUB_DEVELOPMENT"), lookup(&neither)), None);
    }

    #[test]
    fn a_name_that_was_never_renamed_has_no_fallback() {
        let values = HashMap::from([("HOME", "/Users/owner")]);
        assert_eq!(var_os_from(OsStr::new("XDG_CACHE_HOME"), lookup(&values)), None);
        // Before the rename the product name itself is the earlier spelling: no second lookup.
        let earlier = "TESLATLAS_HUB_DEVELOPMENT"; // rename:keep
        assert_eq!(var_os_from(OsStr::new(earlier), lookup(&HashMap::new())), None);
    }
}
