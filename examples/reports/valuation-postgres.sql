-- Synthetic transactions for the valuation and cost basis examples: 300 trades of BTC, ETH and
-- SOL by three wallets, from 2026-09-01 to the end of November. Every value comes from a
-- formula, so the table is the same on every machine; none of it is anyone's real data.
--
-- Make it in DbRover: add a PostgreSQL data source named "ledger-postgres", open this file in
-- its console (File → Open…), and run it all. It replaces the schema "valuation".

DROP SCHEMA IF EXISTS valuation CASCADE;
CREATE SCHEMA valuation;

-- amount is in the asset's smallest unit, as a chain records it: satoshis for BTC (10^-8),
-- wei for ETH (10^-18) and lamports for SOL (10^-9). The assets' units are in DynamoDB
-- (valuation-assets.partiql). A positive amount is an acquisition, a negative one a disposal.
CREATE TABLE valuation.transactions (
  id          integer PRIMARY KEY,
  wallet      text NOT NULL,
  asset_id    text NOT NULL,
  amount      numeric(38,0) NOT NULL,
  executed_at timestamp NOT NULL
);

-- Every fourth trade of an asset disposes of three fifths of the one before it, so no asset's
-- holding ever goes below zero; the others acquire. Each amount is a whole number from a
-- formula: ETH's uses all 18 of its places. No division, no floating point.
CREATE FUNCTION pg_temp.acquired(n integer) RETURNS numeric LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE n % 3
    WHEN 0 THEN ((n * 7919) % 200000 + 1000)::numeric * 1000
    WHEN 1 THEN ((n * 7919) % 30000 + 100)::numeric * 1000000000000000
                + ((n * 104729) % 1000000) * 5
    ELSE ((n * 7919) % 500000 + 1000)::numeric * 1000000 + ((n * 31) % 200) * 5
  END
$$;

INSERT INTO valuation.transactions (id, wallet, asset_id, amount, executed_at)
SELECT n,
       'wallet-' || ((n / 3) % 3 + 1),
       (ARRAY['btc', 'eth', 'sol'])[n % 3 + 1],
       CASE WHEN (n / 3) % 4 = 3 THEN -(pg_temp.acquired(n - 3) * 3 / 5)
            ELSE pg_temp.acquired(n) END,
       timestamp '2026-09-01 00:00' + n * interval '6 hours 47 minutes'
FROM generate_series(1, 300) AS n;
