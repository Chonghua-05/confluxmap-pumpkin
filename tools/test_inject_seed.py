"""Regression tests for inject_seed.py.

The injector edits a config file that a human also edits by hand, so the failure
mode that matters is not "it crashes" but "it silently rewrites something else" -
or "it forgets that the plugin wrote the key commented out". These tests pin the
properties that guarantee neither happens:

  1. the commented-out `# seed = 0` the plugin writes is the line that gets set;
  2. every other line, comments included, comes back byte for byte;
  3. re-running is idempotent, and a seed already set is replaced, not duplicated;
  4. the result is still valid TOML.

Run: python tools/test_inject_seed.py
"""

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from inject_seed import apply, config_path, main as inject_main, read_seed, set_key  # noqa: E402

PLUGIN = "confluxmap-pumpkin"

# The shape the plugin writes: every key commented out, with prose around it.
TEMPLATE = """# confluxmap-pumpkin configuration.
#
# The seed must match `seed` in pumpkin.toml.

# The world seed. Uncomment and fill in.
# seed = 0

# Default: true.
# share_seed = true

# e.g. "1.21.4"
# worldgen = ""
"""

PUMPKIN_TOML = """# the server's own seed
seed = "81985529216486895"

[plugins]
allow_unsigned = true
"""


def with_file(text: str):
    """Runs `apply` over `text` in a temp file and returns (result, appended)."""
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "config.toml"
        with path.open("w", encoding="utf-8", newline="") as handle:
            handle.write(text)
        appended = apply(path, {"seed": "12345"})
        with path.open("r", encoding="utf-8", newline="") as handle:
            return handle.read(), appended


def test_sets_the_commented_out_line_the_plugin_writes():
    out, appended = with_file(TEMPLATE)
    assert "seed = 12345" in out, out
    assert "# seed = 0" not in out, out
    assert appended == [], appended


def test_leaves_every_other_line_alone():
    out, _ = with_file(TEMPLATE)
    expected = TEMPLATE.replace("# seed = 0", "seed = 12345")
    assert out == expected, f"--- got ---\n{out}\n--- want ---\n{expected}"


def test_keeps_the_comment_sitting_above_the_key():
    out, _ = with_file(TEMPLATE)
    assert "# The world seed. Uncomment and fill in." in out, out


def test_replaces_a_seed_already_set():
    out, appended = with_file("seed = 1\nshare_seed = true\n")
    assert out == "seed = 12345\nshare_seed = true\n", out
    assert appended == [], appended


def test_is_idempotent():
    once, _ = with_file("seed = 1\n")
    twice, _ = with_file(once)
    assert once == twice, (once, twice)
    assert once.count("seed =") == 1, once


def test_appends_a_key_the_file_does_not_mention():
    out, appended = with_file("# nothing here\n")
    assert out.endswith("seed = 12345\n"), out
    assert appended == ["seed"], appended


def test_only_the_requested_key_is_touched():
    out, _ = with_file(TEMPLATE)
    assert '# worldgen = ""' in out, out
    assert "# share_seed = true" in out, out


def test_indented_keys_keep_their_indentation():
    lines = ["    # seed = 0\n"]
    assert set_key(lines, "seed", "7")
    assert lines[0] == "    seed = 7\n", lines


def test_crlf_files_stay_crlf():
    out, _ = with_file("# seed = 0\r\nshare_seed = true\r\n")
    assert out == "seed = 12345\r\nshare_seed = true\r\n", repr(out)


def test_a_similar_key_is_not_mistaken_for_the_target():
    # `worlds =` must not be rewritten by a request for `world` or `seed`.
    lines = ["worlds = 3\n"]
    assert not set_key(lines, "seed", "1")
    assert lines == ["worlds = 3\n"], lines


def test_reads_a_quoted_seed():
    assert read_seed(PUMPKIN_TOML) == "81985529216486895"


def test_reads_a_bare_seed():
    assert read_seed("seed = -42\n") == "-42"


def test_ignores_a_commented_out_seed():
    assert read_seed("# seed = 1\n") is None


def test_reports_a_missing_seed():
    assert read_seed("[plugins]\nallow_unsigned = true\n") is None


def test_finds_the_plugin_config_next_to_pumpkin_toml():
    path = config_path(Path("somewhere") / "server" / "pumpkin.toml", PLUGIN)
    assert path.parts[-4:] == ("plugins", "data", PLUGIN, "config.toml"), path


def test_result_round_trips_through_a_toml_parser():
    """The plugin's parser is lenient, but standard tooling must read it too."""
    try:
        import tomllib
    except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
        return
    # The annotated file: only the seed line is uncommented, so the keys the
    # plugin would fall back to must simply be absent rather than malformed.
    parsed = tomllib.loads(with_file(TEMPLATE)[0])
    assert parsed["seed"] == 12345, parsed
    assert "share_seed" not in parsed, parsed

    parsed = tomllib.loads(with_file("seed = 0\nshare_seed = false\ndims = \"a,b\"\n")[0])
    assert parsed == {"seed": 12345, "share_seed": False, "dims": "a,b"}, parsed


def rust_template() -> str:
    """The template the plugin writes, read out of `src/config.rs`.

    The tool has to recognise the lines the plugin emits, so the two definitions
    are coupled by design; reading the real one here is what keeps the coupling
    honest instead of testing a copy that can drift.
    """
    source = (
        Path(__file__).resolve().parent.parent / "src" / "config.rs"
    ).read_text(encoding="utf-8")
    start = source.index('pub const TEMPLATE: &str = "')
    body = source[start:].split('"', 1)[1]
    end = body.index('\n";')
    literal = body[: end + 1]
    # A leading backslash-newline is Rust's line continuation; `\"` is a quote.
    if literal.startswith("\\\n"):
        literal = literal[2:]
    return literal.replace('\\"', '"').replace("\\\\", "\\")


def test_the_plugin_template_is_the_shape_this_tool_expects():
    template = rust_template()
    assert "# seed = 0" in template, template
    out, appended = with_file(template)
    assert "seed = 12345" in out, out
    assert "# seed = 0" not in out, out
    assert appended == [], appended


def test_main_sets_the_seed_end_to_end():
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        (root / "pumpkin.toml").write_text(PUMPKIN_TOML, encoding="utf-8")
        config = config_path(root / "pumpkin.toml", PLUGIN)
        config.parent.mkdir(parents=True)
        config.write_text(rust_template(), encoding="utf-8")

        argv = sys.argv
        sys.argv = ["inject_seed.py", str(root / "pumpkin.toml"), "--no-share"]
        try:
            assert inject_main() == 0
        finally:
            sys.argv = argv

        written = config.read_text(encoding="utf-8")
        assert "seed = 81985529216486895" in written, written
        assert "share_seed = false" in written, written


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
