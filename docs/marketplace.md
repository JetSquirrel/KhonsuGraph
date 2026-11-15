# Marketplaces

A marketplace is where plugins are found by name. It is a folder holding
`marketplace.json` and the packages it lists, one `.zip` per plugin and
platform. The folder can be on disk, in a GitHub repository, or behind any
HTTPS address, and is read the same way.

```sh
khonsu plugin search sql                 # what the marketplaces offer
khonsu plugin install sql --yes          # download, check, install, approve
khonsu plugin update                     # bring installed plugins up to date
```

In the window, **Add source → More Plugins…** shows the same list: install or
update a plugin there, and add or remove a marketplace. A plugin installed
there goes on to the approval dialog, then to its source form.

Every installation starts with the `official` marketplace, served from
`marketplace.khonsugraph.com` and built from the repository
[JetSquirrel/khonsugraph-marketplace](https://github.com/JetSquirrel/khonsugraph-marketplace).

## Using one

```sh
khonsu marketplace add acme/khonsu-plugins          # owner/repo on GitHub
khonsu marketplace add https://plugins.example.com  # any HTTPS address
khonsu marketplace add ~/work/team-plugins          # a folder, such as a checkout
khonsu marketplace list
khonsu marketplace remove team-plugins
```

A marketplace is known by the `name` in its index, or by `--name`. Adding the
same name again replaces it. `khonsu marketplace remove official` turns the
official one off, and `khonsu marketplace restore` brings it back.

`khonsu plugin search [words]` lists every plugin with its version and declared
permissions, the version installed here, and whether a newer one is listed. A
marketplace that can't be reached is reported under `unreachable`, and the rest
are still listed.

`khonsu plugin install <id>` looks in every marketplace. When two list the same
ID, say which: `khonsu plugin install sql@official`. A path to a folder or
`.zip` installs from there instead, as before.

`khonsu plugin update [id]` reinstalls plugins that came from a marketplace
when it lists a newer version. `--dry-run` only says what would change.

## What is checked

- The package must match the SHA-256 the marketplace lists. One that doesn't is
  not installed, and what was installed before stays.
- The package must hold the plugin and version that were listed.
- Everything a local install checks still applies: paths that leave the folder,
  symbolic links and oversized archives are refused.
- Indexes and packages are read over HTTPS, never plain HTTP.

Installing is not approving. A plugin from a marketplace needs approval before
it runs, and an updated one needs it again, because its files changed. `--yes`
approves in the same step. The marketplace's `permissions` are for reading
before you download: approval goes by the installed manifest.

A marketplace says where a package is and what its hash is. It does not make
the plugin safe: see [plugin security](plugin-security.md).

## Publishing

```sh
khonsu plugin publish ./my-plugin --to ~/work/team-plugins
```

This packs the plugin into the marketplace folder and lists it in that folder's
`marketplace.json`, which is made if it isn't there. Then share the folder:
commit and push it, or upload it to a static host. Others add it once with
`khonsu marketplace add`, and install by name.

- **The same version again** replaces that platform's package and keeps the
  others, so each platform can publish its own build of one version.
- **A new version** replaces the entry, and the old version's packages are
  deleted from the folder.
- **`--base-url https://…`** lists the package at that address instead of beside
  the index, for packages uploaded somewhere else, such as a release's assets.

`khonsu plugin pack ./my-plugin` makes the same archive without listing it, and
prints its SHA-256, to hand to someone directly.

### Platforms

A package's `target` is `<os>-<arch>` (`macos-aarch64`, `windows-x86_64`,
`linux-x86_64`) or `any`. A plugin whose `entry` is a program found on `PATH`,
such as `python3 plugin.py`, packs as `any`. One whose `entry` is a file in the
package packs for the machine it was packed on; pass `--target` to say
otherwise. Installing picks this machine's package, then `any`.

A compiled plugin's executable is usually built outside its folder. Add it with
`--bin`:

```sh
cargo build --release -p khonsu-plugin-foo
khonsu plugin publish plugins/foo --bin target/release/khonsu-plugin-foo --to dist
```

The official marketplace does this in CI instead: its repository holds plugin
source, each plugin whose version has no release yet is built and attached to a
release, and the index is updated to point there. Its
[README](https://github.com/JetSquirrel/khonsugraph-marketplace#releasing-a-plugin)
describes it, and is a pattern to copy for a marketplace of compiled plugins.

### What goes in a package

Everything in the folder except `.git`, `.DS_Store`, `__pycache__` and `.pyc`
files, and whatever `.khonsuignore` names:

```text
# .khonsuignore: one path inside the folder, or *.ext, per line
src/
Cargo.toml
*.log
```

### The index

```json
{
  "name": "team-plugins",
  "plugins": [{
    "id": "folders",
    "name": "Folders",
    "version": "0.2.0",
    "description": "A folder tree as Folder and File nodes joined by CONTAINS",
    "permissions": {"read_files": true, "commands": ["python3"]},
    "packages": [
      {"target": "any", "url": "folders-0.2.0-any.zip", "sha256": "7f2f…", "size": 1821}
    ]
  }]
}
```

`url` is a path inside the marketplace or an HTTPS address. `khonsu plugin
publish` writes all of this. A marketplace folder also gains a
`marketplace.json.lock` file as it is published into, which need not be shared.

## Network access

KhonsuGraph contacts a marketplace only when you run `plugin search`, `plugin
install <id>`, `plugin update` or `marketplace add`, and only the addresses of
the marketplaces you have. Nothing is fetched in the background.
