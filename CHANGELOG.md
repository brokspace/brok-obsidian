# Changelog

All notable changes to this project are documented in this file.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - Unreleased

### Added

- Mirrors an Obsidian vault served by the Local REST API plugin into
  `.brokspace/shards-data/brok-obsidian/vault`, read only (`GET` requests over HTTPS verified against the
  plugin's certificate authority), at start, on an interval and on `obsidian:sync`, publishing
  `obsidian:synced` (brokspace/brok#104).
