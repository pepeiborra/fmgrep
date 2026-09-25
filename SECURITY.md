# Security

Report a vulnerability privately, through GitHub's private vulnerability
reporting on this repository ("Report a vulnerability" under the Security
tab), not in a public issue. If that option is missing, open an issue asking
for a private contact, without details. An index is trusted input: a damaged
index can make a search miscount or panic, but never read out of bounds, so
a report that it does is in scope. `fmgrep serve` has no authentication and
listens on 127.0.0.1 by default; do not expose it to other hosts. fmgrep
reads the indexed tree with the permissions of the user running it.
