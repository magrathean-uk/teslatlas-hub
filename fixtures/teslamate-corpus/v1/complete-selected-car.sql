-- Apply after current-schema.sql.  One finished drive and one finished charge
-- share the selected car and linked positions, addresses, and geofence.
INSERT INTO cars (
  id, eid, vid, vin, name, model, efficiency, display_priority, inserted_at, updated_at
) VALUES (
  1, 1001, 2001, '5YJSA111111111111', 'Corpus One', 'Model S', 0.150, 0,
  '2026-01-01 00:00:00', '2026-01-01 00:00:00'
);

INSERT INTO addresses (id, display_name, name) VALUES
  (10, 'Start Street', 'Home'),
  (11, 'Charge Street', 'Charge stop');
INSERT INTO geofences (id, name) VALUES (20, 'Home');

INSERT INTO positions (
  id, car_id, drive_id, date, latitude, longitude, speed, power, odometer,
  ideal_battery_range_km, rated_battery_range_km, battery_level
) VALUES
  (100, 1, 300, '2026-01-01 08:00:00', 51.5000, -0.1200, 0, 0, 1000.0, 400.0, 380.0, 80),
  (101, 1, 300, '2026-01-01 08:30:00', 51.5100, -0.1100, 40, 20, 1010.0, 390.0, 370.0, 78),
  (102, 1, NULL, '2026-01-01 09:00:00', 51.5100, -0.1100, 0, 0, 1010.0, 390.0, 370.0, 78);

INSERT INTO drives (
  id, car_id, start_date, end_date, start_position_id, end_position_id,
  start_address_id, end_address_id, start_geofence_id, end_geofence_id,
  distance, duration_min
) VALUES (
  300, 1, '2026-01-01 08:00:00', '2026-01-01 08:30:00', 100, 101,
  10, 11, 20, NULL, 10.0, 30
);

INSERT INTO charging_processes (
  id, car_id, position_id, address_id, geofence_id, start_date, end_date,
  charge_energy_added, start_battery_level, end_battery_level, duration_min
) VALUES (
  400, 1, 102, 11, NULL, '2026-01-01 09:00:00', '2026-01-01 09:30:00',
  12.5, 78, 90, 30
);

INSERT INTO charges (
  id, charging_process_id, date, battery_level, usable_battery_level,
  charge_energy_added, charger_power, conn_charge_cable, fast_charger_present,
  ideal_battery_range_km, rated_battery_range_km
) VALUES (
  500, 400, '2026-01-01 09:15:00', 84, 84, 6.2, 50, 'IEC', false, 420.0, 400.0
);

INSERT INTO states (id, car_id, state, start_date, end_date) VALUES
  (600, 1, 'online', '2026-01-01 07:59:00', '2026-01-01 09:31:00');
INSERT INTO updates (id, car_id, start_date, end_date, version) VALUES
  (700, 1, '2026-01-01 07:00:00', '2026-01-01 07:01:00', '2026.1.1');
