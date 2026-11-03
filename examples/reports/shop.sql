-- A made-up bike shop for the example reports: its products and three months of order lines,
-- from 2026-09-01 to 2026-11-30. Every value comes from a formula, so the shop is the same on
-- every machine; none of it is anyone's real data.
--
-- Make it in DbRover: add a SQLite data source named "shop" with a new file, open this file
-- in its console (File → Open…), and run it all.

CREATE TABLE products (
  id       INTEGER PRIMARY KEY,
  sku      TEXT NOT NULL UNIQUE,
  name     TEXT NOT NULL,
  -- accessories, bikes, clothing or parts
  category TEXT NOT NULL,
  price    NUMERIC NOT NULL
);

-- The bell is never ordered, so a report that lists every product shows one without sales.
INSERT INTO products (id, sku, name, category, price) VALUES
  (1, 'B-100', 'City bike', 'bikes', 649),
  (2, 'B-200', 'Road bike', 'bikes', 1290),
  (3, 'P-100', 'Chain', 'parts', 24),
  (4, 'P-200', 'Brake pads', 'parts', 18),
  (5, 'C-100', 'Rain jacket', 'clothing', 89),
  (6, 'C-200', 'Gloves', 'clothing', 29),
  (7, 'A-100', 'Lights', 'accessories', 39),
  (8, 'A-200', 'Bell', 'accessories', 12);

-- SQLite has no exact decimal type, so the amounts are floating-point numbers here; the reports
-- declare them decimal(18,2), which rounds each to the cent as it loads. PostgreSQL's numeric
-- would keep every digit. customer_id names a customer whose name and country are in MongoDB
-- (shop-customers.js). A return is a line of its own, with a negative quantity and amount.
CREATE TABLE order_lines (
  id          INTEGER PRIMARY KEY,
  order_no    INTEGER NOT NULL,
  product_id  INTEGER NOT NULL REFERENCES products (id),
  ordered_on  TEXT NOT NULL,
  customer_id TEXT NOT NULL,
  quantity    INTEGER NOT NULL,
  amount      NUMERIC NOT NULL
);

-- 720 lines over 91 days, two to an order, from twelve customers. A bike sells one at a time,
-- anything else one to four at a time, at its price plus a few cents that vary by line. Every
-- tenth line returns what the line before it sold.
INSERT INTO order_lines (order_no, product_id, ordered_on, customer_id, quantity, amount)
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 720),
lines AS (
  SELECT i,
         CASE WHEN i % 10 = 0 THEN i - 1 ELSE i END AS line,
         CASE WHEN i % 10 = 0 THEN -1 ELSE 1 END AS sign
  FROM n
)
SELECT (l.line + 1) / 2,
       p.id,
       date('2026-09-01', '+' || ((l.i - 1) * 91 / 720) || ' days'),
       printf('cust-%02d', ((l.line + 1) / 2) % 12 + 1),
       l.sign * CASE p.category WHEN 'bikes' THEN 1 ELSE 1 + l.line % 4 END,
       l.sign * CASE p.category WHEN 'bikes' THEN 1 ELSE 1 + l.line % 4 END
              * (p.price + ((l.line * 31) % 100) / 100.0)
FROM lines l
JOIN products p ON p.id = (l.line * 5) % 7 + 1
ORDER BY l.i;
