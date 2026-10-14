# COPR "make_srpm" source: an SRPM with every crate vendored, so the RPM
# builds offline in the chroot. COPR runs `make -f .copr/Makefile srpm
# outdir=DIR`; it works the same locally on Fedora.
#
# Release: 1 when HEAD is tagged v<Cargo.toml version>, otherwise a snapshot
# (0.<date>.git<commit>) that sorts below that release.
.ONESHELL:
SHELL := /bin/bash
.SHELLFLAGS := -euo pipefail -c
top := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))..)
outdir ?= $(top)/target/srpm

srpm:
	for tool in cargo rpmbuild git; do
		command -v $$tool >/dev/null || { dnf -y install cargo rpm-build git-core; break; }
	done
	git() { command git -c safe.directory='*' -C '$(top)' "$$@"; }
	version=$$(sed -n 's/^version = "\(.*\)"$$/\1/p' '$(top)/Cargo.toml' | head -n 1)
	if git tag --points-at HEAD | grep -qx "v$$version"; then
		release=1
	else
		release=0.$$(git log -1 --format=%cd --date=format:%Y%m%d%H%M).git$$(git rev-parse --short HEAD)
	fi
	name=ytmcross-$$version
	work=$$(mktemp -d)
	trap 'rm -rf "$$work"' EXIT
	git archive --prefix=$$name/ -o "$$work/$$name.tar.gz" HEAD
	tar -xzf "$$work/$$name.tar.gz" -C "$$work"
	cd "$$work/$$name"
	mkdir .cargo
	cargo vendor --locked --versioned-dirs > .cargo/config.toml
	XZ_OPT=-T0 tar -cJf "$$work/$$name-vendor.tar.xz" vendor .cargo
	sed -e "s/^Version:.*/Version:        $$version/" -e "s/^Release:.*/Release:        $$release%{?dist}/" \
		'$(top)/.copr/ytmcross.spec' > "$$work/ytmcross.spec"
	mkdir -p '$(outdir)'
	rpmbuild -bs --define "_sourcedir $$work" --define "_srcrpmdir $(outdir)" "$$work/ytmcross.spec"
