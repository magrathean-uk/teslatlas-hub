// SPDX-License-Identifier: AGPL-3.0-only

//! hub-http-v1 vehicle history routes: drives, positions, charges and charge samples.

use super::*;
use crate::db::VehicleVisibility;

/// Route-specific wording of the 503 answers of one history route.
struct PublicHistoryText {
    cursor_key_unavailable: &'static str,
    unavailable: &'static str,
    oversize: &'static str,
}

const DRIVE_HISTORY_TEXT: PublicHistoryText = PublicHistoryText {
    cursor_key_unavailable: "drive history cursor integrity is unavailable",
    unavailable: "drive history is temporarily unavailable",
    oversize: "a drive exceeds the response byte limit",
};

const POSITION_HISTORY_TEXT: PublicHistoryText = PublicHistoryText {
    cursor_key_unavailable: "drive history cursor integrity is unavailable",
    unavailable: "drive positions are temporarily unavailable",
    oversize: "a position exceeds the response byte limit",
};

const CHARGE_HISTORY_TEXT: PublicHistoryText = PublicHistoryText {
    cursor_key_unavailable: "charge history cursor integrity is unavailable",
    unavailable: "charge history is temporarily unavailable",
    oversize: "a charge exceeds the response byte limit",
};

const CHARGE_SAMPLE_HISTORY_TEXT: PublicHistoryText = PublicHistoryText {
    cursor_key_unavailable: "charge history cursor integrity is unavailable",
    unavailable: "charge samples are temporarily unavailable",
    oversize: "a charge sample exceeds the response byte limit",
};

/// The access checks every vehicle-scoped JSON history route runs, in this
/// order and before it reads the query: bearer, cursor key, vehicle id, then
/// the vehicle being active AND published (the predicate of `/v1/vehicles`)
/// AND granted to the device. A malformed, non-UTF-8, nil, unknown, retired,
/// unpublished or ungranted vehicle is the same 404 `vehicle_not_found`.
#[allow(clippy::result_large_err)] // an axum Response is the handler error by design
fn require_public_history_access<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    uri: &Uri,
    text: &PublicHistoryText,
) -> Result<(&'a CursorKey, Uuid), Response> {
    let device = match require_authorized_device(state, headers) {
        Ok(device) => device,
        Err(status) => return Err(device_auth_reject(status)),
    };
    let Some(cursor_key) = state.cursor_key.as_deref() else {
        return Err(service_unavailable(text.cursor_key_unavailable));
    };
    let Some(vehicle_id) = path_vehicle_id(uri) else {
        return Err(vehicle_not_found());
    };
    require_vehicle_access(
        state,
        &device,
        vehicle_id,
        VehicleVisibility::PublishedAndActive,
    )
    .map_err(VehicleAccessDenied::into_public_api_response)?;
    Ok((cursor_key, vehicle_id))
}

/// Parse and validate the query of one page route. Run only after the access
/// checks (and the path id of a nested route). A value that is not a decimal
/// integer in range (or an unknown parameter) is `invalid_query`;
/// `invalid_time_range` and `invalid_limit` apply to well-formed values only.
#[allow(clippy::result_large_err)] // an axum Response is the handler error by design
fn parse_page_query(
    cursor_key: &CursorKey,
    kind: PageKind,
    vehicle_id: Uuid,
    scope_id: i64,
    parameters: Result<Query<PageQueryParameters>, QueryRejection>,
) -> Result<PageQuery, Response> {
    let Query(parameters) = parameters.map_err(|_| {
        public_api_error(
            StatusCode::BAD_REQUEST,
            "invalid_query",
            "query parameters are invalid",
        )
    })?;
    PageQuery::parse(cursor_key, kind, vehicle_id, scope_id, parameters).map_err(
        |error| match error {
            PageQueryError::TimeRange => public_api_error(
                StatusCode::BAD_REQUEST,
                "invalid_time_range",
                "from_ms must be nonnegative and earlier than to_ms",
            ),
            PageQueryError::Limit => public_api_error(
                StatusCode::BAD_REQUEST,
                "invalid_limit",
                match kind {
                    PageKind::Drives | PageKind::Charges => "limit must be between 1 and 500",
                    PageKind::Positions | PageKind::ChargeSamples => {
                        "limit must be between 1 and 2048"
                    }
                },
            ),
            PageQueryError::Cursor => public_api_error(
                StatusCode::BAD_REQUEST,
                "invalid_cursor",
                match kind {
                    PageKind::Drives | PageKind::Charges => {
                        "cursor is invalid for this vehicle and time range"
                    }
                    PageKind::Positions | PageKind::ChargeSamples => {
                        "cursor is invalid for this resource and time range"
                    }
                },
            ),
        },
    )
}

/// The store is asked for one row more than the page holds; that row only
/// tells whether a next page exists and is never served.
fn lookahead_limit(query: PageQuery) -> u32 {
    query
        .limit
        .checked_add(1)
        .expect("bounded public page limit has one-row lookahead")
}

fn trim_lookahead<T>(mut rows: Vec<T>, query: PageQuery) -> (Vec<T>, bool) {
    let has_more = rows.len() > query.limit as usize;
    if has_more {
        rows.truncate(query.limit as usize);
    }
    (rows, has_more)
}

/// The shared page response: assemble within the 1 MiB cap without splitting
/// an item, cut the cursor after the last included item, and answer with a
/// strong ETag and `If-None-Match` handling. One item over the cap on its own
/// is a 503.
fn serve_public_page<T: Serialize + PageItem>(
    request_headers: &HeaderMap,
    items: Vec<T>,
    fetched_has_more: bool,
    next_cursor: impl Fn((i64, i64)) -> String,
    text: &PublicHistoryText,
) -> Response {
    let page: PublicPage<T> = match bounded_public_page(items, fetched_has_more, next_cursor) {
        Ok(Some(page)) => page,
        Ok(None) => return service_unavailable(text.oversize),
        Err(error) => {
            tracing::error!(%error, "cannot size public page");
            return service_unavailable(text.unavailable);
        }
    };
    match public_json_response_with_etag(request_headers, &page) {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "cannot serialize public page");
            service_unavailable(text.unavailable)
        }
    }
}

/// A single resource: no ETag, never cached. The profile bounds every
/// response to 1 MiB, so an item over the cap on its own is a 503, as it is
/// on the page routes.
fn public_item_response<T: Serialize>(item: &T, text: &PublicHistoryText) -> Response {
    let body = match serde_json::to_vec(item) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(%error, "cannot serialize public item");
            return service_unavailable(text.unavailable);
        }
    };
    if body.len() > MAX_PUBLIC_PAGE_BYTES {
        return service_unavailable(text.oversize);
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .expect("static public response headers are valid")
}

pub(super) async fn drives(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    parameters: Result<Query<PageQueryParameters>, QueryRejection>,
) -> Response {
    let (cursor_key, vehicle_id) =
        match require_public_history_access(&state, &headers, &uri, &DRIVE_HISTORY_TEXT) {
            Ok(access) => access,
            Err(response) => return response,
        };
    let query = match parse_page_query(cursor_key, PageKind::Drives, vehicle_id, 0, parameters) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let rows = match state.store.materialised_drives_page(
        vehicle_id,
        query.from_ms,
        query.to_ms,
        query.after,
        lookahead_limit(query),
    ) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public drive page");
            return service_unavailable(DRIVE_HISTORY_TEXT.unavailable);
        }
    };
    let (rows, has_more) = trim_lookahead(rows, query);
    let items: Vec<PublicDrive> = rows
        .into_iter()
        .map(|drive| PublicDrive::from_projection(vehicle_id, drive))
        .collect();
    serve_public_page(
        &headers,
        items,
        has_more,
        |last| query.next_cursor(cursor_key, last),
        &DRIVE_HISTORY_TEXT,
    )
}

pub(super) async fn drive(State(state): State<AppState>, uri: Uri, headers: HeaderMap) -> Response {
    // The query string of an item route is ignored.
    let (_, vehicle_id) =
        match require_public_history_access(&state, &headers, &uri, &DRIVE_HISTORY_TEXT) {
            Ok(access) => access,
            Err(response) => return response,
        };
    let Some(drive_id) = path_session_id(&uri) else {
        return drive_not_found();
    };
    match state
        .store
        .materialised_closed_drive_for_vehicle(vehicle_id, drive_id)
    {
        Ok(Some(drive)) => public_item_response(
            &PublicDrive::from_projection(vehicle_id, drive),
            &DRIVE_HISTORY_TEXT,
        ),
        Ok(None) => drive_not_found(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public drive");
            service_unavailable(DRIVE_HISTORY_TEXT.unavailable)
        }
    }
}

pub(super) async fn drive_positions(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    parameters: Result<Query<PageQueryParameters>, QueryRejection>,
) -> Response {
    let (cursor_key, vehicle_id) =
        match require_public_history_access(&state, &headers, &uri, &POSITION_HISTORY_TEXT) {
            Ok(access) => access,
            Err(response) => return response,
        };
    let Some(drive_id) = path_session_id(&uri) else {
        return drive_not_found();
    };
    // The parent drive first, before the query is read: an unknown or
    // unfinished drive is `drive_not_found` whatever the query says, and is
    // not an empty page.
    match state
        .store
        .materialised_closed_drive_for_vehicle(vehicle_id, drive_id)
    {
        Ok(Some(_)) => {}
        Ok(None) => return drive_not_found(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public positions parent drive");
            return service_unavailable(POSITION_HISTORY_TEXT.unavailable);
        }
    }
    let query = match parse_page_query(
        cursor_key,
        PageKind::Positions,
        vehicle_id,
        drive_id,
        parameters,
    ) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let rows = match state.store.drive_positions_window_page(
        vehicle_id,
        drive_id,
        query.from_ms,
        query.to_ms,
        query.after,
        lookahead_limit(query),
    ) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public position page");
            return service_unavailable(POSITION_HISTORY_TEXT.unavailable);
        }
    };
    let (rows, has_more) = trim_lookahead(rows, query);
    let items: Vec<PublicPosition> = rows
        .into_iter()
        .map(|row| PublicPosition::from_projection(drive_id, row.position, row.terrain))
        .collect();
    serve_public_page(
        &headers,
        items,
        has_more,
        |last| query.next_cursor(cursor_key, last),
        &POSITION_HISTORY_TEXT,
    )
}

pub(super) async fn charges(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    parameters: Result<Query<PageQueryParameters>, QueryRejection>,
) -> Response {
    let (cursor_key, vehicle_id) =
        match require_public_history_access(&state, &headers, &uri, &CHARGE_HISTORY_TEXT) {
            Ok(access) => access,
            Err(response) => return response,
        };
    let query = match parse_page_query(cursor_key, PageKind::Charges, vehicle_id, 0, parameters) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let rows = match state.store.materialised_charges_page(
        vehicle_id,
        query.from_ms,
        query.to_ms,
        query.after,
        lookahead_limit(query),
    ) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public charge page");
            return service_unavailable(CHARGE_HISTORY_TEXT.unavailable);
        }
    };
    let (rows, has_more) = trim_lookahead(rows, query);
    let mut items = Vec::with_capacity(rows.len());
    for charge in rows {
        // The store only returns closed sessions, so a missing end date is a
        // catalogue fault, never an item to serve.
        let Some(item) = PublicCharge::from_projection(vehicle_id, charge) else {
            tracing::error!(%vehicle_id, "public charge page returned an unfinished session");
            return service_unavailable(CHARGE_HISTORY_TEXT.unavailable);
        };
        items.push(item);
    }
    serve_public_page(
        &headers,
        items,
        has_more,
        |last| query.next_cursor(cursor_key, last),
        &CHARGE_HISTORY_TEXT,
    )
}

pub(super) async fn charge(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    // The query string of an item route is ignored.
    let (_, vehicle_id) =
        match require_public_history_access(&state, &headers, &uri, &CHARGE_HISTORY_TEXT) {
            Ok(access) => access,
            Err(response) => return response,
        };
    let Some(charge_id) = path_session_id(&uri) else {
        return charge_not_found();
    };
    match state
        .store
        .materialised_charge_for_vehicle(vehicle_id, charge_id)
    {
        Ok(Some(charge)) => match PublicCharge::from_projection(vehicle_id, charge) {
            Some(item) => public_item_response(&item, &CHARGE_HISTORY_TEXT),
            None => {
                tracing::error!(%vehicle_id, "public charge was returned unfinished");
                service_unavailable(CHARGE_HISTORY_TEXT.unavailable)
            }
        },
        Ok(None) => charge_not_found(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public charge");
            service_unavailable(CHARGE_HISTORY_TEXT.unavailable)
        }
    }
}

pub(super) async fn charge_samples(
    State(state): State<AppState>,
    uri: Uri,
    headers: HeaderMap,
    parameters: Result<Query<PageQueryParameters>, QueryRejection>,
) -> Response {
    let (cursor_key, vehicle_id) =
        match require_public_history_access(&state, &headers, &uri, &CHARGE_SAMPLE_HISTORY_TEXT) {
            Ok(access) => access,
            Err(response) => return response,
        };
    let Some(charge_id) = path_session_id(&uri) else {
        return charge_not_found();
    };
    // The parent charge first, before the query is read: an unknown or
    // unfinished charge is `charge_not_found` whatever the query says, and is
    // not an empty page.
    match state
        .store
        .materialised_charge_for_vehicle(vehicle_id, charge_id)
    {
        Ok(Some(_)) => {}
        Ok(None) => return charge_not_found(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public samples parent charge");
            return service_unavailable(CHARGE_SAMPLE_HISTORY_TEXT.unavailable);
        }
    }
    let query = match parse_page_query(
        cursor_key,
        PageKind::ChargeSamples,
        vehicle_id,
        charge_id,
        parameters,
    ) {
        Ok(query) => query,
        Err(response) => return response,
    };
    let rows = match state.store.charge_samples_page(
        vehicle_id,
        charge_id,
        query.from_ms,
        query.to_ms,
        query.after,
        lookahead_limit(query),
    ) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load public charge sample page");
            return service_unavailable(CHARGE_SAMPLE_HISTORY_TEXT.unavailable);
        }
    };
    let (rows, has_more) = trim_lookahead(rows, query);
    let items: Vec<PublicChargeSample> = rows
        .into_iter()
        .map(PublicChargeSample::from_projection)
        .collect();
    serve_public_page(
        &headers,
        items,
        has_more,
        |last| query.next_cursor(cursor_key, last),
        &CHARGE_SAMPLE_HISTORY_TEXT,
    )
}
