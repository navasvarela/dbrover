# Database logos

One logo per database DbRover has a driver for, shown next to data sources in the explorer, in
the New data source menu and in the data source dialog.

| File | Logo | Brand colour |
| --- | --- | --- |
| `postgresql.svg` | PostgreSQL | `#4169E1` |
| `sqlite.svg` | SQLite | `#003B57` |
| `mysql.svg` | MySQL (also shown for MariaDB, which the same driver serves) | `#4479A1` |
| `sqlserver.svg` | A neutral database glyph for SQL Server, drawn for DbRover (see below) | `#CC2927` |
| `duckdb.svg` | DuckDB (its yellow is darkened to a gold on the light theme, where it wouldn't show) | `#FFF000` |
| `clickhouse.svg` | ClickHouse (its yellow is darkened to an amber on the light theme) | `#FFCC01` |
| `redis.svg` | Redis (also shown for Valkey and Dragonfly, which the same driver serves; its red is darkened on the light theme) | `#FF4438` |
| `mongodb.svg` | MongoDB (its green is darkened a little on the light theme) | `#47A248` |
| `elasticsearch.svg` | Elasticsearch (also shown for OpenSearch, which the same driver serves; its teal is lightened on the dark theme) | `#005571` |

## Source and licence

The files come from [Simple Icons](https://simpleicons.org) 16.34.0
(`https://cdn.jsdelivr.net/npm/simple-icons@16.34.0/icons/<slug>.svg`), whose icon data is
released under [CC0 1.0](https://github.com/simple-icons/simple-icons/blob/develop/LICENSE.md).

The only change is `fill="#FFFFFF"` on the root `<svg>` element. Simple Icons leave the fill
unset, which renders black, and egui tints an image by multiplying its colours, so a black
shape would stay black whatever the tint. White takes the tint exactly, which is how each logo
gets a shade that reads in the light and the dark theme (`Palette::driver` in
`crates/dbrover-app/src/theme.rs`).

The logos themselves remain trademarks of their owners. DbRover shows them only to identify the
database a data source connects to. See each project's policy, e.g.
[PostgreSQL's](https://www.postgresql.org/about/policies/trademarks/).

`sqlserver.svg` is the exception: Simple Icons no longer carries Microsoft's SQL Server logo
(its last release with it was 12.4.0), so it is a plain database cylinder drawn for DbRover,
under the same licence as DbRover, in the colour SQL Server's logo uses.

## Adding a database

1. Download the icon from the same Simple Icons release (find its slug on simpleicons.org; some
   brands, such as Oracle, have asked Simple Icons to remove theirs).
2. Add `fill="#FFFFFF"` to its `<svg>` element and save it here as `<slug>.svg`.
3. Add it to `icons::driver_logo`, give it light- and dark-theme shades in `Palette::driver`,
   and add a row to the table above. The contrast test checks the shades.
