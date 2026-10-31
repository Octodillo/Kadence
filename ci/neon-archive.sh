#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Usage: ci/neon-archive.sh PACKAGE...
#
# Adds the KDE neon archive to apt in an Ubuntu 24.04 container and installs
# PACKAGEs from Ubuntu and neon, then writes `available=true` or
# `available=false` to $GITHUB_OUTPUT for the job's later steps.
#
# archive.neon.kde.org has had outages (502s for hours), and during one it
# can answer a request and fail the next. So every download from it happens
# here: the key, the package lists and the packages. When neon can't be used
# on a pull request or a push, nothing is installed and the job is skipped
# with a warning. On any other run (the weekly Packaging run, a manual one)
# it fails, so a long outage still gets noticed. A failure of the Ubuntu
# archive, or of the install itself, always fails.
set -euo pipefail
shopt -s inherit_errexit

export DEBIAN_FRONTEND=noninteractive
neon=https://archive.neon.kde.org
list=/etc/apt/sources.list.d/neon.list
log=/tmp/neon-apt.log

# Retries within apt; any failed fetch is an error, not only a missing
# Release file.
echo 'Acquire::Retries "5";' > /etc/apt/apt.conf.d/80retries
echo 'APT::Update::Error-Mode "any";' > /etc/apt/apt.conf.d/80update-errors

apt-get update
apt-get install -y --no-install-recommends ca-certificates curl gnupg

# Prints why neon can't be used, or nothing once its lists and the packages
# are downloaded.
add_neon() {
    if ! curl -fsSL --retry 5 --retry-all-errors --retry-delay 10 \
        -o /tmp/neon.key "$neon/public.key"; then
        echo "$neon/public.key did not answer"
        return
    fi
    if ! gpg --batch --yes --dearmor -o /usr/share/keyrings/neon.gpg /tmp/neon.key; then
        echo "$neon/public.key is not a valid key"
        return
    fi
    echo "deb [signed-by=/usr/share/keyrings/neon.gpg] $neon/user noble main" > "$list"
    local attempt
    for attempt in 1 2 3; do
        [ "$attempt" = 1 ] || sleep 30
        # Only the neon source, keeping the Ubuntu lists fetched above.
        apt-get update -o Dir::Etc::sourcelist="$list" -o Dir::Etc::sourceparts=- \
            -o APT::Get::List-Cleanup=0 >&2 || continue
        if apt-get install -y --no-install-recommends --download-only "$@" 2>&1 | tee "$log" >&2; then
            return
        fi
        # A package that didn't download from Ubuntu is a real failure.
        if ! grep -q "Failed to fetch $neon" "$log"; then
            echo "::error title=Package download failed::See the log above." >&2
            exit 1
        fi
    done
    echo "$neon did not answer"
}

reason=$(add_neon "$@")
if [ -z "$reason" ]; then
    # From the downloaded packages.
    apt-get install -y --no-install-recommends "$@"
    echo "available=true" >> "$GITHUB_OUTPUT"
    exit 0
fi

rm -f "$list"
if [ "${GITHUB_EVENT_NAME:-}" = pull_request ] || [ "${GITHUB_EVENT_NAME:-}" = push ]; then
    echo "::warning title=KDE neon archive unusable::Skipping this job: $reason. This is not a failure of the code under test."
    echo "available=false" >> "$GITHUB_OUTPUT"
else
    echo "::error title=KDE neon archive unusable::$reason."
    exit 1
fi
