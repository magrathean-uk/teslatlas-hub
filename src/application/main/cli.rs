// SPDX-License-Identifier: AGPL-3.0-only

// The command tree and its admission rules live in the library
// (`teslatlas_hub::runtime::operator`, M02-T10). The binary parses with the
// library's types under their old names, so its dispatch and tests keep them.
use teslatlas_hub::runtime::operator::cli::*;
