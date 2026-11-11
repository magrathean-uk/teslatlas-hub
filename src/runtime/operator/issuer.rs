// SPDX-License-Identifier: AGPL-3.0-only

//! The issuer verbs (design 2.2, ruling R2). They run on the issuer Mac only,
//! against the one canonical ledger, with no Hub service involved: they send
//! nothing to Tesla and open no network connection. The issuer key is read
//! from the path the owner passes, by the window module and, to endorse a
//! trust document, by [`super::trust`].

use std::{path::Path, sync::Arc};

use serde_json::{Value, json};

use super::{admission::OperatorRefusal, authority::ISSUER_ANCHOR_SERVICE, cli::WindowCommand};
use crate::collection::window::{
    IssueRequest, Issuer, KeychainAnchor, SystemClock, TrustDocument,
};

pub const TIME_INVALID: &str = "time_invalid";
pub const OUTPUT_EXISTS: &str = "output_exists";

/// Parses Unix seconds or an RFC 3339 UTC time (`2026-11-12T09:30:00Z`).
pub fn parse_time(text: &str) -> Result<i64, OperatorRefusal> {
    let invalid = || {
        OperatorRefusal::new(
            TIME_INVALID,
            format!("`{text}` is not Unix seconds or an RFC 3339 UTC time such as 2026-11-12T09:30:00Z"),
        )
    };
    if let Ok(seconds) = text.parse::<i64>() {
        return Ok(seconds);
    }
    let text = text.strip_suffix('Z').ok_or_else(invalid)?;
    let (date, time) = text.split_once('T').ok_or_else(invalid)?;
    let numbers = |part: &str, separator: char, count: usize| -> Option<Vec<u32>> {
        let fields = part
            .split(separator)
            .map(|field| field.parse::<u32>().ok())
            .collect::<Option<Vec<_>>>()?;
        (fields.len() == count).then_some(fields)
    };
    let date = numbers(date, '-', 3).ok_or_else(invalid)?;
    let time = numbers(time, ':', 3).ok_or_else(invalid)?;
    let month = time::Month::try_from(u8::try_from(date[1]).map_err(|_| invalid())?)
        .map_err(|_| invalid())?;
    let day = time::Date::from_calendar_date(
        i32::try_from(date[0]).map_err(|_| invalid())?,
        month,
        u8::try_from(date[2]).map_err(|_| invalid())?,
    )
    .map_err(|_| invalid())?;
    let clock = time::Time::from_hms(
        u8::try_from(time[0]).map_err(|_| invalid())?,
        u8::try_from(time[1]).map_err(|_| invalid())?,
        u8::try_from(time[2]).map_err(|_| invalid())?,
    )
    .map_err(|_| invalid())?;
    Ok(time::PrimitiveDateTime::new(day, clock)
        .assume_utc()
        .unix_timestamp())
}

fn write_trust(bytes: &[u8], out: &Path) -> Result<(), OperatorRefusal> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => {
                OperatorRefusal::new(OUTPUT_EXISTS, "the trust document file already exists")
            }
            _ => OperatorRefusal::new("window_io", "the trust document could not be written"),
        })?;
    std::io::Write::write_all(&mut file, bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| OperatorRefusal::new("window_io", "the trust document could not be written"))
}

fn trust_json(document: &TrustDocument) -> Value {
    json!({
        "issuerKeyId": document.issuer_key_id,
        "trustEpoch": document.trust_epoch,
    })
}

/// Runs one issuer verb (or `host-id`) on this Mac.
pub fn run_issuer_command(command: &WindowCommand, home: &Path) -> Result<Value, OperatorRefusal> {
    let host_id = super::host::this_host_id()?;
    let issuer = || {
        Issuer::new(
            &Issuer::canonical_dir(home),
            &host_id,
            Arc::new(KeychainAnchor::new(ISSUER_ANCHOR_SERVICE)),
            Arc::new(SystemClock::new()),
        )
    };
    match command {
        WindowCommand::HostId => Ok(json!({ "hostId": host_id })),
        WindowCommand::IssuerInit { key_out, trust_out } => {
            if trust_out.exists() {
                return Err(OperatorRefusal::new(
                    OUTPUT_EXISTS,
                    "the trust document file already exists",
                ));
            }
            let issuer = issuer();
            let document = issuer.init(key_out).map_err(OperatorRefusal::window)?;
            // The first trust of a Hub is a bootstrap the owner confirms there.
            write_trust(&document.to_bytes().map_err(OperatorRefusal::window)?, trust_out)?;
            let mut report = trust_json(&document);
            report["ledger"] = json!(issuer.ledger_path());
            Ok(report)
        }
        WindowCommand::IssuerTrust {
            key_file,
            trust_out,
            new_epoch,
        } => {
            if trust_out.exists() {
                return Err(OperatorRefusal::new(
                    OUTPUT_EXISTS,
                    "the trust document file already exists",
                ));
            }
            let issuer = issuer();
            let document = if *new_epoch {
                issuer.bump_trust_epoch(key_file)
            } else {
                issuer.trust_document(key_file)
            }
            .map_err(OperatorRefusal::window)?;
            // Endorsed by the issuer key, so a Hub that pins this issuer
            // accepts the new epoch without asking its owner.
            write_trust(&super::trust::endorse(&document, key_file)?, trust_out)?;
            let mut report = trust_json(&document);
            report["endorsed"] = json!(true);
            Ok(report)
        }
        WindowCommand::Issue {
            key_file,
            host_id: target,
            starts_at,
            expires_at,
            approver,
            reason,
            approval_ref,
            out,
        } => {
            let request = IssueRequest {
                host_id: target.clone(),
                starts_at: parse_time(starts_at)?,
                expires_at: parse_time(expires_at)?,
                approver: approver.clone(),
                reason: reason.clone(),
                approval_ref: approval_ref.clone(),
            };
            let grant = issuer()
                .issue(key_file, &request, out)
                .map_err(OperatorRefusal::window)?;
            Ok(json!({
                "status": "issued",
                "seq": grant.body.seq,
                "hostId": grant.body.host_id,
                "startsAt": grant.body.starts_at,
                "expiresAt": grant.body.expires_at,
                "grantDigest": grant.body.digest().map_err(OperatorRefusal::window)?,
                "grant": out,
            }))
        }
        WindowCommand::Void {
            key_file,
            seq,
            reason,
        } => {
            issuer()
                .void(key_file, *seq, reason)
                .map_err(OperatorRefusal::window)?;
            Ok(json!({ "status": "voided", "seq": seq }))
        }
        WindowCommand::Status
        | WindowCommand::Trust { .. }
        | WindowCommand::Open { .. }
        | WindowCommand::Close => Err(OperatorRefusal::new(
            "request_invalid",
            "this window verb is forwarded to the running service",
        )),
    }
}
