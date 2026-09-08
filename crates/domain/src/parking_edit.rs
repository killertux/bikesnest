//! Editable listing information and its versioned proposal/snapshot codec.
use crate::{
    Cost, CurrencyCode, Money, OpeningHours, ParkingLocation, ParkingType, PricingUnit,
    SecurityFeature, SecurityState, TimeRange,
};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct ParkingEdit {
    pub name: String,
    pub address: String,
    pub description: Option<String>,
    pub parking_type: ParkingType,
    pub cost: Cost,
    pub hours: OpeningHours,
    pub security: Vec<SecurityFeature>,
}

impl ParkingEdit {
    pub fn from_location(location: &ParkingLocation) -> Self {
        Self {
            name: location.name().to_string(),
            address: location.address().to_string(),
            description: location.description().map(str::to_string),
            parking_type: location.parking_type(),
            cost: location.cost().clone(),
            hours: location.hours().clone(),
            security: location.security().to_vec(),
        }
    }

    /// Uses the same public-field shape as saved parking revisions.
    pub fn to_json(&self) -> Value {
        let cost = match &self.cost {
            Cost::Paid { price: Some(p) } => {
                json!({"kind":"paid", "cents":p.cents(), "currency":p.currency().as_str(), "unit":p.unit().as_code()})
            }
            other => json!({"kind":other.kind_code()}),
        };
        let rows: Vec<Value> = match &self.hours {
            OpeningHours::Unknown => Vec::new(),
            OpeningHours::Weekly(rows) => rows
                .iter()
                .map(|(day, r)| {
                    json!([
                        day,
                        r.opens_at.to_string(),
                        r.closes_at.to_string(),
                        r.all_day
                    ])
                })
                .collect(),
        };
        let security: Vec<Value> = crate::SECURITY_FEATURE_CODES
            .iter()
            .map(|code| {
                let state = self
                    .security
                    .iter()
                    .find(|f| f.code() == *code)
                    .map(|f| f.state())
                    .unwrap_or(SecurityState::Unknown);
                json!([
                    code,
                    match state {
                        SecurityState::Unknown => 0,
                        SecurityState::Yes => 1,
                        SecurityState::No => 2,
                    }
                ])
            })
            .collect();
        json!({"name":self.name.trim(), "address":self.address.trim(), "description":self.description,
            "type":self.parking_type.as_code(), "cost":cost,
            "hours":{"unknown":self.hours.is_unknown(), "rows":rows}, "security":security})
    }

    /// Reject incomplete or invalid payloads instead of approving a partial edit.
    pub fn from_json(raw: &Value) -> Option<Self> {
        let name = raw.get("name")?.as_str()?.trim();
        let address = raw.get("address")?.as_str()?.trim();
        if name.is_empty()
            || name.chars().count() > 200
            || address.is_empty()
            || address.chars().count() > 500
        {
            return None;
        }
        let description = match raw.get("description")? {
            Value::Null => None,
            Value::String(s) if s.chars().count() <= 5000 => Some(s.clone()),
            _ => return None,
        };
        let c = raw.get("cost")?;
        let price = match c.get("cents") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let cents = v.as_i64()?;
                if cents < 0 {
                    return None;
                }
                Some(Money::new(
                    cents,
                    CurrencyCode::parse(c.get("currency")?.as_str()?).ok()?,
                    PricingUnit::from_code(c.get("unit")?.as_str()?).ok()?,
                ))
            }
        };
        let cost = Cost::from_kind_and_price(c.get("kind")?.as_str()?, price).ok()?;
        let h = raw.get("hours")?;
        let unknown = h.get("unknown")?.as_bool()?;
        let mut hours_rows = Vec::new();
        for row in h.get("rows")?.as_array()? {
            let day = u8::try_from(row.get(0)?.as_u64()?).ok()?;
            if !(1..=7).contains(&day) {
                return None;
            }
            hours_rows.push((
                day,
                TimeRange {
                    opens_at: row.get(1)?.as_str()?.parse().ok()?,
                    closes_at: row.get(2)?.as_str()?.parse().ok()?,
                    all_day: row.get(3)?.as_bool()?,
                },
            ));
        }
        if unknown && !hours_rows.is_empty() {
            return None;
        }
        let mut security = Vec::new();
        for row in raw.get("security")?.as_array()? {
            let code = row.get(0)?.as_str()?;
            if !crate::is_known_security_code(code)
                || security.iter().any(|f: &SecurityFeature| f.code() == code)
            {
                return None;
            }
            let state =
                SecurityState::from_smallint(i16::try_from(row.get(1)?.as_i64()?).ok()?).ok()?;
            security.push(SecurityFeature::new(code, state));
        }
        Some(Self {
            name: name.into(),
            address: address.into(),
            description,
            parking_type: ParkingType::from_code(raw.get("type")?.as_str()?).ok()?,
            cost,
            hours: if unknown {
                OpeningHours::Unknown
            } else {
                OpeningHours::weekly(hours_rows)
            },
            security,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> Value {
        ParkingEdit {
            name: "Rack".into(),
            address: "Street 1".into(),
            description: None,
            parking_type: ParkingType::Rack,
            cost: Cost::Free,
            hours: OpeningHours::Unknown,
            security: vec![],
        }
        .to_json()
    }

    #[test]
    fn edit_payload_round_trips_all_public_fields() {
        let raw = payload();
        assert_eq!(ParkingEdit::from_json(&raw).unwrap().to_json(), raw);
    }

    #[test]
    fn incomplete_and_invalid_edits_cannot_be_approved() {
        for (key, value) in [
            ("name", json!("")),
            ("type", json!("invalid")),
            (
                "cost",
                json!({"kind":"paid","cents":-1,"currency":"BRL","unit":"hour"}),
            ),
            (
                "hours",
                json!({"unknown":false,"rows":[[8,"09:00:00","17:00:00",false]]}),
            ),
            (
                "hours",
                json!({"unknown":true,"rows":[[1,"09:00:00","17:00:00",false]]}),
            ),
            ("security", json!([["cctv", 1], ["cctv", 2]])),
            ("security", json!([["unknown_feature", 1]])),
        ] {
            let mut raw = payload();
            raw[key] = value;
            assert!(
                ParkingEdit::from_json(&raw).is_none(),
                "accepted invalid {key}"
            );
        }
        let mut raw = payload();
        raw.as_object_mut().unwrap().remove("hours");
        assert!(ParkingEdit::from_json(&raw).is_none());
    }
}
