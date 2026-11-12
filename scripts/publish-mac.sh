#!/usr/bin/env bash
# Adds the Mac app to this version's GitHub release and to its update feed.
# Runs on Codemagic after the build; needs GH_TOKEN with write access to the repo's contents.
set -euo pipefail

repo=Avalons89/claude-usage-tray
version=$(node -p 'require("./src-tauri/tauri.conf.json").version')
tag="v$version"
bundle=src-tauri/target/universal-apple-darwin/release/bundle
update="Claude.Usage_${version}_universal.app.tar.gz"
dmg="Claude.Usage_${version}_universal.dmg"
cp "$bundle/macos/Claude Usage.app.tar.gz" "$update"
cp "$bundle/macos/Claude Usage.app.tar.gz.sig" "$update.sig"
cp "$bundle"/dmg/*.dmg "$dmg"

# The contabo-build jobs may not have created the release yet.
gh release view "$tag" -R "$repo" >/dev/null 2>&1 ||
  gh release create "$tag" -R "$repo" --target "$CM_COMMIT" --title "Claude Usage $tag" \
    --notes "Installers for this build. Installed copies update themselves within the hour." ||
  true
gh release upload "$tag" -R "$repo" --clobber "$update" "$update.sig" "$dmg"

# ponytail: read-modify-write; if a contabo-build job rewrites latest.json in the same few seconds,
# one side's entries are lost for this version and the next release puts them back.
gh release download "$tag" -R "$repo" -p latest.json -O feed.json --clobber 2>/dev/null || rm -f feed.json
node scripts/add-mac-to-feed.mjs feed.json "$version" \
  "https://github.com/$repo/releases/download/$tag/$update" "$(cat "$update.sig")" >latest.json
gh release upload "$tag" -R "$repo" --clobber latest.json
