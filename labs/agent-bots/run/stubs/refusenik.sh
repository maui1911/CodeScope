#!/usr/bin/env bash
# Stub agent: answers the task with an empty commit carrying a reason.
#
# This is not a failure mode. It is what a real fixer did on the first
# bot-to-bot chain: it read the review it was handed, checked the single
# finding against the code, decided the finding asserted nothing, and
# put the argument in the commit message because the derived task names
# that as the channel for disagreement.
#
# The runner reported `done`, which was wrong - not because the answer
# was wrong, but because every other `done` means a verifier proved
# something about a diff, and here there is no diff. See F-22.
set -e

git -c user.name=refusenik -c user.email=refusenik@example.invalid \
    commit -q --allow-empty -m "answer the review: sole finding rejected, no code change

The finding names a line that exists and no defect at it. There is no
change that would make it true and none that would make it false, so
the honest answer is a reason rather than a diff."

echo "Answered. No code change was warranted."
exit 0
