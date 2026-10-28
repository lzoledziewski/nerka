# Security policy

nerka opens files that may come from anywhere: logs, DLT files, network
captures (pcap / pcapng), MF4 recordings, query packs and presets. A file
that makes it crash, hang, read out of bounds or use memory without limit
is a security issue.

## Reporting a vulnerability

Please **don't open a public issue**. Report it privately:

- on GitHub: **Security → Report a vulnerability** in this repository, or
- by email to the maintainer: l.zoledziewski@gmail.com

Include the nerka version (`nerka --version`), the platform, what happens,
and if possible a file that triggers it (a minimal, synthetic one; please
don't send real logs that contain private data).

You'll get an answer within a week. Once a fix is released, the report can
be made public, with credit if you want it.

## Supported versions

Fixes go into the next release; older releases aren't patched. While nerka
is at 0.x, only the latest release is supported.
