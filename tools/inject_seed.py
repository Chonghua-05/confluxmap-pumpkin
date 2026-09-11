"""Inject the confluxmap plugin's seed into pumpkin.toml (idempotent).

Pumpkin's plugin API exposes no world-seed accessor, and the WASI sandbox denies
reads of `pumpkin.toml`, so the seed has to be handed to the plugin from outside
the sandbox. Pumpkin supports per-plugin environment overrides, and this script
writes one:

    [plugins.overrides.confluxmap-pumpkin]
    [plugins.overrides.confluxmap-pumpkin.environment]
    CFM_SEED = "<the server's own top-level seed>"

The seed is copied from the top-level `seed = "..."` line, so the injected value
can never drift from the seed the server is actually running.

`CFM_WORLDGEN` is deliberately *not* written by default: the plugin derives the
worldgen version from the running server's own `pumpkin-version` string, which
cannot go stale when the server is upgraded. Pass `--worldgen VER` to pin it
explicitly anyway (useful for a fork whose version string is not parseable).

Usage:
    python inject_seed.py /path/to/pumpkin.toml
    python inject_seed.py pumpkin.toml --worldgen 1.21.4 --dims minecraft:overworld
    python inject_seed.py pumpkin.toml --no-share        # advertise seedGranted=0
"""

import argparse
import re
import sys

DEFAULT_PLUGIN = "confluxmap-pumpkin"


def strip_override(src: str, plugin: str) -> tuple[str, bool]:
    """Removes every line belonging to the plugin's override tables.

    Line-based rather than regex-based on purpose: a TOML table owns every
    following line until the next header, so a pattern that guesses the boundary
    wrong either strands the plugin's keys inside a neighbouring table or
    swallows the neighbour's keys.
    """
    headers = {
        f"[plugins.overrides.{plugin}]",
        f"[plugins.overrides.{plugin}.environment]",
    }
    kept: list[str] = []
    dropping = False
    found = False
    for line in src.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("["):
            # A new table header: it is either ours (drop it and its keys) or the
            # end of ours (keep everything from here on).
            dropping = stripped in headers
            found = found or dropping
            if dropping:
                continue
        if not dropping:
            kept.append(line)
    return "".join(kept), found


def inject(src: str, plugin: str, block: str) -> tuple[str, str]:
    """Replaces the plugin's override block with `block`, appended as a new table.

    Appending is deliberate. Inserting next to a `[plugins]` header - the obvious
    alternative - silently moves the keys that follow that header into the new
    sub-table, quietly changing unrelated server settings.
    """
    body = strip_override(src, plugin)[0].rstrip("\n")
    out = f"{body}\n\n{block}" if body else block
    return out, ("updated" if f"[plugins.overrides.{plugin}]" in src else "added")


def build_block(plugin: str, values: dict) -> str:
    lines = [f"[plugins.overrides.{plugin}]", f"[plugins.overrides.{plugin}.environment]"]
    lines += [f'{key} = "{value}"' for key, value in values.items()]
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("toml", help="path to pumpkin.toml")
    parser.add_argument("--plugin", default=DEFAULT_PLUGIN, help="overridden plugin name")
    parser.add_argument("--worldgen", default=None, help="pin CFM_WORLDGEN explicitly")
    parser.add_argument("--dims", default=None, help="CFM_DIMS, comma-separated dimension ids")
    parser.add_argument(
        "--no-share",
        action="store_true",
        help="write CFM_SHARE_SEED=false (clients get seedGranted=0)",
    )
    args = parser.parse_args()

    with open(args.toml, encoding="utf-8") as handle:
        src = handle.read()

    seed_match = re.search(r'^seed\s*=\s*"([^"]+)"', src, re.M)
    if not seed_match:
        print("ERROR: no top-level `seed = \"...\"` field in pumpkin.toml")
        return 1

    values = {"CFM_SEED": seed_match.group(1)}
    if args.worldgen:
        values["CFM_WORLDGEN"] = args.worldgen
    if args.dims:
        values["CFM_DIMS"] = args.dims
    if args.no_share:
        values["CFM_SHARE_SEED"] = "false"

    block = build_block(args.plugin, values)
    new, action = inject(src, args.plugin, block)

    with open(args.toml, "w", encoding="utf-8") as handle:
        handle.write(new)

    print(f"{action} seed={values['CFM_SEED']} for plugin {args.plugin}")
    if "CFM_WORLDGEN" not in values:
        print("CFM_WORLDGEN not set: the plugin will derive it from the server version")
    print()
    start = new.find(f"[plugins.overrides.{args.plugin}]")
    print(new[start : start + 400])
    return 0


if __name__ == "__main__":
    sys.exit(main())
