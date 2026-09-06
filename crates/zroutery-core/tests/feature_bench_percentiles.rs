#![cfg(feature = "ml")]

use std::time::Instant;
use zroutery_core::config::{ModelCapabilities, ModelTier};
use zroutery_core::ml::features::{extract_features, FeatureContext};
use zroutery_core::policy::{Complexity, TaskProfile, TaskType};
use zroutery_core::stats_ext::ProviderModelStats;

#[test]
fn feature_extraction_percentiles_10k() {
    let task = TaskProfile {
        streaming: true,
        has_tools: true,
        has_vision: false,
        context_tokens: 50_000,
        estimated_output_tokens: 4096,
        complexity: Complexity::Standard,
        task_type: TaskType::ToolUse,
        ..Default::default()
    };
    let caps = ModelCapabilities {
        tools: true,
        thinking: true,
        ..Default::default()
    };
    let mut stats = ProviderModelStats::new("m".into(), "p".into());
    stats.record_success(200.0, Some(50.0));

    let ctx = FeatureContext {
        task: Some(&task),
        message_count: Some(10),
        tier: Some(ModelTier::Standard),
        capabilities: Some(&caps),
        priority: 50,
        observation: None,
        stats: Some(&stats),
        #[cfg(feature = "account")]
        account: None,
    };

    const N: usize = 10_000;
    let mut durations_ns = Vec::with_capacity(N);

    for _ in 0..N {
        let start = Instant::now();
        std::hint::black_box(extract_features(&ctx));
        durations_ns.push(start.elapsed().as_nanos() as u64);
    }

    durations_ns.sort_unstable();

    let p50 = durations_ns[N * 50 / 100];
    let p95 = durations_ns[N * 95 / 100];
    let p99 = durations_ns[N * 99 / 100];
    let min = durations_ns[0];
    let max = durations_ns[N - 1];
    let total_us: u64 = durations_ns.iter().sum::<u64>() / 1_000;
    let mean_ns = durations_ns.iter().sum::<u64>() / N as u64;

    println!("=== Feature Extraction Latency (10,000 calls) ===");
    println!("  Min:    {:>8} ns", min);
    println!("  P50:    {:>8} ns  ({:.3} us)", p50, p50 as f64 / 1000.0);
    println!("  P95:    {:>8} ns  ({:.3} us)", p95, p95 as f64 / 1000.0);
    println!("  P99:    {:>8} ns  ({:.3} us)", p99, p99 as f64 / 1000.0);
    println!("  Max:    {:>8} ns", max);
    println!("  Mean:   {:>8} ns", mean_ns);
    println!("  Total:  {:>8} us", total_us);

    // Gate checks
    let p95_us = p95 as f64 / 1000.0;
    let p99_us = p99 as f64 / 1000.0;
    assert!(
        p95_us <= 1000.0,
        "GATE FAIL: P95 = {:.3} us > 1000 us (1 ms)",
        p95_us
    );
    assert!(
        p99_us <= 3000.0,
        "GATE FAIL: P99 = {:.3} us > 3000 us (3 ms)",
        p99_us
    );
}
