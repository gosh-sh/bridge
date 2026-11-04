# Deprecated: the systemd units of the ursus host

These unit files and env templates ran the relayers as systemd services with
binaries built on the host. Since bridge-audit#132 and #135 the services run from
images with `docker/compose.yaml` (see `docker/README.md`), and these files are not
updated for the flags that changed with it (`--state-dir`, `--work-dir`, `--log-dir`;
no `*_ABI_PATH`). They stay for the runbooks that still cite them and go in a later
documentation change.
