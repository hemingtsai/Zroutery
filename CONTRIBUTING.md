# Contributing

## Commit convention

Every commit must follow the conventional format:

    <type>(<scope>): <summary>

- type: `feat` | `fix` | `refactor` | `test` | `perf` | `docs` | `build` | `ci` | `chore`
- scope: `core` | `router` | `protocol` | `policy` | `runtime` | `ml` | `account` | `provider` | `integration` | `tauri` | `workflow` | `repo`
- summary: lowercase start, imperative, no trailing period, keep it under 72 characters

A commit message must describe only the changes the commit introduces.
Do not copy task prompts, roadmaps, gate reports, test transcripts, or
future-stage plans into commit messages — that information belongs in
`docs/development/`.

CI enforces this: the `commit-lint` workflow checks every commit in a
pull request. Full rules and rationale:
[docs/development/WORKFLOW.md](docs/development/WORKFLOW.md)
