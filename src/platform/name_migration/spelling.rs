// SPDX-License-Identifier: AGPL-3.0-only

//! The product's earlier spelling (DEC-160), used only to find state that an
//! installed Hub wrote before the rename. Every literal of the earlier
//! spelling in this file sits on a line marked `rename:keep`, so the scripted
//! rename leaves it alone; everything else derives from these three.
//!
//! Before the rename every current name still carries the earlier spelling,
//! so [`earlier`] returns `None` for all of them and the migrations do
//! nothing. After the rename the same calls name the earlier identity.

/// The earlier spelling in its three written forms, paired with the current one.
const FORMS: [(&str, &str); 3] = [
    ("tesatlas", "teslatlas"), // rename:keep
    ("Tesatlas", "Teslatlas"), // rename:keep
    ("TESATLAS", "TESLATLAS"), // rename:keep
];

/// The product's name as this build spells it; the scripted rename changes it,
/// and with it [`renamed`].
pub const PRODUCT_NAME: &str = "Teslatlas Hub";

/// Whether this build carries the current spelling, so an installation of the
/// earlier spelling may exist. Before the swap it is false and every
/// migration, fallback and wire alias is inert: the Hub behaves exactly as it
/// did before part A.
pub fn renamed() -> bool {
    #[cfg(test)]
    if let Some(forced) = FORCED.with(std::cell::Cell::get) {
        return forced;
    }
    earlier(PRODUCT_NAME).is_some()
}

#[cfg(test)]
thread_local! {
    static FORCED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Tests only: [`renamed`] answers `true` on this thread until the guard drops,
/// so the swapped behaviour can be exercised before the swap.
#[cfg(test)]
pub(crate) fn assume_renamed() -> impl Drop {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCED.with(|forced| forced.set(None));
        }
    }
    FORCED.with(|forced| forced.set(Some(true)));
    Restore
}

/// The name as it was spelled before the rename, or `None` when `name` does
/// not carry the current spelling (nothing was renamed, so nothing to find).
pub fn earlier(name: &str) -> Option<String> {
    swap(name, |(current, earlier)| (current, earlier))
}

/// The name in the current spelling, or `None` when `name` does not carry the
/// earlier spelling. Used to accept a peer that already sends the new form.
pub fn current(name: &str) -> Option<String> {
    swap(name, |(current, earlier)| (earlier, current))
}

/// The other spelling of `name`, in whichever direction applies.
pub fn other(name: &str) -> Option<String> {
    earlier(name).or_else(|| current(name))
}

type Pair = (&'static str, &'static str);

fn swap(name: &str, direction: impl Fn(Pair) -> Pair) -> Option<String> {
    let mut changed = false;
    let mut result = name.to_owned();
    for form in FORMS {
        let (from, to) = direction(form);
        if result.contains(from) {
            result = result.replace(from, to);
            changed = true;
        }
    }
    changed.then_some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_spelling_is_derived_from_the_current_one() {
        assert_eq!(earlier("com.tesatlas.hub").as_deref(), Some("com.teslatlas.hub")); // rename:keep
        assert_eq!(earlier("Tesatlas Hub").as_deref(), Some("Teslatlas Hub")); // rename:keep
        assert_eq!(earlier("TESATLAS_HUB_X").as_deref(), Some("TESLATLAS_HUB_X")); // rename:keep
        assert_eq!(earlier("uk.magrathean.tesatlas.hub.backup").as_deref(), Some("uk.magrathean.teslatlas.hub.backup")); // rename:keep
    }

    #[test]
    fn a_name_without_the_current_spelling_has_no_earlier_form() {
        assert_eq!(earlier("com.example.hub"), None);
        assert_eq!(earlier("unattended-backup"), None);
        let before_the_rename = "Teslatlas Hub"; // rename:keep
        assert_eq!(earlier(before_the_rename), None);
    }

    #[test]
    fn this_build_is_renamed_only_after_the_swap() {
        let before_the_swap = PRODUCT_NAME == "Teslatlas Hub"; // rename:keep
        assert_eq!(renamed(), !before_the_swap);
        {
            let _renamed = assume_renamed();
            assert!(renamed());
        }
        assert_eq!(renamed(), !before_the_swap);
    }

    #[test]
    fn other_spelling_works_in_both_directions() {
        assert_eq!(other("x-tesatlas-sync-profile").as_deref(), Some("x-teslatlas-sync-profile")); // rename:keep
        assert_eq!(current("x-teslatlas-sync-profile").as_deref(), Some("x-tesatlas-sync-profile")); // rename:keep
        assert_eq!(other("x-request-id"), None);
    }
}
