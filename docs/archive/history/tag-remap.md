# Zroutery Tag Remap — History Treatment 2026-09-11

Boundary: 3e52e0139e4f5105108b93f6fc171fafdfbed31c..38f592cf24a7e5898b81ecc65b67dce34f868760
New head: history-treatment = d3c9d220589d08532ca59aeec0b0acc5d7764b7c (T34)

## Remap table

| TAG | OLD SHA | ARCHIVED AS | NEW SHA | NEW COMMIT SUBJECT |
|---|---|---|---|---|
| stage-1-v1 | (none) | — | 2cce36548db13c2a88f3c28054b9bd8e56a7ee47 | feat(core): add naming style and capability contract |
| stage-2-v1 | (none) | — | 5f536bc25f379ed4b4b9e220f88e37334ced7e57 | feat(protocol): add canonical IR and Responses lifecycle |
| stage-3-v1 | (none) | — | de6bba20dbfe5dbcfc9d2d90470620a5b3a10db4 | feat(runtime): add decision trace and diagnostics |
| stage-4-v1 | (none) | — | dd276d8b71e2ba389a5a77460be51dd40ee1022d | feat(runtime): add runtime statistics |
| stage-5-v1 | (none) | — | 4029e23d36130432d9081517f5e0c63e67e0ffcb | feat(account): add optional account component |
| stage-6-v1 | (none) | — | 83d756bdc6b1a0e5883a2a162efb2cdadd14c146 | feat(ml): add outcome and feedback model |
| stage-7a-v1 | (none) | — | 1b95b6d447e2c09e38cca94b0fe1f5c19de29f01 | feat(ml): add routing feature extraction |
| stage-7b-v1 | (none) | — | 0f7fc60a17b29a1073a733e0545eda5c293090d9 | feat(ml): add training dataset |
| stage-7c-v1 | (none) | — | bd28919bcf044f4e231e646ad3627f9aebca816c | feat(ml): add routing decision models |
| stage-7d-v1 | (none) | — | 2ee86f07d62b4db345b5c12dea7be1991a149258 | feat(ml): add evaluation framework |
| stage-7e0-v1 | 38f592cf24a7e5898b81ecc65b67dce34f868760 (lightweight) | 未归档（用户决定：不进 archive） | d3c9d220589d08532ca59aeec0b0acc5d7764b7c | feat(ml): add immutable model identity and replay |

## Archived old tags (15, all pre-range, pushed to origin)

| TAG | OLD TARGET | ARCHIVED AS |
|---|---|---|
| v0.1.0 | 156e319366d928b07f75bac3eb7141c8d2287cba | archive/v0.1.0 |
| v0.1.1 | ce81ce18ba7fc1387d3958a83ce98a276bba81da | archive/v0.1.1 |
| v0.2.0 | 7c2393dd1d906d616b4af48fa9d6bfcf2f8b821b | archive/v0.2.0 |
| v0.3.0 | 4fac24748ff07c1be54902467d8d5dd79e04b5d6 | archive/v0.3.0 |
| v0.4.0 | f68dc6d36bf7e3508ed4ccef5779cef9df75a3c0 | archive/v0.4.0 |
| v0.4.1 | e07b28678fdd279830fb942efb429d86f0ca8150 | archive/v0.4.1 |
| v0.4.2 | bfe84a4cdcd00ea502e2a193db15aa5640855af2 | archive/v0.4.2 |
| v0.5.0 | 339968f588bd7419fd4bb31ee6fb44c5cb8fa3c5 | archive/v0.5.0 |
| v0.6.0 | 46685a42749f496a093c6a7539437e35a6e61345 | archive/v0.6.0 |
| v0.6.1 | 16ac2513343b58a0af851c0643fb774f413cc4eb | archive/v0.6.1 |
| v0.7.0 | 9eba09a50a51f84e038dd98639036d851e1aec37 | archive/v0.7.0 |
| v0.7.1 | 9663ea85c5a9f13c70378c8ecb0383ad922752f0 | archive/v0.7.1 |
| v0.7.2 | dc0b7cef7b0bfde628dc30080bbfa85e73f50748 | archive/v0.7.2 |
| v0.8.0 | 3e52e0139e4f5105108b93f6fc171fafdfbed31c（治疗边界父提交） | archive/v0.8.0 |
| (pre-history marker) | 38f592cf24a7e5898b81ecc65b67dce34f868760 | archive/pre-history-treatment-20260911（H0 创建） |

Notes:
- All v0.x tags point OUTSIDE the treatment range (before 3e52e01) — they remain valid on the
  unchanged pre-treatment history and need no remap. Archived for permanent reachability.
- Old v0.x tags are NOT deleted (per task rules); they keep pointing at their original SHAs.
- stage-7e0-v1: per user decision, NOT archived; force-moved (re-normalized) from old
  lightweight tag at 38f592c to annotated tag at new T34 node d3c9d22. Old commit 38f592c
  remains reachable via archive/pre-history-treatment (branch + tag).
- Incident record: archive/stage-7e0-v1 was briefly created and pushed by a partially-executed
  command before user rejection; deleted from local and remote immediately (verified absent).

## Fidelity evidence

- stage-2-v1 target (5f536bc, T15) tree == original stage-2 freeze point aab0158 tree
  (git diff --exit-code empty) — fixup-in-order preserves intermediate freeze trees.
- stage-7e0-v1 target (d3c9d22, T34) tree == original main 38f592c tree (H4 gates 1-3).
