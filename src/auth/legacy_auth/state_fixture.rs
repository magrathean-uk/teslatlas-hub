// SPDX-License-Identifier: AGPL-3.0-only
//! Pre-C10 C5 component fixtures retained by X8 section 7 (CL-14).
//! Never called by a V1 manager, a vault, or a vault test's pair producer.
use crate::db::{HubStore,StoreError,OutboundRequestReceiptId,BeginRefusal};
use rusqlite::{params,OptionalExtension};
use uuid::Uuid;
pub(crate) type Row=(Vec<u8>,Vec<u8>,i64,i64,Option<Uuid>,String,String);
pub(crate) fn active(store:&HubStore)->Result<bool,StoreError>{store.open()?.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='legacy_owner_tokens')",[],|r|r.get(0)).map_err(StoreError::Query)}
pub(crate) fn for_store(store:&HubStore)->Result<bool,StoreError>{Ok(active(store)? && store.credential_pair_presence(crate::auth::credential_envelope::CredentialSlot::OwnerApi)?.is_none())}
pub(crate) fn ensure(store:&HubStore){if !active(store).expect("fixture schema"){crate::auth::credential_migration::seam::seed_pre_upgrade_store(&store.open().expect("fixture connection"),None,None,false);}}
pub(crate) fn row(g:Option<Uuid>,a:Vec<u8>,r:Vec<u8>,s:(i64,i64))->Row{(a,r,s.0,s.1,g,String::new(),String::new())}
pub(crate) fn input(g:Uuid)->Row{row(Some(g),vec![0;8],vec![0;8],(1_800_028_800,1_800_021_600))}
pub(crate) fn successor(g:Uuid)->Row{row(Some(g),vec![1;8],vec![2;8],(1_800_028_800,1_800_021_600))}
pub(crate) fn load(store:&HubStore)->Result<Option<Row>,StoreError>{store.open()?.query_row("SELECT access,refresh,expires_at,next_refresh_at,credential_generation FROM legacy_owner_tokens WHERE singleton_id=1",[],|r|{let g:Option<String>=r.get(4)?;Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,g.and_then(|g|Uuid::parse_str(&g).ok()),String::new(),String::new()))}).optional().map_err(StoreError::Query)}
fn refusal()->StoreError{StoreError::RefreshOutcomeUnknown{reason:BeginRefusal::TokenSpent}}
pub(crate) fn write(store:&HubStore,row:&Row)->Result<(),StoreError>{
    ensure(store);let mut db=store.open()?;let tx=db.transaction().map_err(StoreError::Begin)?;
    if let Some(g)=row.4{let spent:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_refresh_input_fences WHERE input_credential_generation=?1)",[g.to_string()],|r|r.get(0)).map_err(StoreError::Query)?;if spent{return Err(refusal());}}
    tx.execute("INSERT INTO legacy_owner_tokens VALUES(1,?1,?2,?3,?4,?5) ON CONFLICT(singleton_id) DO UPDATE SET access=excluded.access,refresh=excluded.refresh,expires_at=excluded.expires_at,next_refresh_at=excluded.next_refresh_at,credential_generation=excluded.credential_generation",params![row.0,row.1,row.2,row.3,row.4.map(|g|g.to_string())]).map_err(StoreError::Query)?;
    tx.execute("UPDATE outbound_request_receipts SET outcome='cancelled',completed_at_ms=started_at_ms,duration_ms=0 WHERE outcome='started' AND id IN (SELECT receipt_id FROM legacy_refresh_receipt_bindings)",[]).map_err(StoreError::Query)?;
    tx.commit().map_err(StoreError::CredentialCommit)
}
pub(crate) fn remove(store:&HubStore)->Result<(),StoreError>{store.open()?.execute("DELETE FROM legacy_owner_tokens",[]).map(|_|()).map_err(StoreError::Query)}
pub(crate) fn unresolved(store:&HubStore)->Result<bool,StoreError>{store.open()?.query_row("SELECT EXISTS(SELECT 1 FROM legacy_refresh_receipt_bindings b JOIN outbound_request_receipts r ON r.id=b.receipt_id JOIN legacy_refresh_input_fences f ON f.input_credential_generation=b.input_credential_generation WHERE r.outcome='started')",[],|r|r.get(0)).map_err(StoreError::Query)}
pub(crate) fn output(store:&HubStore,g:Uuid)->Result<bool,StoreError>{store.open()?.query_row("SELECT EXISTS(SELECT 1 FROM legacy_refresh_receipt_bindings b JOIN outbound_request_receipts r ON r.id=b.receipt_id WHERE b.output_credential_generation=?1 AND r.outcome='success')",[g.to_string()],|r|r.get(0)).map_err(StoreError::Query)}
pub(crate) fn begin(store:&HubStore,g:Uuid)->Result<OutboundRequestReceiptId,StoreError>{
    ensure(store);if load(store)?.and_then(|r|r.4)!=Some(g)||unresolved(store)?{return Err(refusal());}
    let mut db=store.open()?;let tx=db.transaction().map_err(StoreError::Begin)?;
    let spent:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_refresh_input_fences WHERE input_credential_generation=?1)",[g.to_string()],|r|r.get(0)).map_err(StoreError::Query)?;if spent{return Err(refusal());}
    tx.execute("INSERT INTO outbound_request_receipts(correlation_id,started_at_ms,transport,operation,safety_class,precondition,outcome) VALUES(?1,1800000000000,'legacy_auth','token_refresh','non_wake_endpoint','not_required','started')",[Uuid::new_v4().to_string()]).map_err(StoreError::Query)?;
    let receipt=OutboundRequestReceiptId(tx.last_insert_rowid());
    tx.execute("INSERT INTO legacy_refresh_receipt_bindings(receipt_id,attempt_id,input_credential_generation,output_credential_generation) VALUES(?1,?2,?3,NULL)",params![receipt.0,Uuid::new_v4().to_string(),g.to_string()]).map_err(StoreError::Query)?;
    tx.execute("INSERT INTO legacy_refresh_input_fences VALUES(?1)",[g.to_string()]).map_err(StoreError::Query)?;tx.commit().map_err(StoreError::CredentialCommit)?;Ok(receipt)
}
pub(crate) fn finish(store:&HubStore,receipt:OutboundRequestReceiptId,input:Uuid,row:&Row)->Result<(),StoreError>{
    if load(store)?.and_then(|r|r.4)!=Some(input){return Err(StoreError::RefreshOutcomeUnknown{reason:BeginRefusal::RowChanged});}
    let mut db=store.open()?;let tx=db.transaction().map_err(StoreError::Begin)?;
    let open:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM legacy_refresh_receipt_bindings b JOIN outbound_request_receipts r ON r.id=b.receipt_id WHERE b.receipt_id=?1 AND b.input_credential_generation=?2 AND r.outcome='started')",params![receipt.0,input.to_string()],|r|r.get(0)).map_err(StoreError::Query)?;if !open{return Err(refusal());}
    tx.execute("UPDATE legacy_owner_tokens SET access=?1,refresh=?2,expires_at=?3,next_refresh_at=?4,credential_generation=?5 WHERE singleton_id=1",params![row.0,row.1,row.2,row.3,row.4.map(|g|g.to_string())]).map_err(StoreError::Query)?;
    tx.execute("UPDATE legacy_refresh_receipt_bindings SET output_credential_generation=?1 WHERE receipt_id=?2",params![row.4.map(|g|g.to_string()),receipt.0]).map_err(StoreError::Query)?;
    tx.execute("UPDATE outbound_request_receipts SET outcome='success',http_status=200,completed_at_ms=started_at_ms+40,duration_ms=40 WHERE id=?1",[receipt.0]).map_err(StoreError::Query)?;
    if row.4==Some(input){tx.execute("DELETE FROM legacy_refresh_input_fences WHERE input_credential_generation=?1",[input.to_string()]).map_err(StoreError::Query)?;}
    tx.commit().map_err(StoreError::CredentialCommit)
}
