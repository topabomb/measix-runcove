# Agent Space patches to dav-server 0.11.0

Source: https://github.com/messense/dav-server-rs/tree/v0.11.0
Crates.io archive SHA-256: 88e9e4e7a3546a5b348518694e9f3ed5cf3fc8856e50141c197f54d79b5714a8
License: Apache-2.0 (upstream package metadata and source notices retained).

Only source, package manifests and README are vendored; example/test targets are removed from the generated manifest. Upstream tests are not part of the application workspace; the application regression suite covers these patches.

- handle_props.rs: route writable modification-time properties through the filesystem adapter; do not claim Windows attribute writes succeeded without applying them.
- handle_props.rs: collection quota is the same backing-volume accounting at every path, rather than subtracting each child file length from total capacity.
- handle_copymove.rs: regular-file overwrite calls copy/rename without deleting the target first; remove destination locks only after success. RootFs provides atomic file replacement. Reject copying/moving a directory onto an ancestor, or moving it into a descendant, before deleting any destination; compare decoded path components. Other directory operations retain partial-failure semantics.
- davheaders.rs / conditional.rs: malformed one-quote ETags fail parsing without panicking; If-None-Match uses weak comparison, while If-Match and If-Range retain strong comparison. Application files tests cover conditional reads and preservation of a rejected write's existing bytes.
- handle_gethead.rs: reject premature EOF and subsequent seek failures through body errors, never pad or skip content; propagate initial seek failures as filesystem errors. Normalize byte ranges before length arithmetic, clip oversized suffixes/endpoints, accept only decimal digits, and ignore Range on HEAD or empty files. Application large_reads tests cover these patches.

The full vendored source and manifest participate in helper and release content identities. Revisit these patches when upgrading the dependency.
