// SPDX-License-Identifier: AGPL-3.0-only

//! The pack route's body: open, verify and stream one stored pack, with its
//! range handling.

use super::*;

/// The `Range` a pack request may be served: none when an `If-Range` names
/// another representation than this digest's strong ETag (a date included),
/// so the answer is the full 200 body (RFC 9110, section 13.1.5).
pub(super) fn pack_range_header(headers: &HeaderMap, digest: Sha256Digest) -> Option<&str> {
    let range = headers.get(header::RANGE)?.to_str().ok()?;
    match headers.get(header::IF_RANGE) {
        None => Some(range),
        Some(validator) if validator.as_bytes() == format!("\"{digest}\"").as_bytes() => {
            Some(range)
        }
        Some(_) => None,
    }
}

pub(super) async fn stream_pack(
    stored: StoredPack,
    range_header: Option<&str>,
    permit: tokio::sync::OwnedSemaphorePermit,
    device_slot: PackDeviceSlot,
    verify_digest: bool,
    media_type: &'static str,
) -> Response {
    let opened = if verify_digest {
        open_admitted_physical_pack(&stored.path)
    } else {
        tokio::fs::File::open(&stored.path).await
    };
    let mut file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::error!(digest = %stored.digest, "published pack file is missing");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(error) => {
            tracing::error!(%error, digest = %stored.digest, "cannot open pack file");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() && metadata.len() == stored.compressed_bytes => metadata,
        Ok(_) => {
            tracing::error!(digest = %stored.digest, "published pack file size mismatch");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(error) => {
            tracing::error!(%error, digest = %stored.digest, "cannot stat pack file");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    // Every pack, a legacy catalogued one included, is read whole through the
    // descriptor it is served from and checked against its digest before any
    // byte is sent, so a same-size damaged file is refused for a range too.
    let max_bytes = if verify_digest {
        MAX_ADMITTED_PHYSICAL_PACK_BYTES
    } else {
        MAX_CATALOGUED_PACK_BYTES
    };
    let verified_bytes = match read_verified_physical_pack(
        &mut file,
        stored.compressed_bytes,
        stored.digest,
        max_bytes,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, digest = %stored.digest, "cannot verify pack bytes against their digest");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let range = match parse_single_range(range_header, metadata.len()) {
        Ok(range) => range,
        Err(_) => return range_not_satisfiable(metadata.len()),
    };
    let content_length = match HeaderValue::from_str(&range.len().to_string()) {
        Ok(value) => value,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let etag = match HeaderValue::from_str(&format!("\"{}\"", stored.digest)) {
        Ok(value) => value,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut response = Response::builder()
        .status(if range.is_partial() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        )
        .header(header::CONTENT_TYPE, HeaderValue::from_static(media_type))
        .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
        .header(header::CONTENT_LENGTH, content_length)
        .header(header::ETAG, etag);
    if range.is_partial() {
        let content_range = match HeaderValue::from_str(&range.content_range()) {
            Ok(value) => value,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        response = response.header(header::CONTENT_RANGE, content_range);
    }
    let start = usize::try_from(range.start).expect("pack start fits usize");
    let end = usize::try_from(range.end + 1).expect("pack end fits usize");
    let body = verified_pack_body(
        verified_bytes.slice(start..end),
        permit,
        device_slot,
        PACK_STREAM_IDLE_TIMEOUT,
    );
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(super) fn open_admitted_physical_pack(path: &FsPath) -> std::io::Result<tokio::fs::File> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| std::io::Error::other("admitted physical pack cannot be safely opened"))?;
    let file: fs::File = descriptor.into();
    Ok(tokio::fs::File::from_std(file))
}

pub(super) async fn read_verified_physical_pack(
    file: &mut tokio::fs::File,
    expected_bytes: u64,
    expected_digest: Sha256Digest,
    max_bytes: u64,
) -> std::io::Result<Bytes> {
    if !(1..=max_bytes).contains(&expected_bytes) {
        return Err(std::io::Error::other("pack size is outside the profile"));
    }
    tokio::io::AsyncSeekExt::seek(file, std::io::SeekFrom::Start(0)).await?;
    let capacity = usize::try_from(expected_bytes)
        .map_err(|_| std::io::Error::other("admitted physical pack is too large"))?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut bounded = tokio::io::AsyncReadExt::take(file, expected_bytes + 1);
    tokio::io::AsyncReadExt::read_to_end(&mut bounded, &mut bytes).await?;
    if bytes.len() != capacity || Sha256Digest::of_bytes(&bytes) != expected_digest {
        return Err(std::io::Error::other(
            "pack bytes do not match the catalogued digest",
        ));
    }
    Ok(Bytes::from(bytes))
}

fn range_not_satisfiable(complete_length: u64) -> Response {
    let content_range = HeaderValue::from_str(&unsatisfied_content_range(complete_length))
        .expect("content range made only from digits");
    (
        StatusCode::RANGE_NOT_SATISFIABLE,
        [
            (header::ACCEPT_RANGES, HeaderValue::from_static("bytes")),
            (header::CONTENT_RANGE, content_range),
        ],
    )
        .into_response()
}
