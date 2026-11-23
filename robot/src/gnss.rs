//! Read the GNSS owner's snapshot only during an authorized status read.
use robot_cloud_common::{now_ms, status::Location, timestamp};
use serde::Deserialize;
use std::{env, path::Path};

#[derive(Deserialize)]
struct Snapshot {
    location: Option<Fix>,
}

#[derive(Deserialize)]
struct Fix {
    lat: f64,
    lon: f64,
    fix_type: u8,
    ts: f64,
    accuracy_m: Option<f64>,
}

fn decode(bytes: &[u8], now: i64) -> Option<Location> {
    let fix = serde_json::from_slice::<Snapshot>(bytes).ok()?.location?;
    let milliseconds = fix.ts * 1000.0;
    if !milliseconds.is_finite()
        || milliseconds < (now - 3_000) as f64
        || milliseconds > (now + 1_000) as f64
    {
        return None;
    }
    let location = Location {
        lat: fix.lat,
        lon: fix.lon,
        fix_type: fix.fix_type,
        observed_at: timestamp(milliseconds.round() as i64),
        accuracy_m: fix.accuracy_m,
    };
    location.validate().ok()?;
    Some(location)
}

async fn read_at(path: &Path) -> Option<Location> {
    let bytes = tokio::fs::read(path).await.ok()?;
    decode(&bytes, now_ms())
}

pub async fn read() -> Option<Location> {
    let path = env::var_os("DOSO_GNSS_STATUS")
        .unwrap_or_else(|| "/dev/shm/lingtu/gnss_status.json".into());
    read_at(Path::new(&path)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_793_700_000_000;

    fn snapshot() -> serde_json::Value {
        json!({"connected":true,"hdop":0.8,"location":{
            "lat":31.2304,"lon":121.4737,"fix_type":1,
            "ts":NOW as f64 / 1000.0,"accuracy_m":null
        }})
    }

    #[test]
    fn original_time_and_unknown_accuracy_are_preserved() {
        let mut value = snapshot();
        value["location"]["ts"] = json!((NOW - 1234) as f64 / 1000.0);
        let location = decode(&serde_json::to_vec(&value).unwrap(), NOW).unwrap();
        assert_eq!(location.observed_at, timestamp(NOW - 1234));
        assert_eq!(location.accuracy_m, None);
        for offset in [-3000, 1000] {
            value["location"]["ts"] = json!((NOW + offset) as f64 / 1000.0);
            assert!(decode(&serde_json::to_vec(&value).unwrap(), NOW).is_some());
        }
    }

    #[test]
    fn bad_unfixed_and_expired_snapshots_are_omitted() {
        for (field, invalid) in [
            ("lat", json!(90.1)),
            ("lon", json!(-180.1)),
            ("fix_type", json!(0)),
            ("fix_type", json!(9)),
            ("accuracy_m", json!(-1)),
            ("ts", json!((NOW - 3001) as f64 / 1000.0)),
            ("ts", json!((NOW - 4000) as f64 / 1000.0)),
            ("ts", json!((NOW + 1001) as f64 / 1000.0)),
            ("ts", json!((NOW + 1500) as f64 / 1000.0)),
        ] {
            let mut value = snapshot();
            value["location"][field] = invalid;
            assert!(
                decode(&serde_json::to_vec(&value).unwrap(), NOW).is_none(),
                "{field}"
            );
        }
        for bytes in [b"invalid".as_slice(), b"{}", b"{\"location\":null}"] {
            assert!(decode(bytes, NOW).is_none());
        }
    }

    #[tokio::test]
    async fn missing_file_is_unavailable() {
        let path = env::temp_dir().join(format!("missing-{}.json", uuid::Uuid::new_v4()));
        assert!(read_at(&path).await.is_none());
    }
}
