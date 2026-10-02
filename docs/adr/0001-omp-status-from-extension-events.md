# omp status comes from extension events, not the terminal title

omp already encodes working, waiting and idle in its terminal title. A single tmux hook
could have read that title with no omp-side code and no restart of running agents. We chose
extension events instead, because they are omp's explicit interface and herdr's
integration relies on them.

## Considered options

We considered reading the terminal title and rejected it. The title misreports status in
four cases that events handle:

1. It says idle while background jobs or subagents are still running.
2. omp does not restore it when omp is killed or crashes.
3. It keeps the waiting mark until an approved tool finishes.
4. It follows whichever subagent is in view.

## Consequences

Extension events cost more code, which has to track omp's event semantics, and running
agents must restart before they use it.
