# tmux-workspace

tmux-workspace is a personal tmux setup that organises terminals into workspaces, tabs and
panes. It marks each of them with the status of the agents that run there.

## Language

### Layout

**Workspace**:
A named, persistent group of tabs for one project or task.
_Avoid_: session outside tmux commands, project

**Tab**:
One full-screen view inside a workspace that holds one or more panes.
_Avoid_: window, view

**Pane**:
One terminal inside a tab.
_Avoid_: split

**Parked pane**:
A pane hidden from its tab while its program keeps running.

**Home workspace**:
The workspace you parked a pane from. The parked pane counts toward it.

### Agent status

**Agent**:
A coding-agent program that runs interactively in a pane.

**Agent status**:
What an agent is doing from your point of view. It is working, blocked, done or idle.

**Working**:
The agent is running a turn.

**Blocked**:
The agent cannot continue without you. It is waiting for an approval or an answer, or its
turn failed.
_Avoid_: needs input, waiting

**Done**:
The agent finished a turn while you were not looking at its tab, and you have not looked
since.

**Idle**:
The agent is waiting for a prompt and you have seen it.

**Seen**:
A done agent becomes seen, and therefore idle, when a terminal with focus shows its tab.

**Roll-up**:
The status shown for a tab or workspace. It is the most urgent status among its agents, in
the order blocked, done, working, idle.

**Blocker**:
An unresolved technical obstacle that the attention tool finds in an agent's output. A
blocker is not a blocked agent.
