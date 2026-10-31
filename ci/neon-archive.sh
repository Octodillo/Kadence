#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Adds the KDE neon archive to apt in an Ubuntu 24.04 container and fetches
# its package lists, then writes `available=true` or `available=false` to
# $GITHUB_OUTPUT for the job's later steps.
#
# archive.neon.kde.org has had outages (502s for hours), and during one it
# can answer a request and fail the next. So reaching it means fetching its
# lists here, in one go; the job installs from those lists without updating
# them again. When it can't be reached on a pull request or a push, the job
# is skipped with a warning. On any other run (the weekly Packaging run, a
# manual one) it fails, so a long outage still gets noticed.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
neon=https://archive.neon.kde.org
list=/etc/apt/sources.list.d/neon.list

# The Ubuntu archive first, so a hiccup there fails the job rather than
# skipping it.
echo 'Acquire::Retries "5";' > /etc/apt/apt.conf.d/80retries
apt-get update
apt-get install -y --no-install-recommends ca-certificates curl gnupg

add_neon() {
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 10 \
        -o /tmp/neon.key "$neon/public.key" || return 1
    gpg --batch --yes --dearmor -o /usr/share/keyrings/neon.gpg /tmp/neon.key
    echo "deb [signed-by=/usr/share/keyrings/neon.gpg] $neon/user noble main" > "$list"
    # Only the neon source, keeping the Ubuntu lists fetched above. Any
    # failed fetch counts, not only a missing Release file.
    local attempt
    for attempt in 1 2 3; do
        if apt-get update \
            -o Dir::Etc::sourcelist="$list" -o Dir::Etc::sourceparts=- \
            -o APT::Get::List-Cleanup=0 -o APT::Update::Error-Mode=any; then
            return 0
        fi
        [ "$attempt" = 3 ] || sleep 30
    done
    return 1
}

if add_neon; then
    echo "available=true" >> "$GITHUB_OUTPUT"
    exit 0
fi

rm -f "$list"
if [ "${GITHUB_EVENT_NAME:-}" = pull_request ] || [ "${GITHUB_EVENT_NAME:-}" = push ]; then
    echo "::warning title=KDE neon archive unreachable::Skipping this job: $neon did not answer. This is not a failure of the code under test."
    echo "available=false" >> "$GITHUB_OUTPUT"
else
    echo "::error title=KDE neon archive unreachable::$neon did not answer."
    exit 1
fi
