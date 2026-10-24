# SDK 0.1.0-beta.3 release candidate

## Scope and provenance

This candidate releases npm `locho_web` and pub.dev `locho_flutter` independently
of the CLI. Both registries currently offer beta.2; beta.3 is not published until
registry acceptance below is recorded. Keep npm's `beta` channel and leave
`latest` unchanged. Do not create SDK version tags without reviewing the CLI's
version-tag-triggered cargo-dist workflow.

Preparation starts from merged metadata PR #121, main commit
`82ac5f134eddb8e120e3c0e69cf290f0a2a7d66f`. Both packages include the transport
and shared/native source changes since their respective published beta.2 archives,
including iroh 1.4.0 and patched ctutils 0.4.4 (RUSTSEC-2026-0385).

- Browser: additive buffered/streaming upstream-header metadata APIs, preserved
  repeated cookies, existing Response compatibility and streaming ownership.
  There is no automatic cookie jar or native HttpOnly/SameSite enforcement.
- Flutter: refresh bundled core/native implementation while keeping its public API
  and FRB runtime/codegen pin at 2.12.0. Browser metadata methods are not Flutter APIs.
- Wire protocol 3 and ticket format 1 are unchanged. This does not patch already
  published CLI binaries or previously published SDK archives.
- Both remain beta. Public-N0 stale-addressing intermittency, foreground-only
  Flutter lifecycle, physical iOS and broad device/network acceptance remain open.

## Candidate gates

Record exact candidate/merged SHAs and workflow links in the release PR. Previous
PR #121 results (including its local 15-minute soak) do not verify these newly
versioned artifacts. Final publication must use artifacts from a clean, verified
merged release commit, not a dirty preparation checkout or an older cached WASM.

- [ ] Locked Rust security audit and workspace tests.
- [ ] Browser fresh WASM/TypeScript/production/package builds.
- [ ] Browser full source and installed-tarball matrices on all three engines,
      with the pinned reference backend configured.
- [ ] Fresh 900-second page/module-service-worker metadata soak across all engines.
- [ ] Flutter formatting, analysis, tests, bridge runtime tests, Clippy and codegen
      reproducibility; maintain matching FRB pins.
- [ ] Fresh external Flutter Android debug/release builds and 16 KiB alignment.
- [ ] Hosted iOS simulator and unsigned-release external consumer builds.
- [ ] Android runtime smoke where available; distinguish it from compilation.
- [ ] Flutter standalone staging outside Git and pub publication dry run.
- [ ] Candidate PR green, then final merged SHA revalidated.

## Publication and registry acceptance

- [ ] Retain the exact tested npm tarball, integrity hash and clean `build.json`.
- [ ] Publish that tarball with `--tag beta`; complete staged approval if required.
- [ ] Verify npm beta.3 is installable, `beta` selects beta.3 and `latest` is unchanged.
- [ ] Test actual npm beta.3 consumption in a fresh external Vite app.
- [ ] Publish the verified Flutter staging directory outside Git, not the
      development wrapper with sibling Rust paths or the ignored target directory.
- [ ] Verify pub.dev beta.3 archive checksum and bundled `BUILD.json` provenance.
- [ ] Test pub.dev beta.3 in fresh Android/iOS consumers.
- [ ] Record source SHA, registry integrity, workflow/soak results and limitations
      in `docs/sdk-releases.md`; update examples to the published versions.

Registry publication may require the maintainer's interactive npm/pub approval.
Keep these gates pending until they actually complete; do not describe a staged
or locally packed package as publicly installable.
