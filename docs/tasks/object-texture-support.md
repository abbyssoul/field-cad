# Per-object texture/image support

## Goal

Objects now carry an optional cosmetic display color (`WorldObject::color`,
`fieldcad_core::ObjectColor`) so a user can make objects like Earth and the
Moon visually distinct. Texture/image-mapped appearance — "make this sphere
actually look like Earth" — was raised alongside that request as a "maybe"
and deliberately deferred: there is no per-object asset infrastructure
anywhere in the repo today, and building it is a substantially larger,
separate feature from a flat tint.

## Current limitation

- No asset-reference type exists for a world object. `PathBuf` usage in
  `fieldcad-catalog` (`entry.rs`, `source.rs`, `write.rs`, `load.rs`,
  `instantiate.rs`) refers only to catalog *file* locations, never to a
  per-object image/texture file.
- The only textures in `apps/fieldcad-desktop/src/renderer.rs` are internal
  GPU resources for scalar-field-magnitude visualization (color-mapped
  domain/plane rendering) — unrelated to per-object surface texturing, and
  not reusable for it.
- The proxy meshes an object renders as (`ObjectMesh::Sphere`/`Box` in
  `apps/fieldcad-desktop/src/scene/mod.rs`) are flat-shaded, generated
  primitives with no UV/texture-coordinate data at all (see
  `push_triangle`'s doc comment in `scene/mod.rs`: the facet normal decides
  brightness, baked straight into the vertex color at generation time).
  Texturing needs real UV coordinates on these meshes, not just a new vertex
  attribute.
- No catalog support exists for referencing or packaging an image file
  alongside a template (`fieldcad-catalog`'s document format has no
  asset/attachment concept of any kind).

## Required behavior

- Decide the asset-reference model: an on-disk path resolved relative to
  something (the catalog root? the scene document's own location?), an
  embedded/base64 blob in the scene document for portability, or both with
  one as a fallback. This has real portability and security implications
  (arbitrary image files referenced by a shared scene/catalog) that need a
  deliberate decision, not just a "make it work locally" default.
- Add texture-coordinate generation to the sphere/box proxy meshes, and a
  real fragment-shader texture-sampling path in `scene.wgsl` alongside (not
  necessarily replacing) the existing flat-tint `instance.tint` multiply —
  `WorldObject::color` should very likely stay meaningful as a tint/fallback
  even for a textured object (e.g. while the texture loads, or if it fails
  to resolve).
- Add catalog document support for an optional default texture reference,
  following the same "one-time seed, not template-owned, freely
  overridable after instantiation" shape `default_color`
  (`docs/tasks/user-configurable-object-catalog.md`, "Linked instances and
  portable scenes") already established — texture should not diverge from
  that precedent without a specific reason to.
- Desktop-side asset loading: decode common image formats, upload as a
  `wgpu` texture, bind per-instance (or per-material) in the render pass.

## Tests and acceptance

- Analytic/unit coverage wherever the asset-reference type's validation
  lives (mirroring `ObjectColor::validate`'s finite/range checks — a texture
  reference has its own failure modes: missing file, unreadable, wrong
  format, oversized).
- A renderer test (GPU-free, mirroring
  `an_unselected_instance_uses_its_own_color_when_set` in `renderer.rs`)
  asserting the per-instance texture binding follows the object's declared
  reference.
- Manual verification in the running desktop app is unavoidable for the
  actual visual result — this repo has no driven GUI harness (see
  `apps/fieldcad-desktop/AGENTS.md`).

## Relevant code

- `crates/fieldcad-core/src/color.rs` and `WorldObject::color` — the
  existing cosmetic-field precedent a texture-reference field should follow
  (plain `Option<T>` field, no physics involvement, freely editable,
  never re-synced from a catalog template).
- `crates/fieldcad-catalog/src/structure.rs`,
  `crates/fieldcad-catalog/src/document.rs`,
  `crates/fieldcad-catalog/src/instantiate.rs` — where `default_color` was
  added; a default texture reference should mirror this shape.
- `apps/fieldcad-desktop/src/scene/mod.rs` — `ObjectMesh`, the
  `push_triangle`/mesh-generation functions that would need UV output.
- `apps/fieldcad-desktop/src/renderer.rs` — `InstanceRaw`, `SceneRenderer`,
  where a texture-binding path would live alongside the existing tint path.
- `apps/fieldcad-desktop/src/scene.wgsl` — the fragment shader that would
  gain a texture-sampling branch.
