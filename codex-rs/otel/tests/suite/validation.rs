use crate::harness::attributes_to_map;
use crate::harness::find_metric;
use crate::harness::latest_metrics;
use codex_otel::MetricsClient;
use codex_otel::MetricsConfig;
use codex_otel::MetricsError;
use codex_otel::Result;
use codex_otel::SessionMetricTagValues;
use opentelemetry_sdk::metrics::InMemoryMetricExporter;
use opentelemetry_sdk::metrics::data::AggregatedMetrics;
use opentelemetry_sdk::metrics::data::MetricData;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

fn build_in_memory_client() -> Result<MetricsClient> {
    let exporter = InMemoryMetricExporter::default();
    let config = MetricsConfig::in_memory("test", "codex-cli", env!("CARGO_PKG_VERSION"), exporter);
    MetricsClient::new(config)
}

// Ensures invalid tag components are rejected during config build.
#[test]
fn invalid_tag_component_is_rejected() -> Result<()> {
    let err = MetricsConfig::in_memory(
        "test",
        "codex-cli",
        env!("CARGO_PKG_VERSION"),
        InMemoryMetricExporter::default(),
    )
    .with_tag("bad key", "value")
    .unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidTagComponent { label, value }
            if label == "tag key" && value == "bad key"
    ));
    Ok(())
}

// Ensures per-metric tag keys are validated.
#[test]
fn counter_rejects_invalid_tag_key() -> Result<()> {
    let metrics = build_in_memory_client()?;
    let err = metrics
        .counter("codex.turns", /*inc*/ 1, &[("bad key", "value")])
        .unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidTagComponent { label, value }
            if label == "tag key" && value == "bad key"
    ));
    metrics.shutdown()?;
    Ok(())
}

// Ensures per-metric tag values are validated.
#[test]
fn histogram_rejects_invalid_tag_value() -> Result<()> {
    let metrics = build_in_memory_client()?;
    let err = metrics
        .histogram(
            "codex.request_latency",
            /*value*/ 3,
            &[("route", "bad value")],
        )
        .unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidTagComponent { label, value }
            if label == "tag value" && value == "bad value"
    ));
    metrics.shutdown()?;
    Ok(())
}

// Ensures invalid metric names are rejected.
#[test]
fn counter_rejects_invalid_metric_name() -> Result<()> {
    let metrics = build_in_memory_client()?;
    let err = metrics.counter("bad name", /*inc*/ 1, &[]).unwrap_err();
    assert!(matches!(
        err,
        MetricsError::InvalidMetricName { name } if name == "bad name"
    ));
    metrics.shutdown()?;
    Ok(())
}

#[test]
fn counter_rejects_negative_increment() -> Result<()> {
    let metrics = build_in_memory_client()?;
    let err = metrics.counter("codex.turns", /*inc*/ -1, &[]).unwrap_err();
    assert!(matches!(
        err,
        MetricsError::NegativeCounterIncrement { name, inc } if name == "codex.turns" && inc == -1
    ));
    metrics.shutdown()?;
    Ok(())
}

#[test]
fn semver_build_metadata_survives_tags_without_relaxing_keys_or_names() -> Result<()> {
    let version = "0.154.0+aaiyer.1";
    let exporter = InMemoryMetricExporter::default();
    let config = MetricsConfig::in_memory("test", "codex-cli", version, exporter.clone())
        .with_tag("app.version", version)?;
    let metrics = MetricsClient::new(config)?;
    let tags = SessionMetricTagValues {
        auth_mode: None,
        session_source: "cli",
        originator: "codex_cli_rs",
        service_name: None,
        model: "gpt-5.1",
        app_version: version,
    }
    .into_tags()?;
    metrics.counter("codex.turns", /*inc*/ 1, &tags)?;

    assert!(matches!(
        metrics.counter("codex+turns", /*inc*/ 1, &[]),
        Err(MetricsError::InvalidMetricName { name }) if name == "codex+turns"
    ));
    assert!(matches!(
        metrics.counter("codex.turns", /*inc*/ 1, &[("app+version", version)]),
        Err(MetricsError::InvalidTagComponent { label, value })
            if label == "tag key" && value == "app+version"
    ));
    metrics.shutdown()?;

    let resource_metrics = latest_metrics(&exporter);
    let metric = find_metric(&resource_metrics, "codex.turns").expect("counter metric");
    let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data() else {
        panic!("expected counter aggregation");
    };
    let points: Vec<_> = sum.data_points().collect();
    assert_eq!(points.len(), 1);
    assert_eq!(points[0].value(), 1);
    assert_eq!(
        attributes_to_map(points[0].attributes()),
        BTreeMap::from([
            ("app.version".to_string(), version.to_string()),
            ("model".to_string(), "gpt-5.1".to_string()),
            ("originator".to_string(), "codex_cli_rs".to_string()),
            ("session_source".to_string(), "cli".to_string()),
        ])
    );
    assert!(find_metric(&resource_metrics, "codex+turns").is_none());
    Ok(())
}
