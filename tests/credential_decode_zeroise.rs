// SPDX-License-Identifier: AGPL-3.0-only
#![forbid(unsafe_code)]

//! Safe observations of the zeroising wrapper and invalid second-token path.

use std::{cell::RefCell, rc::Rc};

use teslatlas_hub::credentials::{CredentialError, OwnerTokens};
use teslatlas_hub::auth::credential_sign_in::observe_file_token_drops;
use zeroize::{Zeroize, Zeroizing};

struct DropWitness {
    bytes: [u8; 16],
    observed: Rc<RefCell<Vec<u8>>>,
}

impl Zeroize for DropWitness {
    fn zeroize(&mut self) {
        self.bytes.zeroize();
    }
}

impl Drop for DropWitness {
    fn drop(&mut self) {
        *self.observed.borrow_mut() = self.bytes.to_vec();
    }
}

#[test]
fn zeroizing_invokes_zeroize_before_the_inner_value_is_dropped() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let unprotected = DropWitness {
        bytes: [b'X'; 16],
        observed: Rc::clone(&observed),
    };
    drop(unprotected);
    assert_eq!(&*observed.borrow(), &[b'X'; 16]);

    let protected = Zeroizing::new(DropWitness {
        bytes: [b'X'; 16],
        observed: Rc::clone(&observed),
    });
    drop(protected);
    assert_eq!(&*observed.borrow(), &[0; 16]);
}

#[test]
fn invalid_refresh_is_rejected_after_a_valid_access_input() {
    let observed = Rc::new(RefCell::new(Vec::new()));
    let output = Rc::clone(&observed);
    let result = observe_file_token_drops(
        move |before, after| output.borrow_mut().push((before, after)),
        || OwnerTokens::from_file_bytes(
            Zeroizing::new(vec![b'X'; 12_347]),
            Zeroizing::new(vec![0xff]),
        ),
    );
    assert!(matches!(result, Err(CredentialError::InvalidTokenBytes)));
    assert_eq!(&*observed.borrow(), &[(12_347, 0)]);
}
