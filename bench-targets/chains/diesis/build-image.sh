#!/usr/bin/env bash
# Build diesis-node-e2e:<HEAD>-release for fleet.py from a clean node checkout.
#   compute run --class heavy --where local -- bash build-image.sh CHECKOUT
# Runs the node build step of `make e2e-build` (docker compose build node-1) and skips the
# test-runner image and release-evidence registration. contracts/out must be a real
# directory with artifacts, not a symlink: Docker's build context does not follow symlinks.
# A contracts worktree needs `git -C <worktree>/contracts config --worktree core.worktree
# <worktree>/contracts`; never `git submodule update` in a diesis worktree.
set -euo pipefail
cd "${1:?node worktree}"
test -d contracts/out && test ! -L contracts/out || { echo "contracts/out missing or a symlink"; exit 2; }
test -z "$(git status --porcelain --untracked-files=all)" || { echo "dirty worktree"; exit 2; }
line=$(make -n --no-print-directory e2e-build 2>/dev/null | grep -m1 "docker compose -f docker/docker-compose.e2e.yml build")
test -n "$line" || { echo "e2e-build recipe not found"; exit 2; }
line=${line/build node-1 test-runner/build node-1}
line=$(sed -E "s/DIESIS_CARGO_BUILD_JOBS='[0-9]+'/DIESIS_CARGO_BUILD_JOBS='${COMPUTE_JOBS:-6}'/" <<<"$line")
eval "$line"
docker image inspect "diesis-node-e2e:$(git rev-parse HEAD)-release" --format '{{.Id}}'
