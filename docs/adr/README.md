# Architecture decision records

One file per decision that would be expensive to reverse. Numbered, never renumbered; a
superseded ADR stays in place with its status updated and a link to the one replacing it.

| ADR | Title | Status |
| --- | --- | --- |
| [0001](0001-ui-toolkit-egui.md) | UI toolkit: egui with egui_dock | Accepted |
| [0002](0002-native-drivers-behind-a-trait.md) | Native drivers behind a trait, not sqlx | Accepted |
| [0003](0003-ui-engine-message-passing.md) | UI and engine communicate only by messages | Accepted |
| [0004](0004-results-as-text.md) | Query results as text | Accepted; amended by 0006 and 0007 |
| [0005](0005-customisation-files-commands-then-wasm.md) | Customisation through files and commands now, WebAssembly plugins later | Accepted |
| [0006](0006-documents-as-extended-json-text.md) | Documents as Extended JSON text | Accepted |
| [0007](0007-arrow-on-bulk-paths.md) | Arrow batches on bulk paths | Accepted |

## Template

```markdown
# NNNN. Title

- Status: Proposed | Accepted | Superseded by NNNN
- Date: YYYY-MM-DD

## Context
What forces are at play, what problem needs a decision.

## Decision
What we chose.

## Consequences
What becomes easier, what becomes harder, what we accept.

## Alternatives considered
Each option and why it lost.
```
