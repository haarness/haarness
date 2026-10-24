# Bundled ripgrep

Haarness embeds upstream ripgrep 15.2.0 in its single executable. Agent shell tools
and integrated terminals prepend its verified cache directory to their child
process PATH. No installer, Homebrew, global PATH change, or runtime download is
required. `haarness doctor` extracts, verifies, and executes the bundled version.
The existing `grep` and `glob` tools keep their current structured interfaces.

The six payloads cover macOS, Windows, and Linux on arm64 and x86_64. Linux uses
upstream static musl binaries for both GNU and musl targets. Build scripts select
only the current target's payload and verify the compressed and executable SHA256.

`manifest.json` pins the official release URLs, upstream archive SHA256 values,
and compressed/extracted payload hashes. Upstream archives were checked against
the digests published on https://github.com/BurntSushi/ripgrep/releases/tag/15.2.0.
Payloads are unmodified upstream executables, gzip compressed with timestamp zero.
`COPYING`, `LICENSE-MIT`, and `UNLICENSE` are the upstream notices, also embedded
and extracted beside `rg` and copied into desktop resources.

Run `python3 vendor/ripgrep/verify.py` to check all six vendored payloads offline.
`--fetch` downloads the pinned upstream archives, verifies them, and reproduces
the payloads (requires a compatible zlib compressor). To upgrade, review upstream
releases and their published digests, regenerate the manifest/payloads/notices,
and run crate, CLI/server, and packaged desktop tests before landing.

The executable cache is below `$HAARNESS_HOME/cache`, `$XDG_CACHE_HOME/haarness`,
or `~/.cache/haarness`, in a version/target/content-hash directory. First use is
locked across processes and published atomically. Missing, corrupt, or
non-executable cached files repair themselves; executable symlinks are rejected.
When the cache cannot be written (a read-only home, a relative `HAARNESS_HOME`,
a link in the cache), shell calls and terminals still start, with PATH as it is,
so they find any `rg` installed there; the log says so once per process, and
each later call tries the cache again. `haarness doctor` reports the failure.
Shell tool setup restores PATH after login profiles, so their PATH resets do not
hide the bundled binary. Interactive terminals inherit PATH; user shell startup
configuration can still intentionally override that environment.
