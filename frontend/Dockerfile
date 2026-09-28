# syntax=docker/dockerfile:1

# ── build the SPA ──────────────────────────────────────────────────────────
# The two `node:22-slim` tags below are the only copies of the console's Node
# version that cannot read `.nvmrc` (issue #858) — a `FROM` line takes no build
# arg by default and CI resolves the file instead. So these and `frontend/.nvmrc`
# are the pair to bump together; bumping only one means CI stops being evidence
# for the thing that deploys.
FROM docker.io/library/node:22-slim AS build
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .
# Crash reporting is a BUILD-time decision for the console: Vite substitutes
# these into the bundle, so a variable set when the container runs arrives far
# too late. A blank `VITE_SENTRY_DSN` builds a bundle that reports to the
# compiled-in TinyHumans console project; `off` builds one that never
# initializes the SDK (Compose passes `off` unless you set a DSN).
# See `docs/spec/runtime/crash-reporting.md`.
ARG VITE_SENTRY_DSN=
ARG VITE_SENTRY_ENVIRONMENT=
ARG VITE_SENTRY_TRACES_SAMPLE_RATE=
ARG VITE_SENTRY_TRACE_PROPAGATION_TARGETS=
# The commit half of the release tag. `.dockerignore` excludes `.git`, so the
# build has no way to work out which revision it is compiling — without this
# every image reports `opencompany@<version>` with no commit, which is a
# release name that cannot tell two builds apart. That is the question a stack
# trace raises, and `@sentry/vite-plugin` files source maps under the same
# string, so a degraded tag also loses symbolication. The host image already
# takes `OPENCOMPANY_BUILD_COMMIT` for exactly this; this is its console twin.
ARG VITE_BUILD_COMMIT=
ENV VITE_SENTRY_DSN=$VITE_SENTRY_DSN \
    VITE_SENTRY_ENVIRONMENT=$VITE_SENTRY_ENVIRONMENT \
    VITE_SENTRY_TRACES_SAMPLE_RATE=$VITE_SENTRY_TRACES_SAMPLE_RATE \
    VITE_SENTRY_TRACE_PROPAGATION_TARGETS=$VITE_SENTRY_TRACE_PROPAGATION_TARGETS \
    VITE_BUILD_COMMIT=$VITE_BUILD_COMMIT
RUN npm run build && npm run build:pages-sdk

# ── local development ─────────────────────────────────────────────────────
# Source is bind-mounted over /app; node_modules lives in a named volume.
FROM docker.io/library/node:22-slim AS development
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci

# ── serve + proxy the API ──────────────────────────────────────────────────
# nginx serves the static bundle and proxies the API to the host. The upstream
# is configurable so the same image works under Compose (service name),
# single-task ECS (localhost), or any host:port.
FROM docker.io/library/nginx:1.27-alpine AS runtime
ENV OC_UPSTREAM=opencompany:8080
COPY nginx/default.conf.template /etc/nginx/templates/default.conf.template
COPY --from=build /app/dist /usr/share/nginx/html
EXPOSE 80
