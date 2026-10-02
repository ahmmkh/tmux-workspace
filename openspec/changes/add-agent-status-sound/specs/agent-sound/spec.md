# Spec Delta

## Purpose

Tells the user by sound when an agent starts needing them or finishes out of sight, so they do not have to watch the status line.

## ADDED Requirements

### Requirement: A blocked agent plays a sound
The system SHALL play the blocked sound once when the status of a pane becomes blocked, whether or not the Tab of that pane is on screen.

#### Scenario: An agent asks for an approval in another Tab
- **WHEN** an agent in a Tab that is not on screen becomes blocked
- **THEN** the blocked sound plays once

#### Scenario: An agent asks a question in the Tab on screen
- **WHEN** an agent in the Tab the user is looking at becomes blocked
- **THEN** the blocked sound plays once

#### Scenario: The same blocked status is reported twice
- **WHEN** a pane that is already blocked is reported blocked again
- **THEN** no sound plays

### Requirement: A done agent plays a different sound
The system SHALL play the done sound once when the status of a pane becomes done, and the done sound SHALL differ from the blocked sound.

#### Scenario: An agent finishes out of sight
- **WHEN** an agent finishes a turn in a Tab the user is not looking at
- **THEN** the done sound plays once

#### Scenario: An agent finishes in the Tab on screen
- **WHEN** an agent finishes a turn in the Tab the user is looking at and becomes idle
- **THEN** no sound plays

### Requirement: Other status changes are silent
The system MUST NOT play a sound when a pane becomes working or idle, when an agent starts, when an agent is restored, or when a pane has no agent in front.

#### Scenario: Many agents start together
- **WHEN** several agents start or are restored and each becomes idle
- **THEN** no sound plays

#### Scenario: The user views a Tab with done agents
- **WHEN** viewing a Tab turns its done agents idle
- **THEN** no sound plays

### Requirement: The user can silence the sounds
The system SHALL let the user turn all agent sounds off and on with one key, SHALL show in the status line that the sounds are off, and SHALL keep that choice when the configuration is reloaded.

#### Scenario: The user mutes the sounds
- **WHEN** the user presses the mute key while the sounds are on
- **THEN** the status line shows that the sounds are off
- **AND** a pane that becomes blocked or done plays no sound

#### Scenario: The user unmutes the sounds
- **WHEN** the user presses the mute key while the sounds are off
- **THEN** the next pane that becomes blocked or done plays its sound

### Requirement: A sound never delays the terminal
The system MUST start a sound without waiting for it to finish.

#### Scenario: A sound takes time to play
- **WHEN** a sound is playing
- **THEN** the terminal and the status marks keep responding
