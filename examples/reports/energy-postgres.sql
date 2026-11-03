-- Made-up energy flows for the battery examples: 300 charges and discharges of three batteries,
-- from 2026-09-01 to the end of November. Every value comes from a formula, so the table is the
-- same on every machine; none of it is anyone's real data.
--
-- Make it in DbRover: add a PostgreSQL data source named "energy-postgres", open this file in
-- its console (File → Open…), and run it all. It replaces the schema "energy".

DROP SCHEMA IF EXISTS energy CASCADE;
CREATE SCHEMA energy;

-- amount is in the smallest unit the battery's meter records: watt-hours for bat-1 (10^-3
-- kWh), milliwatt-hours for bat-2 (10^-6 kWh) and microwatt-hours for bat-3 (10^-9 kWh). The
-- batteries' units are in DynamoDB (energy-batteries.partiql). A positive amount is a charge,
-- a negative one a discharge.
CREATE TABLE energy.flows (
  id          integer PRIMARY KEY,
  battery_id  text NOT NULL,
  amount      numeric(38,0) NOT NULL,
  metered_at  timestamp NOT NULL
);

-- Every fourth flow of a battery discharges three fifths of the charge before it, so no
-- battery's charge ever goes below zero; the others charge 50 to 500 kWh. Each amount is a
-- whole number from a formula: bat-3's uses all 9 of its meter's places. No floating point.
CREATE FUNCTION pg_temp.charged(n integer) RETURNS numeric LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE n % 3
    WHEN 0 THEN ((n * 7919) % 450000 + 50000)::numeric
    WHEN 1 THEN ((n * 7919) % 450000 + 50000)::numeric * 1000 + (n * 104729) % 1000
    ELSE ((n * 7919) % 450000 + 50000)::numeric * 1000000 + (n * 104729) % 1000000
  END
$$;

INSERT INTO energy.flows (id, battery_id, amount, metered_at)
SELECT n,
       (ARRAY['bat-1', 'bat-2', 'bat-3'])[n % 3 + 1],
       CASE WHEN (n / 3) % 4 = 3 THEN -(pg_temp.charged(n - 3) * 3 / 5)
            ELSE pg_temp.charged(n) END,
       timestamp '2026-09-01 00:00' + n * interval '6 hours 47 minutes'
FROM generate_series(1, 300) AS n;
