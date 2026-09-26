# Docker — Freebuff2API containerization (v0.10)

> When Docker isn't available locally, image build/push is handled automatically by CI (`.github/workflows/docker.yml`) on push to the main branch (multi-arch amd64+arm64 → ghcr.io/lza6/freebuff2api).

## 1. Image

- Registry: `ghcr.io/lza6/freebuff2api`
- Tags: `latest` (default branch), `dev` (dev branch), sha tags
- Dockerfile: `docker/Dockerfile` (multi-stage: build on rust:1.95-bookworm → run on debian:bookworm-slim; only ca-certificates runtime dependency; rustls pure-Rust TLS, no OpenSSL needed)

## 2. Running locally (requires Docker)

```bash
# Build
docker build -f docker/Dockerfile -t freebuff2api:local .

# Prepare config (listen_addr needs 0.0.0.0 for container port mapping; skip_upstream_check works fine if you have no credentials locally)
cat > config.json <<'EOF'
{
  "listen_addr": "0.0.0.0:47821",
  "upstream_base_url": "https://www.codebuff.com",
  "auth_tokens": [],
  "api_keys": [],
  "skip_upstream_check": true,
  "memory_enabled": false,
  "sqlite_path": "/data/freebuff2api.sqlite",
  "telemetry_path": "/data/telemetry.sqlite",
  "memory_path": "/data/memory.sqlite",
  "skills_dir": "/data/skills",
  "redact_logs": true
}
EOF

# Run
docker run -d --name fba -p 47821:47821 -v "$(pwd)/config.json:/data/config.json:ro" -v fba-data:/data freebuff2api:local

# Smoke test
curl -sf http://127.0.0.1:47821/healthz && echo OK
curl -sf http://127.0.0.1:47821/ui | head -c 200

# Cleanup
docker rm -f fba
```

## 3. Known improvements

- Done, v0.10.2: added `HEALTHCHECK` to the Dockerfile (installs curl in the run stage, `curl -sf /healthz`, interval 30s / start_period 10s)
- The container's `thread_cleanup_interval_sec`/`thread_max_age_hours` fall back to the config defaults; for multi-instance deployments, make sure each `/data` volume is unique.

## 4. CI evidence (2026-09-19)

- `Build & Push Docker Image` (docker.yml) on main push (b561752) → **success** (amd64 + arm64 dual-platform build + GHCR manifest merge push)
- Local environment limitation: the current host has no docker CLI; running the containerized healthz check for real must be done on a Docker-capable host by following the commands in section 2 (the commands themselves are the verification script).
</content>
