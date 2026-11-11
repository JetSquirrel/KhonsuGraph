# Plugin security

**A plugin is a program, and it runs as you.** KhonsuGraph doesn't sandbox it. It can
read your files, use the network, start other programs and see your environment
variables, whatever it declares. Only install plugins you trust as much as any other
program you run.

## What is and isn't enforced

| Claim | Enforced? |
|---|---|
| A plugin's declared `network`, `read_files` and `commands` | **No.** They're what the plugin says about itself, shown so you can decide |
| A plugin doesn't run until approved | Yes, wherever its process would be started: `describe`, a connection test, a preview or dry run, a sync and an analysis. Until then it is sent no configuration and no secrets, and what it declares is read from its manifest |
| A changed plugin needs approval again | Yes, for its version, permissions and the files in its folder |
| `entry` stays inside the plugin's folder | Yes, except a bare program name found on `PATH` |
| An archive stays inside its folder and within size limits | Yes |
| Output is limited in size | Yes |
| Secrets are masked in what a plugin says | Partly: see below |

## What this protects against

| Threat | What happens |
|---|---|
| An archive or folder that writes outside itself (`../`, absolute paths, symbolic links) | Refused at install |
| A zip bomb | Refused past 512 MiB or 10,000 entries, counting what actually unpacks |
| A manifest whose `entry` points outside the folder | Refused when read |
| `khonsu plugin uninstall ..` | Refused: IDs are letters, digits, `-` and `_` |
| A plugin swapped or edited after approval, with the same version | Won't run until approved again |
| A reinstall that turns out to be broken | The installed plugin stays as it was |
| An endless line of output | The sync fails past 16 MiB in one line |
| Endless output from `describe` or `test` | Cut at 16 MiB |
| `describe` that never finishes | Stopped after 30 seconds |
| A plugin killed, crashing or sending nonsense partway | The sync is recorded as failed and removes nothing. Records already written stay, and the graph stays whole |
| An analysis that never finishes | Ended after 5 minutes, or by **Stop** in the window |
| An analysis that sends too much | Refused past 16 MiB in one line or 100,000 findings |
| An analysis that misquotes the graph | It only points at records, and KhonsuGraph reads their values itself. A reference, rule or verdict that doesn't match refuses the whole answer. See [offering an analysis](plugins.md#offering-an-analysis) |

## What this doesn't protect against

- **A malicious plugin you approved.** It can do anything you can.
- **Someone who can already write to your files.** The approval record lives beside
  the plugins, in your config folder. Change detection notices accidents and swapped
  files. It's no defense against an attacker with your account.
- **Programs found on `PATH`.** With `"entry": ["python3", "plugin.py"]`, the script is
  checked but `python3` isn't.
- **`__pycache__` and `.pyc` files.** These are left out of the check, because Python
  writes them as it runs, and they can hold code.
- **What a plugin does between approval and running.** It's checked each time its
  process is started.
- **Total volume.** A plugin can send endless valid records until you cancel, and
  they'll be written to the graph.
- **A wrong answer that is well formed.** An analysis can point at real records and
  still be wrong. KhonsuGraph checks that what it says matches the graph, not that its
  rules are right.

## What an analysis is sent

An analysis is sent records from the graph, and no source configuration or secret
fields. Properties that look like secrets are masked first. It is still a program
running as you, so it can read anything else you can.

## Secrets

A source's secret fields are stored in the system keychain, and sent to its plugin on
standard input, not in the environment or on the command line.

Their values are masked wherever KhonsuGraph keeps or shows what the plugin said:
progress, logs, gaps, test results and errors. This only recognizes values it was given
as secrets. It can't catch a credential the plugin got elsewhere, an encoded copy, or
what the plugin sends to the network or writes itself.

The plugin inherits your environment, so credentials you keep in environment variables
reach it.

## Approving

The window asks when you add or edit a source that uses the plugin, before it asks the
plugin anything: the dialog shows what the manifest declares. From the command line:

```sh
khonsu plugin install ./my-plugin        # shows what it declares
khonsu plugin approve my-plugin          # after you've read that
khonsu plugin list                       # approval: approved, not_approved or changed
```

`khonsu plugin install --yes` does both in one step. Before approval,
`khonsu plugin describe`, `khonsu source add` and `khonsu plugin sync` (with or without
`--dry-run`) fail with kind `plugin` and say that it needs approval.

Plugins approved before folder contents were checked read as `changed`, and need
approving once more. Built-in plugins ship with the app and need no approval.

## Network access

KhonsuGraph makes no network calls for a plugin. The AWS plugin calls AWS through the
`aws` CLI only when a source is configured with a profile or access key. With
`fixture_dir` it reads local files only.

## A future sandbox

Nothing here is a sandbox, and none is planned for this milestone. One would need
per-platform work on process isolation, filesystem and network limits, and a way to
grant what a plugin legitimately needs, and is a separate project.
