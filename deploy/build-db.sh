#!/bin/sh
# Fetches mainline tags and rebuilds the database, for the Kubernetes CronJobs.
# Every run, scheduled or manual, holds one lock on the volume for the whole
# fetch and build, so two runs never touch the clone or the database at once.
# Extra arguments go to `kfh build` (e.g. --force).
set -eu

work=${KFH_WORK:-/work}
repo_url=${KFH_REPO_URL:-https://github.com/torvalds/linux.git}
repo=$work/linux.git
mkdir -p "$work/live" "$work/tmp"

# Trust our own clone even if the volume's owner differs from this user (for
# example after a restore). Only this path, not every repository.
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=safe.directory GIT_CONFIG_VALUE_0="$repo"

exec 9>"$work/build-db.lock"
if ! flock -n 9; then
    echo "another build-db run holds $work/build-db.lock; nothing to do"
    exit 0
fi

if [ ! -d "$repo" ]; then
    rm -rf "$repo.partial"
    git clone --bare "$repo_url" "$repo.partial"
    mv "$repo.partial" "$repo"
fi
git -C "$repo" fetch --tags --force origin

# exec keeps descriptor 9, so the lock is held until kfh exits.
exec kfh build --repo "$repo" --db "$work/live/functions.db" "$@"
