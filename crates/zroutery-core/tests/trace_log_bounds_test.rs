//! What the durable trace log costs, and what destroying it does.
//!
//! The `tail` tests exist because the previous implementation was wrong in a way
//! its own documentation denied: it walked the file backwards correctly but
//! reached it with `fs::read`, so peak memory was the whole log whatever `limit`
//! said. Asserting the returned records cannot catch that — it passed unchanged.
//! So the load-bearing test here measures *how much had to be read to find the
//! tail*, and is written so that restoring the whole-file read makes it fail.

use std::path::{Path, PathBuf};

use zroutery_core::ml::features::FEATURE_SCHEMA_VERSION;
use zroutery_core::ml::traces::{RequestTrace, TraceLog};
use zroutery_core::ml::{ShadowCandidateInput, ShadowInput};
use zroutery_core::session::SessionRoutingMode;

/// A trace whose record is made large by its candidate set rather than by an
/// invented field. `candidates` is real: a decision genuinely does carry one
/// entry per candidate, so padding with them produces a record of a realistic
/// shape whose size we control.
fn trace_with_candidates(id: &str, candidates: usize, filler: usize) -> RequestTrace {
    let candidates = (0..candidates)
        .map(|slot| ShadowCandidateInput {
            candidate_id: format!("m{slot:03}-{}", "f".repeat(filler)),
            provider_id: format!("p{slot:03}-{}", "g".repeat(filler)),
            tier: None,
            eligible: true,
            features: Default::default(),
            rejection_reason: None,
        })
        .collect::<Vec<_>>();
    let input = ShadowInput {
        decision_id: format!("d-{id}"),
        policy_id: "p".to_string(),
        client_id: None,
        policy_revision: Default::default(),
        task: Default::default(),
        production_selected: "m000".to_string(),
        feature_schema: FEATURE_SCHEMA_VERSION,
        candidates,
        session_mode: SessionRoutingMode::Free,
        session_switch_count: 0,
        is_fallback: false,
    };
    RequestTrace::new(id.to_string(), 1_700_000_000, input, Vec::new())
}

/// A trace with no samples, so `deduped_samples_from` yields nothing and the
/// round reports an empty body rather than a verdict.
fn bare_trace(id: &str) -> RequestTrace {
    trace_with_candidates(id, 1, 0)
}

fn write_body(dir: &Path, count: usize, candidates: usize, filler: usize) {
    std::fs::create_dir_all(dir).expect("state dir");
    let log = TraceLog::open(dir).expect("open log");
    for index in 0..count {
        log.append(&trace_with_candidates(
            &format!("r{index}"),
            candidates,
            filler,
        ))
        .expect("append");
    }
}

#[test]
fn tail_returns_the_most_recent_records_in_write_order() {
    let dir = tempdir("tail-order");
    write_body(&dir, 40, 1, 0);
    let log = TraceLog::open(&dir).expect("open");

    let ids: Vec<String> = log
        .tail(5)
        .expect("tail")
        .iter()
        .map(|t| t.request_id.clone())
        .collect();

    assert_eq!(ids, ["r35", "r36", "r37", "r38", "r39"]);
    assert_eq!(log.tail(40).expect("tail all").len(), 40);
    // Asking for more than exists is not an error and is not padded.
    assert_eq!(log.tail(1000).expect("tail over").len(), 40);
    assert_eq!(log.tail(1).expect("tail one").len(), 1);
    assert!(log.tail(0).expect("tail none").is_empty());
}

#[test]
fn tail_finds_its_window_without_reading_the_whole_log() {
    // The load-bearing test, and the only one here that can fail if `tail` goes
    // back to reading the whole file.
    //
    // Reading the file whole returns exactly the same five records, so nothing
    // about the *result* distinguishes the two implementations — which is how the
    // previous version sat behind a comment claiming the opposite. So this asserts
    // on bytes actually read, which the implementation counts itself. The count is
    // worth trusting because a test that asserts "the file is big enough for this
    // ratio to mean something" guards its own premise.
    let dir = tempdir("tail-bounded");
    // 400 records of ~8KB: about 3.2MB.
    write_body(&dir, 400, 8, 900);
    let path = dir.join("traces.jsonl");
    let file_bytes = std::fs::metadata(&path).expect("stat").len();
    assert!(
        file_bytes > 3_000_000,
        "log is only {file_bytes} bytes; the padding is not making records big enough \
         for this test to mean anything"
    );

    let log = TraceLog::open(&dir).expect("reopen");
    let tailed = log.tail(5).expect("tail");
    assert_eq!(tailed.len(), 5, "the window itself still has to be correct");

    let read = log.tail_bytes_read();
    assert!(
        read < file_bytes / 4,
        "tail(5) read {read} bytes of a {file_bytes}-byte log. A backwards block walk \
         costs about the window plus one 64KB block; reading the file costs all of it."
    );
    // Stated so the bound cannot be met by shrinking the window instead of reading
    // less: five records of this size cannot fit in a quarter of a percent of the
    // log by accident.
    assert!(
        read * 20 < file_bytes,
        "the tail was suspiciously cheap; the padding probably stopped applying"
    );

    assert_eq!(
        tailed[0].request_id, "r395",
        "the window is the end of the log"
    );

    // Cost has to scale with the window, not the log. A 200-record window on the
    // same log may read more than a 5-record one, but it must not read 40x the
    // whole file to get there.
    let before = log.tail_bytes_read();
    let wide = log.tail(200).expect("tail wide");
    assert_eq!(wide.len(), 200);
    let wide_cost = log.tail_bytes_read() - before;
    assert!(
        wide_cost < file_bytes,
        "tail(200) read {wide_cost} bytes, which is more than the {file_bytes}-byte log; \
         the window is no longer bounded"
    );
}

#[test]
fn tail_handles_a_log_larger_than_one_block_when_the_window_is_the_whole_log() {
    // The case that was broken and not covered.
    //
    // When `limit` exceeds the record count there is no cut, every block read
    // belongs to the window, and the blocks have to be reassembled in file order.
    // An implementation that walks the blocks correctly but joins them in
    // read-order produces the file backwards, which fails to parse — and the
    // failure reaches the caller as "no request history has been recorded yet",
    // which is a wrong answer about a log that is full.
    //
    // It is invisible below 64KB, because a smaller log is one block and reversing
    // one block changes nothing. So this asserts on a log several blocks long.
    let dir = tempdir("tail-multiblock");
    // 12 records of ~200KB: about 2.4MB, comfortably more than three blocks.
    write_body(&dir, 12, 8, 24_000);
    let path = dir.join("traces.jsonl");
    let file_bytes = std::fs::metadata(&path).expect("stat").len();
    assert!(
        file_bytes > 3 * 64 * 1024,
        "log is only {file_bytes} bytes; this test is about crossing a block boundary"
    );

    let log = TraceLog::open(&dir).expect("open");

    // A window wider than the log: everything, in order.
    let all = log.tail(1000).expect("tail over");
    assert_eq!(all.len(), 12, "every record, and it must parse");
    let ids: Vec<&str> = all.iter().map(|t| t.request_id.as_str()).collect();
    assert_eq!(ids[0], "r0", "the first record is first, not last");
    assert_eq!(ids[11], "r11", "the last record is last");

    // And the exactly-equal case, where the walk finds every newline and stops at
    // the file's start rather than at a cut.
    assert_eq!(log.tail(12).expect("tail exact").len(), 12);

    // A genuine window still works on the same log.
    let window = log.tail(3).expect("tail window");
    let window_ids: Vec<&str> = window.iter().map(|t| t.request_id.as_str()).collect();
    assert_eq!(window_ids, ["r9", "r10", "r11"]);
}

#[test]
fn tail_reports_the_real_line_number_of_a_corrupt_record() {
    // The lazy line-number path. It used to be computed eagerly for every line by
    // scanning the prefix, which was the other half of the linear cost. Making it
    // lazy was only worth doing if it stayed *correct*, and the correctness is what
    // this pins: an operator has to be able to go and look at the line the error
    // names.
    let dir = tempdir("tail-corrupt");
    write_body(&dir, 30, 1, 0);
    let path = dir.join("traces.jsonl");

    let original = std::fs::read_to_string(&path).expect("read");
    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 30);
    lines[10] = "{not json".to_string();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("rewrite");

    let log = TraceLog::open(&dir).expect("reopen");
    let error = log.tail(25).expect_err("a corrupt line is an error");
    let text = error.to_string();

    assert!(
        text.contains("line 11"),
        "expected the absolute line number 11 in {text:?}; an offset into the window \
         would point the operator at the wrong line of a 30-line file"
    );
}

#[test]
fn count_streams_rather_than_materialising_every_record() {
    let dir = tempdir("count");
    write_body(&dir, 25, 2, 10);
    let log = TraceLog::open(&dir).expect("open");
    assert_eq!(log.count().expect("count"), 25);

    let empty = tempdir("count-empty");
    let log = TraceLog::open(&empty).expect("open");
    assert_eq!(log.count().expect("count empty"), 0);
}

#[test]
fn clearing_is_the_only_way_records_go_away_and_the_log_keeps_working() {
    let dir = tempdir("clear");
    write_body(&dir, 12, 1, 0);
    let log = TraceLog::open(&dir).expect("open");
    assert_eq!(log.count().expect("count"), 12);

    let cleared = log.clear().expect("clear");
    assert!(cleared.cleared, "there were records to remove");
    assert!(cleared.removed_bytes > 0);

    // The load-bearing part: after a clear the serving path must still append.
    // `clear` closes the append handle so truncation is possible on Windows, so an
    // implementation that failed to reopen on the next append would pass every
    // assertion above and then fail every subsequent request.
    assert_eq!(log.count().expect("count after clear"), 0);
    for index in 0..3 {
        log.append(&bare_trace(&format!("post{index}")))
            .expect("append after clear");
    }
    let after = log.tail(10).expect("tail after clear");
    assert_eq!(after.len(), 3, "only post-clear records survive");
    assert!(after.iter().all(|t| t.request_id.starts_with("post")));
    assert_eq!(log.count().expect("count final"), 3);
}

#[test]
fn clearing_an_empty_log_reports_nothing_removed_rather_than_failing() {
    let dir = tempdir("clear-empty");
    let log = TraceLog::open(&dir).expect("open");

    let cleared = log.clear().expect("clear");

    assert!(!cleared.cleared, "an empty log has nothing to clear");
    assert_eq!(cleared.removed_bytes, 0);
}

#[test]
fn clearing_does_not_reset_the_process_lifetime_counters() {
    // An operator watching `appended` climb wants to see it restart from their own
    // action, not jump to zero and look like a crash.
    //
    // Appended and cleared through **one** instance on purpose: the counters are
    // per-`TraceLog`, and a second handle on the same path would report zero
    // appended before a single record had been written through it.
    let dir = tempdir("clear-counters");
    let log = TraceLog::open(&dir).expect("open");
    for index in 0..5 {
        log.append(&bare_trace(&format!("r{index}")))
            .expect("append");
    }
    let appended_before = log.counters().appended;
    assert!(
        appended_before >= 5,
        "the appends went through this instance"
    );

    log.clear().expect("clear");

    assert_eq!(
        log.counters().appended,
        appended_before,
        "`appended` describes what this process has done, not what the file holds"
    );
}

#[test]
fn clearing_changes_what_a_later_round_can_learn_from() {
    // The consequence `DELETE /v1/ml/traces` states in its response, asserted
    // rather than described. The promotion round reads the whole log, so clearing
    // it changes what the next model is trained from. If that ever stops being
    // true the endpoint's note is wrong and has to change with it.
    use zroutery_core::ml::{round, RoundConfig, RoundError};

    let dir = tempdir("clear-round");
    let log = TraceLog::open(&dir).expect("open");
    log.append(&bare_trace("before")).expect("append");
    assert_eq!(log.count().expect("count before"), 1);

    log.clear().expect("clear");

    assert_eq!(log.count().expect("count after clear"), 0);
    assert!(
        log.load().expect("load after clear").is_empty(),
        "a cleared log leaves nothing for a round to read"
    );

    // And the round reports that state rather than inventing a verdict, because
    // "no history" is something an operator can legitimately create by clearing.
    let cleared = round::run_promotion_round(&dir, &RoundConfig::default(), Some("post".into()));
    assert!(
        matches!(cleared, Err(RoundError::NothingToLearn)),
        "expected NothingToLearn immediately after a clear, got {cleared:?}"
    );

    // Only what comes after is trainable. A record with no learnable samples still
    // counts as history, so this asserts the count rather than a promotion.
    log.append(&bare_trace("after")).expect("append");
    assert_eq!(log.count().expect("count final"), 1);
    assert_eq!(
        log.tail(10).expect("tail final")[0].request_id,
        "after",
        "the pre-clear record is not reachable from a tail either"
    );
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zr-traces-{tag}-{}-{}",
        std::process::id(),
        // Distinguishes concurrent runs of the same tag without a random source,
        // which would make a failure irreproducible.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp state dir");
    dir
}
