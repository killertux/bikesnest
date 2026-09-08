//! SQL-backed parking details.

use crate::Db;
use crate::parking::search::reader_err;
use async_trait::async_trait;
use bikesnest_application::{ParkingDetailsReader, ReaderError};
use bikesnest_domain::{
    Cost, CurrencyCode, GeoPoint, ModerationState, Money, OpeningHours, ParkingLocation,
    ParkingType, PricingUnit, Rating, SecurityFeature, SecurityState, TimeRange,
};

pub struct SqlxParkingDetailsReader {
    db: Db,
}

impl SqlxParkingDetailsReader {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct LocationRow {
    id: i64,
    name: String,
    address: String,
    description: Option<String>,
    parking_type: String,
    cost_kind: String,
    price_cents: Option<i64>,
    price_currency: Option<String>,
    price_unit: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    timezone: String,
    hours_unknown: bool,
    rating_avg: Option<f64>,
    rating_count: i32,
    moderation_state: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    last_meaningful_update_at: Option<chrono::DateTime<chrono::Utc>>,
    last_verified_at: Option<chrono::DateTime<chrono::Utc>>,
    version: i64,
}

#[derive(sqlx::FromRow)]
struct HoursRow {
    day_of_week: i16,
    opens_at: chrono::NaiveTime,
    closes_at: chrono::NaiveTime,
    all_day: bool,
}

#[derive(sqlx::FromRow)]
struct SecurityRow {
    feature_code: String,
    state: i16,
}

#[async_trait]
impl ParkingDetailsReader for SqlxParkingDetailsReader {
    async fn details(&self, id: i64) -> Result<Option<ParkingLocation>, ReaderError> {
        let Some(row) = sqlx::query_as::<_, LocationRow>(
            r#"
            SELECT id, name, address, description, parking_type, cost_kind, price_cents,
                   price_currency, price_unit, COALESCE(lat, 0) AS lat, COALESCE(lon, 0) AS lon,
                   timezone, hours_unknown,
                   rating_avg::float8 AS rating_avg, rating_count, moderation_state,
                   created_at, updated_at, last_meaningful_update_at, last_verified_at, version
            FROM parking_location
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| reader_err("details.acquire", e))?,
        )
        .await
        .map_err(|e| reader_err("details.details", e))?
        else {
            return Ok(None);
        };

        let hours_rows = sqlx::query_as::<_, HoursRow>(
            r#"
            SELECT day_of_week, opens_at, closes_at, all_day
            FROM opening_hours WHERE location_id = $1
            ORDER BY day_of_week, opens_at
            "#,
        )
        .bind(id)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| reader_err("details.acquire", e))?,
        )
        .await
        .map_err(|e| reader_err("details.details", e))?;

        let security_rows = sqlx::query_as::<_, SecurityRow>(
            r#"
            SELECT feature_code, state
            FROM parking_security
            WHERE location_id = $1
            ORDER BY feature_code
            "#,
        )
        .bind(id)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| reader_err("details.acquire", e))?,
        )
        .await
        .map_err(|e| reader_err("details.details", e))?;

        to_domain(row, hours_rows, security_rows).map(Some)
    }
}

fn to_domain(
    row: LocationRow,
    hours: Vec<HoursRow>,
    security: Vec<SecurityRow>,
) -> Result<ParkingLocation, ReaderError> {
    let timezone: chrono_tz::Tz = row
        .timezone
        .parse()
        .map_err(|_| ReaderError::Unexpected(format!("bad timezone {}", row.timezone)))?;

    let price = match (row.price_cents, &row.price_currency, &row.price_unit) {
        (Some(cents), Some(cur), Some(unit)) => Some(Money::new(
            cents,
            CurrencyCode::parse(cur).map_err(|e| ReaderError::Unexpected(e.to_string()))?,
            PricingUnit::from_code(unit).map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        )),
        _ => None,
    };
    let cost = Cost::from_kind_and_price(&row.cost_kind, price)
        .map_err(|e| ReaderError::Unexpected(e.to_string()))?;

    let opening = if row.hours_unknown {
        OpeningHours::Unknown
    } else {
        OpeningHours::weekly(
            hours
                .into_iter()
                .map(|h| {
                    (
                        h.day_of_week as u8,
                        TimeRange {
                            opens_at: h.opens_at,
                            closes_at: h.closes_at,
                            all_day: h.all_day,
                        },
                    )
                })
                .collect(),
        )
    };

    let security = security
        .into_iter()
        .map(|s| {
            Ok(SecurityFeature::new(
                s.feature_code,
                SecurityState::from_smallint(s.state)
                    .map_err(|e| ReaderError::Unexpected(e.to_string()))?,
            ))
        })
        .collect::<Result<Vec<_>, ReaderError>>()?;

    ParkingLocation::new(
        row.id,
        row.name,
        row.address,
        row.description,
        ParkingType::from_code(&row.parking_type)
            .map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        cost,
        GeoPoint::new(row.lat.unwrap_or(0.0), row.lon.unwrap_or(0.0))
            .map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        timezone,
        opening,
        security,
        ModerationState::from_code(&row.moderation_state)
            .map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        Rating::new(row.rating_avg, i64::from(row.rating_count))
            .map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        row.created_at,
        row.updated_at,
        row.last_meaningful_update_at,
        row.last_verified_at,
        row.version,
    )
    .map_err(|e| ReaderError::Unexpected(e.to_string()))
}
