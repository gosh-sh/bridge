# tools/ — legacy override target

The `tools/` directory used to be the single place where all five
acki-nacki tool binaries lived, regardless of OS. That has been
replaced by per-OS folders under `bridge/multithreading/`:

- **macOS:** [`../bins_macOS/`](../bins_macOS/) (with `env.sh`)
- **Linux:** `../bins_linux/` (populate on the target box)

`runbooks/run_multipath.sh` picks the right one automatically via
`uname -s`. See [`../bins_macOS/README.md`](../bins_macOS/README.md)
for the full recipe.

`tools/` still works as a `TOOLS_DIR` override if you want to point
the wrapper at some other location:

```bash
TOOLS_DIR=/path/to/bridge/multithreading/tools ./runbooks/run_multipath.sh
```

Nothing in `tools/` is tracked in git.
