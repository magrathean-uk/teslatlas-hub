// SPDX-License-Identifier: AGPL-3.0-only

use super::super::terrain_pass::{
    RuntimeAdmissionCheck, SensitiveAccessCheck, TerrainPassReport, run_terrain_enrichment_pass,
    run_terrain_tidy_only_pass,
};
use super::super::*;
use crate::db::{IMPORT_SOURCE_KIND, ImportBoundary, LifecycleCommit};
use crate::hub_pack::{ProjectionDrive, ProjectionPosition};
use crate::terrain::{SRTM3_SIDE, TERRAIN_DATASET_VERSION};
use rusqlite::params;
use serde_json::json;
use std::sync::atomic::{AtomicI64, AtomicUsize};

const T0: i64 = 1_760_000_000_000;
const T1: i64 = T0 + 60_000_000;

struct ManualClock(AtomicI64);
impl ManualClock {
    fn new() -> Self {
        Self(AtomicI64::new(T1))
    }
    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}
impl TerrainClock for ManualClock {
    fn now_ms(&self) -> Result<i64, CollectorError> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
struct Access(bool);
impl SensitiveAccessCheck for Access {
    fn check(&self) -> Result<(), CollectorError> {
        self.0
            .then_some(())
            .ok_or(CollectorError::SensitiveAccessUnavailable)
    }
}
struct Permit(bool);
impl EgressGuard for Permit {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        self.0.then_some(()).ok_or(EgressGuardError)
    }
}

#[derive(Clone)]
enum Script {
    Value(i16),
    Ramp,
    Nodes(Vec<(usize, i16)>),
    BadResponse,
    Timeout,
    Quota,
    Archive,
    Store,
}
struct Source {
    scripts: Mutex<BTreeMap<String, Script>>,
    cached: Mutex<BTreeSet<String>>,
    calls: Mutex<Vec<String>>,
    network: Mutex<Vec<String>>,
    clock: Arc<ManualClock>,
    advance_ms: i64,
    after: Mutex<Option<Box<dyn Fn(TileId) + Send>>>,
}
impl Source {
    fn new(clock: &Arc<ManualClock>) -> Self {
        Self {
            scripts: Mutex::new(BTreeMap::new()),
            cached: Mutex::new(BTreeSet::new()),
            calls: Mutex::new(Vec::new()),
            network: Mutex::new(Vec::new()),
            clock: clock.clone(),
            advance_ms: 0,
            after: Mutex::new(None),
        }
    }
    fn script(&self, name: &str, script: Script) {
        self.scripts.lock().unwrap().insert(name.into(), script);
    }
    fn cached(&self, name: &str) {
        self.cached.lock().unwrap().insert(name.into());
    }
    fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
    fn networks(&self, name: &str) -> usize {
        self.network
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.as_str() == name)
            .count()
    }
}
impl TileSource for Source {
    fn acquire<'a>(
        &'a self,
        tile: TileId,
        guard: &'a dyn EgressGuard,
        inner: &'a dyn EgressGuard,
    ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>> {
        Box::pin(async move {
            let name = tile.name();
            self.calls.lock().unwrap().push(name.clone());
            let cached = self.cached.lock().unwrap().contains(&name);
            (if cached { inner } else { guard })
                .assert_egress_allowed()
                .map_err(|_| AcquireFailure {
                    kind: AcquireErrorKind::Cache(TerrainCacheError::EgressDenied),
                    provenance: None,
                })?;
            if !cached {
                self.network.lock().unwrap().push(name.clone());
            }
            tokio::task::yield_now().await;
            self.clock.0.fetch_add(self.advance_ms, Ordering::SeqCst);
            if let Some(after) = self.after.lock().unwrap().as_ref() {
                after(tile);
            }
            let script = self
                .scripts
                .lock()
                .unwrap()
                .get(&name)
                .cloned()
                .unwrap_or(Script::Value(0));
            let error = match script {
                Script::BadResponse => Some(TerrainCacheError::BadResponse),
                Script::Timeout => Some(TerrainCacheError::Timeout),
                Script::Quota => Some(TerrainCacheError::CacheQuotaExceeded),
                Script::Archive => Some(TerrainCacheError::InvalidArchive),
                Script::Store => Some(TerrainCacheError::Store(Box::new(
                    StoreError::CreateDataDir(std::io::Error::other("synthetic")),
                ))),
                _ => None,
            };
            if let Some(error) = error {
                return Err(AcquireFailure {
                    kind: AcquireErrorKind::Cache(error),
                    provenance: None,
                });
            }
            let mut bytes = vec![0; SRTM3_SIDE * SRTM3_SIDE * 2];
            for row in 0..SRTM3_SIDE {
                for column in 0..SRTM3_SIDE {
                    let value = match &script {
                        Script::Value(v) => *v,
                        Script::Ramp => (column * 2) as i16,
                        Script::Nodes(nodes) => nodes
                            .iter()
                            .find(|(c, _)| *c == column)
                            .map_or(0, |(_, v)| *v),
                        _ => unreachable!(),
                    };
                    let offset = (row * SRTM3_SIDE + column) * 2;
                    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
                }
            }
            let tile = HgtTile::from_bytes(&name, bytes).unwrap();
            let provenance = TileProvenance {
                tile_name: name,
                tile_hash: tile.sha256_hex().unwrap(),
                dataset_source: "aws".into(),
                dataset_version: TERRAIN_DATASET_VERSION.into(),
            };
            Ok(AcquiredTile { tile, provenance })
        })
    }
}

fn fixture(pending: bool) -> (tempfile::TempDir, HubStore, Uuid) {
    let dir = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(dir.path()).unwrap();
    let vehicle = vehicle(&store, "one");
    if !pending {
        store.apply_terrain_tidy_chunk(250, T1).unwrap();
    }
    (dir, store, vehicle)
}
fn vehicle(store: &HubStore, name: &str) -> Uuid {
    let source = store
        .register_source(&SourceDescriptor::new("owner_api_compat", "c9-pass"), T0)
        .unwrap();
    store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, name), T0)
        .unwrap()
        .vehicle_id
}
fn point(id: i64, drive: i64, column: usize, longitude: i32) -> ProjectionPosition {
    serde_json::from_value(json!({"id":id, "drive_id":drive, "car_id":1, "date_ms": T0+id*1000, "latitude":10.5, "longitude": f64::from(longitude)+column as f64/1200.0})).unwrap()
}
fn drive(id: i64) -> ProjectionDrive {
    serde_json::from_value(
        json!({"id":id,"car_id":1,"start_date_ms":T0-1,"end_date_ms":T1,"ascent":77,"descent":77}),
    )
    .unwrap()
}
fn commit(
    store: &HubStore,
    vehicle: Uuid,
    drives: Vec<ProjectionDrive>,
    positions: Vec<ProjectionPosition>,
) {
    let state = OpenSessionState::new().encode().unwrap();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: T1,
            delta: &crate::lifecycle::LifecycleDelta {
                drives,
                positions,
                ..Default::default()
            },
        })
        .unwrap();
}
fn seed(store: &HubStore, vehicle: Uuid, positions: Vec<ProjectionPosition>) {
    let drives: BTreeSet<_> = positions.iter().filter_map(|p| p.drive_id).collect();
    commit(
        store,
        vehicle,
        drives.into_iter().map(drive).collect(),
        positions,
    );
    clear_log(store);
}
fn clear_log(store: &HubStore) {
    store
        .open()
        .unwrap()
        .execute_batch("DELETE FROM sync_mutations; DELETE FROM export_outbox;")
        .unwrap();
}
fn scalar(store: &HubStore, sql: &str) -> i64 {
    store
        .open()
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}
fn cursor(store: &HubStore, vehicle: Uuid) -> i64 {
    store
        .terrain_vehicles()
        .unwrap()
        .into_iter()
        .find(|v| v.vehicle_id == vehicle)
        .unwrap()
        .cursor_position_id
}
fn elevation(store: &HubStore, vehicle: Uuid, id: i64) -> Option<i64> {
    store.open().unwrap().query_row("SELECT json_extract(position_json, '$.elevation') FROM materialised_positions WHERE vehicle_id=?1 AND position_id=?2", params![vehicle.to_string(),id], |row| row.get(0)).unwrap()
}
fn pair(store: &HubStore, vehicle: Uuid, id: i64) -> (Option<i64>, Option<i64>) {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT ascent,descent FROM materialised_drives WHERE vehicle_id=?1 AND drive_id=?2",
            params![vehicle.to_string(), id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}
fn provenance(store: &HubStore, vehicle: Uuid, id: i64) -> (String, i64, Option<String>, i64, i64) {
    store.open().unwrap().query_row("SELECT status,attempts,error_code,attempted_at_ms,retry_after_ms FROM terrain_elevation_provenance WHERE vehicle_id=?1 AND position_id=?2", params![vehicle.to_string(),id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).unwrap()
}
fn table(store: &HubStore, name: &str) -> Vec<Vec<String>> {
    let conn = store.open().unwrap();
    let mut q = conn
        .prepare(&format!("SELECT * FROM {name} ORDER BY 1,2"))
        .unwrap();
    let columns = q.column_count();
    q.query_map([], |r| {
        (0..columns)
            .map(|c| r.get_ref(c).map(|v| format!("{v:?}")))
            .collect()
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}
fn durable(store: &HubStore) -> Vec<Vec<Vec<String>>> {
    [
        "materialised_positions",
        "materialised_drives",
        "terrain_elevation_provenance",
        "sync_mutations",
        "export_outbox",
        "terrain_enrichment_state",
    ]
    .into_iter()
    .map(|t| table(store, t))
    .collect()
}
fn data_version(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap()
}
async fn pass(
    store: &HubStore,
    source: &Source,
    state: &mut TerrainWorkerState,
) -> TerrainPassReport {
    run_terrain_enrichment_pass(
        store,
        source,
        state,
        &Access(true),
        &Permit(true),
        source.clock.as_ref(),
    )
    .await
    .unwrap()
}
fn plant(store: &HubStore, vehicle: Uuid, id: i64, status: &str, value: Option<i64>, due: i64) {
    store.open().unwrap().execute("INSERT INTO terrain_elevation_provenance(vehicle_id,position_id,drive_id,latitude,longitude,elevation_m,tile_name,tile_hash,dataset_source,dataset_version,status,error_code,attempts,attempted_at_ms,retry_after_ms) SELECT vehicle_id,position_id,drive_id,json_extract(position_json,'$.latitude'),json_extract(position_json,'$.longitude'),?3,'N10E020','synthetic','aws','1',?4,NULL,1,?5,?6 FROM materialised_positions WHERE vehicle_id=?1 AND position_id=?2", params![vehicle.to_string(),id,value,status,T1-1,due]).unwrap();
}
fn import_source(store: &HubStore) -> Uuid {
    store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "pass-import"),
            T0,
        )
        .unwrap()
        .source_id
}
fn watermark(store: &HubStore, source: Uuid, vehicle: Uuid, id: i64, date: i64) {
    store.open().unwrap().execute("INSERT INTO lifecycle_source_watermarks(source_id,vehicle_id,domain,max_source_row_id,max_timestamp_ms) VALUES (?1,?2,'positions',?3,?4) ON CONFLICT(source_id,vehicle_id,domain) DO UPDATE SET max_source_row_id=MAX(max_source_row_id,excluded.max_source_row_id),max_timestamp_ms=MAX(max_timestamp_ms,excluded.max_timestamp_ms)",params![source.to_string(),vehicle.to_string(),id,date]).unwrap();
}
fn reset_marker(store: &HubStore) {
    store.open().unwrap().execute_batch("UPDATE terrain_tidy_state SET completed_at_ms=NULL,resume_vehicle_id='',resume_position_id=0,examined=0,cleared=0,requeued=0,removed=0,kept=0,drives_changed=0,run=run+1,armed_at_ms=NULL;").unwrap();
}

#[tokio::test]
async fn c9_st13_to_st17_bounds() {
    let (_dir, store, v) = fixture(false);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.script("N10E020", Script::Ramp);
    seed(
        &store,
        v,
        (1..=1200)
            .map(|id| point(id, 1, (id - 1) as usize, 20))
            .collect(),
    );
    let mut state = TerrainWorkerState::default();
    let first = pass(&store, &source, &mut state).await;
    assert_eq!(
        (first.filled, first.tiles, first.deferred, first.full_batch),
        (1000, 1, 0, true)
    );
    assert_eq!(cursor(&store, v), 1000);
    for id in 1..=1000 {
        assert_eq!(elevation(&store, v, id), Some(2 * (id - 1)));
    }
    let conn = store.open().unwrap();
    let mut q=conn.prepare("SELECT json_extract(payload_json,'$.ascent'),json_extract(payload_json,'$.descent') FROM sync_mutations WHERE entity='drive' ORDER BY revision").unwrap();
    let totals: Vec<(i64, i64)> = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(totals, [(498, 0), (998, 0), (1498, 0), (1998, 0)]);
    assert_eq!(
        scalar(
            &store,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
        ),
        1000
    );
    clock.set(T1 + 2000);
    assert_eq!(pass(&store, &source, &mut state).await.filled, 200);
    assert_eq!(cursor(&store, v), 1200);
    assert_eq!(pair(&store, v, 1), (Some(2398), Some(0)));
    let before = durable(&store);
    let calls = source.count();
    let empty = pass(&store, &source, &mut state).await;
    assert_eq!((empty.filled, source.count()), (0, calls));
    assert_eq!(before, durable(&store));
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=6)
            .map(|id| point(id, 1, 0, if [1, 2, 5].contains(&id) { 20 } else { 21 }))
            .collect(),
    );
    let source = Source::new(&clock);
    let report = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!((report.filled, report.tiles), (6, 2));
    assert_eq!(*source.calls.lock().unwrap(), ["N10E020", "N10E021"]);
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=5).map(|id| point(id, 1, 0, 19 + id as i32)).collect(),
    );
    let source = Source::new(&clock);
    let mut state = TerrainWorkerState::default();
    let first = pass(&s, &source, &mut state).await;
    assert_eq!(
        (
            first.filled,
            first.deferred,
            first.full_batch,
            cursor(&s, v)
        ),
        (4, 1, true, 4)
    );
    assert_eq!(elevation(&s, v, 5), None);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE position_id=5"
        ),
        0
    );
    assert_eq!(pass(&s, &source, &mut state).await.filled, 1);
    assert_eq!(cursor(&s, v), 5);
}

#[tokio::test]
async fn c9_st28_car_drive() {
    let (_d, s, v) = fixture(false);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let mut rows: Vec<_> = (1..=3).map(|id| point(id, 1, 0, 20)).collect();
    rows[1].elevation = Some(120);
    seed(&s, v, rows);
    let before = table(&s, "materialised_drives");
    let report = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!((report.filled, source.count(), cursor(&s, v)), (0, 0, 3));
    assert!(report.wrote);
    assert_eq!(elevation(&s, v, 2), Some(120));
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        0
    );
    assert_eq!(before, table(&s, "materialised_drives"));
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=12000)
            .map(|id| {
                let mut p = point(id, (id - 1) / 1000 + 1, 0, 20);
                p.elevation = Some(5);
                p
            })
            .collect(),
    );
    let mut state = TerrainWorkerState::default();
    for expected in [5000, 10000, 12000] {
        let r = pass(&s, &source, &mut state).await;
        assert_eq!(cursor(&s, v), expected);
        assert_eq!(r.full_batch, expected != 12000);
        assert!(r.wrote, "skipped windows still persist the cursor");
    }
    let before = durable(&s);
    pass(&s, &source, &mut state).await;
    assert_eq!(before, durable(&s));
    assert_eq!(source.count(), 0);
    // C9 R5, C9 R7: matching drive and position ids in another vehicle stay separate.
    let (_d, s, a) = fixture(false);
    let b = vehicle(&s, "second");
    let mut vehicles = [a, b];
    vehicles.sort();
    let mut car: Vec<_> = (1..=3).map(|id| point(id, 1, 0, 20)).collect();
    car[1].elevation = Some(120);
    seed(&s, vehicles[0], car);
    seed(
        &s,
        vehicles[1],
        (1..=3).map(|id| point(id, 1, 0, 20)).collect(),
    );
    let car_before = pair(&s, vehicles[0], 1);
    let source = Source::new(&clock);
    let mut state = TerrainWorkerState::default();
    assert_eq!(pass(&s, &source, &mut state).await.filled, 0);
    assert_eq!(pass(&s, &source, &mut state).await.filled, 3);
    assert_eq!(pair(&s, vehicles[0], 1), car_before);
    assert_eq!(elevation(&s, vehicles[0], 2), Some(120));
    for id in 1..=3 {
        assert_eq!(elevation(&s, vehicles[1], id), Some(0));
    }
    assert_eq!(source.count(), 1);
}

#[tokio::test]
async fn c9_st39_rotation() {
    let (_d, s, a) = fixture(false);
    let b = vehicle(&s, "two");
    let mut ids = [a, b];
    ids.sort();
    for v in ids {
        seed(&s, v, (1..=1200).map(|id| point(id, 1, 0, 20)).collect());
    }
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let mut state = TerrainWorkerState::default();
    for (index, expected) in [(0, 1000), (1, 1000), (0, 1200), (1, 1200)] {
        let r = pass(&s, &source, &mut state).await;
        assert_eq!(r.filled, if expected == 1000 { 1000 } else { 200 });
        assert_eq!(cursor(&s, ids[index]), expected);
    }
    assert_eq!(source.count(), 4);
}

#[tokio::test]
async fn c9_st40_retry_share() {
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=2300)
            .map(|id| point(id, 1, 0, if id <= 300 { 21 } else { 20 }))
            .collect(),
    );
    for id in 1..=300 {
        plant(&s, v, id, "failed", None, T1 - 1000);
    }
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let r = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!((r.filled, r.full_batch, cursor(&s, v)), (1000, true, 1050));
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE position_id<=300 AND status='success'"
        ),
        250
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE position_id>300 AND status='success'"
        ),
        750
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE status='failed' AND attempts=1"
        ),
        50
    );
    // C9 R3: a full retry slice alone is not a pass-size continuation.
    let (_d, only, ov) = fixture(false);
    seed(&only, ov, (1..=250).map(|id| point(id, 1, 0, 20)).collect());
    for id in 1..=250 {
        plant(&only, ov, id, "failed", None, T1 - 1);
    }
    let r = pass(&only, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(r.filled, 250);
    assert!(!r.full_batch);
}

#[tokio::test]
async fn c9_st54_slots() {
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=1060)
            .map(|id| {
                point(
                    id,
                    1,
                    0,
                    if id <= 60 {
                        21 + ((id - 1) / 10) as i32
                    } else {
                        20
                    },
                )
            })
            .collect(),
    );
    for id in 1..=60 {
        plant(
            &s,
            v,
            id,
            "failed",
            None,
            T1 - 6000 + ((id - 1) / 10) * 1000,
        );
    }
    let before: Vec<_> = (31..=60).map(|id| provenance(&s, v, id)).collect();
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let r = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(
        (r.filled, r.deferred, r.tiles, r.full_batch, cursor(&s, v)),
        (970, 30, 4, true, 1000)
    );
    assert_eq!(
        *source.calls.lock().unwrap(),
        ["N10E020", "N10E021", "N10E022", "N10E023"]
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE position_id>60 AND status='success'"
        ),
        940
    );
    assert_eq!(
        before,
        (31..=60)
            .map(|id| provenance(&s, v, id))
            .collect::<Vec<_>>()
    );
    // C9 R13: four forward tiles cannot consume the reserved retry slot.
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=5).map(|id| point(id, 1, 0, 19 + id as i32)).collect(),
    );
    plant(&s, v, 5, "failed", None, T1 - 1);
    let source = Source::new(&clock);
    let r = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!((r.filled, r.deferred, cursor(&s, v)), (4, 1, 3));
    assert_eq!(
        *source.calls.lock().unwrap(),
        ["N10E020", "N10E021", "N10E022", "N10E024"]
    );
    assert_eq!(elevation(&s, v, 4), None);
}

fn breaker_fixture(cached: bool) -> (tempfile::TempDir, HubStore, Uuid, Arc<ManualClock>, Source) {
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=5)
            .map(|id| {
                point(
                    id,
                    1,
                    0,
                    if id <= 2 {
                        20
                    } else if id <= 4 {
                        21
                    } else {
                        22
                    },
                )
            })
            .collect(),
    );
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.script("N10E020", Script::BadResponse);
    source.script("N10E021", Script::Timeout);
    if cached {
        source.cached("N10E022");
    }
    (d, s, v, clock, source)
}
#[tokio::test]
async fn c9_st18_to_st21_breaker() {
    for last in [Script::Value(0), Script::Timeout, Script::Archive] {
        let (_d, s, v, clock, source) = breaker_fixture(false);
        let mut state = TerrainWorkerState::default();
        let r = pass(&s, &source, &mut state).await;
        assert_eq!(
            (
                r.failed,
                r.filled,
                r.deferred,
                r.breaker_deferred,
                cursor(&s, v)
            ),
            (4, 0, 1, true, 4)
        );
        assert_eq!(source.networks("N10E022"), 0);
        assert_eq!(state.breaker_open_until(T1), Some(T1 + 300000));
        for (id, code) in [
            (1, "invalid_response"),
            (2, "invalid_response"),
            (3, "timeout"),
            (4, "timeout"),
        ] {
            assert_eq!(
                provenance(&s, v, id),
                ("failed".into(), 1, Some(code.into()), T1, T1 + 900000)
            );
        }
        clock.set(T1 + 240000);
        pass(&s, &source, &mut state).await;
        assert_eq!(source.networks("N10E022"), 0);
        assert_eq!(
            scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
            4
        );
        source.script("N10E022", last.clone());
        clock.set(T1 + 300000);
        assert_eq!(state.breaker_open_until(T1 + 300000), None);
        pass(&s, &source, &mut state).await;
        assert_eq!(source.networks("N10E022"), 1);
        match last {
            Script::Value(_) => {
                assert_eq!(elevation(&s, v, 5), Some(0));
                assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
            }
            Script::Timeout => {
                assert_eq!(
                    provenance(&s, v, 5),
                    (
                        "failed".into(),
                        1,
                        Some("timeout".into()),
                        T1 + 300000,
                        T1 + 1200000
                    )
                );
                assert_eq!(state.breaker_open_until(T1 + 300000), Some(T1 + 900000));
            }
            Script::Archive => {
                assert_eq!(provenance(&s, v, 5).4, T1 + 300000 + 21600000);
                assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
            }
            _ => unreachable!(),
        }
    }
    let (_d, s, v, _clock, source) = breaker_fixture(true);
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.failed, r.filled), (4, 1));
    assert_eq!(elevation(&s, v, 5), Some(0));
    assert_eq!(state.breaker_open_until(T1), Some(T1 + 300000));
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20), point(2, 1, 0, 21)]);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.script("N10E020", Script::Timeout);
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.failed, r.filled), (1, 1));
    assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
    // C9 R11: already-due cached retries are not selected while Open at pass start.
    let (_d, only, ov) = fixture(false);
    seed(&only, ov, vec![point(1, 1, 0, 20)]);
    plant(&only, ov, 1, "failed", None, T1 - 1);
    let source = Source::new(&clock);
    source.cached("N10E020");
    let mut state = TerrainWorkerState::default();
    state.breaker.lock().unwrap().state = BreakerState::Open {
        until_ms: clock.now_ms().unwrap() + 300000,
    };
    let before = provenance(&only, ov, 1);
    let r = pass(&only, &source, &mut state).await;
    assert_eq!((r.filled, r.failed, source.count()), (0, 0, 0));
    assert_eq!(before, provenance(&only, ov, 1));
}

#[tokio::test]
async fn c9_st34_to_st36_failures() {
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20), point(2, 1, 0, 21)]);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.cached("N10E020");
    source.script("N10E021", Script::Quota);
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, r.failed), (1, 1));
    assert_eq!(
        provenance(&s, v, 2),
        (
            "failed".into(),
            1,
            Some("cache_quota_exceeded".into()),
            T1,
            T1 + 3600000
        )
    );
    assert_eq!(state.breaker_open_until(T1), Some(T1 + 300000));
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=3).map(|id| point(id, 1, 0, 19 + id as i32)).collect(),
    );
    let source = Source::new(&clock);
    source.cached("N10E020");
    struct Revoke;
    impl EgressGuard for Revoke {
        fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
            Err(EgressGuardError)
        }
    }
    // C9 ST35: cached A has already committed when B loses admission.
    struct AfterCached<'a>(&'a Source);
    impl TileSource for AfterCached<'_> {
        fn acquire<'a>(
            &'a self,
            t: TileId,
            g: &'a dyn EgressGuard,
            i: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            self.0.acquire(
                t,
                g,
                if t.name() == "N10E020" {
                    &Permit(true)
                } else {
                    i
                },
            )
        }
    }
    let mut state = TerrainWorkerState::default();
    let r = run_terrain_enrichment_pass(
        &s,
        &AfterCached(&source),
        &mut state,
        &Access(true),
        &Revoke,
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!(
        (
            r.filled,
            r.failed,
            r.deferred,
            r.access_denied,
            cursor(&s, v)
        ),
        (1, 0, 2, true, 1)
    );
    assert_eq!(source.count(), 2);
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        1
    );
    assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20), point(2, 1, 0, 21)]);
    let source = Source::new(&clock);
    source.script("N10E021", Script::BadResponse);
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!(state.breaker.lock().unwrap().state, BreakerState::Closed);
    assert_eq!((r.filled, r.failed), (1, 1));
    assert_eq!(elevation(&s, v, 1), Some(0));
    assert_eq!(provenance(&s, v, 2).2, Some("invalid_response".into()));
    let (_d, s, v) = fixture(false);
    seed(&s, v, vec![point(1, 1, 0, 20)]);
    let source = Source::new(&clock);
    source.script("N10E020", Script::Store);
    let r = pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(r.worker_condition, Some("store"));
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        0
    );
}

#[tokio::test]
async fn c9_st44_refused() {
    let (_d, s, v) = fixture(true);
    seed(&s, v, vec![point(1, 1, 0, 20)]);
    let before = durable(&s);
    let conn = s.open().unwrap();
    let version = data_version(&conn);
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    let readonly = s.instrumentation().read_only_opens.load(Ordering::SeqCst);
    let writable = s.instrumentation().writable_opens.load(Ordering::SeqCst);
    let before_reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
    let r = run_terrain_enrichment_pass(
        &s,
        &source,
        &mut TerrainWorkerState::default(),
        &Access(false),
        &Permit(true),
        clock.as_ref(),
    )
    .await
    .unwrap();
    assert!(r.access_denied);
    assert_eq!(
        s.instrumentation().read_only_opens.load(Ordering::SeqCst),
        readonly
    );
    assert_eq!(
        s.instrumentation().writable_opens.load(Ordering::SeqCst),
        writable
    );
    assert_eq!(source.count(), 0);
    assert_eq!(
        before_reads,
        crate::db::TERRAIN_MARKER_READS.with(|n| n.get())
    );
    assert_eq!(before, durable(&s));
    assert_eq!(version, data_version(&conn));
    // C9 R4: a refused access check also protects a broken, unreadable marker.
    s.open()
        .unwrap()
        .execute_batch("DROP TABLE terrain_tidy_state;")
        .unwrap();
    assert!(
        run_terrain_enrichment_pass(
            &s,
            &source,
            &mut TerrainWorkerState::default(),
            &Access(false),
            &Permit(true),
            clock.as_ref()
        )
        .await
        .unwrap()
        .access_denied
    );
}

#[tokio::test]
async fn c9_st30_restart() {
    let (d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=1000)
            .map(|id| point(id, 1, (id - 1) as usize, 20))
            .collect(),
    );
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.script("N10E020", Script::Ramp);
    s.open().unwrap().execute_batch("CREATE TRIGGER fail_third BEFORE INSERT ON terrain_elevation_provenance WHEN NEW.position_id=501 BEGIN SELECT RAISE(ABORT,'synthetic third chunk'); END;").unwrap();
    let result = run_terrain_enrichment_pass(
        &s,
        &source,
        &mut TerrainWorkerState::default(),
        &Access(true),
        &Permit(true),
        clock.as_ref(),
    )
    .await;
    assert!(matches!(result, Err(CollectorError::Store(_))));
    assert_eq!(cursor(&s, v), 500);
    assert_eq!(
        scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
        500
    );
    assert_eq!(elevation(&s, v, 501), None);
    drop(s);
    let s = HubStore::initialize(d.path()).unwrap();
    s.open()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_third;")
        .unwrap();
    pass(&s, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(cursor(&s, v), 1000);
    assert_eq!(pair(&s, v, 1), (Some(1998), Some(0)));
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE attempts=1 AND status='success'"
        ),
        1000
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
        ),
        1000
    );
    for id in 1..=1000 {
        assert_eq!(elevation(&s, v, id), Some((id - 1) * 2));
    }
    let (_d, gold, g) = fixture(false);
    seed(
        &gold,
        g,
        (1..=1000)
            .map(|id| point(id, 1, (id - 1) as usize, 20))
            .collect(),
    );
    pass(&gold, &source, &mut TerrainWorkerState::default()).await;
    assert_eq!(pair(&s, v, 1), pair(&gold, g, 1));
    assert_eq!(
        table(&s, "terrain_elevation_provenance")
            .into_iter()
            .map(|r| r[1..].to_vec())
            .collect::<Vec<_>>(),
        table(&gold, "terrain_elevation_provenance")
            .into_iter()
            .map(|r| r[1..].to_vec())
            .collect::<Vec<_>>()
    );
}

fn st27_rows() -> Vec<ProjectionPosition> {
    (1..=4)
        .map(|id| point(id, 1, ((id - 1) * 6) as usize, 20))
        .collect()
}
fn st27_source(clock: &Arc<ManualClock>) -> Source {
    let source = Source::new(clock);
    source.script(
        "N10E020",
        Script::Nodes(vec![(0, 100), (3, 150), (6, 103), (12, 101), (18, 106)]),
    );
    source
}
#[tokio::test]
async fn c9_st31_replay() {
    let (_d, s, v) = fixture(false);
    seed(&s, v, st27_rows());
    let clock = Arc::new(ManualClock::new());
    let source = st27_source(&clock);
    let mut state = TerrainWorkerState::default();
    assert_eq!(pass(&s, &source, &mut state).await.filled, 4);
    assert_eq!(pair(&s, v, 1), (Some(8), Some(2)));
    let before = durable(&s);
    let conn = s.open().unwrap();
    let version = data_version(&conn);
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, r.failed, r.tiles), (0, 0, 0));
    assert_eq!(source.count(), 1);
    assert_eq!(before, durable(&s));
    assert_eq!(version, data_version(&conn));
}

fn pack_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(root: &Path, here: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in std::fs::read_dir(here).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.push((path.strip_prefix(root).unwrap().to_path_buf(), Vec::new()));
                walk(root, &path, out);
            } else {
                out.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[tokio::test]
async fn c9_st41_no_snapshot() {
    let (dir, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=1200)
            .map(|id| point(id, 1, (id - 1) as usize, 20))
            .collect(),
    );
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    source.script("N10E020", Script::Ramp);
    let conn = s.open().unwrap();
    let tables:Vec<String>=conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE '%catalog%' OR name LIKE '%pack%' OR name LIKE '%lineage%') ORDER BY name").unwrap().query_map([],|r|r.get(0)).unwrap().map(Result::unwrap).collect();
    let before: Vec<_> = tables.iter().map(|t| table(&s, t)).collect();
    let packs = dir.path().join("packs");
    std::fs::create_dir_all(&packs).unwrap();
    std::fs::write(packs.join("synthetic-sentinel"), b"unchanged").unwrap();
    let before_packs = pack_tree(&packs);
    let mut state = TerrainWorkerState::default();
    pass(&s, &source, &mut state).await;
    pass(&s, &source, &mut state).await;
    assert_eq!(before_packs, pack_tree(&packs));
    assert_eq!(
        before,
        tables.iter().map(|t| table(&s, t)).collect::<Vec<_>>()
    );
    assert_eq!(
        std::fs::read(packs.join("synthetic-sentinel")).unwrap(),
        b"unchanged"
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
        ),
        1200
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='drive'"
        ),
        5
    );
    assert_eq!(scalar(&s, "SELECT COUNT(*) FROM sync_mutations"), 1205);
}

#[tokio::test]
async fn c9_st43_safe_logs() {
    let log = super::DecisionLog::start();
    async {
        for failure in [false, true] {
            let (_d, s, v) = fixture(false);
            let rows = (1..=5)
                .map(|id| {
                    let mut p = point(id, 1, 0, 20);
                    p.latitude = 10.123457 + (id - 1) as f64 * 0.0025;
                    p.longitude = 20.654321
                        + (id - 1) as f64 * 0.005
                        + if failure { ((id - 1) / 2) as f64 } else { 0.0 };
                    p
                })
                .collect();
            seed(&s, v, rows);
            let clock = Arc::new(ManualClock::new());
            let source = Source::new(&clock);
            for lon in 20..=22 {
                source.script(
                    &format!("N10E{lon:03}"),
                    if failure {
                        if lon == 20 {
                            Script::BadResponse
                        } else if lon == 21 {
                            Script::Timeout
                        } else {
                            Script::Value(4321)
                        }
                    } else {
                        Script::Value(4321)
                    },
                );
            }
            pass(&s, &source, &mut TerrainWorkerState::default()).await;
        }
    }
    .await;
    let text = log.contents();
    assert!(text.contains("terrain pass complete"));
    assert!(!text.contains("10.123"));
    assert!(!text.contains("20.654"));
    for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        assert!(!matches!(
            token,
            "4321" | "4322" | "4323" | "4324" | "4325" | "4326" | "4327" | "4328" | "4329"
        ));
    }
    for bytes in text.as_bytes().windows(7) {
        assert!(
            !(matches!(bytes[0], b'N' | b'S')
                && bytes[1..3].iter().all(u8::is_ascii_digit)
                && matches!(bytes[3], b'E' | b'W')
                && bytes[4..].iter().all(u8::is_ascii_digit))
        );
    }
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("terrain pass complete"))
            .count(),
        2
    );
}

#[tokio::test]
async fn c9_st59_serial() {
    let (_d, s, v) = fixture(false);
    seed(&s, v, (1..=500).map(|id| point(id, 1, 0, 20)).collect());
    let clock = Arc::new(ManualClock::new());
    let source = Source::new(&clock);
    struct Held<'a> {
        source: &'a Source,
        entered: tokio::sync::Notify,
        resume: tokio::sync::Notify,
        held: AtomicBool,
    }
    impl TileSource for Held<'_> {
        fn acquire<'a>(
            &'a self,
            t: TileId,
            g: &'a dyn EgressGuard,
            i: &'a dyn EgressGuard,
        ) -> Pin<Box<dyn Future<Output = Result<AcquiredTile, AcquireFailure>> + Send + 'a>>
        {
            Box::pin(async move {
                if !self.held.swap(true, Ordering::SeqCst) {
                    self.entered.notify_one();
                    self.resume.notified().await;
                }
                self.source.acquire(t, g, i).await
            })
        }
    }
    struct CountAccess {
        count: AtomicUsize,
        changed: tokio::sync::Notify,
    }
    impl SensitiveAccessCheck for CountAccess {
        fn check(&self) -> Result<(), CollectorError> {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.changed.notify_one();
            Ok(())
        }
    }
    let held = Held {
        source: &source,
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
        held: AtomicBool::new(false),
    };
    let access = CountAccess {
        count: AtomicUsize::new(0),
        changed: tokio::sync::Notify::new(),
    };
    let marker_reads = crate::db::TERRAIN_MARKER_READS.with(|n| n.get());
    let mut a = TerrainWorkerState::default();
    let mut b = TerrainWorkerState::default();
    let first =
        run_terrain_enrichment_pass(&s, &held, &mut a, &access, &Permit(true), clock.as_ref());
    let second = async {
        held.entered.notified().await;
        run_terrain_enrichment_pass(&s, &held, &mut b, &access, &Permit(true), clock.as_ref()).await
    };
    let release = async {
        while access.count.load(Ordering::SeqCst) < 3 {
            access.changed.notified().await;
        }
        // C9 R27, C9 ST73: the waiting pass has not read its marker yet.
        assert_eq!(
            crate::db::TERRAIN_MARKER_READS.with(|n| n.get()) - marker_reads,
            1
        );
        assert_eq!(
            scalar(&s, "SELECT COUNT(*) FROM terrain_elevation_provenance"),
            0
        );
        held.resume.notify_one();
    };
    let (a, b, ()) = tokio::join!(first, second, release);
    assert_eq!(a.unwrap().filled + b.unwrap().filled, 500);
    assert_eq!(source.count(), 1);
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM terrain_elevation_provenance WHERE attempts=1"
        ),
        500
    );
    assert_eq!(
        scalar(
            &s,
            "SELECT COUNT(*) FROM sync_mutations WHERE entity='position'"
        ),
        500
    );
}

#[tokio::test]
async fn c9_st64_clock_stamps() {
    let (_d, s, v) = fixture(false);
    seed(
        &s,
        v,
        (1..=3).map(|id| point(id, 1, 0, 19 + id as i32)).collect(),
    );
    let clock = Arc::new(ManualClock::new());
    let mut source = Source::new(&clock);
    source.advance_ms = 400000;
    source.script("N10E021", Script::Timeout);
    let mut state = TerrainWorkerState::default();
    let r = pass(&s, &source, &mut state).await;
    assert_eq!((r.filled, r.failed), (2, 1));
    assert_eq!(provenance(&s, v, 1).3, T1 + 400000);
    assert_eq!(
        provenance(&s, v, 2),
        (
            "failed".into(),
            1,
            Some("timeout".into()),
            T1 + 800000,
            T1 + 1700000
        )
    );
    assert_eq!(provenance(&s, v, 3).3, T1 + 1200000);
    assert_eq!(
        state.breaker.lock().unwrap().provider_failures,
        [(TileId::parse("N10E021").unwrap(), T1 + 800000)]
    );
    assert_eq!(
        scalar(&s, "SELECT updated_at_ms FROM terrain_enrichment_state"),
        T1 + 1200000
    );
}

#[path = "b3_c9_p_checks.rs"]
mod checks;
#[path = "b3_c9_p_refills.rs"]
mod refills;
#[path = "b3_c9_p_tidy.rs"]
mod tidy;
