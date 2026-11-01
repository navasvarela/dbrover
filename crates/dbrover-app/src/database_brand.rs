//! What the app shows of each database engine: its logo and the logo's colours. The one
//! place the app looks these up per engine, so a new engine adds a row here and its SVG.

use dbrover_core::DriverKind;
use egui::Color32;

/// A database's logo and the colours it is tinted with.
pub struct DatabaseBrand {
    /// The image loader's cache key for the logo.
    pub logo_uri: &'static str,
    /// A Simple Icons SVG; see `assets/icons/databases/README.md` for where each comes from
    /// and how to add one.
    pub logo_svg: &'static [u8],
    /// The brand's own colour, from Simple Icons, for the light theme.
    pub light: Color32,
    /// A lighter shade of the same hue for the dark theme, where the brand colour would be
    /// too dark to see.
    pub dark: Color32,
}

pub fn of(driver: DriverKind) -> DatabaseBrand {
    match driver {
        DriverKind::Postgres => DatabaseBrand {
            logo_uri: "bytes://logos/postgresql.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/postgresql.svg"),
            light: Color32::from_rgb(0x41, 0x69, 0xE1),
            dark: Color32::from_rgb(0x7F, 0xA2, 0xF5),
        },
        DriverKind::Sqlite => DatabaseBrand {
            logo_uri: "bytes://logos/sqlite.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/sqlite.svg"),
            light: Color32::from_rgb(0x00, 0x3B, 0x57),
            dark: Color32::from_rgb(0x5A, 0xB0, 0xE0),
        },
        DriverKind::Mysql => DatabaseBrand {
            logo_uri: "bytes://logos/mysql.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/mysql.svg"),
            light: Color32::from_rgb(0x44, 0x79, 0xA1),
            dark: Color32::from_rgb(0x7F, 0xB2, 0xDB),
        },
        // A neutral glyph in SQL Server's red: Simple Icons no longer carries its logo.
        DriverKind::SqlServer => DatabaseBrand {
            logo_uri: "bytes://logos/sqlserver.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/sqlserver.svg"),
            light: Color32::from_rgb(0xCC, 0x29, 0x27),
            dark: Color32::from_rgb(0xF0, 0x7C, 0x78),
        },
        // DuckDB's yellow on the dark theme; on the light one, a gold dark enough to see.
        DriverKind::DuckDb => DatabaseBrand {
            logo_uri: "bytes://logos/duckdb.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/duckdb.svg"),
            light: Color32::from_rgb(0x8A, 0x78, 0x00),
            dark: Color32::from_rgb(0xFF, 0xF0, 0x00),
        },
        // ClickHouse's yellow on the dark theme; on the light one, an amber dark enough to
        // see and apart from DuckDB's gold.
        DriverKind::ClickHouse => DatabaseBrand {
            logo_uri: "bytes://logos/clickhouse.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/clickhouse.svg"),
            light: Color32::from_rgb(0x9A, 0x6A, 0x00),
            dark: Color32::from_rgb(0xFF, 0xCC, 0x01),
        },
        // Redis's red on the dark theme, lightened to read there; on the light one, a darker
        // red than the brand's #FF4438, which is too light on white. Valkey's data sources
        // share it: one driver serves both.
        DriverKind::Redis => DatabaseBrand {
            logo_uri: "bytes://logos/redis.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/redis.svg"),
            light: Color32::from_rgb(0xD8, 0x2C, 0x20),
            dark: Color32::from_rgb(0xFF, 0x6E, 0x63),
        },
        // MongoDB's green on the light theme, darkened a little to read on white; a lighter
        // green on the dark one.
        DriverKind::Mongo => DatabaseBrand {
            logo_uri: "bytes://logos/mongodb.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/mongodb.svg"),
            light: Color32::from_rgb(0x3B, 0x8A, 0x3C),
            dark: Color32::from_rgb(0x6C, 0xC6, 0x6D),
        },
        // Elastic's teal on the light theme, as it is; a lighter teal on the dark one. Also
        // shown for OpenSearch, which the same driver serves.
        DriverKind::Elasticsearch => DatabaseBrand {
            logo_uri: "bytes://logos/elasticsearch.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/elasticsearch.svg"),
            light: Color32::from_rgb(0x00, 0x55, 0x71),
            dark: Color32::from_rgb(0x4F, 0xB8, 0xD4),
        },
        // Cassandra's blue on the light theme, as it is; a lighter blue on the dark one. Also
        // shown for ScyllaDB, which the same driver serves.
        DriverKind::Cassandra => DatabaseBrand {
            logo_uri: "bytes://logos/apachecassandra.svg",
            logo_svg: include_bytes!("../../../assets/icons/databases/apachecassandra.svg"),
            light: Color32::from_rgb(0x12, 0x87, 0xB1),
            dark: Color32::from_rgb(0x6C, 0xC4, 0xE6),
        },
    }
}
