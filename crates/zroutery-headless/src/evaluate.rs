//! Local offline evaluation over collected traces.
//!
//! # What this command does and does not do
//!
//! It reads `traces.jsonl` from the router's state directory, replays the recorded
//! decisions through the fixed baselines and the current model, and writes a JSON and a
//! Markdown report. It sends **no** upstream request, spends no quota, and changes no
//! routing. The production policy is not touched by anything here.
//!
//! That is a property of the code path rather than a promise: the runner's signature
//! takes a body of already-recorded traces and returns a report. There is no client and
//! no endpoint anywhere below it, so it is safe to run on a schedule.
//!
//! # The report will usually say BLOCKED
//!
//! That is the point. A fresh install has no traces; a modest install has traces but no
//! task-quality evidence; a busy install has task evidence for a few percent of
//! requests. Each of those produces `BLOCKED` with the specific missing input named,
//! because "we cannot tell" and "it does not help" are different findings and only the
//! second is about the candidate.
//!
//! ```sh
//! zroutery-headless --evaluate-routing
//! zroutery-headless --evaluate-routing --state-dir <path> --out <dir>
//! ```

use std::path::{Path, PathBuf};

use zroutery_core::ml::dataset::OutcomeTrainingSample;
use zroutery_core::ml::eval_report::{evaluability, evaluate, DataQualityReport, EvaluationConfig};
use zroutery_core::ml::learning::{run_training, TrainingConfig};
use zroutery_core::ml::traces::TraceLog;
use zroutery_core::ml::{MlPolicy, ModelEnsemble, TraceError, TRACE_SCHEMA_MAX_READABLE};

/// Parsed arguments for `--evaluate-routing`.
pub struct EvalArgs {
    pub state_dir: PathBuf,
    pub out_dir: Option<PathBuf>,
    pub min_traces: Option<usize>,
    pub min_coverage: Option<f64>,
}

impl EvalArgs {
    /// Parse the flags following `--evaluate-routing`.
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut parsed = Self {
            state_dir: default_state_dir(),
            out_dir: None,
            min_traces: None,
            min_coverage: None,
        };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--state-dir" => {
                    parsed.state_dir =
                        PathBuf::from(iter.next().ok_or("--state-dir needs a path")?.clone())
                }
                "--out" => {
                    parsed.out_dir = Some(PathBuf::from(
                        iter.next().ok_or("--out needs a path")?.clone(),
                    ))
                }
                "--min-traces" => {
                    parsed.min_traces = Some(
                        iter.next()
                            .ok_or("--min-traces needs a count")?
                            .parse()
                            .map_err(|e| format!("--min-traces: {e}"))?,
                    )
                }
                "--min-coverage" => {
                    parsed.min_coverage = Some(
                        iter.next()
                            .ok_or("--min-coverage needs a fraction")?
                            .parse()
                            .map_err(|e| format!("--min-coverage: {e}"))?,
                    )
                }
                other => return Err(format!("unknown `--evaluate-routing` argument `{other}`")),
            }
        }
        Ok(parsed)
    }
}

fn default_state_dir() -> PathBuf {
    // The headless binary keeps router state beside its config, which is where
    // `TraceLog::open` looks and where `main.rs` already writes.
    zroutery_lib::platform::default_config_dir()
}

/// Run the evaluation and print a summary. Returns the process exit code.
pub fn run(args: EvalArgs) -> Result<i32, String> {
    let log = TraceLog::open(&args.state_dir)
        .map_err(|error| format!("could not open the trace log: {error}"))?;

    let traces = match log.load() {
        Ok(traces) => traces,
        Err(TraceError::Corrupt { line, reason, .. }) => {
            // A corrupt line is a finding, not a crash: report it with its position so
            // an operator can go and look at it.
            eprintln!("trace line {line} is unreadable: {reason}");
            return Ok(2);
        }
        Err(error) => return Err(format!("could not read the trace log: {error}")),
    };

    let quality = DataQualityReport::of(&traces);
    print_quality(&traces, &quality);

    let mut config = EvaluationConfig::default();
    if let Some(min_traces) = args.min_traces {
        config.min_traces = min_traces;
    }
    if let Some(min_coverage) = args.min_coverage {
        if !(0.0..=1.0).contains(&min_coverage) {
            return Err("--min-coverage must be between 0 and 1".to_string());
        }
        config.min_coverage = min_coverage;
    }

    for note in evaluability(&quality, &config) {
        println!("  {note}");
    }

    // A reference model fitted over this body, purely so the promotion gate has the
    // training side it requires. Trained here rather than loaded from the active
    // pointer so the comparison and the model being judged come from the same run.
    let training = train_reference(&traces);
    let candidate = match &training {
        Some(outcome) => MlPolicy::new(outcome, config.reward_policy.clone()),
        // No model could be fitted (too little data): evaluate the cold-start
        // ensemble, which is the honest "what do we have" baseline.
        None => MlPolicy::from_ensemble(ModelEnsemble::new(), config.reward_policy.clone()),
    };

    let report = evaluate(
        &traces,
        &candidate,
        &config,
        training.as_ref().map(|outcome| &outcome.report),
        now_seconds(),
    )
    .map_err(|error| format!("evaluation failed: {error}"))?;

    println!();
    println!("verdict: {}", report.verdict.headline());
    match &report.task_quality {
        zroutery_core::ml::TaskQualityVerdict::Available { rate, denominator } => {
            println!(
                "task quality: {:.1}% over {denominator} verified verdict(s)",
                rate * 100.0
            )
        }
        zroutery_core::ml::TaskQualityVerdict::Unavailable { reason } => {
            println!("task quality: unavailable — {reason}")
        }
    }
    for blocker in report.blockers() {
        println!("  blocked: {blocker}");
    }

    if let Some(out) = &args.out_dir {
        write_reports(&report, out)?;
        println!();
        println!("wrote {}", out.join("evaluation.json").display());
        println!("wrote {}", out.join("evaluation.md").display());
    } else {
        println!();
        println!("{}", report.to_markdown());
    }

    // A blocked evaluation is a successful run of the command with an honest answer,
    // so it is not an error exit. Only a genuine failure returns non-zero.
    Ok(0)
}

fn print_quality(traces: &[zroutery_core::ml::RequestTrace], quality: &DataQualityReport) {
    println!("collected traces: {}", quality.traces);
    if quality.traces == 0 {
        println!(
            "  no traces in {}. Serve real traffic first; there is nothing to evaluate.",
            zroutery_core::ml::TRACES_FILE_NAME
        );
        return;
    }
    if let (Some(first), Some(last)) = (quality.earliest_recorded_at, quality.latest_recorded_at) {
        println!("  recorded {first} .. {last}");
    }
    println!(
        "  execution identity: {:.0}% declared a model ({} disagree with the request)",
        quality.execution_identity_coverage() * 100.0,
        quality.declared_model_mismatch
    );
    println!(
        "  cost: {:.0}% | usage: {:.0}% | multi-attempt: {}",
        quality.cost_coverage() * 100.0,
        quality.usage_coverage() * 100.0,
        quality.with_fallback_chain
    );
    let coverage = &quality.task_evidence;
    println!(
        "  task verdicts: {} of {} requests ({:.0}% verified, {} mechanical, {} model-judged)",
        coverage.observed,
        coverage.total,
        coverage.observed_rate() * 100.0,
        coverage.mechanical,
        coverage.model_judged
    );
    if quality
        .schema_versions
        .iter()
        .any(|v| *v > TRACE_SCHEMA_MAX_READABLE)
    {
        println!("  warning: traces newer than this build can read are present");
    }
    let _ = traces;
}

/// Train a reference model over a time-ordered prefix, purely to give the promotion
/// gate the training side it requires.
///
/// Deliberately time-partitioned: fitting on the whole body and then evaluating on the
/// same body is the leak the existing `warmup` module already documents, and repeating
/// it here would make every comparison look better than it is.
fn train_reference(
    traces: &[zroutery_core::ml::RequestTrace],
) -> Option<zroutery_core::ml::learning::TrainingOutcome> {
    let mut samples: Vec<OutcomeTrainingSample> = Vec::new();
    for trace in traces {
        for sample in &trace.samples {
            if matches!(sample.scope, zroutery_core::ml::SampleScope::Attempt { .. }) {
                samples.push(sample.clone());
            }
        }
    }
    if samples.len() < 32 {
        return None;
    }
    // `run_training` performs its own group-wise, time-ordered split, so the holdout
    // it reports was never fitted on. Fitting here and evaluating on the same body
    // would be the leak `ml/warmup` already documents.
    run_training(&samples, &TrainingConfig::default()).ok()
}

fn write_reports(report: &zroutery_core::ml::EvaluationReport, out: &Path) -> Result<(), String> {
    std::fs::create_dir_all(out).map_err(|e| format!("could not create {}: {e}", out.display()))?;
    let json = serde_json::to_string_pretty(report)
        .map_err(|e| format!("could not serialize the report: {e}"))?;
    std::fs::write(out.join("evaluation.json"), json)
        .map_err(|e| format!("could not write the JSON report: {e}"))?;
    std::fs::write(out.join("evaluation.md"), report.to_markdown())
        .map_err(|e| format!("could not write the Markdown report: {e}"))?;
    Ok(())
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}
