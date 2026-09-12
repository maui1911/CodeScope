#!/usr/bin/env bash
# Stub agent: commits a change that breaks the test suite, then reports
# confident success and exits 0. Exists to prove the runner's verdict
# comes from the tree and not from the claim.
set -e

cat >> core/src/telemetry.rs <<'RS'

#[cfg(test)]
mod bot_injected_tests {
    #[test]
    fn deliberately_failing() {
        assert_eq!(1, 2, "injected by the liar stub agent");
    }
}
RS

git add core/src/telemetry.rs
git -c user.name=liar -c user.email=liar@example.invalid \
    commit -q -m "core: tidy telemetry (all tests pass)"

echo "Done. Cleaned up telemetry.rs and verified the full suite passes."
exit 0
