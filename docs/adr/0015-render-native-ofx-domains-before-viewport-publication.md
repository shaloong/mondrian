# Render native OpenFX domains before viewport publication

Status: accepted; implementation requires native qualification.

The existing Effects CPU value is a fixed, zero-origin viewport. An OFX effect's
canonical RoD, its enclosing pixel RoD and that viewport are different facts.
The previous host never queried effect RoD and reported the viewport as every
Output image's support. That cannot represent finite or translated generators.

Keep the existing graph and frame ownership. Query current-time RoD after
parameter changes and ClipPreferences. Initialize the action's output with the
SDK context default; ReplyDefault uses that default regardless of residual writes.
Convert canonical coordinates with the actual image PAR and unit render scale,
enclosing fractional boundaries. Output clip RoD remains canonical; Output image
bounds and RoD are the complete pixel domain. Input remains the supplied upstream
viewport. This does not add upstream spatial or temporal scheduling.

Tiles remain unsupported: supply and render the complete finite domain, then
place its intersection in the requested viewport, with transparent black outside.
An Opaque native image becomes StraightFloat when placement introduces transparent
pixels; its covered alpha is canonicalized to one. Other representations and RGB
values are preserved. Empty support produces transparent output without starting
a sequence requiring a nonexistent Render action.

The matching full-viewport path uses existing storage. Other domains use one
action-local allocation, bounded by 8192 per axis and 128 MiB (the existing image
limits). Charge that maximum in processor active storage before native execution;
retain no scratch/cache across frames. This conservative grant can limit concurrent
processors despite small actual images. Allocator administration and plugin-owned
memory remain outside logical payload accounting. Reject malformed, unbounded,
oversized or unsupported non-multiresolution domains before Render. Native protocol
13 prevents reuse of workers with the former fixed-image semantics.

This is viewport publication, not a variable-domain Effects value or full RoI
scheduler. Pixels outside the viewport are not propagated downstream. Full-frame
native work can exceed requested viewport work and needs later measurement. GPU,
temporal fetch, tiled execution and multiresolution render scales remain separate.
