# Maintenance instructions

When changing or releasing a Project Z library, complete dependency propagation.
Enumerate every direct and transitive consumer, including libraries, applications,
services and installation bundles. Coordinate with existing owners before
changing an active checkout. Propagate exact versions, immutable source pins
and lockfiles, run the relevant consumer tests, and commit/push the results.

Publishing the library or sending a notification does not complete this work.
Maintain a receipt for each consumer until propagation is verified, or record a
concrete blocker and the named owner who will take over. Register newly found
consumers in the Project Z dependency graph. See [maintenance](docs/maintenance.md)
for the procedure and release records.
