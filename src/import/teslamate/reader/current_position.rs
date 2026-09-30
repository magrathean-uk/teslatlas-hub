// SPDX-License-Identifier: AGPL-3.0-only

/// A single coherent source row. No position/location coordinates are selected.
pub(crate) struct TeslaMateCurrentPosition {
    pub car_id: i64,
    pub eid: i64,
    pub vin: Option<String>,
    pub position_id: i64,
    pub observed_at_ms: i64,
    pub source_now_ms: i64,
    pub battery_level: Option<i32>,
    pub ideal_range_km: Option<f64>,
    pub est_range_km: Option<f64>,
    pub rated_range_km: Option<f64>,
    pub odometer_km: Option<f64>,
}

const CURRENT_POSITION_SQL: &str = "SELECT c.id::bigint AS car_id, c.eid::bigint AS eid, c.vin,
    p.id::bigint AS position_id,
    (EXTRACT(EPOCH FROM p.date AT TIME ZONE 'UTC') * 1000)::bigint AS observed_at_ms,
    (EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint AS source_now_ms,
    p.battery_level::integer AS battery_level,
    p.ideal_battery_range_km::double precision AS ideal_range_km,
    p.est_battery_range_km::double precision AS est_range_km,
    p.rated_battery_range_km::double precision AS rated_range_km,
    p.odometer::double precision AS odometer_km
    FROM public.cars c JOIN LATERAL (
        SELECT id, date, battery_level, ideal_battery_range_km, est_battery_range_km, rated_battery_range_km, odometer
        FROM public.positions WHERE car_id = c.id ORDER BY date DESC, id DESC LIMIT 1
    ) p ON true WHERE c.id = $1";

pub(crate) async fn read_current_position(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    car_id: i64,
) -> Result<Option<TeslaMateCurrentPosition>, TeslaMateReaderError> {
    let car_id = selected_source_car_id(car_id)?;
    let limits = TeslaMateReadLimits {
        connect_timeout: Duration::from_secs(5),
        ..Default::default()
    };
    let (client, task) = connect_source(source, password, limits).await?;
    let session = TeslaMateSnapshotSession::new(client, task);
    let result = async {
        for sql in source.session_sql() {
            session.client().batch_execute(sql).await?;
        }
        session
            .client()
            .batch_execute("SET LOCAL statement_timeout = '5000ms'")
            .await?;
        let row = session
            .client()
            .query_opt(CURRENT_POSITION_SQL, &[&car_id])
            .await?;
        row.map(|row| {
            Ok(TeslaMateCurrentPosition {
                car_id: row.try_get("car_id")?,
                eid: row.try_get("eid")?,
                vin: row.try_get("vin")?,
                position_id: row.try_get("position_id")?,
                observed_at_ms: row.try_get("observed_at_ms")?,
                source_now_ms: row.try_get("source_now_ms")?,
                battery_level: row.try_get("battery_level")?,
                ideal_range_km: row.try_get("ideal_range_km")?,
                est_range_km: row.try_get("est_range_km")?,
                rated_range_km: row.try_get("rated_range_km")?,
                odometer_km: row.try_get("odometer_km")?,
            })
        })
        .transpose()
        .map_err(TeslaMateReaderError::Postgres)
    }
    .await;
    let finished = session.finish().await;
    match result {
        Ok(value) => {
            finished?;
            Ok(value)
        }
        Err(error) => Err(error),
    }
}
