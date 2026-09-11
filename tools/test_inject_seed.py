"""Regression tests for inject_seed.py.

The injector edits a user's `pumpkin.toml`, so the failure mode that matters is
not "it crashes" but "it silently rewrites something else". These tests pin the
three properties that guarantee it does not:

  1. a neighbouring table's keys are never absorbed into the injected table;
  2. re-running is idempotent;
  3. a block already present is replaced in place, not duplicated.

Run: python tools/test_inject_seed.py
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from inject_seed import build_block, inject  # noqa: E402

PLUGIN = "confluxmap-pumpkin"

BASE = """seed = "123456789"
[plugins]
allow_unsigned = true
ask_permission_confirmation = false

[networking]
port = 25565
"""


def block(seed: str) -> str:
    return build_block(PLUGIN, {"CFM_SEED": seed})


def test_does_not_absorb_the_neighbour_keys():
    out, action = inject(BASE, PLUGIN, block("123456789"))
    # The whole point: `[plugins]` keeps its own keys.
    plugins_table = out.split("[plugins]")[1].split("[")[0]
    assert "allow_unsigned = true" in plugins_table, out
    assert "ask_permission_confirmation = false" in plugins_table, out
    # ...and the injected table holds only the injected value.
    env_table = out.split(f"[plugins.overrides.{PLUGIN}.environment]")[1].split("[")[0]
    assert env_table.strip() == 'CFM_SEED = "123456789"', out
    assert action == "added", action

    # A table that came after `[plugins]` is untouched too.
    networking_table = out.split("[networking]")[1]
    assert "port = 25565" in networking_table, out


def test_rerunning_is_idempotent():
    once, _ = inject(BASE, PLUGIN, block("123456789"))
    twice, action = inject(once, PLUGIN, block("123456789"))
    assert once == twice, f"not idempotent:\n--- once ---\n{once}\n--- twice ---\n{twice}"
    assert action == "updated", action
    assert twice.count(f"[plugins.overrides.{PLUGIN}]") == 1, twice


def test_updates_the_seed_in_place():
    once, _ = inject(BASE, PLUGIN, block("123456789"))
    twice, _ = inject(once, PLUGIN, block("987654321"))
    # Only the override block changes; the top-level `seed` line is the operator's
    # (and is in fact where the injected value is read from).
    assert once.split("\n")[0] == twice.split("\n")[0], (once, twice)
    env_table = twice.split(f"[plugins.overrides.{PLUGIN}.environment]")[1].split("[")[0]
    assert env_table.strip() == 'CFM_SEED = "987654321"', twice


def test_removes_the_environment_table_it_previously_wrote():
    # `--no-share` adds CFM_SHARE_SEED; dropping the flag must drop the key
    # again rather than leaving a stale one behind.
    once, _ = inject(BASE, PLUGIN, build_block(PLUGIN, {"CFM_SEED": "1", "CFM_SHARE_SEED": "false"}))
    twice, _ = inject(once, PLUGIN, block("1"))
    assert "CFM_SHARE_SEED" not in twice, twice


def test_works_on_a_file_without_a_plugins_table():
    src, action = inject('seed = "123456789"\n', PLUGIN, block("123456789"))
    assert action == "added", action
    assert 'seed = "123456789"' in src, src
    assert 'CFM_SEED = "123456789"' in src, src


def test_result_round_trips_through_a_toml_parser():
    """If the output is not valid TOML the server will not start at all."""
    try:
        import tomllib
    except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
        return
    out, _ = inject(BASE, PLUGIN, block("123456789"))
    parsed = tomllib.loads(out)
    assert parsed["seed"] == "123456789", parsed
    assert parsed["plugins"]["allow_unsigned"] is True, parsed
    assert parsed["networking"]["port"] == 25565, parsed
    overrides = parsed["plugins"]["overrides"][PLUGIN]["environment"]
    assert overrides == {"CFM_SEED": "123456789"}, parsed


def main() -> int:
    tests = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    failures = 0
    for test in tests:
        try:
            test()
        except AssertionError as exc:
            failures += 1
            print(f"FAIL {test.__name__}: {exc}")
        else:
            print(f"ok   {test.__name__}")
    print(f"\n{len(tests) - failures}/{len(tests)} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
