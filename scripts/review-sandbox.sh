#!/usr/bin/env bash
# Mutation sandbox for reviewers.
#
# A reviewer proves a test can fail by breaking the code it claims to pin and
# watching a named test die. That has to happen somewhere other than the working
# tree, which holds the change under review.
#
# Invariants this script enforces, each because getting it wrong silently
# produces a wrong review rather than an error:
#
#   * A mutation never runs in the working tree. Every command but `init` refuses
#     unless it is standing in a sandbox.
#   * A result is never reported for an unchanged tree. `check` compares against
#     the baseline first, so an edit that did not land reads as "nothing mutated"
#     and never as "the mutation survived".
#   * Two mutations never stack. `check` restores the baseline once it has an
#     answer, so every result is attributable to exactly one change.
#   * Two sandboxes never share compiled artifacts. Cargo cannot tell two copies
#     of this workspace apart — same package names, same paths inside the
#     workspace — so a shared build directory lets one sandbox run another's
#     binary. Each sandbox gets its own, named after it.
#
# The build directory outlives `clean` on purpose: it is what makes the next
# review under the same name cost seconds instead of a cold build. The name is
# therefore a cache key, and it is supplied from outside — one name per body of
# work, reused by every review round on it. Inventing a fresh name per round
# throws the cache away and leaves a multi-gigabyte build directory behind.
#
# Usage:
#   scripts/review-sandbox.sh init <name> [--from-head]
#       Make /tmp/loti-sandbox-<name> from the working tree (or from HEAD, to
#       prove a survivor pre-dates the change under review). Refuses to clobber
#       an existing sandbox: clean it first.
#
#   scripts/review-sandbox.sh check [--in <crate>] "<label>"   (in a sandbox)
#       Test whatever you have edited. Prints the diff it is testing, runs the
#       suite, says KILLED with every failing test named, SURVIVED, or
#       DID-NOT-COMPILE, appends the result to mutations.log, and restores the
#       baseline if the answer was conclusive.
#
#       --in <crate> runs only that crate's tests, which skips rebuilding every
#       downstream test binary. A SURVIVED verdict is never concluded from it:
#       the run escalates to the whole workspace before reporting one. A KILLED
#       verdict from a narrow run names only that crate's killers, so it cannot
#       found a redundancy claim; the log says which command answered.
#
#   scripts/review-sandbox.sh hold                   (from inside a sandbox)
#       Adopt the current tree as the baseline, so `check` restores to it. Only
#       for confirming that a test is redundant: remove the test, hold, then run
#       the mutations it claimed to catch.
#
#   scripts/review-sandbox.sh clean [--cache]        (from inside a sandbox)
#       Remove the sandbox. Keeps the build directory unless --cache is given,
#       so the next round under the same name starts warm.
#
#   scripts/review-sandbox.sh discard <name>         (from anywhere)
#       Remove every sandbox and build directory under <name>, including the
#       <name>-head twin. This is what finishes the job `clean` deliberately
#       leaves open: the build directory outlives `clean` so the next round is
#       warm, and by the time the work is done there is no sandbox left to
#       stand in. Run it when the ticket closes — each build directory is well
#       over a gigabyte.
#
# Exit codes: 0 killed, 10 survived, 20 did-not-compile, 30 nothing-mutated,
# 40 sandbox unsound or misused.

set -uo pipefail

readonly EX_KILLED=0 EX_SURVIVED=10 EX_NOCOMPILE=20 EX_UNCHANGED=30 EX_UNSOUND=40
readonly MARKER=.loti-sandbox
# What a sandbox holds. Not `target`, which is enormous and rebuilt anyway, and
# not `.git`, whose presence invites a checkout that would silently revert a
# mutation and turn a killed mutant into a survivor.
readonly CONTENTS=(crates Cargo.toml Cargo.lock rust-toolchain.toml)
# One test command, so two reviewers cannot reach different verdicts on the same
# mutation. `--no-fail-fast` because a mutation killed by tests in two different
# modules is the free signal that those tests overlap.
readonly TEST_CMD=${LOTI_SANDBOX_TEST:-"cargo test --workspace --no-fail-fast"}

die() { printf 'review-sandbox: %s\n' "$*" >&2; exit $EX_UNSOUND; }

# The repository this script lives in, however it was invoked.
repo_root() { cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd; }

# The sandbox the caller is standing in. Walking up rather than taking a path
# argument is what makes "run a mutation in the working tree" unrepresentable.
sandbox_root() {
    local dir=$PWD
    while [[ $dir != / ]]; do
        [[ -d $dir/$MARKER ]] && { printf '%s\n' "$dir"; return 0; }
        dir=$(dirname "$dir")
    done
    die "not inside a sandbox. Run 'init <name>' from the repository first."
}

# Every tracked entry, replaced wholesale. Whole-tree copies rather than a patch
# so a mutation that adds or removes a file is undone as completely as one that
# edits a line.
sync_tree() {
    local from=$1 to=$2 entry
    for entry in "${CONTENTS[@]}"; do
        [[ -e $from/$entry ]] || continue
        rm -rf "${to:?}/$entry"
        cp -r "$from/$entry" "$to/$entry"
    done
}

tree_diff() {
    local sb=$1 entry
    for entry in "${CONTENTS[@]}"; do
        diff -r -q "$sb/$MARKER/baseline/$entry" "$sb/$entry" 2>&1
    done
}

# The changed files, named the way a reviewer would cite them, because this ends
# up in mutations.log and from there in the verdict.
diffstat() {
    local sb=$1
    tree_diff "$sb" \
        | sed -e "s|^Files $sb/$MARKER/baseline/\(.*\) and .* differ$|\1|" \
              -e "s|^Only in $sb/$MARKER/baseline/\([^:]*\): \(.*\)$|\1/\2 (removed)|" \
              -e "s|^Only in $sb/\([^:]*\): \(.*\)$|\1/\2 (added)|"
}

cmd_init() {
    local name=${1:-} from_head=${2:-}
    [[ -n $name ]] || die "init needs a name. Use the one you were given, so the build cache is reused."
    [[ $name =~ ^[A-Za-z0-9._-]+$ ]] || die "a sandbox name may hold letters, digits, dot, dash and underscore."

    local repo sandbox target
    repo=$(repo_root)
    sandbox=/tmp/loti-sandbox-$name
    target=/tmp/loti-sandbox-$name-target

    [[ -e $sandbox ]] && die "$sandbox already exists. Clean it, or pick another name."

    mkdir -p "$sandbox/$MARKER/baseline"
    if [[ $from_head == --from-head ]]; then
        git -C "$repo" archive HEAD "${CONTENTS[@]}" | tar -x -C "$sandbox" \
            || die "could not export HEAD. Is the repository a git checkout?"
    else
        local entry
        for entry in "${CONTENTS[@]}"; do
            [[ -e $repo/$entry ]] || die "the repository has no $entry"
            cp -r "$repo/$entry" "$sandbox/$entry"
        done
    fi

    # Belt and braces on the two things that must never be in a sandbox.
    [[ -e $sandbox/.git ]] && die "the copy carries a .git directory"
    [[ -e $sandbox/target ]] && die "the copy carries a target directory"

    sync_tree "$sandbox" "$sandbox/$MARKER/baseline"
    {
        printf 'name %s\n' "$name"
        printf 'created %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'source %s\n' "$([[ $from_head == --from-head ]] && echo HEAD || echo 'working tree')"
        printf 'revision %s\n' "$(git -C "$repo" rev-parse --short HEAD 2>/dev/null || echo unknown)"
        printf 'held 0\n'
    } > "$sandbox/$MARKER/meta"
    printf 'baseline: pristine\n' > "$sandbox/$MARKER/mutations.log"

    printf 'warming the build directory (once per name; later reviews reuse it)\n'
    ( cd "$sandbox" && CARGO_TARGET_DIR=$target $TEST_CMD --no-run >/dev/null 2>&1 ) \
        || die "the sandbox does not build before any mutation. Fix the working tree first."

    cat <<EOF
sandbox ready: $sandbox
  build dir:   $target  (kept by clean, so your next review starts warm)
  next:        cd $sandbox
               $repo/scripts/review-sandbox.sh check "<label>"
EOF
}

# Classify one test run, setting verdict/status/killers for the caller.
# Order matters: cargo prints "error: test failed" when a test fails, so a
# search for "error" before a search for a failing test reports a kill as a
# compilation failure.
classify_run() {
    local out=$1
    if grep -qE '^test .* FAILED|test result: FAILED' <<<"$out"; then
        verdict=KILLED; status=$EX_KILLED
        killers=$(grep -E '^test .* \.\.\. FAILED' <<<"$out" | sed 's/^test //; s/ \.\.\. FAILED//' | sort -u)
    elif grep -qE '^error(\[E[0-9]+\])?:|could not compile' <<<"$out"; then
        verdict=DID-NOT-COMPILE; status=$EX_NOCOMPILE
        killers=$(grep -E '^error' <<<"$out" | head -3)
    else
        verdict=SURVIVED; status=$EX_SURVIVED
        killers=
    fi
}

cmd_check() {
    local label= scope=
    while [[ $# -gt 0 ]]; do
        case $1 in
            --in) scope=${2:-}; [[ -n $scope ]] || die "--in needs a crate name."; shift 2 ;;
            *)    label=$1; shift ;;
        esac
    done
    [[ -n $label ]] || die "check needs a label; it becomes the row in mutations.log."

    local sb target held baseline_note
    sb=$(sandbox_root) || exit $EX_UNSOUND
    target=/tmp/loti-sandbox-$(sed -n 's/^name //p' "$sb/$MARKER/meta")-target
    held=$(sed -n 's/^held //p' "$sb/$MARKER/meta")
    if [[ $held -gt 0 ]]; then
        baseline_note="pristine + $held held change(s)"
    else
        baseline_note="pristine"
    fi

    if [[ -z $(tree_diff "$sb") ]]; then
        printf 'nothing mutated: the tree matches the baseline.\n'
        printf 'Your edit did not land. Fix it before drawing any conclusion.\n'
        exit $EX_UNCHANGED
    fi

    printf 'baseline: %s\n' "$baseline_note"
    printf 'testing:  %s\n' "$label"
    diffstat "$sb" | sed 's/^/    /'

    # Whether the killer list is one crate's rather than the whole suite's. An
    # escalated run clears it: the canonical command named everything.
    local out status verdict killers ran partial=0
    if [[ -n $scope ]]; then
        partial=1
        ran="cargo test -p $scope --no-fail-fast"
        out=$( cd "$sb" && CARGO_TARGET_DIR=$target cargo test -p "$scope" --no-fail-fast 2>&1 )
    else
        ran=$TEST_CMD
        out=$( cd "$sb" && CARGO_TARGET_DIR=$target $TEST_CMD 2>&1 )
    fi
    classify_run "$out"

    # A narrow run can only ever report the killers it compiled. "Nothing caught
    # it" is a claim about the whole suite, so it is never concluded from one
    # crate: escalate and let the canonical command answer.
    if [[ -n $scope && $verdict == SURVIVED ]]; then
        printf '   nothing in %s caught it; escalating to the whole workspace\n' "$scope"
        out=$( cd "$sb" && CARGO_TARGET_DIR=$target $TEST_CMD 2>&1 )
        ran="$TEST_CMD (escalated from -p $scope)"
        partial=0
        classify_run "$out"
    fi

    printf '=> %s\n' "$verdict"
    [[ -n $killers ]] && sed 's/^/     /' <<<"$killers"

    [[ $verdict == KILLED && $partial -eq 1 ]] && printf \
        '   killers named within %s only; do not build a redundancy claim on this row\n' "$scope"

    {
        printf '[%s] %s  %s\n' "$(date -u +%H:%M:%S)" "$verdict" "$label"
        printf '     baseline: %s\n' "$baseline_note"
        printf '     ran:      %s\n' "$ran"
        [[ -n $killers ]] && sed 's/^/     /' <<<"$killers"
        diffstat "$sb" | sed 's/^/     /'
    } >> "$sb/$MARKER/mutations.log"

    # Restore only on an answer. A mutation that did not compile is still being
    # written, and throwing it away would cost the reviewer the edit.
    if [[ $verdict == KILLED || $verdict == SURVIVED ]]; then
        sync_tree "$sb/$MARKER/baseline" "$sb"
        printf '   baseline restored; ready for the next mutation\n'
    else
        printf '   tree left as it is, so you can repair the mutation\n'
    fi
    exit $status
}

cmd_hold() {
    local sb held target
    sb=$(sandbox_root) || exit $EX_UNSOUND
    target=/tmp/loti-sandbox-$(sed -n 's/^name //p' "$sb/$MARKER/meta")-target

    [[ -n $(tree_diff "$sb") ]] || die "nothing to hold: the tree matches the baseline."

    # A baseline that does not build makes every later mutation report a
    # compilation failure, which reads as a broken mutation rather than a broken
    # baseline. Removing a test by hand is easy to get wrong, so check before
    # adopting rather than leaving the reviewer to work it out.
    printf 'checking the tree builds before adopting it\n'
    ( cd "$sb" && CARGO_TARGET_DIR=$target $TEST_CMD --no-run >/dev/null 2>&1 ) \
        || die "this tree does not build, so it cannot be a baseline. Repair it and hold again."

    printf 'adopting as the baseline:\n'
    diffstat "$sb" | sed 's/^/    /'
    sync_tree "$sb" "$sb/$MARKER/baseline"

    held=$(sed -n 's/^held //p' "$sb/$MARKER/meta")
    held=$((held + 1))
    sed -i "s/^held .*/held $held/" "$sb/$MARKER/meta"
    printf 'baseline: pristine + %s held change(s)\n' "$held" >> "$sb/$MARKER/mutations.log"

    cat <<EOF
=> baseline is now pristine + $held held change(s)
   Every later result says so. Hold only to remove a test for a redundancy
   check; for a different starting point, init a new sandbox.
EOF
}

cmd_clean() {
    local sb name target
    sb=$(sandbox_root) || exit $EX_UNSOUND
    name=$(sed -n 's/^name //p' "$sb/$MARKER/meta")
    target=/tmp/loti-sandbox-$name-target

    cd /
    rm -rf "$sb"
    printf 'removed %s\n' "$sb"
    if [[ ${1:-} == --cache ]]; then
        rm -rf "$target"
        printf 'removed the build directory %s\n' "$target"
    else
        printf 'kept the build directory %s, so your next review starts warm\n' "$target"
    fi
}

# Remove a name's sandboxes and build directories from outside any of them.
# Addressed by name rather than by standing inside, because the thing most worth
# removing — the build directory — is the one that outlives the sandbox.
cmd_discard() {
    local name=${1:-}
    [[ -n $name ]] || die "discard needs the name the sandboxes were made under."
    [[ $name =~ ^[A-Za-z0-9._-]+$ ]] || die "a sandbox name may hold letters, digits, dot, dash and underscore."

    # Standing inside what is about to be removed leaves the shell in a deleted
    # directory, and every later command in the session fails obscurely.
    cd /

    local removed=0 path
    for path in /tmp/loti-sandbox-"$name" /tmp/loti-sandbox-"$name"-*; do
        [[ -e $path ]] || continue
        # Only ever remove something this script made: a sandbox carries the
        # marker directory, and a build directory is only ever named -target.
        [[ -d $path/$MARKER || $path == *-target ]] || continue
        rm -rf "$path"
        printf 'removed %s\n' "$path"
        removed=$((removed + 1))
    done

    (( removed > 0 )) || printf 'nothing to discard under %s\n' "$name"
}

case ${1:-} in
    init)    shift; cmd_init "$@" ;;
    check)   shift; cmd_check "$@" ;;
    hold)    shift; cmd_hold "$@" ;;
    clean)   shift; cmd_clean "$@" ;;
    discard) shift; cmd_discard "$@" ;;
    *)     sed -n '/^# Usage:/,/^# 40 sandbox/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit $EX_UNSOUND ;;
esac
