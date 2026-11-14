# File Transactions Reference

> Authority: reference · Owner: backend · Last verified: 2026-11-14 · Verified by: `cargo test --workspace --all-features --locked`

A file transaction replaces a small set of files with crash recovery. It is used
where a multi-file write must either finish or leave a readable journal, notably
client takeover.

## The unit

`FileChange` is one file's expected and desired state
(`crates/zroutery-core/src/file_transaction.rs::FileChange`):

- `path`;
- `before`: the exact original bytes, or `None` when the file was missing;
- `after`: the exact target bytes, or `None` when the file should be removed.

`Some(Vec::new())` is an existing empty file; `None` is absence. They are not
interchangeable.

## Commit

`commit(journal_dir, changes)` (`crates/zroutery-core/src/file_transaction.rs::commit`):

1. takes an exclusive journal lock;
2. recovers any transaction already in the journal;
3. normalises destinations to absolute, rejects duplicates and destinations
   inside the journal;
4. checks each live file still equals its `before` bytes;
5. stages snapshots, target blobs, hashes and permissions;
6. flushes the manifest and writes a durable commit marker;
7. rolls forward each destination, then cleans the completed journal.

A marker is the commit decision. If the marker write returns an error but the
marker is visible, the transaction is retained for recovery; it is not
reinterpreted as uncommitted (`crates/zroutery-core/src/file_transaction.rs::commit`).

## Recovery

`recover(journal_dir)` (`crates/zroutery-core/src/file_transaction.rs::recover`):

- a pending journal without a marker is discarded; live files are not touched;
- a committed journal is rolled forward;
- a destination must be at its exact `before` or `after` bytes; anything else is
  an external edit and is refused, never overwritten.

Recovery only rolls forward. It does not roll a committed transaction back.

## Guarantees and limits

- Each file replacement is atomic; readers can observe an intermediate
  multi-file state until publication or recovery finishes
  (`crates/zroutery-core/src/file_transaction.rs::commit`).
- The journal is locked, so two processes cannot stage the same journal
  (`crates/zroutery-core/src/file_transaction.rs::commit`).
- Bounds prevent a damaged journal from requesting absurd allocations:
  at most 4,096 changes, 256 MiB per blob, 512 MiB total, 4 MiB manifest,
  4 KiB commit marker, 32 KiB path
  (`crates/zroutery-core/src/file_transaction.rs::commit`).
- Symlinks are not followed when deciding whether a destination or journal is
  safe.

## Use

Client takeover builds `FileChange`s and calls `commit` for its multi-file
writes (`crates/zroutery-core/src/agent_takeover/gemini.rs`).
The transaction is the durability layer; ownership and conflict decisions stay
in takeover.

## Not implemented

- No multi-file instantaneous visibility for readers.
- No automatic rollback of a committed transaction; recovery is forward-only.
- No replacement for the takeover's own preview, stale-file detection or
  ownership rules.

## Sources

- `crates/zroutery-core/src/file_transaction.rs::FileChange`
- `crates/zroutery-core/src/file_transaction.rs::commit`
- `crates/zroutery-core/src/file_transaction.rs::recover`
- `crates/zroutery-core/src/agent_takeover/gemini.rs`
