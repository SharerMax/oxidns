// SPDX-FileCopyrightText: 2025 Sven Shi
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared plugin metrics registry and Prometheus renderer.
//!
//! This module is intentionally small and strict because it backs request-path
//! observability for plugins:
//! - hot paths may only update pre-owned counters with `AtomicU64::{fetch_add,
//!   load}(Ordering::Relaxed)`;
//! - hot paths must not lock, allocate strings, look up metric names in maps,
//!   or dynamically register metrics;
//! - labels must be startup-fixed, low-cardinality dimensions such as
//!   `plugin_tag`, `name`, `kind`, `reason`, or `result`;
//! - high-cardinality values such as qname, client IP, or upstream address must
//!   stay out of this generic layer and belong in heavier recorders;
//! - derived values such as hit ratio are computed at scrape time or by
//!   Prometheus queries, never on the hot path;
//! - v1 uses plain `AtomicU64` counters. Sharded counters should only be added
//!   later if profiling shows contention here.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};

use crate::infra::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Counter,
    Gauge,
}

impl MetricKind {
    #[inline]
    fn as_prometheus_type(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetricLabel<'a> {
    pub key: &'static str,
    pub value: &'a str,
}

impl<'a> MetricLabel<'a> {
    #[inline]
    pub const fn new(key: &'static str, value: &'a str) -> Self {
        Self { key, value }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MetricSample<'a> {
    pub name: &'static str,
    pub help: &'static str,
    pub kind: MetricKind,
    pub labels: &'a [MetricLabel<'a>],
    pub value: u64,
}

impl<'a> MetricSample<'a> {
    #[inline]
    pub const fn new(
        name: &'static str,
        help: &'static str,
        kind: MetricKind,
        labels: &'a [MetricLabel<'a>],
        value: u64,
    ) -> Self {
        Self {
            name,
            help,
            kind,
            labels,
            value,
        }
    }

    #[inline]
    pub const fn counter(
        name: &'static str,
        help: &'static str,
        labels: &'a [MetricLabel<'a>],
        value: u64,
    ) -> Self {
        Self::new(name, help, MetricKind::Counter, labels, value)
    }

    #[inline]
    pub const fn gauge(
        name: &'static str,
        help: &'static str,
        labels: &'a [MetricLabel<'a>],
        value: u64,
    ) -> Self {
        Self::new(name, help, MetricKind::Gauge, labels, value)
    }
}

pub trait MetricSink {
    fn emit(&mut self, sample: MetricSample<'_>);
}

pub trait MetricSource: Send + Sync {
    fn tag(&self) -> &str;
    fn plugin_type(&self) -> &'static str;
    fn collect(&self, sink: &mut dyn MetricSink);
}

#[derive(Default)]
struct MetricsRegistry {
    sources: Mutex<Vec<Arc<dyn MetricSource>>>,
}

fn metrics_registry() -> &'static MetricsRegistry {
    static REGISTRY: OnceLock<MetricsRegistry> = OnceLock::new();
    REGISTRY.get_or_init(MetricsRegistry::default)
}

// --- Staged-runtime metric registration -----------------------------------
//
// A staged reload builds the candidate runtime while the old one still
// serves. Candidate plugins register metric sources with the same tags as
// the old generation; registering directly would shadow the live sources,
// and the old generation's destroy-time `unregister_metric_source` would
// then remove the candidate's entries. Staging parks candidate
// registrations in a side list that `flush_metric_staging` publishes only
// after the old generation has been destroyed.

static METRIC_STAGING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pending_sources() -> &'static Mutex<Vec<Arc<dyn MetricSource>>> {
    static PENDING: OnceLock<Mutex<Vec<Arc<dyn MetricSource>>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// Park subsequent metric registrations in the staging list instead of the
/// live registry.
pub fn begin_metric_staging() {
    METRIC_STAGING.store(true, std::sync::atomic::Ordering::Release);
}

/// Publish staged registrations into the live registry and resume direct
/// registration. Entries are pushed without tag dedup: the previous
/// generation's same-tag sources stay until its destroy-time unregister
/// removes the oldest match, and the renderer dedups by tag in the
/// interim.
pub fn flush_metric_staging() {
    METRIC_STAGING.store(false, std::sync::atomic::Ordering::Release);
    let staged = std::mem::take(&mut *pending_sources().lock().expect("pending metrics poisoned"));
    if staged.is_empty() {
        return;
    }
    let mut sources = metrics_registry()
        .sources
        .lock()
        .expect("metrics sources poisoned");
    sources.extend(staged);
}

/// Discard staged registrations and resume direct registration. Called when
/// the staged candidate failed and is being destroyed.
pub fn abort_metric_staging() {
    METRIC_STAGING.store(false, std::sync::atomic::Ordering::Release);
    pending_sources()
        .lock()
        .expect("pending metrics poisoned")
        .clear();
}

pub fn register_metric_source(source: Arc<dyn MetricSource>) -> Result<()> {
    if METRIC_STAGING.load(std::sync::atomic::Ordering::Acquire) {
        pending_sources()
            .lock()
            .expect("pending metrics poisoned")
            .push(source);
        return Ok(());
    }
    let mut sources = metrics_registry()
        .sources
        .lock()
        .expect("metrics sources poisoned");
    sources.retain(|existing| existing.tag() != source.tag());
    sources.push(source);
    Ok(())
}

pub fn unregister_metric_source(tag: &str) {
    // While staging, only the candidate's pending entries can be torn down
    // (a failed candidate destroying itself); the live generation keeps its
    // sources. After flush, unregister removes only the OLDEST same-tag
    // entry so the previous generation's destroy does not strip the
    // candidate's freshly flushed sources.
    if METRIC_STAGING.load(std::sync::atomic::Ordering::Acquire) {
        pending_sources()
            .lock()
            .expect("pending metrics poisoned")
            .retain(|source| source.tag() != tag);
        return;
    }
    let mut sources = metrics_registry()
        .sources
        .lock()
        .expect("metrics sources poisoned");
    if let Some(position) = sources.iter().position(|source| source.tag() == tag) {
        sources.swap_remove(position);
    }
}

pub fn render_prometheus_metrics() -> String {
    let sources = {
        let sources = metrics_registry()
            .sources
            .lock()
            .expect("metrics sources poisoned");
        sources.clone()
    };

    // During a staged swap the registry briefly holds both generations'
    // sources with identical tags; keep the newest registration per tag so
    // scrapes stay valid while the old generation drains.
    let mut latest: Vec<Arc<dyn MetricSource>> = Vec::with_capacity(sources.len());
    for source in sources {
        match latest
            .iter()
            .position(|existing| existing.tag() == source.tag())
        {
            Some(position) => latest[position] = source,
            None => latest.push(source),
        }
    }

    let mut sink = PrometheusRenderSink::default();
    for source in latest {
        source.collect(&mut sink);
    }
    sink.finish()
}

#[derive(Default)]
struct PrometheusRenderSink {
    out: String,
    seen_defs: HashSet<&'static str>,
}

impl PrometheusRenderSink {
    fn finish(self) -> String {
        self.out
    }
}

impl MetricSink for PrometheusRenderSink {
    fn emit(&mut self, sample: MetricSample<'_>) {
        if self.seen_defs.insert(sample.name) {
            let _ = writeln!(self.out, "# HELP {} {}", sample.name, sample.help);
            let _ = writeln!(
                self.out,
                "# TYPE {} {}",
                sample.name,
                sample.kind.as_prometheus_type()
            );
        }

        self.out.push_str(sample.name);
        if !sample.labels.is_empty() {
            self.out.push('{');
            for (idx, label) in sample.labels.iter().enumerate() {
                if idx > 0 {
                    self.out.push(',');
                }
                self.out.push_str(label.key);
                self.out.push_str("=\"");
                escape_label_value_into(label.value, &mut self.out);
                self.out.push('"');
            }
            self.out.push('}');
        }
        let _ = writeln!(self.out, " {}", sample.value);
    }
}

fn escape_label_value_into(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
pub(crate) fn reset_metrics_for_tests() {
    metrics_registry()
        .sources
        .lock()
        .expect("metrics sources poisoned")
        .clear();
}

#[cfg(test)]
pub(crate) fn metrics_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD.lock().expect("metrics test guard poisoned")
}

#[cfg(test)]
mod tests {
    use super::{
        MetricLabel, MetricSample, MetricSink, MetricSource, abort_metric_staging,
        begin_metric_staging, flush_metric_staging, register_metric_source,
        render_prometheus_metrics, unregister_metric_source,
    };

    struct CountingSource {
        tag: &'static str,
        value: AtomicU64,
    }

    impl CountingSource {
        fn new(tag: &'static str) -> Arc<Self> {
            Arc::new(Self {
                tag,
                value: AtomicU64::new(0),
            })
        }
    }

    impl MetricSource for CountingSource {
        fn tag(&self) -> &str {
            self.tag
        }

        fn plugin_type(&self) -> &'static str {
            "test"
        }

        fn collect(&self, sink: &mut dyn MetricSink) {
            sink.emit(MetricSample::counter(
                "test_metric_total",
                "help",
                &[MetricLabel::new("plugin_tag", self.tag)],
                self.value.load(std::sync::atomic::Ordering::Relaxed),
            ));
        }
    }

    #[test]
    fn staged_registration_survives_old_generation_unregister() {
        let old = CountingSource::new("gen-tag");
        let candidate = CountingSource::new("gen-tag");
        register_metric_source(old.clone()).expect("register old");
        register_metric_source(candidate.clone()).expect("shadowing register dedups");

        // Staged reload: candidate parks in pending while old serves.
        begin_metric_staging();
        let candidate2 = CountingSource::new("gen-tag");
        register_metric_source(candidate2.clone()).expect("staged register");

        // Flush publishes without dedup: old + candidate2 coexist.
        flush_metric_staging();

        // Old generation destroy unregisters the oldest same-tag entry only.
        unregister_metric_source("gen-tag");

        let rendered = render_prometheus_metrics();
        let gen_series = rendered
            .lines()
            .filter(|line| line.contains("plugin_tag=\"gen-tag\"") && !line.starts_with('#'))
            .count();
        assert_eq!(
            gen_series, 1,
            "renderer emits exactly one gen-tag series after old-generation unregister"
        );
    }

    #[test]
    fn aborted_staging_leaves_live_registry_untouched() {
        let live = CountingSource::new("live-tag");
        register_metric_source(live.clone()).expect("register live");

        begin_metric_staging();
        let staged = CountingSource::new("staged-tag");
        register_metric_source(staged).expect("staged register");
        // A failed candidate destroys itself; its unregister must not touch
        // live sources.
        unregister_metric_source("staged-tag");
        abort_metric_staging();

        let rendered = render_prometheus_metrics();
        assert!(rendered.contains("test_metric_total"));
    }

    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[derive(Debug)]
    struct TestSource {
        tag: String,
        value: AtomicU64,
    }

    impl TestSource {
        fn new(tag: &str, value: u64) -> Self {
            Self {
                tag: tag.to_string(),
                value: AtomicU64::new(value),
            }
        }
    }

    impl MetricSource for TestSource {
        fn tag(&self) -> &str {
            &self.tag
        }

        fn plugin_type(&self) -> &'static str {
            "test"
        }

        fn collect(&self, sink: &mut dyn MetricSink) {
            let labels = [MetricLabel::new("plugin_tag", self.tag.as_str())];
            sink.emit(MetricSample::counter(
                "test_total",
                "Total test events.",
                &labels,
                self.value.load(Ordering::Relaxed),
            ));
            sink.emit(MetricSample::gauge(
                "test_inflight",
                "Current test gauge.",
                &labels,
                7,
            ));
        }
    }

    #[derive(Debug)]
    struct EscapedSource;

    impl MetricSource for EscapedSource {
        fn tag(&self) -> &str {
            "escaped"
        }

        fn plugin_type(&self) -> &'static str {
            "test"
        }

        fn collect(&self, sink: &mut dyn MetricSink) {
            let labels = [MetricLabel::new("plugin_tag", "a\"b\\c\nd")];
            sink.emit(MetricSample::counter(
                "escape_total",
                "Escaped labels.",
                &labels,
                1,
            ));
        }
    }

    #[test]
    fn render_prometheus_escapes_labels_and_deduplicates_defs() {
        let _guard = metrics_test_guard();
        reset_metrics_for_tests();
        register_metric_source(Arc::new(TestSource::new("one", 3))).unwrap();
        register_metric_source(Arc::new(TestSource::new("two", 5))).unwrap();
        register_metric_source(Arc::new(EscapedSource)).unwrap();

        let output = render_prometheus_metrics();
        assert_eq!(output.matches("# HELP test_total").count(), 1);
        assert_eq!(output.matches("# TYPE test_total counter").count(), 1);
        assert!(output.contains("test_total{plugin_tag=\"one\"} 3"));
        assert!(output.contains("test_total{plugin_tag=\"two\"} 5"));
        assert!(output.contains("test_inflight{plugin_tag=\"one\"} 7"));
        assert!(output.contains("escape_total{plugin_tag=\"a\\\"b\\\\c\\nd\"} 1"));
    }

    #[test]
    fn unregister_metric_source_removes_matching_tag() {
        let _guard = metrics_test_guard();
        reset_metrics_for_tests();
        register_metric_source(Arc::new(TestSource::new("keep", 1))).unwrap();
        register_metric_source(Arc::new(TestSource::new("drop", 2))).unwrap();

        unregister_metric_source("drop");

        let output = render_prometheus_metrics();
        assert!(output.contains("test_total{plugin_tag=\"keep\"} 1"));
        assert!(!output.contains("plugin_tag=\"drop\""));
    }
}
