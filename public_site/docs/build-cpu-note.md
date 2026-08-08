# Build CPU spike — follow-up

**Date:** 2026-08-05 (deploy of `public_site` → `deepwa7er.com`)
**Symptom:** `docker buildx build --platform linux/amd64` pegged all cores via QEMU emulation (`linuxkit` VM) for 5–10 min. User saw "VM running and using up a bunch of cpu".

**Root cause:** `fleet/public_site/deploy.toml` pins `linux/amd64` because
- Mac is arm64 (`deepwater-1`)
- VPS (`deepwa7er`) is x86_64

`buildx` emulates amd64 on arm64 to cross-build. From `deploy.toml:22`:

> buildx runs the Linux build under emulation, which is slow but avoids needing a builder on a 2GB host.

`bundle install` (native gems: nokogiri, bootsnap, puma, nio4r, racc, msgpack, etc.) dominates — ~300s of pure emulation. Running twice (5min timeout → retry with 10min) doubled the hit.

**Why we do it this way:** VPS has 2GB RAM and no buildx builder; cross-build avoids needing toolchain on VPS. Same pattern used for `blog`/`readout`.

**Options to optimize (pick one later):**

1. **Remote builder on `laptop` (`fedora-1`, x86_64, 100.100.110.47):** `docker context create laptop` + `docker buildx create --name fleet --driver remote tcp://laptop:1234` — native amd64, no emulation. Laptop is usually on, has Docker, runs Jellyfin/Campfire. Need to expose `dockerd` TCP or SSH tunnel. Best perf/cost.

2. **Build natively on VPS (cold):** `ssh vps 'docker build ...'` — no emulation, but 2GB host + slow disk. Only viable if we bump VPS RAM or use `build --no-cache` sparingly. Tugboat would need a `build_on=host` mode.

3. **Native arm64 image + runtime QEMU:** Ship `linux/arm64` image and let Docker on VPS run it via `qemu-user-static` at runtime — avoids build-time QEMU but moves cost to every request (not desired for Puma).

4. **Cached cross-build:** Keep emulation but speed it up: enable `gha`/`registry` cache in `buildx` (`--cache-from type=gha --cache-to type=gha,mode=max`) so `bundle install` layer is reused across deploys. `rsync` already delta-compresses the tar; caching would make second deploys ~30s.

5. **Pre-built base image:** Bake `ruby:3.4.8-slim + apt deps + bundler` into a fleet base image (`ghcr.io/deepwa7er/ruby-base:3.4.8`) so `public_site`'s Dockerfile only does `COPY Gemfile && bundle install` delta.

6. **Depot / GitHub Actions builder:** Use hosted amd64 builder (Depot, Fly, GH Actions) — zero local CPU, but adds external dependency + secrets wiring.

**Recommendation:** (1) if `laptop` is reliable, otherwise (4)+(5) as quick wins. Both keep `deploy.toml` contract (`docker buildx build ... && docker save`) but swap the builder.

**Action:** Leave deploy as-is for now; open follow-up to wire `laptop` as remote builder or add `buildx` cache. This file is intentionally outside the Docker context (in `docs/`, not shipped) and gitignored-safe.

**Verification done:** `tugboat deploy --working-tree` succeeded after ~6–7min emulation; `https://deepwa7er.com` verified. No change to `deploy.toml` in this note — just documentation.
