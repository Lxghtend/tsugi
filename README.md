# tsugi (次)

Switch between declarative desktop profiles in Niri 26.04. `tsugi` reuses matching windows, starts missing apps, and arranges the resulting columns and stacks. Switching does not require a daemon. After a successful switch, each monitor shows its topmost workspace in Niri’s current order, and keyboard focus returns to the monitor you started on if it’s still connected.

## Get started

```sh
nix develop
cargo test --locked
nix build
mkdir -p ~/.config/tsugi/profiles
cp -n profiles/*.toml ~/.config/tsugi/profiles/
```

If you set `XDG_CONFIG_HOME`, use that instead of `~/.config`. Before switching, declare the named workspaces used by your profiles in Niri's KDL configuration. For the sample profiles:

```kdl
workspace "web"
workspace "dev"
workspace "games"
workspace "_stash"
```

```sh
./result/bin/tsugi list
./result/bin/tsugi switch default
./result/bin/tsugi --timeout 30 developing
./result/bin/tsugi status
./result/bin/tsugi save current
./result/bin/tsugi save demo --ignore-focused-window
```

Run inside the compositor session (`NIRI_SOCKET` and `XDG_RUNTIME_DIR` must be set for switching). `status` reports the last successful switch for this session, not a live layout comparison. `TSUGI_LOG=debug tsugi daemon` monitors Niri events in the foreground; it does not enforce a profile continuously.

## Profiles

See [`profiles/developing.toml`](profiles/developing.toml) for a tabbed stack. Array order is left to right for columns and top to bottom for windows. Widths may be percentages or positive logical pixels. The optional workspace `output` must identify a connected output. `app_id` is a full-string regex (escape metacharacters for literal IDs); `title_match` is an optional regex search on the title. `command` is an argument vector, not a shell string. `cwd` supports `~` and `~/…`.

If `unmatched_policy` is omitted, it defaults to `close`: tsugi asks Niri to close every unmatched window. Set `ignore` to leave unmatched windows where they are, or `stash` to move them to `stash_workspace` (default `_stash`). Hooks are trusted shell code: `on_exit` runs before leaving the previous profile and `on_enter` after placement. Failures are reported but switching is not transactional. The timeout applies to individual waits, not the whole switch. Spawn matching is based on app ID and title; avoid opening indistinguishable windows during a switch.

`save` only snapshots tiled windows on named workspaces, excluding `_stash`. Niri exposes tile geometry but not configured width proportions or tab display mode, so saved widths are approximate pixels and tab mode is omitted. Review saved commands before replaying or sharing them: `/proc` command lines can contain private arguments, and some apps need a launcher instead of the raw executable invocation. Vesktop uses its launcher when available on PATH. Other windows without a resolvable captured executable are skipped with a warning. Snapshots never overwrite an existing profile.

## Home Manager

Import `inputs.tsugi.homeManagerModules.default` and set:

```nix
programs.tsugi = {
  enable = true;
  profiles.default = builtins.fromTOML (builtins.readFile ./default.toml);
};
```

The module installs the package and generates the TOML files under `xdg.configHome`. Keep snapshots under distinct names from managed profiles.
