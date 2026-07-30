#!/usr/bin/env bash
# Mutation check for reviewers.
#
# A reviewer proves a test can fail by breaking the code it claims to pin and
# watching a named test die. The change under review is already committed on a
# ticket branch, so the working tree is where a deliberate defect is held:
# `git reset --hard` puts it back exactly, and nothing unreviewed lives there to
# lose.
#
# Invariants this script enforces, each because getting it wrong silently
# produces a wrong review rather than an error:
#
#   * Only the mutation surface is ever touched or restored. A mutation edits
#     tracked files under crates/; a tree carrying anything else is refused
#     rather than restored, because restoring would discard work that was never
#     a mutation. This is the guard against running a review before the change
#     under review was committed.
#   * A result is never reported for an unchanged tree. The tree is compared
#     against what the previous run left behind, so an edit that did not land
#     reads as "nothing mutated" and never as "the mutation survived".
#   * Two mutations never stack. The tree is restored once there is an answer,
#     unless --hold says this tree is deliberately the ground for the next run.
#   * Every row records what differed from HEAD, so a run made against a held
#     tree can never be read as a run against a pristine one.
#   * One test command, so two reviewers cannot reach different verdicts on the
#     same mutation.
#
# Usage:
#   scripts/mutation-check.sh [--in <crate>] [--hold] "<label>"
#
#       Test whatever you have edited. Prints the diff it is testing, runs the
#       suite, says KILLED with every failing test named, SURVIVED, or
#       DID-NOT-COMPILE, appends the result to .git/loti-mutations.log, and
#       restores the tree if the answer was conclusive.
#
#       --in <crate> runs only that crate's tests, which skips rebuilding every
#       downstream test binary. A SURVIVED verdict is never concluded from it:
#       the run escalates to the whole workspace before reporting one. A KILLED
#       verdict from a narrow run names only that crate's killers, so it cannot
#       found a redundancy claim; the log says which command answered.
#
#       --hold leaves the tree in place instead of restoring it, so the next run
#       is made against it. That is how a test is shown to be redundant: delete
#       the test, hold, then run the mutation it claimed to catch with the
#       deletion still there. The next run without --hold clears both. Hold one
#       thing at a time, and only for the question in front of you.
#
# You never commit. The orchestrator restores the tree when the review ends, but
# `git reset --hard` cannot remove a file that was never tracked: delete any
# scratch file you made before you report, or it will be taken for part of the
# change.
#
# Exit codes: 0 killed, 10 survived, 20 did-not-compile, 30 nothing-mutated,
# 40 misused.

set -uo pipefail

readonly EX_KILLED=0 EX_SURVIVED=10 EX_NOCOMPILE=20 EX_UNCHANGED=30 EX_UNSOUND=40
# One test command, so two reviewers cannot reach different verdicts on the same
# mutation. `--no-fail-fast` because a mutation killed by tests in two different
# modules is the free signal that those tests overlap.
readonly TEST_CMD="cargo test --workspace --no-fail-fast"
# The mutation surface. Everything a mutation may touch, and so everything this
# script is entitled to throw away.
readonly SURFACE=crates

die() { printf 'mutation-check: %s\n' "$*" >&2; exit $EX_UNSOUND; }

# A fingerprint of every tracked change in the tree. Comparing it against what
# the previous run left behind is what makes "your edit did not land" a distinct
# answer from "nothing caught it", including when the previous run was held.
tree_fingerprint() { git diff HEAD | sha1sum | cut -d' ' -f1; }

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

main() {
    local label= scope= hold=0
    while [[ $# -gt 0 ]]; do
        case $1 in
            --in)   scope=${2:-}; [[ -n $scope ]] || die "--in needs a crate name."; shift 2 ;;
            --hold) hold=1; shift ;;
            -*)     die "unknown option $1" ;;
            *)      label=$1; shift ;;
        esac
    done
    [[ -n $label ]] || die "a label is required; it becomes the row in the log."

    local repo; repo=$(git rev-parse --show-toplevel 2>/dev/null) || die "not inside a git checkout."
    cd "$repo" || die "cannot enter $repo"
    local logfile=.git/loti-mutations.log
    local statefile=.git/loti-mutation-held

    # Refuse a tree that carries anything but a mutation. The answer would be
    # meaningless and the restore would silently discard whatever else is there
    # — which is what happens if a review is run before the change under review
    # has been committed.
    local stray; stray=$(git diff --name-only HEAD -- . ":(exclude)$SURFACE/**")
    [[ -z $stray ]] || die "the tree carries changes outside $SURFACE/:
$(sed 's/^/    /' <<<"$stray")
A mutation edits tracked files under $SURFACE/ only. Refusing, because restoring
the tree would throw these away. Commit or revert them first."

    # What the previous run left: a held tree's fingerprint, or nothing, meaning
    # the tree was restored and should currently match HEAD.
    local previous=''
    [[ -f $statefile ]] && previous=$(<"$statefile")
    local current; current=$(tree_fingerprint)

    if [[ -z $previous ]]; then
        if git diff --quiet HEAD; then
            printf 'nothing mutated: the tree matches HEAD.\n'
            printf 'Your edit did not land. Fix it before drawing any conclusion.\n'
            exit $EX_UNCHANGED
        fi
    elif [[ $current == "$previous" ]]; then
        printf 'nothing mutated: the tree is exactly what the held run left.\n'
        printf 'Your edit did not land. Fix it before drawing any conclusion.\n'
        exit $EX_UNCHANGED
    fi

    local baseline_note='HEAD'
    [[ -n $previous ]] && baseline_note='HEAD + a held change'

    printf 'baseline: %s (%s)\n' "$baseline_note" "$(git log -1 --format='%h %s')"
    printf 'testing:  %s\n' "$label"
    git diff --stat HEAD | sed 's/^/    /'

    local out status verdict killers ran partial=0
    if [[ -n $scope ]]; then
        partial=1
        ran="cargo test -p $scope --no-fail-fast"
        out=$(cargo test -p "$scope" --no-fail-fast 2>&1)
    else
        ran=$TEST_CMD
        out=$($TEST_CMD 2>&1)
    fi
    classify_run "$out"

    # A narrow run can only ever report the killers it compiled. "Nothing caught
    # it" is a claim about the whole suite, so it is never concluded from one
    # crate: escalate and let the canonical command answer.
    if [[ -n $scope && $verdict == SURVIVED ]]; then
        printf '   nothing in %s caught it; escalating to the whole workspace\n' "$scope"
        out=$($TEST_CMD 2>&1)
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
        printf '     baseline: %s (%s)\n' "$baseline_note" "$(git log -1 --format='%h %s')"
        printf '     ran:      %s\n' "$ran"
        [[ -n $killers ]] && sed 's/^/     /' <<<"$killers"
        git diff --stat HEAD | sed 's/^/     /'
    } >> "$logfile"

    # Restore only on an answer. A mutation that did not compile is still being
    # written, and throwing it away would cost the reviewer the edit.
    if [[ $verdict == KILLED || $verdict == SURVIVED ]]; then
        if [[ $hold -eq 1 ]]; then
            tree_fingerprint > "$statefile"
            printf '   tree held; the next run is made against it, and the run after\n'
            printf '   that (without --hold) clears it\n'
        else
            # From HEAD explicitly, not from the index: a reviewer never stages,
            # and restoring to a staged copy would silently keep a mutation.
            git checkout HEAD -- "$SURFACE" || die "could not restore $SURFACE/"
            rm -f "$statefile"
            printf '   %s/ restored to HEAD; ready for the next mutation\n' "$SURFACE"
        fi
    else
        printf '   tree left as it is, so you can repair the mutation\n'
    fi
    exit $status
}

main "$@"
