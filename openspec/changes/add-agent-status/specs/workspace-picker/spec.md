# Spec Delta

## Purpose

Lets the user choose a workspace, a tab or a parked pane from a list that shows agent
status, and lets the user create a workspace by typing a new name.

## ADDED Requirements

### Requirement: The workspace picker shows each workspace's rolled-up status

The workspace picker SHALL show the rolled-up agent status of each live workspace as a
coloured status word in its row. The status SHALL include the parked agents whose home is
that workspace. A row for a workspace without an agent SHALL show no status. The existing
marker for a live workspace SHALL keep its meaning.

#### Scenario: A workspace has a blocked agent

- **WHEN** the user opens the workspace picker and one workspace holds a blocked agent
- **THEN** the row of that workspace shows the word blocked

#### Scenario: A workspace has a parked agent

- **WHEN** the only agent of a workspace is parked and done
- **THEN** the row of that workspace shows the word done

#### Scenario: A workspace has no agent

- **WHEN** a workspace runs no agent
- **THEN** its row shows no status

#### Scenario: The marker for a live workspace stays

- **WHEN** a row shows a status word
- **THEN** the row still shows the marker that says the workspace is live

### Requirement: Status text never changes which workspace the picker opens or creates

The workspace picker SHALL open or create a workspace by its name alone. The status text
of a row SHALL never become part of that name. The search SHALL not match status text.

#### Scenario: The user selects a row that shows a status

- **WHEN** the user selects the row of the workspace `odoo-project` and that row shows blocked
- **THEN** the picker opens the workspace named `odoo-project`
- **AND** the picker creates no workspace

#### Scenario: The user searches for a status word

- **WHEN** the user types `blocked` and no row has that text outside its status
- **THEN** no row matches

#### Scenario: The user creates a workspace named like a status

- **WHEN** the user types `done`, no row matches and the user presses Enter
- **THEN** the picker creates a workspace named `done`

### Requirement: Typing a name that matches nothing creates that workspace

The workspace picker SHALL create and open a workspace with the typed name when the user
types a name that matches no row and presses Enter. The picker SHALL create nothing else.
The picker SHALL create nothing when the user cancels.

#### Scenario: The user types a new name

- **WHEN** the user types `new-client`, no row matches and the user presses Enter
- **THEN** the picker creates a workspace named `new-client` and opens it
- **AND** the picker creates no other workspace

#### Scenario: The typed text matches a row

- **WHEN** the user types text that matches a row and presses Enter
- **THEN** the picker opens the workspace of the selected row
- **AND** the picker creates no workspace from the typed text

#### Scenario: The user cancels

- **WHEN** the user types a name and cancels the picker
- **THEN** the picker opens nothing and creates nothing

### Requirement: The tab picker shows each tab's status

The tab picker SHALL show the rolled-up agent status of each tab as a status word after
the tab name. A row for a tab without an agent SHALL show no status. The status word SHALL
not change which tab the picker switches to.

#### Scenario: A tab has a working agent

- **WHEN** the user opens the tab picker and one tab holds a working agent
- **THEN** the row of that tab shows the word working after the tab name

#### Scenario: A tab has no agent

- **WHEN** a tab runs no agent
- **THEN** its row shows no status

#### Scenario: The user selects a row that shows a status

- **WHEN** the user selects the row of a tab and that row shows a status word
- **THEN** the picker switches to that tab

### Requirement: The parked chooser shows each parked pane's status

The parked chooser SHALL show the agent status of each parked pane as a status word in its
row. A row for a parked pane without an agent SHALL show no status.

#### Scenario: A parked pane holds a done agent

- **WHEN** the user opens the parked chooser and a parked pane holds a done agent
- **THEN** the row of that pane shows the word done

#### Scenario: A parked pane holds no agent

- **WHEN** a parked pane runs no agent
- **THEN** its row shows no status

### Requirement: The pickers work when they cannot read status

The workspace picker, the tab picker and the parked chooser SHALL keep working when they
cannot read agent status. Each SHALL show its rows without status, and selection and creation
SHALL behave as they do with status.

#### Scenario: The workspace picker cannot read status

- **WHEN** the user opens the workspace picker and the picker cannot read agent status
- **THEN** the picker lists every workspace without a status
- **AND** selecting a row opens that workspace
- **AND** typing a name that matches nothing creates that workspace

#### Scenario: The tab picker cannot read status

- **WHEN** the user opens the tab picker and the picker cannot read agent status
- **THEN** the picker lists every tab without a status
- **AND** selecting a row switches to that tab

#### Scenario: The parked chooser cannot read status

- **WHEN** the user opens the parked chooser and the chooser cannot read agent status
- **THEN** the chooser lists every parked pane without a status
- **AND** choosing a row brings that pane back
