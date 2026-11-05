#!/usr/bin/env bash
# Prepare one task worktree from local main; never build, test, or commit.
set -euo pipefail

fail() {
    printf 'start-task: %s\n' "$*" >&2
    exit 1
}

if [[ $# -ne 1 || ! $1 =~ ^(T[0-9]+\.[0-9]+|M[0-9]+-[0-9]+)$ ]]; then
    printf 'usage: bash tests/start-task.sh <card> (e.g. T1.2, M2-01)\n' >&2
    exit 2
fi

card=$1
repo=/workspace/openwrt/aether-dns
wt_root=/workspace/openwrt/aether-dns-wt
wt=$wt_root/$card

# Compare the heading's card token literally, not as a regular expression.
branch=$(awk -v card="$card" '
    /^## / {
        if (inside) exit
        inside = ($2 == card)
        next
    }
    inside && /^- \*\*分支\*\*：/ {
        if (match($0, /`[^`]+`/))
            print substr($0, RSTART + 1, RLENGTH - 2)
        exit
    }
' "$repo"/docs/tasks/*.md)
[[ -n $branch ]] || fail "card $card has no readable 分支 line in docs/tasks/*.md"
git check-ref-format --branch "$branch" >/dev/null || fail "invalid task branch: $branch"

# This gate also applies to reuse. Do not create even the parent when dirty.
status=$(git -C "$repo" status --porcelain)
[[ -z $status ]] || fail "主检出不干净 (main checkout has uncommitted changes); refusing worktree changes: $repo"

mkdir -p "$wt_root"

# Only reuse a registered worktree belonging to this repository and card branch.
worktrees=$(git -C "$repo" worktree list --porcelain)
registered_branch=$(awk -v target="$wt" '
    /^worktree / { here = (substr($0, 10) == target) }
    here && /^branch / { print substr($0, 8) }
' <<< "$worktrees")
registered_path=$(awk -v target="$wt" '
    /^worktree / && substr($0, 10) == target { print target }
' <<< "$worktrees")
if [[ -n $registered_path ]]; then
    [[ $registered_branch == "refs/heads/$branch" && -d $wt ]] ||
        fail "target is not a reusable $card worktree; stop and ask the design owner: $wt"
    actual_root=$(git -C "$wt" rev-parse --show-toplevel)
    actual_branch=$(git -C "$wt" symbolic-ref -q HEAD)
    [[ $actual_root == "$wt" && $actual_branch == "refs/heads/$branch" ]] ||
        fail "target worktree identity mismatch; stop and ask the design owner: $wt"
    printf '%s\n' "$wt"
    exit 0
fi

[[ ! -e $wt && ! -L $wt ]] ||
    fail "target already exists and is not this card's registered worktree; stop and ask the design owner: $wt"

if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
    git -C "$repo" worktree add "$wt" "$branch" >&2
else
    git -C "$repo" worktree add -b "$branch" "$wt" main >&2
fi
printf '%s\n' "$wt"
