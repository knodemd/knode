# Moving a person's notes into Knode

You are an agent setting up Knode, the local memory a person's agents share, for that person.
Do it now, from start to end: do not ask whether they want to. Follow these steps in order with the
`knode` command on this machine. Speak to the person in their language, briefly. Look things up
yourself instead of asking; ask only once, in step 3, everything that needs their judgment or
their consent, then do all of it without stopping again unless a command fails.

Source folder: {source}

## Rules

- Never change, move or delete anything in the source folder. Knode imports a copy.
- Keep every file you write (plans, notes) outside the source folder, for example in a temporary
  folder. A plan written inside the source changes what it describes.
- Never store credentials in notes. Knode already leaves out key files and replaces obvious secrets
  in agent conversations; tell the person if you see others.
- Change an agent's configuration or a project's instruction file only with the consent given in
  step 3, and back up every file you edit first (next to it, or in one backup folder you name).
- Never commit or push in the person's repositories: leave your edits for them to review.
- If a command fails, show the person the error and stop that step; do not work around it.

## 0. Find out where the person starts

Look before asking. Check `knode spaces`, then where notes and projects usually live: Obsidian
vaults (folders with a `.obsidian` folder), and git repositories and Markdown folders in the home
folder, `~/Desktop`, `~/Documents`, `~/Developer`, `~/Projects`, `~/Proyectos`, `~/code`, `~/src`
(one or two levels deep; do not walk `~/Library` or the whole disk). Then decide which of these
fits, and follow its path (confirm it in step 3):

- **A folder of notes**, such as an Obsidian vault or a folder that holds their projects: steps 1
  to 8 below, with that folder as the source.
- **Projects spread around**, such as twenty folders on the Desktop next to photos, downloads and
  other files, or projects in several places (Desktop, `~/Developer`, `~/Documents`): first
  "Projects spread around", then steps 1 to 8.
- **Nothing yet**: they want to start using Knode with their agents now. Go to "Starting from
  nothing", asking its questions together with those of step 3 that apply, then steps 6 to 8.

Run `knode spaces`. If the person already has a space (not marked archived), ask whether this
import should become their memory or stay a separate space; see "If they already have a space".
A space that only holds captured conversations (kind `conversation`) is not their memory yet:
treat it as empty.

Also look for memory their agents already keep, to bring it in step 4: Claude Code's
`~/.claude/projects/*/memory/` folders, global instruction files (`~/.claude/CLAUDE.md`,
`~/.codex/AGENTS.md`), and memory tools or plugins (for Claude Code, `claude plugin list`; for
example claude-mem). Once Knode holds that memory, two memories drift apart: in step 3, propose to
turn off Claude Code's own memory (`"autoMemoryEnabled": false` in `~/.claude/settings.json`) and
any memory plugin that injects context into every session (`claude plugin disable <plugin>`).

To bring Claude Code's memory, use `knode import --from claude-code-memory` when `knode import
--help` lists it: it links each memory to its project. Otherwise, by hand: each folder in `~/.claude/projects/` is a working folder whose
path has every character that is not a letter or digit replaced by `-`
(`/Users/ada/Work/web.app` is `-Users-ada-Work-web-app`); skip folders that are symbolic links.
Match each to the project whose folder it is, or that contains it, by encoding the project
folders the same way and taking the longest match. Copy each `memory/*.md` file (not the
`MEMORY.md` index) into `<working folder>/<project>/Agent memory/`, and the ones of no project
into `<working folder>/_agent-memory/`, import the working folder after step 4
(`knode --space <space> import <working folder>`), then link each note to its project:
`knode --space <space> link <note id> "<project>" --kind part-of` (ids are in an export, or in
`knode search`). A project that has no node yet can be created by importing a note whose
frontmatter has `kind: project`.

## Projects spread around

Knode reads only Markdown and the files notes refer to: photos, PDFs, downloads, installers and
code in a folder are never imported. So a parent folder full of other things can still be the
source.

1. List the candidate folders: `ls` the places the person names (Desktop, Documents, Developer,
   Projects…), and for each subfolder note whether it is a git repository
   (`git -C <folder> rev-parse --show-toplevel`), how many Markdown files it has, and what else is
   in it.
2. Sort them into projects (code, writing, client or study work, with or without git), the person's
   notes, and everything else (photos, downloads, archives, installers, other people's material).
   Show the person the list and let them correct it.
3. If all their projects live under one folder, use that folder as the source and, in step 2,
   leave out the groups that are not projects.
4. If they live in several places, do not move anything. Make a working folder outside all of
   them, for example `/tmp/knode-source`, and copy into `<working folder>/<project>/` only the
   Markdown files of each project that are the person's notes (in a git repository,
   `git -C <project> ls-files` tells repository documentation apart; decide as in step 2), keeping
   their relative paths and the images and PDFs they link to. Use the working folder as the source
   in step 1 with `--mode vault`. Projects become one group each.
5. Register every project's real folder so future agent conversations go to it:
   `knode --space <space> folders add <project folder> --kind project`, after step 4. Agent
   conversations held in those folders are brought with `--sessions` only when the source holds the
   real folders; with a working folder, say so to the person.

Moving their projects into one folder is a separate decision, never a side effect. Only if they ask:
check first `git worktree list` and submodules in each repository, absolute paths in scripts,
launch agents and editors, and the agent's own per-folder settings and history (for Claude Code,
`~/.claude/projects/` and the projects in `~/.claude.json`); move, run `git worktree repair`, and
leave a link at the old place until nothing uses it.

## Starting from nothing

1. Set up the space you are connected to, if any: when Knode's instructions say this space is not
   set up yet, or `knode spaces` shows a default space that holds no notes of the person, that
   space is `<Name>` below; do not create another one. Only when there is no space at all, create
   one and make it the default: `knode init "<Name>"` and `knode spaces default "<Name>"`. A new
   space captures the conversations of every agent from the start: unless the person chose
   capture in step 3, turn it off right away with `knode spaces capture "<Name>" off`.
2. Ask which projects they work on now, and where. For each: register its folder
   (`knode --space "<Name>" folders add <folder> --kind project`); if that does not report a
   project node, create one by importing a folder holding a note named after the project whose
   frontmatter has `kind: project` (a space with projects counts as set up, so agents stop being
   asked to set it up); then read its `README`, instruction
   files and recent history (`git log --oneline -20`), and save one short context note for it:
   what it is, its goal, its current state, key decisions and the next step, marked as written by
   you from those sources. Ask the person to correct anything you could not verify.
3. Bring the memory their agents already keep (see step 0): import a folder that holds those
   files, `knode --space "<Name>" import <folder>`, and link each note to its project.
4. Capture, as the person chose in step 3: `knode spaces capture "<Name>" on|off`.

Then continue with steps 6 to 8.

## If they already have a space

When the new import is meant to replace an existing space (for example, a first try made by hand):

1. Before archiving anything, list the notes only the old space has: export both
   (`knode --space <old> export <folder>` and the same for the new one) and compare paths and
   titles; notes written in Knode itself (checkpoints, decisions) are the usual ones. Import those
   into the new space and link them to their projects.
2. Put the old one aside, never delete it: `knode onboard archive --target <old space>` for a
   space onboarding created; otherwise leave it and tell the person.
3. Give the new space the name the person and their instruction files already use:
   `knode spaces rename <new space> "<Name>"`, make it the default, and connect the agents to it
   (step 6), since their connection still points at the old space.

## 1. Look before deciding

```sh
knode --version
knode spaces
knode onboard analyze --mode vault --source "{source}" > /tmp/knode-plan.json
```

Use `--mode project` instead when the source is a single project folder. The plan is JSON:

- `groups`: the projects Knode found from folders and git repositories; `name` is editable, and
  groups with the same name merge.
- `documents`: every Markdown file, with its `project`, whether it is included (`include`) and,
  when it starts unchecked because a repository inside the source tracks it, that `repository`.
- `warnings`: what was left out and why (generated folders, other checkouts, credentials,
  oversized files) and attachment references not found.

Summarize it for yourself: projects, included documents, unchecked documents per repository.

## 2. Decide what is the person's memory

Knode should hold the person's own notes: status, decisions, plans, research, session logs, course
or client material. It should not hold what already lives, and changes, with code.

For each repository with unchecked documents, look at their paths (and open a few):

- **Bring back** the person's notes kept in a repository: status or state notes (`Estado.md`,
  `Status.md`), session logs (`Sesiones/`, `Sessions/`, `journal/`), decisions, research, plans,
  course or content folders, and repositories that are mostly notes. Set `include` to true.
- **Leave out** repository documentation: `README`, `CHANGELOG`, `CONTRIBUTING`, `SECURITY`,
  `LICENSE`, `CODE_OF_CONDUCT`, `AGENTS.md`, `CLAUDE.md`, API and architecture docs of the code,
  and anything generated.

Then look at the included documents for noise:

- notes that only list or link every other file, such as Obsidian maps, dashboards or indexes of
  everything: Knode makes a project page for each project, so they add little;
- copies of the same repository (a folder named like `*-review` or `*-copy` whose
  `git -C <folder> remote get-url origin` matches another repository's): keep one;
- folders of material copied from elsewhere, such as vendored references.

Some projects use their notes as files on purpose: a tool builds PDFs, sites or slides from them,
or a dedicated agent reads them from the folder. Leave those projects' notes as files (exclude
them) so their workflow keeps working, and tell the person.

## 3. Ask the person once

Write a short summary, for example: "I found 40 projects and 380 of your notes. I will also bring
the notes kept inside `crm`, `course` and `finances` (status and session logs), and leave out
1,200 README, CHANGELOG and similar files of your repositories, which stay with their code. I will
leave out the map notes, which only list other notes. `team-coaching` builds PDFs from its notes,
so it stays as files. Shall I go ahead?" List only what is doubtful as questions. In the same
message, with a recommended answer for each so the person can reply "yes to all":

- the name of the new space (suggest "Memory" in their language, or the source folder's name);
- whether to bring the conversations their agents held in these project folders
  (`knode onboard sessions --plan /tmp/knode-plan.json` lists them; recommend yes);
- whether to bring their agents' existing memory (step 0; recommend yes);
- which agents to connect (those `knode connect --list` detects; recommend the ones they use);
- whether Knode should keep every agent conversation from now on (capture; recommend off at
  first: whole transcripts crowd search, and the agents already save what matters);
- whether to update their projects' instruction files (step 7; recommend yes), and to turn off
  other memories (step 0; recommend yes).

Wait for the answer, then do steps 4 to 8 without asking again.

## 4. Apply

Edit `/tmp/knode-plan.json` as agreed: `include`, `project` and group `name` only. Never edit
digests or `files`; if the source changed meanwhile, analyze again.

If you are connected to a Knode space that is not set up yet (the `projects` MCP tool lists no
project, or `knode spaces` shows a default space holding only captured conversations), apply into
that space, so you and the person keep the connection you have: no reconnection, no new session.

```sh
knode onboard apply --plan /tmp/knode-plan.json --into "<space>" --sessions
knode onboard inspect --target "<space path>"
```

`<space>` is its name or `.knode` path, as `knode spaces` lists it; what it already holds stays.
A space that is already set up is refused: then, or when the person wants a separate space, create
a new one instead:

```sh
knode onboard apply --plan /tmp/knode-plan.json --destination "$HOME/Knode/<Name>.knode" --name "<Name>" --sessions
knode onboard inspect --target "$HOME/Knode/<Name>.knode"
```

Leave out `--sessions` if the person said no. Existing spaces are never overwritten. Both leave
capture off: if the person chose it in step 3, `knode spaces capture "<space>" on`. If a new space
is to be their main memory: `knode spaces default "$HOME/Knode/<Name>.knode"`. In the steps below,
use the space you applied into wherever they say `$HOME/Knode/<Name>.knode`, and skip connecting
again in step 6 the agents already connected to it.

## 5. Check it

- In the inspect output, `unresolved` lists links that found no note. Links to documents you left
  out on purpose are expected; mention any others that look important.
- Search for two or three things the person cares about and confirm the right notes come first:
  `knode --space "$HOME/Knode/<Name>.knode" search "<words>"`.

## 6. Connect the agents

Show what would change, then connect with the person's agreement:

```sh
knode connect --list
knode --space "$HOME/Knode/<Name>.knode" connect <agent> --print
knode --space "$HOME/Knode/<Name>.knode" connect <agent>
```

Sessions already open keep their old configuration: tell the person to start a new session of each
agent you connected. Check the result: for Claude Code, `claude mcp get knode` shows `knode mcp`
with the new space. If the agent was connected before to another space (for example, a first
try), connecting again replaces it. For Claude Code, also offer one line in its global
instruction file (`~/.claude/CLAUDE.md`), with a backup: "Your memory is Knode, space <Name>:
search it before asking about earlier work, and save decisions there." Global files load in
every session, so keep it to that line.

## 7. Move the memory habit to Knode

Instruction files (`CLAUDE.md`, `AGENTS.md`) often tell agents to keep memory in files: "update
`Estado.md`", "create `Sessions/YYYY-MM-DD.md`", "read the last session log first". Now that those
notes are in Knode, agents should search Knode when they start and save there when they finish;
otherwise two versions drift apart. With the person's agreement, for each project whose notes you
imported:

- back up the instruction file, then replace those file-memory steps with: search Knode for this
  project's context and last checkpoint when starting; save decisions, verified findings and the
  next step to Knode when finishing; the old status and session files are frozen as of today;
- add Knode's short workflow block: `knode instructions <agent> --project <folder> --apply`;
- move long status sections out of instruction files into a Knode note of that project, keeping
  only rules in the file: instruction files load in every session, Knode notes only when needed.
  Do it for files over about 4 KB that hold status, dated decisions or history. Rules stay in the
  file with their details (validation commands and their counts or known warnings, versions in
  security warnings, compliance and cost rules, who owns what); state, reasoning and history go,
  verbatim, to one note titled `<project> — context, status and decisions` (in the person's
  language), part of the project, and the file gets one line pointing to it. Then compare the
  original with the file and the note line by line: every fact must be in one of the two.
  Credentials never move to Knode; name variables or files instead of values.
- with the consent from step 3, turn off the other memories (step 0), after their content is in
  Knode.

Do not change instruction files of projects you left as files, or of repositories other people
share, without asking.

## 8. Leave a checkpoint

Save one note in the new space with what you did, what you left out and why, and what is pending,
part of the project it concerns. With Knode's MCP tools use `write_node`; from the command line,
import a folder that holds the note: `knode --space "<space>" import <folder>`.

Then tell the person, in a few lines (and to open new agent sessions so they use Knode): the space
and how many projects and notes it holds, what
stayed as files, what changed in their agents and instruction files (with the backups), and that
`knode onboard archive --target "<space>"` puts the copy aside if they want to start again.
