# Security Policy

EventExplorer parses untrusted binary input (`.elog` / `.elog.gz` files, files
fetched over SSH, and files uploaded into the in-browser wasm build), so we are
interested in reports of crashes, denial-of-service, or memory-safety issues
triggered by malformed captures.

## Reporting a Vulnerability

Please report potential security vulnerabilities through the
[Netflix bug bounty program on HackerOne](https://hackerone.com/netflix).
Mention **EventExplorer** (or this repository name) in the report title so it
reaches the right people.

We currently accept reports for the latest released version. Note that this
project is not, on its own, eligible for a bounty.
