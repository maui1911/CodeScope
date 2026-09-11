#!/usr/bin/env bash
# Stub agent: does the job, passes the verifier, stays inside scope -
# and leaves an unlinked TODO behind. Exists to prove that the charter's
# fourth acceptance criterion is enforced by the runner rather than
# merely written down. See F-18.
set -e

cat >> core/src/telemetry.rs <<'RS'

// TODO: revisit this once the parser refactor lands
RS

git add core/src/telemetry.rs
git -c user.name=sloppy -c user.email=sloppy@example.invalid \
    commit -q -m "core: note a follow-up in telemetry"

echo "Done. One small change, tests pass."
exit 0
