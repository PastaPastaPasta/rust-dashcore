#!/usr/bin/env bash
# Fails unless every base SDK package in the workspace resolves to the one
# pinned revision of the local fork (A1 §1.1 rule R1). Checks the root
# manifest's declaration and [patch] tables, every member manifest, the
# committed Cargo.lock, the base SDK [patch] tables and lockfiles of the
# repository's other workspaces (node-fuzz, cs2-diff, any new one), and the
# source Cargo really builds (no path override, source replacement or edited
# Cargo checkout). See docs/node-program/BASE-SDK-FORK.md.
#
# Threat model (DEC-21): this catches accidental drift and misconfiguration,
# such as a member pinning base SDK itself, a second revision, a stale
# lockfile, a forgotten local [patch] or `paths` override, a stray Cargo
# config file or environment variable, or a hand-edited Cargo checkout. It
# does not defend against a local actor who can write ~/.cargo or the
# toolchain: forged Git objects or refs in Cargo's git cache (replace refs are
# ignored only because that is one environment variable), a one-off
# `--config` on a Cargo command line, or toolchain wrappers and flags
# (RUSTC_WRAPPER, RUSTFLAGS with --extern, a modified cargo or rustc).
#
# Usage: contrib/node/check-base-sdk-pins.sh
exec python3 "$(dirname "$0")/base_sdk_pins.py" check
