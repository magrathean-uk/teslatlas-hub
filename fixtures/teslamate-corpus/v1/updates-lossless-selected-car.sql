-- Deterministic TeslaMate v4 updates source-shape fixture.
-- PostgreSQL COPY CSV format: comma delimiter, \N null marker, LF row ending.
BEGIN;

CREATE TABLE cars (
  id smallint PRIMARY KEY
);

CREATE TABLE updates (
  id integer PRIMARY KEY,
  car_id smallint NOT NULL REFERENCES cars(id),
  start_date timestamp without time zone NOT NULL,
  end_date timestamp without time zone,
  version character varying(255),
  CONSTRAINT positive_duration CHECK (end_date >= start_date)
);

COPY cars (id) FROM stdin WITH (FORMAT csv, NULL '\N');
-32768
\.

COPY updates (id, car_id, start_date, end_date, version) FROM stdin WITH (FORMAT csv, NULL '\N');
-2147483648,-32768,-infinity,1999-12-31 23:59:59.999999,"  βeta 🚗  "
101,-32768,2026-01-01 00:00:00.123456,2026-01-01 00:00:00.223457,2026.2.1
99,-32768,2026-01-01 00:00:00.123456,2026-01-01 00:00:00.323456,""
-1,-32768,2026-01-02 00:00:00.999999,2026-01-02 00:00:01.000001,\N
0,-32768,2026-01-03 04:05:06.000001,\N,\N
2147483647,-32768,2030-12-31 23:59:59.999999,infinity,release-∞
\.

COMMIT;
