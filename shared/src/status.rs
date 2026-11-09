use crate::Result;
use anyhow::ensure;
use serde::{Deserialize, Serialize};

// Order follows the numeric enums in brainstem_api/cms.proto.
pub const KINDS: &[&str] = &[
    "CMS_STATE_KIND_ZERO",
    "CMS_STATE_KIND_GROUNDED",
    "CMS_STATE_KIND_STANDING",
    "CMS_STATE_KIND_WALKING",
    "CMS_STATE_KIND_TRANSITIONING",
];
pub const TRANSITIONS: &[&str] = &[
    "CMS_TRANSITION_KIND_NONE",
    "CMS_TRANSITION_KIND_STAND_UP",
    "CMS_TRANSITION_KIND_SIT_DOWN",
    "CMS_TRANSITION_KIND_GESTURE",
];
pub const OWNERS: &[&str] = &[
    "CONTROL_OWNER_NONE",
    "CONTROL_OWNER_GRPC",
    "CONTROL_OWNER_YUNZHUO",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct State {
    pub kind: String,
    pub transition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Control {
    pub owner: String,
    pub motor_output_enabled: bool,
    pub critical_motor_fault: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
    pub fix_type: u8,
    pub observed_at: String,
    pub accuracy_m: Option<f64>,
}

impl Location {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.lat.is_finite()
                && (-90.0..=90.0).contains(&self.lat)
                && self.lon.is_finite()
                && (-180.0..=180.0).contains(&self.lon),
            "invalid_coordinates"
        );
        ensure!((1..=8).contains(&self.fix_type), "invalid_fix_type");
        ensure!(
            self.accuracy_m
                .is_none_or(|value| value.is_finite() && (0.0..=100_000.0).contains(&value)),
            "invalid_accuracy"
        );
        crate::parse_time(&self.observed_at)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Status {
    pub state: State,
    pub control: Control,
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
}

impl Status {
    pub fn validate(&self) -> Result<()> {
        ensure!(KINDS.contains(&self.state.kind.as_str()), "invalid_state");
        ensure!(
            TRANSITIONS.contains(&self.state.transition.as_str()),
            "invalid_transition"
        );
        ensure!(
            OWNERS.contains(&self.control.owner.as_str()),
            "invalid_owner"
        );
        ensure!(
            ["local-insecure", "mtls"].contains(&self.transport.as_str()),
            "invalid_transport"
        );
        if let Some(location) = &self.location {
            location.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_status_and_location_round_trip() {
        let legacy = json!({"state":{"kind":KINDS[1],"transition":TRANSITIONS[0]},
            "control":{"owner":OWNERS[0],"motorOutputEnabled":false,"criticalMotorFault":false},
            "transport":"local-insecure"});
        let mut status: Status = serde_json::from_value(legacy.clone()).unwrap();
        status.validate().unwrap();
        assert_eq!(serde_json::to_value(&status).unwrap(), legacy);
        let location = Location {
            lat: 31.2304,
            lon: 121.4737,
            fix_type: 1,
            observed_at: crate::timestamp(1_793_700_000_000),
            accuracy_m: None,
        };
        status.location = Some(location.clone());
        status.validate().unwrap();
        let value = serde_json::to_value(&status).unwrap();
        assert!(value["location"]["accuracyM"].is_null());
        let restored: Status = serde_json::from_value(value).unwrap();
        assert_eq!(restored.location, Some(location));
    }

    #[test]
    fn coordinates_fix_time_and_accuracy_are_validated() {
        let valid = Location {
            lat: 90.0,
            lon: -180.0,
            fix_type: 8,
            observed_at: crate::timestamp(1_793_700_000_000),
            accuracy_m: Some(0.0),
        };
        valid.validate().unwrap();
        for location in [
            Location {
                lat: f64::NAN,
                ..valid.clone()
            },
            Location {
                lon: f64::INFINITY,
                ..valid.clone()
            },
            Location {
                lat: 90.1,
                ..valid.clone()
            },
            Location {
                lon: -180.1,
                ..valid.clone()
            },
            Location {
                fix_type: 0,
                ..valid.clone()
            },
            Location {
                fix_type: 9,
                ..valid.clone()
            },
            Location {
                accuracy_m: Some(f64::INFINITY),
                ..valid.clone()
            },
            Location {
                accuracy_m: Some(-0.1),
                ..valid.clone()
            },
            Location {
                accuracy_m: Some(100_000.1),
                ..valid.clone()
            },
            Location {
                observed_at: "bad".into(),
                ..valid.clone()
            },
        ] {
            assert!(location.validate().is_err());
        }
    }
}
