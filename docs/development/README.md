# Zroutery Development Record

Git history carries code changes only. Process, history treatment and subsystem
design records live here, so a commit message never has to explain the roadmap
(`WORKFLOW.md` is the rule; this file is the map).

## Reading order

1. [WORKFLOW.md](WORKFLOW.md) — commit content rule, message format, what CI
   enforces, and the history of the 2026-09-11 rewrite.
2. [history-treatment-map.md](history-treatment-map.md) — the rewrite itself:
   every original commit, its KEEP / SQUASH / FIXUP treatment, the 34 resulting
   nodes and the nine rulings taken along the way (Chinese).
3. [tag-remap.md](tag-remap.md) — where the `stage-*` tags ended up, which old
   tags were archived, and the tree-equality evidence for the rewrite.
4. Subsystem records, one file per subsystem or non-obvious change, named after
   the subsystem:

| Record | Covers |
|---|---|
| [newapi-adapter.md](newapi-adapter.md) | The NewAPI account adapter: panel endpoints and auth, quota-unit semantics, what "quota exhausted" means there, test approach, and the application-side wiring that is still missing. |

## Adding a record

- One file per subsystem or decision, named after the subject
  (`newapi-adapter.md`, not `notes-2026-09.md`).
- Record what the code does and why, where the upstream reference is pinned
  (repository + commit), and what is deliberately not implemented.
- Keep the file out of commit messages: link it from the commit body at most.
- If a claim can drift — a test count, an endpoint list, a preset table — write
  the command or source it was measured from next to it, so the next reader can
  re-check it instead of guessing.

## Language

User-facing documentation (`README.md`) and the history treatment map are
Chinese; the workflow, tag remap, contributing guide and subsystem records are
English. There is no stated policy yet — pick the majority language for the
document's audience and stay consistent within a file.
