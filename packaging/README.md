# packaging/

Installers, `.desktop` entries, icons and signing scripts.

This directory is one of the few permitted to contain the product name
(build-spec §2.2), and it is where the neutral `px-browser` binary acquires its
user-facing name. Cargo cannot template a `[[bin]]` name, so the rename happens
here rather than in a manifest — which also keeps the whole `crates/` tree
brand-free, and keeps the cost of a rename inside one directory.

Empty until Phase 20.
