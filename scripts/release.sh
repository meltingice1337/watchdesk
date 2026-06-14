#!/usr/bin/env bash
#
# Interactive release tool for WatchDesk (run from git-bash).
#
#   ./scripts/release.sh                 # interactive: pick patch/minor/major, review, confirm
#   ./scripts/release.sh patch           # preselect the bump, still shows the plan + confirm
#   ./scripts/release.sh 1.4.0           # explicit version
#   ./scripts/release.sh minor --dry-run # print the plan and stop (no build/commit/push)
#   ./scripts/release.sh patch --yes     # skip the final confirmation
#   ./scripts/release.sh patch --no-push # build, commit, tag locally; don't push or publish
#
# It bumps Cargo.toml, generates a grouped changelog from the last tag to HEAD,
# builds the release, packages a .zip, updates CHANGELOG.md, commits, tags, pushes,
# and creates a GitHub release (via gh if installed, otherwise prints manual steps).
# No `set -e`: it silently kills interactive scripts (e.g. a false `[ ]` test).
# We check the operations that matter explicitly via `die` instead.
set -uo pipefail
die() { echo "error: $*" >&2; exit 1; }

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || die "cannot cd to $repo_root"
cargo_path="$repo_root/Cargo.toml"
target="x86_64-pc-windows-msvc"
build_dir="$repo_root/target/release-build"   # isolated so a running service never locks the build
exe="$build_dir/$target/release/watchdesk.exe"
dist="$repo_root/target/dist"
bsdtar="/c/Windows/System32/tar.exe"   # ships zip support on Windows 10/11

# ---- args ----
bump=""; dry_run=0; assume_yes=0; no_push=0
for a in "$@"; do
  case "$a" in
    major|minor|patch)       bump="$a" ;;
    [0-9]*.[0-9]*.[0-9]*)    bump="$a" ;;
    --dry-run|-n)            dry_run=1 ;;
    --yes|-y)                assume_yes=1 ;;
    --no-push)               no_push=1 ;;
    -h|--help)               grep '^#' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $a (see --help)" >&2; exit 1 ;;
  esac
done

command -v cargo >/dev/null 2>&1 || die "cargo not found on PATH"
command -v git   >/dev/null 2>&1 || die "git not found on PATH"

# ---- current version ----
current="$(grep -E '^version = "' "$cargo_path" | head -1 | sed -E 's/^version = "([^"]+)".*/\1/')"
[ -n "$current" ] || die "could not read version from Cargo.toml"
IFS='.' read -r MA MI PA <<< "$current"

# ---- choose the new version ----
if [ -z "$bump" ]; then
  pa="$MA.$MI.$((PA + 1))"; mi="$MA.$((MI + 1)).0"; ma="$((MA + 1)).0.0"
  echo "Current version: $current"
  PS3="Choose release type: "
  select _opt in "patch  -> $pa" "minor  -> $mi" "major  -> $ma" "custom" "cancel"; do
    case "${REPLY:-}" in
      1) new="$pa"; break ;;
      2) new="$mi"; break ;;
      3) new="$ma"; break ;;
      4) read -rp "Enter version (x.y.z): " new; break ;;
      5) echo "Cancelled."; exit 0 ;;
      *) echo "Invalid choice." ;;
    esac
  done
else
  case "$bump" in
    major) new="$((MA + 1)).0.0" ;;
    minor) new="$MA.$((MI + 1)).0" ;;
    patch) new="$MA.$MI.$((PA + 1))" ;;
    *)     new="$bump" ;;
  esac
fi
[[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version: $new"
tag="v$new"

# ---- repo url + previous tag ----
remote="$(git remote get-url origin 2>/dev/null || true)"
repo_url="https://github.com/meltingice1337/watchdesk"
if [[ "$remote" =~ ^git@github\.com:(.+)$ ]]; then
  repo_url="https://github.com/${BASH_REMATCH[1]%.git}"
elif [[ "$remote" =~ ^https?://github\.com/(.+)$ ]]; then
  repo_url="https://github.com/${BASH_REMATCH[1]%.git}"
fi
prev_tag="$(git describe --tags --abbrev=0 2>/dev/null || true)"

# ---- collect + group commits ----
log_args=(log --no-merges --pretty=format:'%h%x1f%s')
[ -n "$prev_tag" ] && log_args+=("$prev_tag..HEAD")
US=$'\x1f'
# Keep the regex in a variable: bash's [[ =~ ]] lexer mis-parses ')' inside the pattern.
conv_re='^([a-zA-Z]+)(\([^)]*\))?!?:[[:space:]]*(.+)$'
feats=""; fixes=""; perfs=""; refas=""; docsx=""; maint=""; others=""
while IFS= read -r line; do
  [ -n "$line" ] || continue
  hash="${line%%${US}*}"
  subj="${line#*${US}}"
  if [[ "$subj" =~ $conv_re ]]; then
    type="$(printf '%s' "${BASH_REMATCH[1]}" | tr '[:upper:]' '[:lower:]')"
    desc="${BASH_REMATCH[3]}"
  else
    type=""; desc="$subj"
  fi
  desc="$(printf '%s' "${desc:0:1}" | tr '[:lower:]' '[:upper:]')${desc:1}"
  entry="- $desc ([$hash]($repo_url/commit/$hash))"$'\n'
  case "$type" in
    feat)                         feats+="$entry" ;;
    fix)                          fixes+="$entry" ;;
    perf)                         perfs+="$entry" ;;
    refactor)                     refas+="$entry" ;;
    docs)                         docsx+="$entry" ;;
    chore|build|ci|test|style)    maint+="$entry" ;;
    *)                            others+="$entry" ;;
  esac
done < <(git "${log_args[@]}")

build_changelog() {
  printf '## %s - %s\n' "$tag" "$(date +%Y-%m-%d)"
  [ -n "$feats"  ] && printf '\n### Features\n%s'      "$feats"
  [ -n "$fixes"  ] && printf '\n### Bug Fixes\n%s'     "$fixes"
  [ -n "$perfs"  ] && printf '\n### Performance\n%s'   "$perfs"
  [ -n "$refas"  ] && printf '\n### Refactoring\n%s'   "$refas"
  [ -n "$docsx"  ] && printf '\n### Documentation\n%s' "$docsx"
  [ -n "$maint"  ] && printf '\n### Maintenance\n%s'   "$maint"
  [ -n "$others" ] && printf '\n### Other\n%s'         "$others"
  [ -n "$prev_tag" ] && printf '\n**Full changelog:** [%s...%s](%s/compare/%s...%s)\n' \
    "$prev_tag" "$tag" "$repo_url" "$prev_tag" "$tag"
  return 0
}
changelog="$(build_changelog)"

# ---- plan ----
push_line="origin HEAD + tag $tag"; [ "$no_push" = 1 ] && push_line="NO (--no-push)"
gh_line="manual (gh not installed)"; command -v gh >/dev/null 2>&1 && gh_line="via gh"
zip_path="$dist/watchdesk-$tag-$target.zip"

echo
echo "====================== Release plan ======================"
printf '  Version    : %s  ->  %s\n' "$current" "$new"
printf '  Tag        : %s\n' "$tag"
printf '  Prev tag   : %s\n' "${prev_tag:-<none, first release>}"
printf '  Build      : cargo build --release (%s)\n' "$target"
printf '  Package    : %s\n' "$zip_path"
printf '  Edits      : Cargo.toml, Cargo.lock, CHANGELOG.md\n'
printf '  Commit     : chore: release %s\n' "$tag"
printf '  Push       : %s\n' "$push_line"
printf '  GH release : %s\n' "$gh_line"
echo "----------------------------------------------------------"
printf '%s\n' "$changelog"
echo "=========================================================="

dirty="$(git status --porcelain)"
if [ -n "$dirty" ]; then
  echo
  echo "WARNING: working tree has uncommitted changes. They are NOT part of this"
  echo "release (only Cargo.toml/Cargo.lock/CHANGELOG.md get committed). Commit them"
  echo "first if they belong in $tag."
fi

if [ "$dry_run" = 1 ]; then echo; echo "Dry run - nothing built, committed, or pushed."; exit 0; fi

if [ "$assume_yes" != 1 ]; then
  echo
  read -rp "Proceed with release $tag? [y/N] " ans
  case "$ans" in y|Y|yes|YES) ;; *) echo "Aborted."; exit 0 ;; esac
fi

# ---- execute ----
echo "==> Bumping Cargo.toml to $new"
sed -i -E "s/^version = \"[0-9][^\"]*\"/version = \"$new\"/" "$cargo_path" || die "failed to edit Cargo.toml"

echo "==> Building release (isolated dir)"
cargo build --release --target-dir "$build_dir" || die "cargo build failed"
[ -f "$exe" ] || die "build did not produce $exe"

echo "==> Packaging $zip_path"
staging="$dist/watchdesk-$tag"
rm -rf "$staging"; mkdir -p "$staging"
cp "$exe" "$staging/watchdesk.exe"
cp "$repo_root/README.md" "$staging/"
cat > "$staging/config.example.toml" <<'EOF'
[mqtt]
host = "192.168.1.100"
port = 1883
# username = "user"
# password = "pass"

[device]
name = "My Desktop"
EOF
rm -f "$zip_path"
if command -v zip >/dev/null 2>&1; then
  ( cd "$staging" && zip -r -q "$zip_path" . ) || die "zip failed"
elif [ -x "$bsdtar" ]; then
  "$bsdtar" -a -c -f "$zip_path" -C "$staging" . || die "packaging (tar) failed"
else
  die "no zip tool available (install 'zip' or rely on Windows tar.exe)"
fi
notes_path="$dist/RELEASE_NOTES-$tag.md"
printf '%s\n' "$changelog" > "$notes_path"

echo "==> Updating CHANGELOG.md"
cl="$repo_root/CHANGELOG.md"
if [ -f "$cl" ]; then
  title="$(head -n1 "$cl")"; rest="$(tail -n +2 "$cl")"
  printf '%s\n\n%s\n\n%s\n' "$title" "$changelog" "$rest" > "$cl"
else
  printf '# Changelog\n\n%s\n' "$changelog" > "$cl"
fi

echo "==> Committing + tagging"
git add Cargo.toml Cargo.lock CHANGELOG.md || die "git add failed"
git commit -m "chore: release $tag" || die "git commit failed"
git tag -a "$tag" -m "Release $tag" || die "git tag failed"

if [ "$no_push" = 1 ]; then
  echo "Done (local). Tag $tag created; zip at $zip_path. Not pushed (--no-push)."
  exit 0
fi

echo "==> Pushing"
git push origin HEAD || die "git push failed"
git push origin "$tag" || die "git push (tag) failed"

if command -v gh >/dev/null 2>&1; then
  gh release create "$tag" "$zip_path" --title "$tag" --notes-file "$notes_path"
  echo "GitHub release $tag created."
else
  echo
  echo "gh not installed - tag pushed. Finish the release manually:"
  echo "  1. Open : $repo_url/releases/new?tag=$tag"
  echo "  2. Notes: $notes_path"
  echo "  3. Asset: $zip_path"
fi
