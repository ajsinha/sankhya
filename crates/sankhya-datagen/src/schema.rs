//! The synthetic schemas.

/// How a table behaves under mutation. This drives which storage strategy the engine
/// chooses, so the fixture set must cover all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WriteProfile {
    /// Rows are never updated. The base table is the append target and there is no
    /// merge cost at all.
    AppendOnly,
    /// Rows are updated occasionally, usually soon after insertion.
    SlowlyChanging,
    /// Rows are updated frequently and throughout their life. The expensive case.
    HotMutable,
}

/// A column's logical shape, independent of any storage system's type names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColumnKind {
    /// Monotonically increasing key.
    Serial,
    /// Bounded-cardinality label — the shape that dictionary-encodes well.
    Category { cardinality: u32 },
    /// High-cardinality identifier — the shape that defeats range pruning and where a
    /// probabilistic filter starts to pay.
    Identifier { cardinality: u32 },
    /// Free text of roughly the given length.
    Text { mean_len: u32 },
    /// Incompressible payload large enough to be stored out-of-line, which is what
    /// produces withheld values on update.
    LargePayload { len: u32 },
    /// Exact decimal.
    Decimal { precision: u8, scale: u8 },
    /// Approximate real number.
    Real,
    /// Whole number in a range.
    Integer { min: i64, max: i64 },
    /// True or false.
    Boolean,
    /// A point in time, increasing with the row sequence.
    Timestamp,
    /// A calendar date derived from the timestamp — the natural partition key.
    Date,
    /// Structured document.
    Json,
}

impl ColumnKind {
    /// Approximate uncompressed width, used to size a generation run without
    /// materialising it.
    #[must_use]
    pub const fn approx_bytes(self) -> u32 {
        match self {
            Self::Serial | Self::Timestamp | Self::Integer { .. } | Self::Real => 8,
            Self::Date => 4,
            Self::Boolean => 1,
            Self::Category { .. } => 12,
            Self::Identifier { .. } => 20,
            Self::Text { mean_len } => mean_len,
            Self::LargePayload { len } => len,
            Self::Decimal { .. } => 16,
            Self::Json => 96,
        }
    }
}

/// One column.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub name: &'static str,
    pub kind: ColumnKind,
    pub nullable: bool,
}

const fn col(name: &'static str, kind: ColumnKind, nullable: bool) -> Column {
    Column {
        name,
        kind,
        nullable,
    }
}

/// A synthetic table.
#[derive(Clone, Copy, Debug)]
pub struct Schema {
    pub name: &'static str,
    pub columns: &'static [Column],
    pub profile: WriteProfile,
    /// Fraction of the table's rows that receive an update, in parts per thousand.
    pub update_rate_per_mille: u32,
    /// Fraction deleted, in parts per thousand.
    pub delete_rate_per_mille: u32,
}

impl Schema {
    /// Approximate uncompressed bytes per row.
    #[must_use]
    pub fn approx_row_bytes(&self) -> u64 {
        self.columns
            .iter()
            .map(|c| u64::from(c.kind.approx_bytes()))
            .sum()
    }

    /// Rows needed to reach approximately the requested uncompressed size.
    #[must_use]
    pub fn rows_for_bytes(&self, bytes: u64) -> u64 {
        bytes / self.approx_row_bytes().max(1)
    }
}

// --- the ten schemas -------------------------------------------------------------
// Deliberately spread across unrelated domains. None is financial.

/// Vehicle telemetry: the extreme narrow-and-numerous shape.
static SHIPMENT_SCANS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "consignment_ref",
        ColumnKind::Identifier {
            cardinality: 2_000_000,
        },
        false,
    ),
    col("hub_code", ColumnKind::Category { cardinality: 240 }, false),
    col("status", ColumnKind::Category { cardinality: 12 }, false),
    col(
        "weight_kg",
        ColumnKind::Decimal {
            precision: 10,
            scale: 3,
        },
        false,
    ),
    col("scanned_at", ColumnKind::Timestamp, false),
    col("scan_date", ColumnKind::Date, false),
];

static DEVICE_READINGS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "device_id",
        ColumnKind::Identifier {
            cardinality: 500_000,
        },
        false,
    ),
    col("metric", ColumnKind::Category { cardinality: 32 }, false),
    col("value", ColumnKind::Real, false),
    col("quality", ColumnKind::Integer { min: 0, max: 100 }, false),
    col("observed_at", ColumnKind::Timestamp, false),
    col("observed_date", ColumnKind::Date, false),
];

static ORDER_LINES: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "order_ref",
        ColumnKind::Identifier {
            cardinality: 1_500_000,
        },
        false,
    ),
    col(
        "product_code",
        ColumnKind::Category {
            cardinality: 40_000,
        },
        false,
    ),
    col("quantity", ColumnKind::Integer { min: 1, max: 500 }, false),
    col(
        "unit_price",
        ColumnKind::Decimal {
            precision: 12,
            scale: 4,
        },
        false,
    ),
    col(
        "discount",
        ColumnKind::Decimal {
            precision: 6,
            scale: 4,
        },
        true,
    ),
    col("placed_at", ColumnKind::Timestamp, false),
    col("placed_date", ColumnKind::Date, false),
];

static INVENTORY_LEVELS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "location_code",
        ColumnKind::Category { cardinality: 1_200 },
        false,
    ),
    col(
        "product_code",
        ColumnKind::Category {
            cardinality: 40_000,
        },
        false,
    ),
    col(
        "on_hand",
        ColumnKind::Integer {
            min: 0,
            max: 100_000,
        },
        false,
    ),
    col(
        "reserved",
        ColumnKind::Integer {
            min: 0,
            max: 10_000,
        },
        false,
    ),
    col("updated_at", ColumnKind::Timestamp, false),
    col("updated_date", ColumnKind::Date, false),
];

static SUPPORT_TICKETS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col("subject", ColumnKind::Text { mean_len: 72 }, false),
    col("body", ColumnKind::Text { mean_len: 900 }, true),
    col("queue", ColumnKind::Category { cardinality: 48 }, false),
    col("priority", ColumnKind::Category { cardinality: 4 }, false),
    col("resolved", ColumnKind::Boolean, false),
    col("opened_at", ColumnKind::Timestamp, false),
    col("opened_date", ColumnKind::Date, false),
];

static MEDIA_ASSETS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "asset_ref",
        ColumnKind::Identifier {
            cardinality: 800_000,
        },
        false,
    ),
    col("format", ColumnKind::Category { cardinality: 18 }, false),
    col("thumbnail", ColumnKind::LargePayload { len: 9_000 }, true),
    col(
        "duration_s",
        ColumnKind::Integer {
            min: 1,
            max: 14_400,
        },
        true,
    ),
    col("ingested_at", ColumnKind::Timestamp, false),
    col("ingested_date", ColumnKind::Date, false),
];

static ENERGY_INTERVALS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "meter_ref",
        ColumnKind::Identifier {
            cardinality: 900_000,
        },
        false,
    ),
    col("tariff", ColumnKind::Category { cardinality: 26 }, false),
    col(
        "kwh",
        ColumnKind::Decimal {
            precision: 12,
            scale: 6,
        },
        false,
    ),
    col("estimated", ColumnKind::Boolean, false),
    col("interval_start", ColumnKind::Timestamp, false),
    col("interval_date", ColumnKind::Date, false),
];

static ROUTE_LEGS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "route_ref",
        ColumnKind::Identifier {
            cardinality: 300_000,
        },
        false,
    ),
    col("from_hub", ColumnKind::Category { cardinality: 240 }, false),
    col("to_hub", ColumnKind::Category { cardinality: 240 }, false),
    col(
        "distance_km",
        ColumnKind::Decimal {
            precision: 9,
            scale: 2,
        },
        false,
    ),
    col("departed_at", ColumnKind::Timestamp, false),
    col("departed_date", ColumnKind::Date, false),
];

static SENSOR_CALIBRATIONS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "device_id",
        ColumnKind::Identifier {
            cardinality: 500_000,
        },
        false,
    ),
    col(
        "offset_value",
        ColumnKind::Decimal {
            precision: 10,
            scale: 6,
        },
        false,
    ),
    col(
        "technician",
        ColumnKind::Category { cardinality: 2_400 },
        false,
    ),
    col("notes", ColumnKind::Text { mean_len: 140 }, true),
    col("calibrated_at", ColumnKind::Timestamp, false),
    col("calibrated_date", ColumnKind::Date, false),
];

static ACCESS_EVENTS: &[Column] = &[
    col("id", ColumnKind::Serial, false),
    col(
        "principal_ref",
        ColumnKind::Identifier {
            cardinality: 120_000,
        },
        false,
    ),
    col("resource", ColumnKind::Text { mean_len: 60 }, false),
    col("action", ColumnKind::Category { cardinality: 20 }, false),
    col("allowed", ColumnKind::Boolean, false),
    col("context", ColumnKind::Json, true),
    col("occurred_at", ColumnKind::Timestamp, false),
    col("occurred_date", ColumnKind::Date, false),
];

static SCHEMAS: &[Schema] = &[
    Schema {
        name: "shipment_scans",
        columns: SHIPMENT_SCANS,
        profile: WriteProfile::AppendOnly,
        update_rate_per_mille: 0,
        delete_rate_per_mille: 0,
    },
    Schema {
        name: "device_readings",
        columns: DEVICE_READINGS,
        profile: WriteProfile::AppendOnly,
        update_rate_per_mille: 0,
        delete_rate_per_mille: 0,
    },
    Schema {
        name: "order_lines",
        columns: ORDER_LINES,
        profile: WriteProfile::SlowlyChanging,
        update_rate_per_mille: 45,
        delete_rate_per_mille: 3,
    },
    Schema {
        name: "inventory_levels",
        columns: INVENTORY_LEVELS,
        profile: WriteProfile::HotMutable,
        update_rate_per_mille: 600,
        delete_rate_per_mille: 5,
    },
    Schema {
        name: "support_tickets",
        columns: SUPPORT_TICKETS,
        profile: WriteProfile::HotMutable,
        update_rate_per_mille: 320,
        delete_rate_per_mille: 8,
    },
    Schema {
        name: "media_assets",
        columns: MEDIA_ASSETS,
        profile: WriteProfile::SlowlyChanging,
        update_rate_per_mille: 60,
        delete_rate_per_mille: 4,
    },
    Schema {
        name: "energy_intervals",
        columns: ENERGY_INTERVALS,
        profile: WriteProfile::AppendOnly,
        update_rate_per_mille: 0,
        delete_rate_per_mille: 0,
    },
    Schema {
        name: "route_legs",
        columns: ROUTE_LEGS,
        profile: WriteProfile::SlowlyChanging,
        update_rate_per_mille: 30,
        delete_rate_per_mille: 2,
    },
    Schema {
        name: "sensor_calibrations",
        columns: SENSOR_CALIBRATIONS,
        profile: WriteProfile::SlowlyChanging,
        update_rate_per_mille: 80,
        delete_rate_per_mille: 6,
    },
    Schema {
        name: "access_events",
        columns: ACCESS_EVENTS,
        profile: WriteProfile::AppendOnly,
        update_rate_per_mille: 0,
        delete_rate_per_mille: 0,
    },
];

/// Every synthetic schema.
#[must_use]
pub fn all_schemas() -> &'static [Schema] {
    SCHEMAS
}

/// Look one up by name.
#[must_use]
pub fn schema_by_name(name: &str) -> Option<&'static Schema> {
    SCHEMAS.iter().find(|s| s.name == name)
}
