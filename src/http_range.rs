//! Strict single-range parsing for immutable transport-pack objects.
//!
//! The pack endpoint deliberately supports one HTTP byte range only.  This is
//! enough for resumable `URLSessionDownloadTask` downloads while avoiding
//! multipart response generation, ambiguous cache behaviour, and unbounded
//! range lists.

use thiserror::Error;

/// A resolved inclusive byte interval within one non-empty representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedByteRange {
    /// First byte to send, inclusive.
    pub start: u64,
    /// Last byte to send, inclusive.
    pub end: u64,
    /// Length of the complete representation, not the selected range.
    pub complete_length: u64,
    /// `true` if a `Range` header selected the interval.
    pub requested: bool,
}

impl ResolvedByteRange {
    /// Number of bytes in this inclusive interval.
    pub const fn len(self) -> u64 {
        self.end - self.start + 1
    }

    pub const fn is_empty(self) -> bool {
        false
    }

    pub const fn is_partial(self) -> bool {
        self.requested
    }

    /// RFC 9110 `Content-Range` value for a satisfiable response.
    pub fn content_range(self) -> String {
        format!("bytes {}-{}/{}", self.start, self.end, self.complete_length)
    }
}

/// Parse one `Range` field value against a representation length.
///
/// `None` selects the full representation.  A present header must be exactly
/// one `bytes` range: `bytes=start-end`, `bytes=start-`, or `bytes=-suffix`.
/// Whitespace, other range units, comma-separated ranges, signed numbers, and
/// integer overflow are rejected rather than quietly reinterpreted.
pub fn parse_single_range(
    header: Option<&str>,
    complete_length: u64,
) -> Result<ResolvedByteRange, RangeError> {
    if complete_length == 0 {
        return Err(RangeError::EmptyRepresentation);
    }
    let Some(header) = header else {
        return Ok(ResolvedByteRange {
            start: 0,
            end: complete_length - 1,
            complete_length,
            requested: false,
        });
    };

    let value = header
        .strip_prefix("bytes=")
        .ok_or_else(|| invalid_unit_or_syntax(header))?;
    if value.is_empty() {
        return Err(RangeError::Malformed);
    }
    if value.contains(',') {
        return Err(RangeError::MultipleRanges);
    }
    if value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(RangeError::Malformed);
    }

    let (first, last) = value.split_once('-').ok_or(RangeError::Malformed)?;
    if last.contains('-') {
        return Err(RangeError::Malformed);
    }
    match (first.is_empty(), last.is_empty()) {
        (true, true) => Err(RangeError::Malformed),
        (true, false) => resolve_suffix(parse_decimal(last)?, complete_length),
        (false, true) => resolve_open_ended(parse_decimal(first)?, complete_length),
        (false, false) => {
            resolve_bounded(parse_decimal(first)?, parse_decimal(last)?, complete_length)
        }
    }
}

/// RFC 9110 `Content-Range` value for a rejected/unsatisfiable request.
pub fn unsatisfied_content_range(complete_length: u64) -> String {
    format!("bytes */{complete_length}")
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    #[error("the representation has no bytes to range over")]
    EmptyRepresentation,
    #[error("only the bytes range unit is supported")]
    UnsupportedUnit,
    #[error("multiple ranges are not supported")]
    MultipleRanges,
    #[error("range header is malformed")]
    Malformed,
    #[error("range number overflows an unsigned 64-bit integer")]
    Overflow,
    #[error("range is unsatisfiable for this representation")]
    Unsatisfiable,
    #[error("suffix range must request at least one byte")]
    ZeroLengthSuffix,
}

fn invalid_unit_or_syntax(header: &str) -> RangeError {
    if header.contains('=') {
        RangeError::UnsupportedUnit
    } else {
        RangeError::Malformed
    }
}

fn parse_decimal(value: &str) -> Result<u64, RangeError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RangeError::Malformed);
    }
    let mut number = 0_u64;
    for digit in value.bytes() {
        number = number
            .checked_mul(10)
            .and_then(|number| number.checked_add(u64::from(digit - b'0')))
            .ok_or(RangeError::Overflow)?;
    }
    Ok(number)
}

fn resolve_bounded(
    start: u64,
    requested_end: u64,
    complete_length: u64,
) -> Result<ResolvedByteRange, RangeError> {
    if start >= complete_length || requested_end < start {
        return Err(RangeError::Unsatisfiable);
    }
    Ok(ResolvedByteRange {
        start,
        end: requested_end.min(complete_length - 1),
        complete_length,
        requested: true,
    })
}

fn resolve_open_ended(start: u64, complete_length: u64) -> Result<ResolvedByteRange, RangeError> {
    if start >= complete_length {
        return Err(RangeError::Unsatisfiable);
    }
    Ok(ResolvedByteRange {
        start,
        end: complete_length - 1,
        complete_length,
        requested: true,
    })
}

fn resolve_suffix(
    suffix_length: u64,
    complete_length: u64,
) -> Result<ResolvedByteRange, RangeError> {
    if suffix_length == 0 {
        return Err(RangeError::ZeroLengthSuffix);
    }
    let length = suffix_length.min(complete_length);
    Ok(ResolvedByteRange {
        start: complete_length - length,
        end: complete_length - 1,
        complete_length,
        requested: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_range_selects_the_whole_nonempty_representation() {
        let range = parse_single_range(None, 12).expect("whole representation");
        assert_eq!(range.start, 0);
        assert_eq!(range.end, 11);
        assert_eq!(range.len(), 12);
        assert!(!range.is_partial());
        assert_eq!(range.content_range(), "bytes 0-11/12");
    }

    #[test]
    fn resolves_bounded_and_open_ended_ranges() {
        let bounded = parse_single_range(Some("bytes=2-5"), 12).expect("bounded range");
        assert_eq!(bounded.start, 2);
        assert_eq!(bounded.end, 5);
        assert_eq!(bounded.len(), 4);
        assert!(bounded.is_partial());

        let clipped = parse_single_range(Some("bytes=8-99"), 12).expect("clipped range");
        assert_eq!((clipped.start, clipped.end), (8, 11));

        let open = parse_single_range(Some("bytes=7-"), 12).expect("open range");
        assert_eq!((open.start, open.end), (7, 11));
    }

    #[test]
    fn resolves_suffix_ranges_without_underflow() {
        let suffix = parse_single_range(Some("bytes=-4"), 12).expect("suffix range");
        assert_eq!((suffix.start, suffix.end), (8, 11));

        let full_suffix = parse_single_range(Some("bytes=-99"), 12).expect("full suffix");
        assert_eq!((full_suffix.start, full_suffix.end), (0, 11));
        assert_eq!(full_suffix.content_range(), "bytes 0-11/12");
    }

    #[test]
    fn rejects_multiple_units_whitespace_and_malformed_forms() {
        for value in [
            "bytes=0-1,3-4",
            "items=0-1",
            "bytes =0-1",
            "bytes= 0-1",
            "bytes=0-1 ",
            "bytes=",
            "bytes=-",
            "bytes=0--1",
            "bytes=+0-1",
            "bytes=0-+1",
            "0-1",
        ] {
            assert!(parse_single_range(Some(value), 10).is_err(), "{value}");
        }
        assert!(matches!(
            parse_single_range(Some("bytes=0-1,3-4"), 10),
            Err(RangeError::MultipleRanges)
        ));
        assert!(matches!(
            parse_single_range(Some("items=0-1"), 10),
            Err(RangeError::UnsupportedUnit)
        ));
    }

    #[test]
    fn rejects_overflow_unsatisfiable_and_zero_length_cases() {
        assert!(matches!(
            parse_single_range(Some("bytes=18446744073709551616-"), 10),
            Err(RangeError::Overflow)
        ));
        assert!(matches!(
            parse_single_range(Some("bytes=10-"), 10),
            Err(RangeError::Unsatisfiable)
        ));
        assert!(matches!(
            parse_single_range(Some("bytes=8-7"), 10),
            Err(RangeError::Unsatisfiable)
        ));
        assert!(matches!(
            parse_single_range(Some("bytes=-0"), 10),
            Err(RangeError::ZeroLengthSuffix)
        ));
        assert!(matches!(
            parse_single_range(None, 0),
            Err(RangeError::EmptyRepresentation)
        ));
        assert!(matches!(
            parse_single_range(Some("bytes=0-0"), 0),
            Err(RangeError::EmptyRepresentation)
        ));
        assert_eq!(unsatisfied_content_range(0), "bytes */0");
    }
}
