# Moving a person's notes into Knode

You are an agent helping a person bring their existing notes into Knode, the local memory their
agents share. Follow these steps in order. Work with the `knode` command on this machine. Speak to
the person in their language, briefly. Ask them only what these steps say to ask.

Source folder: {source}

## Rules

- Never change, move or delete anything in the source folder. Knode imports a copy.
- Keep every file you write (plans, notes) outside the source folder, for example in a temporary
  folder. A plan written inside the source changes what it describes.
- Never store credentials in notes. Knode already leaves out key files and replaces obvious secrets
  in agent conversations; tell the person if you see others.
- Ask before you change an agent's configuration or a project's instruction file, and back up
  every file you edit.
- If a command fails, show the person the error and stop that step; do not work around it.

## 0. Find out where the person starts

Ask, unless it is already clear, which of these fits, and follow its path:

- **A folder of notes**, such as an Obsidian vault or a folder that holds their projects: steps 1
  to 8 below, with that folder as the source.
- **Projects spread around**, such as twenty folders on the Desktop next to photos, downloads and
  other files, or projects in several places (Desktop, `~/Developer`, `~/Documents`): first
  "Projects spread around", then steps 1 to 8.
- **Nothing yet**: they want to start using Knode with their agents now. Go to "Starting from
  nothing", then steps 6 to 8.

Also look for memory their agents already keep, and offer to bring it: Claude Code's
`~/.claude/projects/*/memory/` folders, global instruction files (`~/.claude/CLAUDE.md`,
`~/.codex/AGENTS.md`), and memory tools or plugins. If another memory tool injects context into
every session, ask whether to turn it off once Knode is in place, so agents do not keep two
memories.

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

1. Create the space and make it the default: `knode init "<Name>"` and
   `knode spaces default "<Name>"`.
2. Ask which projects they work on now, and where. For each: register its folder
   (`knode --space "<Name>" folders add <folder> --kind project`), read its `README`, instruction
   files and recent history (`git log --oneline -20`), and save one short context note for it:
   what it is, its goal, its current state, key decisions and the next step, marked as written by
   you from those sources. Ask the person to correct anything you could not verify.
3. Bring the memory their agents already keep (see step 0): import a folder that holds those
   files, `knode --space "<Name>" import <folder>`, and link each note to its project.
4. Ask whether Knode should also keep their agent conversations from now on (capture): it is off
   unless they choose it, per space (`knode spaces capture "<Name>" on`).

Then continue with steps 6 to 8.

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
so it stays as files. Shall I go ahead?" List only what is doubtful as questions. Also ask:

- the name of the new space (suggest the source folder's name, or "Memory");
- whether to bring the conversations their agents held in these project folders
  (`knode onboard sessions --plan /tmp/knode-plan.json` lists them; recommend yes).

Wait for the answer.

## 4. Apply

Edit `/tmp/knode-plan.json` as agreed: `include`, `project` and group `name` only. Never edit
digests or `files`; if the source changed meanwhile, analyze again.

```sh
knode onboard apply --plan /tmp/knode-plan.json --destination "$HOME/Knode/<Name>.knode" --name "<Name>" --sessions
knode onboard inspect --target "$HOME/Knode/<Name>.knode"
```

Leave out `--sessions` if the person said no. Existing spaces are never overwritten. If the person
wants this to be their main memory: `knode spaces default "$HOME/Knode/<Name>.knode"`.

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

The agent needs a new session to see Knode. For Claude Code, also offer one line in its global
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

Do not change instruction files of projects you left as files, or of repositories other people
share, without asking.

## 8. Leave a checkpoint

Save one note in the new space with what you did, what you left out and why, and what is pending,
part of the project it concerns. With Knode's MCP tools use `write_node`; from the command line,
import a folder that holds the note: `knode --space "<space>" import <folder>`.

Then tell the person, in a few lines: the space and how many projects and notes it holds, what
stayed as files, what changed in their agents and instruction files (with the backups), and that
`knode onboard archive --target "<space>"` puts the copy aside if they want to start again.
