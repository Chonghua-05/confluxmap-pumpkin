"""Set the seed in the plugin's own configuration file (idempotent).

Pumpkin gives a plugin no way to read the world seed: the plugin API has no seed
accessor, and the WASI sandbox preopens only the plugin's private data folder. The
seed therefore lives in

    plugins/data/confluxmap-pumpkin/config.toml

which the plugin writes (annotated) the first time it loads. This script copies
the seed there from the server's own top-level `seed`, so the two cannot drift
apart - a mismatch renders a wrong map on every client and nothing in the log
says so.

Only the line for each key being set is rewritten, so the plugin's comments and
every other setting survive verbatim. Rewriting works on the commented-out
`# seed = 0` the plugin writes as well: that is the line a hand-edit would
uncomment.

`worldgen` is deliberately not written by default: the plugin derives the
worldgen version from the running server's own `pumpkin-version` string, which
cannot go stale when the server is upgraded. Pass `--worldgen VER` to pin it
explicitly anyway (useful for a fork whose version string is not parseable).

Usage:
    python inject_seed.py /path/to/pumpkin.toml
    python inject_seed.py pumpkin.toml --worldgen 1.21.4 --dims minecraft:overworld
    python inject_seed.py pumpkin.toml --no-share        # advertise seedGranted=0
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

DEFAULT_PLUGIN = "confluxmap-pumpkin"
CONFIG_FILE = "config.toml"


def read_seed(toml_text: str) -> str | None:
    """The top-level `seed` of pumpkin.toml, quoted or bare."""
    match = re.search(
        r'^seed\s*=\s*"?(-?[0-9][0-9_]*)"?\s*(?:#.*)?$', toml_text, re.MULTILINE
    )
    return match.group(1) if match else None


def config_path(pumpkin_toml: Path, plugin: str) -> Path:
    """The plugin's config, next to pumpkin.toml, where Pumpkin puts plugin data."""
    return pumpkin_toml.resolve().parent / "plugins" / "data" / plugin / CONFIG_FILE


def line_ending(line: str) -> str:
    """The line's own terminator, so a CRLF file stays CRLF throughout."""
    stripped = line.rstrip("\r\n")
    return line[len(stripped) :] or "\n"


def set_key(lines: list[str], key: str, value: str) -> bool:
    """Rewrites the line defining `key`, commented out or not.

    Returns False when the file does not mention the key at all, leaving the
    caller to decide where to append it.
    """
    pattern = re.compile(rf"^(\s*)#?\s*{re.escape(key)}\s*=")
    for index, line in enumerate(lines):
        match = pattern.match(line)
        if match:
            lines[index] = f"{match.group(1)}{key} = {value}{line_ending(line)}"
            return True
    return False


def apply(path: Path, values: dict[str, str]) -> list[str]:
    """Applies `values` to the file at `path`; returns the keys it had to append.

    Both ends bypass newline translation so the file's own line endings survive a
    round trip - a CRLF config stays CRLF even when the script runs on Windows.
    """
    with path.open("r", encoding="utf-8", newline="") as handle:
        lines = handle.read().splitlines(keepends=True)

    ending = line_ending(lines[-1]) if lines else "\n"
    appended = []
    for key, value in values.items():
        if not set_key(lines, key, value):
            appended.append(key)
            lines.append(f"{key} = {value}{ending}")

    with path.open("w", encoding="utf-8", newline="") as handle:
        handle.write("".join(lines))
    return appended


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("toml", help="path to pumpkin.toml")
    parser.add_argument("--plugin", default=DEFAULT_PLUGIN, help="plugin name, i.e. its data folder")
    parser.add_argument("--worldgen", default=None, help="pin `worldgen` explicitly")
    parser.add_argument("--dims", default=None, help="`dims`, comma-separated dimension ids")
    parser.add_argument(
        "--no-share",
        action="store_true",
        help="write share_seed = false (clients get seedGranted=0)",
    )
    args = parser.parse_args()

    pumpkin_toml = Path(args.toml)
    seed = read_seed(pumpkin_toml.read_text(encoding="utf-8"))
    if seed is None:
        print('ERROR: no top-level `seed = "..."` field in pumpkin.toml')
        return 1

    path = config_path(pumpkin_toml, args.plugin)
    if not path.exists():
        print(f"ERROR: {path} does not exist.")
        print("Start the server once so the plugin writes its configuration, then run this again.")
        return 1

    values = {"seed": seed}
    if args.no_share:
        values["share_seed"] = "false"
    if args.worldgen:
        values["worldgen"] = f'"{args.worldgen}"'
    if args.dims:
        values["dims"] = f'"{args.dims}"'

    appended = apply(path, values)
    print(f"set {', '.join(f'{k} = {v}' for k, v in values.items())}")
    print(f"in  {path}")
    if appended:
        print(f"note: appended {', '.join(appended)} (not present in the file)")
    print("Run `/cfm reload` in the server console, or restart, to apply it.")
    if "worldgen" not in values:
        print("worldgen left unset: the plugin derives it from the server version")
    return 0


if __name__ == "__main__":
    sys.exit(main())
