#!/usr/bin/env bash
# build_partner_pack.sh — assemble a tagged zip bundle of docs/code for a
# partner (Alina, Serhii, …) without giving them GitLab access.
#
# Usage:
#     scripts/build_partner_pack.sh <pack-name> [--manifest <path>]
#
# Inputs:
#     <pack-name>         Slug used in the zip filename. Conventionally
#                         <topic>_for_<recipient>, e.g. circuit4_for_alina,
#                         halo2_tvm_for_serhii. The pattern /*_for_*.zip is
#                         auto-ignored by .gitignore, so the output never
#                         accidentally gets committed.
#     --manifest <path>   Optional path to a manifest file listing the
#                         repo-relative paths to include, one per line.
#                         Lines starting with `#` are comments.
#                         Lines of the form `src => dest` rename `src` to
#                         `dest` inside the pack (handy for ordering files
#                         like `01_design_memo.md`).
#                         Default: scripts/partner_packs/<pack-name>.manifest
#
# Behaviour:
#     - Stages files under /tmp/<pack-name>/ preserving the manifest order.
#     - Renders a README.md prepended with provenance (repo URL, commit
#       SHA, date) followed by the manifest's documentation footer (after
#       a line containing only `---` in the manifest, if present).
#     - Produces both a .zip and a .tar.gz (operators on Windows / non-UNIX
#       machines tend to ask for one or the other).
#     - Emits SHA-256 next to each artefact.
#     - Copies the artefacts to the repo root for easy attachment in
#       messengers. Cleans up the staging directory afterwards.
#
# Why this script exists:
#     The original delivery of `alina_review_pack_2026-05-18` and the
#     2026-05-21 `circuit4_for_alina` / `halo2_tvm_for_serhii` packs were
#     all assembled by hand, with consistency drift on README format and
#     SHA placement. This script captures the recipe so the next pack
#     takes ~30 seconds instead of ~30 minutes, and so all packs share
#     the same provenance header that lets a recipient compute exactly
#     which `git log` revision they're looking at.

set -euo pipefail

if [[ $# -lt 1 ]]; then
    cat >&2 <<EOF
Usage: $0 <pack-name> [--manifest <path>]

See header of $0 for full docs.

Example manifest (scripts/partner_packs/circuit4_for_alina.manifest):

    # Files to include (repo-relative paths). Optional 'src => dest' rename.
    docs/an_partner_questions_circuit4_2026-05-17.md
    docs/an_partner_circuit4_concept_response_2026-05-21.md
    docs/an_partner_circuit4_alina_replies_2026-05-21.md => 03_alina_replies_2026-05-21.md
    docs/an_partner_integration_plan.md

    ---
    ## Reading order
    Start with the questions file, then the replies, then the integration
    plan §3 Phase 8 + Phase 9 for ceremony detail.
EOF
    exit 2
fi

pack_name="$1"
shift

manifest_path="scripts/partner_packs/${pack_name}.manifest"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --manifest)
            manifest_path="$2"
            shift 2
            ;;
        *)
            echo "Unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if [[ ! -f "$manifest_path" ]]; then
    echo "Manifest not found: $manifest_path" >&2
    echo "Hint: copy scripts/partner_packs/_template.manifest and edit." >&2
    exit 1
fi

today="$(date +%Y-%m-%d)"
pack_dir_name="${pack_name}_${today}"
stage_dir="/tmp/${pack_dir_name}"
zip_name="${pack_dir_name}.zip"
tar_name="${pack_dir_name}.tar.gz"

remote_url="$(git config --get remote.origin.url || echo '(no remote)')"
commit_sha="$(git rev-parse HEAD)"
short_sha="$(git rev-parse --short HEAD)"

# Stage files according to the manifest.
rm -rf "$stage_dir"
mkdir -p "$stage_dir"

# Split the manifest into a files-section (above `---`) and a footer
# (below `---`) for the README. The footer is free-form Markdown.
files_section="$(awk '/^---[[:space:]]*$/ { exit } { print }' "$manifest_path")"
footer_section="$(awk 'started { print } /^---[[:space:]]*$/ { started = 1 }' "$manifest_path")"

declare -a included_files
while IFS= read -r line; do
    # Skip blank lines and comments.
    [[ -z "${line// }" ]] && continue
    [[ "$line" =~ ^[[:space:]]*# ]] && continue

    if [[ "$line" =~ ^(.*[^[:space:]])[[:space:]]*=\>[[:space:]]*(.*[^[:space:]])$ ]]; then
        src="${BASH_REMATCH[1]}"
        dest="${BASH_REMATCH[2]}"
    else
        src="$line"
        dest="$(basename "$src")"
    fi

    if [[ ! -f "$src" ]]; then
        echo "Manifest references missing file: $src" >&2
        exit 1
    fi

    # Allow `dest` to contain subdirectories — auto-create the parent.
    dest_parent="$stage_dir/$(dirname "$dest")"
    mkdir -p "$dest_parent"
    cp "$src" "$stage_dir/$dest"
    included_files+=("$dest")
done <<< "$files_section"

# Build per-file table of contents for the README.
toc=""
for f in "${included_files[@]}"; do
    toc+="- \`$f\`"$'\n'
done

# Render README.md with provenance header + optional footer.
cat > "$stage_dir/README.md" <<EOF
# ${pack_name//_/ } pack ($today)

**Generated**: $today
**Source repo**: \`$remote_url\`
**Commit**: \`$commit_sha\`
**Short SHA**: \`$short_sha\`

This bundle was assembled by \`scripts/build_partner_pack.sh\` from the
\`acki-nacki-bridge\` GitLab repository. The contents below are
verbatim snapshots of files at the commit above; for the live
versions, see the repo at \`$remote_url\`.

## Contents

$toc
EOF

if [[ -n "${footer_section// }" ]]; then
    printf '\n%s\n' "$footer_section" >> "$stage_dir/README.md"
fi

# Produce per-file MANIFEST.sha256 inside the pack.
(cd "$stage_dir" && find . -type f ! -name 'MANIFEST.sha256' -print0 \
    | sort -z \
    | xargs -0 sha256sum > MANIFEST.sha256)

# Pack as zip + tar.gz from /tmp so the archive's top-level dir is named
# nicely (the pack_dir_name slug).
cd /tmp
rm -f "$zip_name" "$tar_name" "${zip_name}.sha256" "${tar_name}.sha256"
zip -r -q "$zip_name" "$pack_dir_name"
tar -czf "$tar_name" "$pack_dir_name"
sha256sum "$zip_name" > "${zip_name}.sha256"
sha256sum "$tar_name" > "${tar_name}.sha256"

# Copy artefacts to the repo root for easy attachment.
cp "$zip_name" "${zip_name}.sha256" "$tar_name" "${tar_name}.sha256" "$repo_root/"

# Tidy up the staging directory but keep the /tmp archives for re-use.
rm -rf "$stage_dir"

echo
echo "Pack assembled successfully:"
echo "  $repo_root/$zip_name"
echo "  $repo_root/$tar_name"
echo
echo "SHA-256:"
cat "$repo_root/${zip_name}.sha256"
cat "$repo_root/${tar_name}.sha256"
echo
echo "Both files match the /*_for_*.zip and /*_for_*.tar.gz gitignore"
echo "patterns and will NOT be tracked by git."
