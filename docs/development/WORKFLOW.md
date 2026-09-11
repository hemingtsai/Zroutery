# Zroutery Development Workflow

This directory holds the development record: workflow rules, history
treatment records, and stage documentation. Git commit history carries
code changes only — process and planning artifacts live here.

## Commit Content Rule

COMMIT CONTENT RULE

Git commit message must describe only the changes introduced by the commit.

Do not copy the task prompt, roadmap, workflow, gate report, test transcript,
future-stage plan, or implementation diary into the commit message.

Keep the subject concise and semantic: <type>(<scope>): <summary>

Stage, Gate, Roadmap, and Workflow information belongs to the development
record (docs/development/), not Git commit history.

### Format

- type: `feat` | `fix` | `refactor` | `test` | `perf` | `docs` | `build` | `ci` | `chore`
- scope: `core` | `router` | `protocol` | `policy` | `runtime` | `ml` | `account` | `provider` | `integration` | `tauri` | `workflow` | `repo`
- summary: lowercase start, imperative, ≤ 72 characters, no trailing period
- body: 0–5 lines answering only What changed / Why / Compatibility note

### Enforcement

The `commit-lint` workflow validates every commit in a pull request:

- subject must match
  `^(feat|fix|refactor|test|perf|docs|build|ci|chore)\((core|router|protocol|policy|runtime|ml|account|provider|integration|tauri|workflow|repo)\): [a-z].{0,70}$`
- subject and body must not contain (case-insensitive)
  `\bStage\s*\d`, `\[WORKFLOW\]`, `Gate\s+\w+\s*[—-]\s*PASS`, `^Roadmap:`,
  `^Next Stage:`, `^Tests:\s*\d`, `^Risks:`, `^Scope:`, `^Implementation:`

### History

Main was rewritten once (2026-09-11) from 73 commits to 34 curated commits —
see `history-treatment-map.md` and `tag-remap.md` in this directory. Old
history remains reachable via `archive/pre-history-treatment`. Further
history rewrites are frozen.
