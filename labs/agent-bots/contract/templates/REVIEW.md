---
task: T-0000
reviewed: 0000000000000000000000000000000000000000
verdict: approve
---

# What I checked

One paragraph. Which files, and what you were looking for in them. A
reader has to be able to tell a careful pass from a skim, and only you
can tell them.

# Findings

- core/src/example.rs:142 — the claim, in one sentence. What is wrong,
  not what to do about it.
  > let span = info_span!("example", id = %id);
- core/src/example.rs:301 — second finding.
  > if buffer.len() > CAP {
  >     buffer.clear();

Ranked by what would actually break, hardest first. Every line starts
with a real `path:line` inside the task's `touches:`. The runner reads
the blob at the commit under review and refuses the run if the path is
not a file there, or if the line is past its end.

**Every finding quotes what it is about**, on `> ` lines under its
bullet, and the runner checks the quote against that blob. One line, or
several for consecutive lines starting at the one you cited. The match
is a substring after whitespace is normalised — quote a fragment of a
long line if that is the honest part, and re-indent it if you like;
what you may not do is quote something that is not there. Fewer than
eight characters of content is refused, because a quote that short
matches half the file and identifies nothing.

Why the quote: a citation that resolves proves a file was opened. A
quote that matches proves the line was read. The gap between those two
is the whole failure mode — a review whose paths, line numbers and
frontmatter all check out, describing something that is not in the
code.

These citations are also the scope of whatever comes next: when the
task declares `on_changes_requested:`, this list is what the derived
task's `touches:` is built from.

If there is nothing to report, this section is exactly:

    No findings.

That is a complete review, and it is a better one than three
observations invented to look thorough.

# What I could not check

The blind spots. Files you could not reach, behaviour only a running
build would show, assumptions you had to take on trust. This section is
never empty — a review that lists only findings implies it covered
everything, which is never true, and this is the one part the reader
cannot reconstruct for themselves.

---

<!--
Field reference:

  task      the task id this review answers.
  reviewed  the commit SHA you were given. The runner compares it with
            the one it checked out and refuses a review of anything else
            - a review of the wrong tree is worse than no review.
  verdict   approve | changes-requested | blocked
            approve            nothing here should stop this landing.
            changes-requested  at least one finding must be answered.
            blocked            the review could not be completed; say
                               why under "What I could not check".

The runner harvests this file, deletes it from the worktree, and stores
it in the control plane. Its verdict goes on the handoff; the file
itself is the artifact a human reads. You never write the handoff.

`changes-requested` with no findings is refused: a verdict is a claim
about the findings, and asking for work while naming none of it is not
something a reader can resolve.
-->
