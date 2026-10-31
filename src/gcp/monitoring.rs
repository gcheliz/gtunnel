use anyhow::Result;
use serde::Deserialize;
use std::collections::BTreeMap;

use super::GcpClient;

pub const REQUEST_COUNT_METRIC: &str = "loadbalancing.googleapis.com/https/request_count";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LbStats {
    pub total: u64,
    pub by_class: BTreeMap<String, u64>,
}

#[derive(Deserialize)]
struct TimeSeriesResponse {
    #[serde(default, rename = "timeSeries")]
    time_series: Vec<TimeSeries>,
}

#[derive(Deserialize)]
struct TimeSeries {
    #[serde(default)]
    metric: Metric,
    #[serde(default)]
    points: Vec<Point>,
}

#[derive(Deserialize, Default)]
struct Metric {
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Point {
    #[serde(default)]
    value: PointValue,
}

#[derive(Deserialize, Default)]
struct PointValue {
    #[serde(default, rename = "int64Value")]
    int64_value: Option<String>,
    #[serde(default, rename = "doubleValue")]
    double_value: Option<f64>,
}

impl PointValue {
    fn as_u64(&self) -> u64 {
        if let Some(raw) = &self.int64_value {
            return raw.parse().unwrap_or(0);
        }
        self.double_value.map(|v| v.round() as u64).unwrap_or(0)
    }
}

fn rfc3339(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    // Days since the epoch, converted with the civil-from-days algorithm so no
    // date crate is needed for one timestamp
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

// Request counts for one backend service, grouped by response class, as the
// TUI and `gtunnel list` show them
pub async fn lb_request_stats(
    client: &GcpClient,
    project: &str,
    backend_service: &str,
    minutes: u32,
) -> Result<LbStats> {
    let now = std::time::SystemTime::now();
    let start = now - std::time::Duration::from_secs(u64::from(minutes) * 60);
    let filter = format!(
        "metric.type = \"{REQUEST_COUNT_METRIC}\" AND resource.labels.backend_target_name = \"{backend_service}\""
    );
    let url = format!(
        "{}/projects/{project}/timeSeries",
        client.endpoints.monitoring
    );
    let query = [
        ("filter", filter),
        ("interval.startTime", rfc3339(start)),
        ("interval.endTime", rfc3339(now)),
        (
            "aggregation.alignmentPeriod",
            format!("{}s", u64::from(minutes) * 60),
        ),
        ("aggregation.perSeriesAligner", "ALIGN_SUM".to_string()),
        ("aggregation.crossSeriesReducer", "REDUCE_SUM".to_string()),
        (
            "aggregation.groupByFields",
            "metric.labels.response_code_class".to_string(),
        ),
    ];
    let encoded: Vec<String> = query
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect();
    let resp: TimeSeriesResponse = client.get(&format!("{url}?{}", encoded.join("&"))).await?;

    let mut stats = LbStats::default();
    for series in resp.time_series {
        let class = series
            .metric
            .labels
            .get("response_code_class")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let sum: u64 = series.points.iter().map(|p| p.value.as_u64()).sum();
        *stats.by_class.entry(class).or_insert(0) += sum;
        stats.total += sum;
    }
    Ok(stats)
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcp::GcpClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn lb_request_stats_sums_each_response_class() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/p/timeSeries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "timeSeries": [
                    {
                        "metric": {"labels": {"response_code_class": "200"}},
                        "points": [
                            {"value": {"int64Value": "7"}},
                            {"value": {"doubleValue": 3.0}}
                        ]
                    },
                    {
                        "metric": {"labels": {"response_code_class": "500"}},
                        "points": [{"value": {"int64Value": "2"}}]
                    }
                ]
            })))
            .mount(&server)
            .await;

        let stats = lb_request_stats(&GcpClient::for_mock(&server), "p", "gt-dev-api", 60)
            .await
            .expect("stats");
        assert_eq!(stats.total, 12);
        assert_eq!(stats.by_class.get("200"), Some(&10));
        assert_eq!(stats.by_class.get("500"), Some(&2));
    }

    #[tokio::test]
    async fn lb_request_stats_is_empty_when_there_are_no_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let stats = lb_request_stats(&GcpClient::for_mock(&server), "p", "gt-dev-api", 60)
            .await
            .expect("stats");
        assert_eq!(stats.total, 0);
        assert!(stats.by_class.is_empty());
    }
}
