# Spec Delta

## Purpose

Shows for every agent that runs in a pane whether it is working, blocked, done or idle.
Rolls that status up to the tab and the workspace, so the user knows where an agent needs
attention without visiting each pane.

## ADDED Requirements

### Requirement: A pane has an agent status

The system SHALL give every pane that runs an agent exactly one status at a time. The four
statuses are working, blocked, done and idle. A pane that runs no agent SHALL have no
status and SHALL show no mark.

#### Scenario: An agent runs a turn

- **WHEN** the user sends a prompt to an agent
- **THEN** the status of its pane is working

#### Scenario: An agent waits for its first prompt

- **WHEN** an agent starts in a pane and waits for a prompt
- **THEN** the status of its pane is idle

#### Scenario: A pane runs no agent

- **WHEN** a pane has never run an agent
- **THEN** the pane has no status
- **AND** the pane shows no mark

### Requirement: Blocked means the agent cannot continue without the user

The system SHALL show blocked when the agent waits for an approval, waits for an answer to
a question, has submitted a plan that awaits approval, or has a failed turn. The status
SHALL return to working when the user answers and the agent continues its turn.

#### Scenario: The agent asks for an approval

- **WHEN** an agent asks the user to approve an action
- **THEN** the status of its pane is blocked

#### Scenario: The agent asks a question

- **WHEN** an agent asks the user a question and waits for the answer
- **THEN** the status of its pane is blocked

#### Scenario: A plan awaits approval

- **WHEN** an agent submits a plan for the user to approve
- **THEN** the status of its pane is blocked

#### Scenario: A turn fails

- **WHEN** the turn of an agent ends with an error
- **THEN** the status of its pane is blocked

#### Scenario: The user answers and the agent continues

- **WHEN** the user answers the approval or the question and the agent continues its turn
- **THEN** the status of its pane is working

### Requirement: A failed turn stays blocked until the next prompt

The system SHALL keep a pane blocked after a failed turn until the user sends the next
prompt. This SHALL hold whether or not the user was watching the pane when the turn failed.

#### Scenario: A turn fails while the user watches

- **WHEN** the turn of an agent fails in a tab that a terminal with focus is showing
- **THEN** the status of its pane is blocked
- **AND** the status does not change to idle

#### Scenario: The user prompts again after a failed turn

- **WHEN** a pane is blocked because its turn failed and the user sends a new prompt
- **THEN** the status of its pane is working

### Requirement: omp stays working while its background work runs

The system SHALL keep a pane that runs omp working while background work that omp started
is still running. Background work is a background job or a subagent. The turn SHALL count
as finished only when omp and all of its background work have stopped.

#### Scenario: The reply ends while a background job runs

- **WHEN** omp finishes its reply and a background job that it started is still running
- **THEN** the status of its pane stays working

#### Scenario: The last background work finishes

- **WHEN** omp has finished its reply and its last background work ends
- **THEN** the turn is finished
- **AND** the status of its pane becomes done or idle under the rule for a finished turn

### Requirement: Done appears only when a turn finishes out of sight

The system SHALL decide the status of a pane when its turn finishes. A pane is in sight
when its tab is the current tab of a workspace that a terminal with focus is showing. A
pane in sight SHALL become idle. A pane out of sight SHALL become done. A tab on screen in
a terminal without focus SHALL count as out of sight. A terminal that cannot report focus
SHALL count as having focus.

#### Scenario: A turn finishes in sight

- **WHEN** a turn finishes in the current tab of a workspace that a terminal with focus is showing
- **THEN** the status of its pane is idle
- **AND** the pane never shows done

#### Scenario: A turn finishes in another tab

- **WHEN** a turn finishes in a tab that is not the current tab of its workspace
- **THEN** the status of its pane is done

#### Scenario: A turn finishes in another workspace

- **WHEN** a turn finishes in a workspace that no terminal is showing
- **THEN** the status of its pane is done

#### Scenario: A turn finishes while the terminal has no focus

- **WHEN** a turn finishes in a tab that is on screen in a terminal without focus
- **THEN** the status of its pane is done

### Requirement: Viewing a tab makes its done agents seen

The system SHALL treat a tab as viewed when any terminal with focus shows it. Viewing a tab
SHALL make every done agent in that tab seen, and each of those panes SHALL become idle.
This SHALL apply to every pane of the tab and not only to the active pane. Viewing SHALL
never clear blocked and SHALL not change working.

#### Scenario: The user switches to a tab with done agents

- **WHEN** two panes of a tab are done and the user switches to that tab in a terminal with focus
- **THEN** both panes become idle

#### Scenario: The user switches to a workspace

- **WHEN** the user switches to a workspace whose current tab holds a done agent
- **THEN** that pane becomes idle

#### Scenario: The terminal regains focus

- **WHEN** a tab with a done agent is on screen and its terminal regains focus
- **THEN** that pane becomes idle

#### Scenario: A terminal without focus shows the tab

- **WHEN** a tab with a done agent is on screen in a terminal without focus
- **THEN** that pane stays done

#### Scenario: The user views a blocked agent

- **WHEN** the user switches to a tab that holds a blocked agent
- **THEN** that pane stays blocked

### Requirement: Tabs and workspaces roll up status in a fixed order

The system SHALL give each tab the most urgent status among the agents in its panes. The
system SHALL give each workspace the most urgent status among the agents in its tabs and
its parked agents. The order of urgency SHALL be blocked, then done, then working, then
idle. A tab or workspace without an agent SHALL have no status.

#### Scenario: Blocked outranks every other status

- **WHEN** a tab holds one working agent and one blocked agent
- **THEN** the status of the tab is blocked

#### Scenario: Done outranks working

- **WHEN** a workspace holds a done agent in one tab and a working agent in another tab
- **THEN** the status of the workspace is done

#### Scenario: Working outranks idle

- **WHEN** a tab holds one working agent and one idle agent
- **THEN** the status of the tab is working

#### Scenario: Only idle agents

- **WHEN** every agent in a workspace is idle
- **THEN** the status of the workspace is idle

#### Scenario: No agent

- **WHEN** no pane of a tab runs an agent
- **THEN** the tab has no status

### Requirement: Marks appear on pane borders, tabs and the workspace name

The system SHALL show a mark for the status of each pane on its pane border. The system
SHALL show a mark for the rolled-up status of each tab beside that tab in the tab list.
The system SHALL show a mark for the rolled-up status of the workspace beside the workspace
name. A tab SHALL keep the same width whether or not it shows a mark. A pane, tab or
workspace without a status SHALL show no mark.

#### Scenario: A pane border shows the mark of its pane

- **WHEN** a pane runs a working agent
- **THEN** the border of that pane shows the mark for working

#### Scenario: A tab shows its rolled-up mark

- **WHEN** a tab holds a blocked agent
- **THEN** the tab list shows the mark for blocked beside that tab

#### Scenario: The workspace name shows its rolled-up mark

- **WHEN** any agent of the workspace is blocked
- **THEN** the mark for blocked appears beside the workspace name

#### Scenario: Tabs keep their width

- **WHEN** a tab gains or loses a mark
- **THEN** the tab takes the same number of columns as before
- **AND** no other tab moves

#### Scenario: The current tab keeps a readable mark

- **WHEN** the current tab has a status
- **THEN** its mark sits outside the highlight of the current tab and keeps its own colour

### Requirement: Marks use coloured dots and a ring for idle

The system SHALL draw each mark as a coloured dot, or as a ring for idle. Blocked SHALL be
a red dot. Done SHALL be a cyan dot. Working SHALL be a yellow dot. Idle SHALL be a green
ring.

#### Scenario: Each status has its own mark

- **WHEN** four panes hold a blocked, a done, a working and an idle agent
- **THEN** their borders show a red dot, a cyan dot, a yellow dot and a green ring in that order

#### Scenario: Idle differs by shape

- **WHEN** an agent is idle
- **THEN** its mark is a ring and not a filled dot

### Requirement: A parked agent counts toward its home workspace

The system SHALL count a parked agent toward the status of its home workspace. A parked
agent SHALL be out of sight. A parked agent that is done SHALL keep showing done until the
user brings its pane back into a tab.

#### Scenario: A parked agent is blocked

- **WHEN** an agent in a parked pane becomes blocked
- **THEN** the mark of its home workspace shows blocked

#### Scenario: A parked agent finishes a turn

- **WHEN** an agent in a parked pane finishes its turn
- **THEN** the status of its pane is done
- **AND** its home workspace shows done unless a more urgent status outranks it

#### Scenario: The user views the home workspace

- **WHEN** a parked agent is done and the user views every tab of its home workspace
- **THEN** the parked agent stays done

#### Scenario: The user brings the pane back

- **WHEN** the user brings a parked pane with a done agent back into a tab in sight
- **THEN** that pane becomes idle

### Requirement: An agent that exits shows no mark

The system SHALL show no mark for a pane whose agent has exited. That pane SHALL stop
counting toward the status of its tab and its workspace. This SHALL also hold when the
agent dies without a clean exit and the pane is back at its shell.

#### Scenario: An agent exits

- **WHEN** an agent exits and its pane returns to the shell
- **THEN** the pane shows no mark
- **AND** the pane no longer counts toward its tab or its workspace

#### Scenario: An agent is killed

- **WHEN** an agent is killed while it is working and its pane returns to the shell
- **THEN** the pane shows no mark

### Requirement: Snapshots do not save status

The system SHALL not record agent status in a snapshot. An agent that a restore starts
again SHALL start fresh, and its status SHALL come only from what that agent reports after
it starts.

#### Scenario: A blocked agent is saved and restored

- **WHEN** the system takes a snapshot while an agent is blocked and a restore later starts that agent again
- **THEN** the restored pane does not show blocked from the snapshot
- **AND** the pane shows the status that the restored agent reports

### Requirement: Only the pane's own interactive agent sets its status

The system SHALL accept status only from the interactive agent that the user talks to in
that pane. A subagent, a nested agent and a headless agent SHALL not change the status of
the pane. An approval or a question that reaches the user through the pane's own agent
SHALL count as that agent being blocked, whichever part of it raised the request.

#### Scenario: A subagent finishes

- **WHEN** a subagent of the pane's agent finishes while the agent is still working
- **THEN** the status of the pane stays working

#### Scenario: A headless agent runs inside the pane

- **WHEN** the pane's agent starts another agent without an interface as part of its turn
- **THEN** that other agent does not change the status of the pane

#### Scenario: A subagent needs an approval from the user

- **WHEN** a subagent of the pane's agent asks the user to approve an action
- **THEN** the status of the pane is blocked

### Requirement: Status stays on the machine

The system SHALL keep agent status on the user's machine. The system SHALL never send
agent status to a model provider.

#### Scenario: A status changes

- **WHEN** the status of a pane changes
- **THEN** the system makes no network request

#### Scenario: An AI tool of the workspace calls a model provider

- **WHEN** an AI tool of the workspace sends a request to a model provider
- **THEN** the request contains no agent status

### Requirement: omp and Claude Code report status

The system SHALL report status for omp and for Claude Code in the first version. A pane
that runs any other agent SHALL show no mark.

#### Scenario: omp runs in a pane

- **WHEN** omp runs interactively in a pane
- **THEN** the pane has a status and shows its mark

#### Scenario: Claude Code runs in a pane

- **WHEN** Claude Code runs interactively in a pane
- **THEN** the pane has a status and shows its mark

#### Scenario: Another agent runs in a pane

- **WHEN** an agent other than omp or Claude Code runs in a pane
- **THEN** the pane shows no mark
