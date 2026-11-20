# Dependency propagation

This rule was explicitly requested by the project owner on 2026-11-20.

A library update includes its reverse-dependency closure. Repository ownership
does not end at the upstream release boundary.

## Procedure

1. Enumerate direct and transitive consumers from the Project Z graph. In that
   repository, run `python3 scripts/project_z.py impact --repositories zuse`.
   Include package aliases such as zuse-computer, git submodules, application
   manifests, runtime bundles and installers. Register newly discovered edges.
2. Identify the active owner and checkout for each consumer. Agree who changes
   the dependency set. Use a separate worktree for assigned work; do not edit
   another owner's active tree or start duplicate model/build work.
3. Propagate a real released version or an agreed immutable source revision,
   with the corresponding checksums, gitlinks and genuine lockfiles. Do not
   fabricate registry checksums or relabel an older source tree.
4. Run the relevant consumer contract and application tests. Keep compile-only,
   fixture and live-device results separate. Record the tested source and
   artifact fingerprints.
5. Commit and push each completed change. Record source, publication and
   deployment status separately.
6. Close the receipt only after checking those results. A queued message,
   delivered notification or upstream release is not a completed integration.
   If completion is genuinely blocked, record the specific blocker, current
   immutable state and named takeover owner.

## Current release records

- [0.2.2 delivery and dependency status](delivery-0.2.2.zh-CN.md)
- [0.2.2 release integrity](evidence/release-0.2.2.json)
- [0.2.2 source validation](validation-0.2.2.md)
- [0.2.0 release integrity](evidence/release-0.2.0.json)
- [0.2.0 native validation](validation-0.2.0.md)

The reverse-dependency query for zuse on 2026-11-20 returned ztype, ssmcp, zrc and
oh-my-ztty, as well as the zuse root. Project Z's aggregate catalog/gitlinks
also need to track the selected release. Integration status is recorded in the
delivery report with consumer owners and exact evidence; an active coordinated
consumer release is not silently overwritten to make the table appear complete.
