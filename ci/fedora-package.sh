#!/bin/sh
# Build the Fedora RPM offline from a make-dist.sh tarball, lint, install and
# smoke-test it, as root in a Fedora container that has rpm-build, rpmlint,
# dnf-plugins-core and the smoke test's display servers. The binary and
# source RPMs are left in OUT.
#
#   ci/fedora-package.sh TARBALL OUT
set -eu

tarball=${1:?usage: $0 TARBALL OUT}
out=$(mkdir -p "${2:?usage: $0 TARBALL OUT}" && cd "$2" && pwd)
top=$(cd "$(dirname "$0")/.." && pwd)
spec=$top/packaging/fedora/kadence.spec

mkdir -p ~/rpmbuild/SOURCES
if [ "$(dirname "$(readlink -f "$tarball")")" != "$(readlink -f ~/rpmbuild/SOURCES)" ]; then
    cp "$tarball" ~/rpmbuild/SOURCES/
fi
dnf -y builddep "$spec"
rpmbuild -ba "$spec"
rpmlint "$spec" ~/rpmbuild/RPMS/*/kadence-[0-9]*.rpm
dnf -y install ~/rpmbuild/RPMS/*/kadence-[0-9]*.rpm
id tester > /dev/null 2>&1 || useradd -m tester
(cd "$top" && su tester -c "REQUIRE_ALL=1 ci/smoke-matrix.sh /usr/bin/kadence")
cp ~/rpmbuild/RPMS/*/kadence-[0-9]*.rpm ~/rpmbuild/SRPMS/kadence-[0-9]*.src.rpm "$out/"
